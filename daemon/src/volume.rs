use std::net::{IpAddr, SocketAddr, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use log::{debug, warn};
use rust_cast::NoCertificateVerification;
use rust_cast::message_manager::{CastMessage, CastMessagePayload, MessageManager};
use rustls::{ClientConnection, StreamOwned};
use serde_json::{Value, json};

// Our own sender id, distinct from rust_cast's "sender-0": two connections
// sharing an id look like one virtual connection to the device, so a separate
// id keeps this one from disrupting the cast session.
const SENDER_ID: &str = "sender-gsc-volume";
const RECEIVER_ID: &str = "receiver-0";
const NS_CONNECTION: &str = "urn:x-cast:com.google.cast.tp.connection";
const NS_HEARTBEAT: &str = "urn:x-cast:com.google.cast.tp.heartbeat";
const NS_RECEIVER: &str = "urn:x-cast:com.google.cast.receiver";

/// Socket read timeout; also paces the run loop, so a requested volume is
/// applied within roughly this long.
const READ_TIMEOUT: Duration = Duration::from_millis(200);
const PING_INTERVAL: Duration = Duration::from_secs(5);

type Manager = MessageManager<StreamOwned<ClientConnection, TcpStream>>;

/// Controls the Chromecast's receiver volume over its own connection (the
/// extension's slider drives it via `SetVolume`). Reconnects on demand and
/// answers heartbeats so the device keeps the connection alive; dropping this
/// stops the worker thread.
pub struct VolumeControl {
    tx: Sender<f32>,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl VolumeControl {
    /// Starts the worker. `on_level` reports the receiver's volume (0.0-1.0) on
    /// the initial read and after each change, for the daemon to push to the slider.
    pub fn start<F: Fn(f32) + Send + 'static>(
        addr: IpAddr,
        port: u16,
        on_level: F,
    ) -> VolumeControl {
        let (tx, rx) = channel();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_flag = stop.clone();
        let handle = thread::Builder::new()
            .name("cast-volume".into())
            .spawn(move || run(addr, port, &rx, &stop_flag, &on_level))
            .ok();
        VolumeControl { tx, stop, handle }
    }

    /// A handle for requesting volume levels; sending never blocks.
    pub fn sender(&self) -> Sender<f32> {
        self.tx.clone()
    }
}

impl Drop for VolumeControl {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn run(
    addr: IpAddr,
    port: u16,
    rx: &Receiver<f32>,
    stop: &AtomicBool,
    on_level: &(dyn Fn(f32) + Send),
) {
    let mut manager: Option<Manager> = None;
    let mut request_id: u32 = 0;
    let mut last_ping = Instant::now();

    while !stop.load(Ordering::Relaxed) {
        // Reconnect if needed and read the current volume up front.
        if manager.is_none() {
            match connect(addr, port) {
                Ok(m) => {
                    request_id += 1;
                    let _ = send(
                        &m,
                        NS_RECEIVER,
                        RECEIVER_ID,
                        &json!({"type": "GET_STATUS", "requestId": request_id}),
                    );
                    manager = Some(m);
                    last_ping = Instant::now();
                }
                Err(e) => {
                    warn!("cast volume: connect failed: {e:#}");
                    // Back off without busy-looping, staying responsive to stop.
                    for _ in 0..10 {
                        if stop.load(Ordering::Relaxed) {
                            return;
                        }
                        thread::sleep(Duration::from_millis(200));
                    }
                    continue;
                }
            }
        }
        let Some(m) = manager.as_ref() else {
            continue;
        };

        // Apply the most recent requested level (coalesce a drag's worth).
        let mut level = None;
        while let Ok(next) = rx.try_recv() {
            level = Some(next);
        }
        if let Some(level) = level {
            request_id += 1;
            if let Err(e) = send(
                m,
                NS_RECEIVER,
                RECEIVER_ID,
                &json!({"type": "SET_VOLUME", "volume": {"level": level}, "requestId": request_id}),
            ) {
                debug!("cast volume: set failed, reconnecting: {e}");
                manager = None;
                continue;
            }
            on_level(level);
        }

        // Keep the connection alive so the device does not drop it.
        if last_ping.elapsed() >= PING_INTERVAL {
            let _ = send(m, NS_HEARTBEAT, RECEIVER_ID, &json!({"type": "PING"}));
            last_ping = Instant::now();
        }

        // Service incoming messages (heartbeats, volume updates); the read
        // timeout paces the loop.
        match m.receive() {
            Ok(message) => handle(m, &message, on_level),
            Err(rust_cast::errors::Error::Io(e))
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(e) => {
                debug!("cast volume: connection lost: {e}");
                manager = None;
            }
        }
    }
}

fn handle(manager: &Manager, message: &CastMessage, on_level: &(dyn Fn(f32) + Send)) {
    let Some(payload) = json_payload(message) else {
        return;
    };
    match (
        message.namespace.as_str(),
        payload["type"].as_str().unwrap_or(""),
    ) {
        (NS_HEARTBEAT, "PING") => {
            let _ = send(
                manager,
                NS_HEARTBEAT,
                &message.source,
                &json!({"type": "PONG"}),
            );
        }
        (NS_RECEIVER, "RECEIVER_STATUS") => {
            if let Some(level) = payload["status"]["volume"]["level"].as_f64() {
                on_level(level as f32);
            }
        }
        _ => {}
    }
}

fn connect(addr: IpAddr, port: u16) -> Result<Manager> {
    let socket_addr = SocketAddr::new(addr, port);
    let tcp = TcpStream::connect_timeout(&socket_addr, Duration::from_secs(4))
        .with_context(|| format!("connecting to {socket_addr}"))?;
    tcp.set_read_timeout(Some(READ_TIMEOUT))?;
    tcp.set_nodelay(true)?;

    let config = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(NoCertificateVerification))
        .with_no_client_auth();
    let server_name = rustls::pki_types::ServerName::try_from(addr.to_string())
        .context("building TLS server name")?;
    let connection =
        ClientConnection::new(Arc::new(config), server_name).context("creating TLS connection")?;
    let manager = MessageManager::new(StreamOwned::new(connection, tcp));

    send(
        &manager,
        NS_CONNECTION,
        RECEIVER_ID,
        &json!({"type": "CONNECT", "userAgent": "gnome-shell-cast"}),
    )?;
    Ok(manager)
}

fn send(manager: &Manager, namespace: &str, destination: &str, payload: &Value) -> Result<()> {
    manager
        .send(CastMessage {
            namespace: namespace.to_string(),
            source: SENDER_ID.to_string(),
            destination: destination.to_string(),
            payload: CastMessagePayload::String(payload.to_string()),
        })
        .map_err(|e| anyhow!("sending cast message: {e}"))
}

fn json_payload(message: &CastMessage) -> Option<Value> {
    match &message.payload {
        CastMessagePayload::String(s) => serde_json::from_str(s).ok(),
        CastMessagePayload::Binary(_) => None,
    }
}
