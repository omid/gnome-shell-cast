use std::net::{IpAddr, SocketAddr, TcpStream};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use log::{debug, info, warn};
use rust_cast::ChannelMessage;
use rust_cast::channels::connection::ConnectionChannel;
use rust_cast::channels::heartbeat::{HeartbeatChannel, HeartbeatResponse};
use rust_cast::channels::media::MediaChannel;
use rust_cast::channels::media::{Media, Metadata, MusicTrackMediaMetadata, StreamType};
use rust_cast::channels::receiver::{CastDeviceApp, ReceiverChannel};
use rust_cast::errors::Error as CastError;
use rust_cast::message_manager::MessageManager;
use rustls::{ClientConnection, StreamOwned};
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::oneshot;

use crate::tls;

const DESTINATION_ID: &str = "receiver-0";

type CastStream = StreamOwned<ClientConnection, TcpStream>;
type CastManager = MessageManager<CastStream>;

struct CastDeviceCompat {
    manager: Rc<CastManager>,
    connection: ConnectionChannel<'static, CastStream>,
    heartbeat: HeartbeatChannel<'static, CastStream>,
    media: MediaChannel<'static, CastStream>,
    receiver: ReceiverChannel<'static, CastStream>,
}

impl CastDeviceCompat {
    fn receive(&self) -> Result<ChannelMessage, CastError> {
        let message = self.manager.receive()?;
        if self.connection.can_handle(&message) {
            return Ok(ChannelMessage::Connection(self.connection.parse(&message)?));
        }
        if self.heartbeat.can_handle(&message) {
            return Ok(ChannelMessage::Heartbeat(self.heartbeat.parse(&message)?));
        }
        if self.media.can_handle(&message) {
            return Ok(ChannelMessage::Media(self.media.parse(&message)?));
        }
        if self.receiver.can_handle(&message) {
            return Ok(ChannelMessage::Receiver(self.receiver.parse(&message)?));
        }
        Ok(ChannelMessage::Raw(message))
    }
}

/// The media to load once the stream is ready: URL and HTTP content type (HLS
/// playlist for screen casts, a progressive audio type for audio-only casts).
#[derive(Debug)]
pub struct LoadMedia {
    pub url: String,
    pub content_type: String,
    /// Now-playing title shown by the receiver; `None` leaves it untitled.
    pub title: Option<String>,
    /// Secondary line under the title, e.g. the casting computer's hostname.
    pub artist: Option<String>,
}

#[derive(Debug)]
pub enum CastEvent {
    /// The receiver app launched and accepted the media URL.
    Playing,
    /// The connection ended or failed; the session should shut down.
    Ended(String),
}

/// Keeps the `CASTv2` connection to the Chromecast alive on a dedicated thread
/// (the `rust_cast` API is blocking). Setting `stop` asks the thread to stop
/// the receiver app and disconnect; the device pings every few seconds, so
/// the flag is noticed within roughly that interval.
pub struct CastControl {
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

pub fn start(
    addr: IpAddr,
    port: u16,
    url_rx: oneshot::Receiver<LoadMedia>,
    events: UnboundedSender<CastEvent>,
) -> CastControl {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_flag = Arc::clone(&stop);

    #[allow(
        clippy::expect_used,
        reason = "spawning a thread only fails on OS resource exhaustion, which is unrecoverable"
    )]
    let handle = thread::Builder::new()
        .name("cast-control".into())
        .spawn(move || {
            if let Err(e) = run(addr, port, url_rx, &stop_flag, &events) {
                warn!("cast control ended: {e}");
                let _ = events.send(CastEvent::Ended(e.to_string()));
            } else {
                let _ = events.send(CastEvent::Ended("stopped".into()));
            }
        })
        .expect("failed to spawn cast-control thread");

    CastControl {
        stop,
        handle: Some(handle),
    }
}

