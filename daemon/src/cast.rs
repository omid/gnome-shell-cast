use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;

use anyhow::{anyhow, Result};
use log::{debug, info, warn};
use rust_cast::channels::heartbeat::HeartbeatResponse;
use rust_cast::channels::media::{Media, StreamType};
use rust_cast::channels::receiver::CastDeviceApp;
use rust_cast::{CastDevice, ChannelMessage};
use tokio::sync::mpsc::UnboundedSender;

const DESTINATION_ID: &str = "receiver-0";

#[derive(Debug)]
pub enum CastEvent {
    /// The receiver app launched and accepted the media URL.
    Playing,
    /// The connection ended or failed; the session should shut down.
    Ended(String),
}

/// Keeps the CASTv2 connection to the Chromecast alive on a dedicated thread
/// (the rust_cast API is blocking). Setting `stop` asks the thread to stop
/// the receiver app and disconnect; the device pings every few seconds, so
/// the flag is noticed within roughly that interval.
pub struct CastControl {
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

pub fn start(
    addr: IpAddr,
    port: u16,
    url: String,
    events: UnboundedSender<CastEvent>,
) -> CastControl {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_flag = stop.clone();

    let handle = thread::Builder::new()
        .name("cast-control".into())
        .spawn(move || {
            if let Err(e) = run(addr, port, &url, &stop_flag, &events) {
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
    url: &str,
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

    info!("loading {url}");
    device
        .media
        .load(
            app.transport_id.as_str(),
            app.session_id.as_str(),
            &Media {
                content_id: url.to_string(),
                content_type: "application/vnd.apple.mpegurl".to_string(),
                stream_type: StreamType::Live,
                duration: None,
                metadata: None,
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
