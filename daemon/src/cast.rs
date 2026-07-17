use std::net::IpAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use anyhow::{Result, anyhow};
use log::{debug, info, warn};
use rust_cast::channels::heartbeat::HeartbeatResponse;
use rust_cast::channels::media::{Media, Metadata, MusicTrackMediaMetadata, StreamType};
use rust_cast::channels::receiver::CastDeviceApp;
use rust_cast::{CastDevice, ChannelMessage};
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::oneshot;

const DESTINATION_ID: &str = "receiver-0";

/// What to hand the Default Media Receiver once the stream is ready: the URL
/// and its HTTP content type (HLS playlist for screen casts, a progressive
/// audio type like `audio/mpeg` for audio-only casts).
#[derive(Debug)]
pub struct LoadMedia {
    pub url: String,
    pub content_type: String,
    /// Now-playing title shown by the receiver (the built-in Default Media
    /// Receiver app's own name can't be changed); `None` leaves it untitled.
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
    let stop_flag = stop.clone();

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
    let device = CastDevice::connect_without_host_verification(addr.to_string(), port)
        .map_err(|e| anyhow!("connecting: {e}"))?;

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

impl Drop for CastControl {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}