fn run(
    addr: IpAddr,
    port: u16,
    mut url_rx: oneshot::Receiver<LoadMedia>,
    stop: &AtomicBool,
    events: &UnboundedSender<CastEvent>,
) -> Result<()> {
    info!("connecting to chromecast at {addr}:{port}");
    let device = connect(addr, port)?;

    device
        .connection
        .connect(DESTINATION_ID)
        .map_err(|e| anyhow!("handshake: {e}"))?;
    device.heartbeat.ping().map_err(|e| anyhow!("ping: {e}"))?;

    let app = device
        .receiver
        .launch_app(&CastDeviceApp::DefaultMediaReceiver)
        .map_err(|e| anyhow!("launching media receiver: {e}"))?;
    device
        .connection
        .connect(app.transport_id.as_str())
        .map_err(|e| anyhow!("connecting to app: {e}"))?;

    // The encoder is warming up in parallel; wait for the stream URL. Poll so
    // a stop request (or the session failing before a URL exists) still gets
    // the receiver app shut down.
    let media = loop {
        if stop.load(Ordering::Relaxed) {
            info!("stopping receiver app");
            let _ = device.receiver.stop_app(app.session_id.as_str());
            return Ok(());
        }
        match url_rx.try_recv() {
            Ok(media) => break media,
            Err(oneshot::error::TryRecvError::Empty) => {
                thread::sleep(Duration::from_millis(100));
            }
            Err(oneshot::error::TryRecvError::Closed) => {
                let _ = device.receiver.stop_app(app.session_id.as_str());
                return Err(anyhow!("session ended before the stream was ready"));
            }
        }
    };

    info!("loading {} ({})", media.url, media.content_type);
    let metadata = (media.title.is_some() || media.artist.is_some()).then(|| {
        Metadata::MusicTrack(MusicTrackMediaMetadata {
            title: media.title,
            artist: media.artist,
            ..Default::default()
        })
    });
    device
        .media
        .load(
            app.transport_id.as_str(),
            app.session_id.as_str(),
            &Media {
                content_id: media.url,
                content_type: media.content_type,
                stream_type: StreamType::Live,
                duration: None,
                metadata,
            },
        )
        .map_err(|e| anyhow!("loading media: {e}"))?;
    let _ = events.send(CastEvent::Playing);

    // Keep the sender connection alive: the Default Media Receiver tears
    // itself down when its last sender disappears.
    loop {
        if stop.load(Ordering::Relaxed) {
            info!("stopping receiver app");
            let _ = device.receiver.stop_app(app.session_id.as_str());
            return Ok(());
        }

        match device.receive() {
            Ok(ChannelMessage::Heartbeat(response)) => {
                if matches!(response, HeartbeatResponse::Ping) {
                    device.heartbeat.pong().map_err(|e| anyhow!("pong: {e}"))?;
                }
            }
            Ok(message) => debug!("cast message: {message:?}"),
            Err(e) => {
                if stop.load(Ordering::Relaxed) {
                    return Ok(());
                }
                return Err(anyhow!("connection lost: {e}"));
            }
        }
    }
}

fn connect(addr: IpAddr, port: u16) -> Result<CastDeviceCompat> {
    let socket_addr = SocketAddr::new(addr, port);
    let tcp = TcpStream::connect_timeout(&socket_addr, Duration::from_secs(4))
        .with_context(|| format!("connecting to {socket_addr}"))?;
    let config = tls::client_config();
    let server_name = rustls::pki_types::ServerName::try_from(addr.to_string())
        .context("building TLS server name")?;
    let connection =
        ClientConnection::new(Arc::new(config), server_name).context("creating TLS connection")?;
    let manager = Rc::new(MessageManager::new(StreamOwned::new(connection, tcp)));
    Ok(CastDeviceCompat {
        connection: ConnectionChannel::new("sender-0", Rc::clone(&manager)),
        heartbeat: HeartbeatChannel::new("sender-0", DESTINATION_ID, Rc::clone(&manager)),
        media: MediaChannel::new("sender-0", Rc::clone(&manager)),
        receiver: ReceiverChannel::new("sender-0", DESTINATION_ID, Rc::clone(&manager)),
        manager,
    })
}

impl Drop for CastControl {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}
