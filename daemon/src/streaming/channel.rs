//! Cast control channel for a mirroring session: connects over TLS (castv2
//! framing via `rust_cast`'s public `MessageManager`), launches the Chrome
//! Mirroring receiver app, performs the OFFER/ANSWER exchange, then babysits
//! the connection (heartbeat, receiver-side close) until stopped.

use std::io::ErrorKind;
use std::net::{IpAddr, SocketAddr, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use log::{debug, info, warn};
use rust_cast::message_manager::{CastMessage, CastMessagePayload, MessageManager};
use rustls::{ClientConnection, StreamOwned};
use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedSender;

use super::openscreen::messages::{self, Answer};
use crate::tls;

const SENDER_ID: &str = "sender-0";
const RECEIVER_ID: &str = "receiver-0";
const NS_CONNECTION: &str = "urn:x-cast:com.google.cast.tp.connection";
const NS_HEARTBEAT: &str = "urn:x-cast:com.google.cast.tp.heartbeat";
const NS_RECEIVER: &str = "urn:x-cast:com.google.cast.receiver";

/// How long the whole launch + OFFER/ANSWER negotiation may take before we
/// declare mirroring unavailable and fall back to HLS.
const NEGOTIATION_TIMEOUT: Duration = Duration::from_secs(12);
/// Socket read timeout; doubles as the tick interval of the run loop.
const READ_TIMEOUT: Duration = Duration::from_secs(1);
const PING_INTERVAL: Duration = Duration::from_secs(5);
/// How long we wait for the receiver to confirm the mirroring app has stopped.
const STOP_CONFIRM_TIMEOUT: Duration = Duration::from_secs(3);

type Manager = MessageManager<StreamOwned<ClientConnection, TcpStream>>;

pub enum ChannelEvent {
    /// The receiver ended the session (app closed, connection lost, ...).
    Ended(String),
}

pub struct MirrorChannel {
    manager: Manager,
    session_id: String,
    transport_id: String,
    request_id: u32,
    last_ping_at: Option<Instant>,
}

impl MirrorChannel {
    /// Connects, launches the mirroring app and negotiates the streams in
    /// `offer_body`. Blocking; run on a dedicated thread.
    pub fn negotiate(addr: IpAddr, port: u16, offer_body: &Value) -> Result<(Self, Answer)> {
        let Some(deadline) = Instant::now().checked_add(NEGOTIATION_TIMEOUT) else {
            bail!("the monotonic clock is at its maximum");
        };
        let manager = connect_tls(addr, port)?;
        let mut channel = Self {
            manager,
            session_id: String::new(),
            transport_id: String::new(),
            request_id: 0,
            last_ping_at: None,
        };

        channel.send_json(
            NS_CONNECTION,
            RECEIVER_ID,
            &json!({"type": "CONNECT", "userAgent": "gnome-shell-cast"}),
        )?;

        // LAUNCH and wait for a RECEIVER_STATUS that contains the app.
        let launch_request = channel.next_request_id();
        channel.send_json(
            NS_RECEIVER,
            RECEIVER_ID,
            &json!({"type": "LAUNCH", "appId": messages::MIRRORING_APP_ID,
                    "requestId": launch_request}),
        )?;
        let (session_id, transport_id) = channel.wait_for_app(deadline)?;
        info!("mirroring app launched (session {session_id})");
        channel.session_id = session_id;
        channel.transport_id.clone_from(&transport_id);

        // Open a virtual connection to the app itself, then OFFER.
        channel.send_json(NS_CONNECTION, &transport_id, &json!({"type": "CONNECT"}))?;
        let seq_num = offer_body
            .get("seqNum")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        channel.send_json(messages::WEBRTC_NAMESPACE, &transport_id, offer_body)?;
        let answer = channel.wait_for_answer(seq_num, deadline)?;
        info!(
            "receiver ANSWER: udp port {}, streams {:?}",
            answer.udp_port, answer.send_indexes
        );
        Ok((channel, answer))
    }

    /// Runs the channel until `stop` is set or the receiver ends the session.
    /// Consumes the channel; stopping the receiver app on the way out.
    pub fn run(mut self, stop: &AtomicBool, events: &UnboundedSender<ChannelEvent>) {
        let reason = loop {
            if stop.load(Ordering::Relaxed) {
                break None;
            }
            self.maybe_ping();
            match self.receive_tick() {
                Ok(Some(reason)) => break Some(reason),
                Ok(None) => {}
                // Detail to the journal; the user just gets "lost".
                Err(e) => {
                    warn!("mirror channel connection lost: {e:#}");
                    break Some("the connection was lost".into());
                }
            }
        };

        info!("stopping mirroring receiver app");
        let request_id = self.next_request_id();
        let _ = self.send_json(
            NS_RECEIVER,
            RECEIVER_ID,
            &json!({"type": "STOP", "sessionId": self.session_id, "requestId": request_id}),
        );
        self.await_app_stopped();
        if let Some(reason) = reason {
            let _ = events.send(ChannelEvent::Ended(reason));
        }
    }

    /// One receive attempt; `Ok(None)` when the read timed out, as it does on
    /// every idle tick.
    fn try_receive(&mut self) -> Result<Option<CastMessage>> {
        match self.manager.receive() {
            Ok(message) => Ok(Some(message)),
            Err(rust_cast::errors::Error::Io(e))
                if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) =>
            {
                Ok(None)
            }
            Err(e) => Err(anyhow!("{e}")),
        }
    }

    fn pong(&self, destination: &str) -> Result<()> {
        self.send_json(NS_HEARTBEAT, destination, &json!({"type": "PONG"}))
    }

    /// Waits (briefly) for the receiver to report the mirroring app gone;
    /// relaunching it mid-shutdown gets the new session killed seconds in.
    fn await_app_stopped(&mut self) {
        let Some(deadline) = Instant::now().checked_add(STOP_CONFIRM_TIMEOUT) else {
            return;
        };
        while Instant::now() < deadline {
            let message = match self.try_receive() {
                Ok(Some(message)) => message,
                Ok(None) => continue,
                // The connection is already gone, so the app is too.
                Err(_) => return,
            };
            let Some(payload) = json_payload(&message) else {
                continue;
            };
            match payload.get("type").and_then(Value::as_str).unwrap_or("") {
                "PING" => {
                    let _ = self.pong(&message.source);
                }
                "RECEIVER_STATUS" if !status_has_session(&payload, &self.session_id) => return,
                _ => {}
            }
        }
        debug!("receiver never confirmed the mirroring app stopped");
    }

    /// One receive cycle. Returns Ok(Some(reason)) when the session ended.
    fn receive_tick(&mut self) -> Result<Option<String>> {
        let Some(message) = self.try_receive()? else {
            return Ok(None);
        };
        let Some(payload) = json_payload(&message) else {
            return Ok(None);
        };
        let message_type = payload.get("type").and_then(Value::as_str).unwrap_or("");
        match (message.namespace.as_str(), message_type) {
            (NS_HEARTBEAT, "PING") => {
                self.pong(&message.source)?;
            }
            (NS_CONNECTION, "CLOSE") if message.source == self.transport_id => {
                return Ok(Some("the device closed the session".into()));
            }
            (NS_RECEIVER, "RECEIVER_STATUS") => {
                if !status_has_session(&payload, &self.session_id) {
                    return Ok(Some("the cast was stopped on the device".into()));
                }
            }
            _ => debug!("mirror channel message on {}: {payload}", message.namespace),
        }
        Ok(None)
    }

    fn maybe_ping(&mut self) {
        let due = self
            .last_ping_at
            .is_none_or(|t| t.elapsed() >= PING_INTERVAL);
        if due {
            let _ = self.send_json(NS_HEARTBEAT, RECEIVER_ID, &json!({"type": "PING"}));
            self.last_ping_at = Some(Instant::now());
        }
    }

    fn wait_for_app(&mut self, deadline: Instant) -> Result<(String, String)> {
        loop {
            if Instant::now() > deadline {
                bail!("timed out waiting for the mirroring app to launch");
            }
            let Some(message) = self.try_receive().context("waiting for launch")? else {
                continue;
            };
            let Some(payload) = json_payload(&message) else {
                continue;
            };
            match payload.get("type").and_then(Value::as_str).unwrap_or("") {
                "PING" => {
                    self.pong(&message.source)?;
                }
                "LAUNCH_ERROR" => {
                    bail!(
                        "mirroring app launch failed: {}",
                        payload.get("reason").unwrap_or(&Value::Null)
                    );
                }
                "RECEIVER_STATUS" => {
                    if let Some(app) = find_app(&payload, messages::MIRRORING_APP_ID) {
                        let session = app["sessionId"].as_str().unwrap_or_default();
                        let transport = app["transportId"].as_str().unwrap_or_default();
                        if !session.is_empty() && !transport.is_empty() {
                            return Ok((session.to_owned(), transport.to_owned()));
                        }
                    }
                }
                _ => {}
            }
        }
    }

    fn wait_for_answer(&mut self, seq_num: u64, deadline: Instant) -> Result<Answer> {
        loop {
            if Instant::now() > deadline {
                bail!("timed out waiting for the receiver's ANSWER");
            }
            let Some(message) = self.try_receive().context("waiting for ANSWER")? else {
                continue;
            };
            let Some(payload) = json_payload(&message) else {
                continue;
            };
            match (
                message.namespace.as_str(),
                payload.get("type").and_then(Value::as_str).unwrap_or(""),
            ) {
                (NS_HEARTBEAT, "PING") => {
                    self.pong(&message.source)?;
                }
                (messages::WEBRTC_NAMESPACE, "ANSWER") => {
                    if payload.get("seqNum").and_then(Value::as_u64) == Some(seq_num) {
                        return messages::parse_answer(&payload);
                    }
                }
                _ => debug!("pre-answer message: {payload}"),
            }
        }
    }

    fn send_json(&self, namespace: &str, destination: &str, payload: &Value) -> Result<()> {
        self.manager
            .send(CastMessage {
                namespace: namespace.to_owned(),
                source: SENDER_ID.to_owned(),
                destination: destination.to_owned(),
                payload: CastMessagePayload::String(payload.to_string()),
            })
            .map_err(|e| anyhow!("sending cast message: {e}"))
    }

    fn next_request_id(&mut self) -> u32 {
        self.request_id = self.request_id.wrapping_add(1);
        self.request_id
    }
}

