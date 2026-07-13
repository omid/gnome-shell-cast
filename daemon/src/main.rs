mod capture;
mod cast;
mod discovery;
mod http;
mod mirror;
mod pipeline;
mod session;

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::Result;
use log::{info, warn};
// parking_lot's Mutex has no poisoning, so `lock()` returns the guard directly
// with no `unwrap()`. We only ever hold these locks briefly and never across an
// `.await`, which is exactly what it's good for.
use parking_lot::Mutex;
use tokio::sync::{mpsc, oneshot};
use zbus::object_server::SignalEmitter;
use zbus::zvariant::OwnedValue;

use crate::discovery::Device;
use crate::pipeline::StreamSettings;

const BUS_NAME: &str = "org.gnome.ShellCast";
const OBJECT_PATH: &str = "/org/gnome/ShellCast";
/// The daemon exits after this long with no casting and no D-Bus calls.
const IDLE_EXIT: Duration = Duration::from_mins(10);

#[derive(Debug)]
pub enum Event {
    DevicesChanged,
    StateChanged,
}

pub struct SharedState {
    pub devices: Mutex<HashMap<String, Device>>,
    /// (state, `device_id`); state is one of idle|connecting|casting|error.
    pub status: Mutex<(String, String)>,
    /// Dropping the sender stops the running cast session.
    pub active: Mutex<Option<oneshot::Sender<()>>>,
    pub events: mpsc::UnboundedSender<Event>,
    pub last_activity: Mutex<Instant>,
    pub generation: AtomicU64,
}

impl SharedState {
    fn new(events: mpsc::UnboundedSender<Event>) -> Self {
        Self {
            devices: Mutex::new(HashMap::new()),
            status: Mutex::new(("idle".into(), String::new())),
            active: Mutex::new(None),
            events,
            last_activity: Mutex::new(Instant::now()),
            generation: AtomicU64::new(0),
        }
    }

    pub fn touch(&self) {
        *self.last_activity.lock() = Instant::now();
    }

    pub fn set_status(&self, state: &str, device_id: &str) {
        *self.status.lock() = (state.to_string(), device_id.to_string());
        self.touch();
        let _ = self.events.send(Event::StateChanged);
    }

    pub fn status(&self) -> (String, String) {
        self.status.lock().clone()
    }
}

struct ShellCast {
    state: Arc<SharedState>,
}

#[zbus::interface(name = "org.gnome.ShellCast1")]
impl ShellCast {
    async fn list_devices(&self) -> Vec<(String, String, String, u32)> {
        self.state.touch();
        let devices = self.state.devices.lock();
        let mut list: Vec<_> = devices
            .values()
            .map(|d| {
                (
                    d.id.clone(),
                    d.name.clone(),
                    format!("{}:{}", d.addr, d.port),
                    d.ca,
                )
            })
            .collect();
        list.sort_by(|a, b| a.1.cmp(&b.1));
        list
    }

    async fn get_status(&self) -> (String, String) {
        self.state.touch();
        self.state.status()
    }

    /// The daemon's own version, so the extension can detect a daemon that is
    /// older (or newer) than the version it was built against.
    async fn get_version(&self) -> String {
        env!("CARGO_PKG_VERSION").to_string()
    }

    async fn start_cast(
        &self,
        device_id: String,
        source: u32,
        options: HashMap<String, OwnedValue>,
    ) -> zbus::fdo::Result<()> {
        self.state.touch();

        let source = match source {
            0 => capture::SourceKind::Screen,
            1 => capture::SourceKind::Window,
            2 => capture::SourceKind::Audio,
            other => {
                return Err(zbus::fdo::Error::InvalidArgs(format!(
                    "unknown source type: {other}"
                )));
            }
        };

        let device = {
            let devices = self.state.devices.lock();
            let device = devices
                .get(&device_id)
                .ok_or_else(|| zbus::fdo::Error::Failed(format!("unknown device: {device_id}")))?;
            if !device.has_video() && source != capture::SourceKind::Audio {
                return Err(zbus::fdo::Error::Failed(format!(
                    "{} is audio-only and cannot receive screen casts",
                    device.name
                )));
            }
            device.clone()
        };

        let settings = StreamSettings::from_options(&options);
        info!(
            "start cast to {} ({}) with {settings:?}",
            device.name, device.addr
        );

        let (stop_tx, stop_rx) = oneshot::channel();
        // Dropping a previous sender (if any) makes that session's stop_rx
        // resolve, shutting the old cast down before the new one starts.
        *self.state.active.lock() = Some(stop_tx);
        let generation = self.state.generation.fetch_add(1, Ordering::SeqCst) + 1;

        tokio::spawn(session::run(
            self.state.clone(),
            generation,
            device,
            source,
            settings,
            stop_rx,
        ));
        Ok(())
    }

    async fn stop_cast(&self) {
        self.state.touch();
        if let Some(stop) = self.state.active.lock().take() {
            let _ = stop.send(());
        }
    }

    #[zbus(signal)]
    async fn devices_changed(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn state_changed(
        emitter: &SignalEmitter<'_>,
        state: &str,
        device_id: &str,
    ) -> zbus::Result<()>;
}

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    gstreamer::init()?;

    let (events_tx, mut events_rx) = mpsc::unbounded_channel();
    let state = Arc::new(SharedState::new(events_tx));

    // Claim the bus name first so D-Bus activation always succeeds promptly at
    // login; discovery is best-effort and must never delay or fail it.
    let connection = zbus::connection::Builder::session()?
        .name(BUS_NAME)?
        .serve_at(
            OBJECT_PATH,
            ShellCast {
                state: state.clone(),
            },
        )?
        .build()
        .await?;
    info!("listening on {BUS_NAME}");

    // mDNS discovery runs for the daemon's whole lifetime (best-effort).
    discovery::start(state.clone());

    // Forward internal events to D-Bus signals.
    let iface = connection
        .object_server()
        .interface::<_, ShellCast>(OBJECT_PATH)
        .await?;
    let signal_state = state.clone();
    let (bus_dead_tx, mut bus_dead_rx) = oneshot::channel::<()>();
    tokio::spawn(async move {
        while let Some(event) = events_rx.recv().await {
            let result = match event {
                Event::DevicesChanged => ShellCast::devices_changed(iface.signal_emitter()).await,
                Event::StateChanged => {
                    let (s, d) = signal_state.status();
                    ShellCast::state_changed(iface.signal_emitter(), &s, &d).await
                }
            };
            if let Err(e) = result {
                warn!("failed to emit signal: {e}");
                // An I/O error means the bus connection is gone for good; a
                // session daemon without its bus is useless, so shut down and
                // let D-Bus activation start a fresh instance when needed.
                if matches!(e, zbus::Error::InputOutput(_)) {
                    let _ = bus_dead_tx.send(());
                    break;
                }
            }
        }
    });

    // Exit when idle so the D-Bus-activated daemon doesn't linger forever.
    let mut tick = tokio::time::interval(Duration::from_secs(30));
    loop {
        tokio::select! {
            _ = tick.tick() => {}
            _ = &mut bus_dead_rx => {
                warn!("D-Bus connection lost, exiting");
                break;
            }
        }
        let (current, _) = state.status();
        let idle_for = state.last_activity.lock().elapsed();
        if (current == "idle" || current == "error") && idle_for > IDLE_EXIT {
            info!("idle for {idle_for:?}, exiting");
            break;
        }
    }

    Ok(())
}