fn connect_tls(addr: IpAddr, port: u16) -> Result<Manager> {
    let socket_addr = SocketAddr::new(addr, port);
    let tcp = TcpStream::connect_timeout(&socket_addr, Duration::from_secs(4))
        .with_context(|| format!("connecting to {socket_addr}"))?;
    tcp.set_read_timeout(Some(READ_TIMEOUT))?;
    tcp.set_nodelay(true)?;

    let config = tls::client_config();
    let server_name = rustls::pki_types::ServerName::try_from(addr.to_string())
        .context("building TLS server name")?;
    let connection =
        ClientConnection::new(Arc::new(config), server_name).context("creating TLS connection")?;
    Ok(MessageManager::new(StreamOwned::new(connection, tcp)))
}

fn json_payload(message: &CastMessage) -> Option<Value> {
    match &message.payload {
        CastMessagePayload::String(s) => serde_json::from_str(s).ok(),
        CastMessagePayload::Binary(_) => None,
    }
}

fn applications(status: &Value) -> Option<&Vec<Value>> {
    status
        .get("status")
        .and_then(|status| status.get("applications"))
        .and_then(Value::as_array)
}

fn find_app<'a>(status: &'a Value, app_id: &str) -> Option<&'a Value> {
    applications(status)?
        .iter()
        .find(|app| app.get("appId").and_then(Value::as_str) == Some(app_id))
}

fn status_has_session(status: &Value, session_id: &str) -> bool {
    applications(status).is_some_and(|apps| {
        apps.iter()
            .any(|app| app.get("sessionId").and_then(Value::as_str) == Some(session_id))
    })
}

/// Owns the channel run-loop thread; dropping stops the receiver app.
pub struct ChannelControl {
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl ChannelControl {
    pub fn spawn(channel: MirrorChannel, events: UnboundedSender<ChannelEvent>) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_flag = Arc::clone(&stop);
        #[allow(
            clippy::expect_used,
            reason = "spawning a thread only fails on OS resource exhaustion, which is unrecoverable"
        )]
        let handle = thread::Builder::new()
            .name("mirror-channel".into())
            .spawn(move || channel.run(&stop_flag, &events))
            .expect("failed to spawn mirror-channel thread");
        Self {
            stop,
            handle: Some(handle),
        }
    }
}

impl Drop for ChannelControl {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take()
            && handle.join().is_err()
        {
            warn!("mirror-channel thread panicked");
        }
    }
}
