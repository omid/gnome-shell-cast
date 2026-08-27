use std::fmt;
use std::os::fd::OwnedFd;
use std::path::PathBuf;

use anyhow::{Context, Result, anyhow};
use ashpd::desktop::screencast::{
    CursorMode, OpenPipeWireRemoteOptions, Screencast, SelectSourcesOptions, SourceType,
    StartCastOptions,
};
use ashpd::desktop::{CreateSessionOptions, PersistMode, ResponseError, Session};
use ashpd::enumflags2::BitFlags;
use futures_util::StreamExt;
use log::{info, warn};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    Screen,
    Window,
    /// Screen or window, picked in GNOME's "Display / Window" portal dialog.
    Choose,
    /// A monitor that does not exist yet: mutter creates one for the cast and
    /// destroys it when the portal session closes, so the receiver becomes a
    /// second desktop rather than a copy of an existing one.
    Virtual,
    /// System audio only (for audio-only receivers); no portal involved.
    Audio,
}

/// The user dismissed the portal's screen-picker dialog. Not an error: the
/// caller should quietly return to idle rather than surfacing a failure.
#[derive(Debug)]
pub struct Cancelled;

impl fmt::Display for Cancelled {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("screen share cancelled by the user")
    }
}

impl std::error::Error for Cancelled {}

/// An open XDG `ScreenCast` portal session. The `PipeWire` stream stays alive
/// for as long as this struct (and in particular `session`) is kept around;
/// dropping it closes the portal session, which is what makes GNOME's "screen
/// is being shared" indicator disappear - and, for a virtual monitor, what
/// removes the monitor again.
pub struct Capture {
    pub fd: OwnedFd,
    pub node_id: u32,
    /// The frame size to ask `pipewiresrc` for, set only for a virtual monitor.
    /// `RecordVirtual` takes no size: mutter creates the monitor at whatever the
    /// *consumer* negotiates, so requesting it is the only way to choose it. A
    /// real monitor or window has its own size and this stays `None`.
    pub source_size: Option<(i32, i32)>,
    session: Option<Session<Screencast>>,
}

impl Capture {
    /// A monitor mutter created for this cast, rather than something that was
    /// already on screen. The distinction matters to the pipeline: the source
    /// size is ours to pick, and must not be changed once the monitor exists.
    pub fn is_virtual(&self) -> bool {
        self.source_size.is_some()
    }

    /// Resolves when the *compositor* ends the session - GNOME's screen-sharing
    /// indicator has a stop button, and pressing it leaves the capture dead
    /// while `pipewiresrc resend-last=true` happily reships the last frame
    /// forever, so the cast has to be told. Never resolves for an audio-only
    /// cast, which has no portal session.
    pub async fn closed(&self) {
        let Some(session) = self.session.as_ref() else {
            return std::future::pending().await;
        };
        match session.receive_closed().await {
            Ok(mut closed) => {
                closed.next().await;
            }
            Err(e) => {
                warn!("cannot watch for the portal session closing: {e}");
                std::future::pending::<()>().await;
            }
        }
    }

    /// Closes the portal session, waiting for the compositor. A cast started
    /// right after would otherwise race this teardown and get a dead stream -
    /// or, for a virtual monitor, leave two of them on screen at once.
    pub async fn close(mut self) {
        let Some(session) = self.session.take() else {
            return;
        };
        match session.close().await {
            Ok(()) => info!("closed screen-cast portal session"),
            Err(e) => warn!("closing screen-cast portal session: {e}"),
        }
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        // Safety net for paths that skip `close()`; without it the compositor
        // keeps showing the screen-sharing indicator. Async, so spawn it.
        let Some(session) = self.session.take() else {
            return;
        };
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                handle.spawn(async move {
                    match session.close().await {
                        // Logged so the portal teardown is visible in the
                        // journal - this is what clears GNOME's sharing icon.
                        Ok(()) => info!("closed screen-cast portal session"),
                        Err(e) => warn!("closing screen-cast portal session: {e}"),
                    }
                });
            }
            // A virtual monitor outlives us here, until the portal notices our
            // bus name go away. Nothing else can clean it up from this point.
            Err(_) => warn!("no tokio runtime available to close the portal session"),
        }
    }
}

/// Asks the portal for a capture. GNOME shows its native source picker dialog as
/// part of this call - except for screen and virtual-monitor casts with a saved
/// restore token, which reuse the previous selection without a dialog.
///
/// `requested_size` is honoured only for `SourceKind::Virtual`, where it becomes
/// the new monitor's resolution.
pub async fn open(source: SourceKind, requested_size: Option<(i32, i32)>) -> Result<Capture> {
    if source == SourceKind::Virtual {
        // The portal accepts a Virtual request on X11 and mutter then fails much
        // later, at PipeWire negotiation, with nothing reaching us but the
        // session closing. Refuse up front so the user gets a reason instead of
        // a cast that appears to start and vanishes. `AvailableSourceTypes` is
        // no help: the GNOME portal reports Virtual unconditionally.
        if std::env::var("XDG_SESSION_TYPE").as_deref() == Ok("x11") {
            return Err(anyhow!(
                "Casting a new monitor needs a Wayland session; this one is X11."
            ));
        }
    }

    let proxy = Screencast::new()
        .await
        .context("connecting to the ScreenCast portal")?;
    let session = proxy
        .create_session(CreateSessionOptions::default())
        .await?;

    // Persist the selection for the whole-desktop kinds only: re-picking the
    // same monitor every time is pure friction, while window casts usually mean
    // a *different* window, so those should always show the picker.
    let (source_type, persist, restore_token) = match source {
        SourceKind::Screen => (
            BitFlags::from(SourceType::Monitor),
            PersistMode::ExplicitlyRevoked,
            load_restore_token(source),
        ),
        SourceKind::Window => (BitFlags::from(SourceType::Window), PersistMode::DoNot, None),
        // Only ever one option, which the portal pre-selects, so this is a
        // single confirmation the first time and no dialog at all after that.
        SourceKind::Virtual => (
            BitFlags::from(SourceType::Virtual),
            PersistMode::ExplicitlyRevoked,
            load_restore_token(source),
        ),
        // Never persisted: the whole point is to be asked again next time.
        SourceKind::Choose => (choose_source_types(), PersistMode::DoNot, None),
        SourceKind::Audio => return Err(anyhow!("audio-only casts do not use the portal")),
    };
    proxy
        .select_sources(
            &session,
            SelectSourcesOptions::default()
                .set_cursor_mode(CursorMode::Embedded)
                .set_sources(source_type)
                .set_multiple(false)
                .set_persist_mode(persist)
                .set_restore_token(restore_token.as_deref()),
        )
        .await
        .map_err(|e| map_cancel(&e))?;

    let response = proxy
        .start(&session, None, StartCastOptions::default())
        .await
        .map_err(|e| map_cancel(&e))?
        .response()
        .map_err(|e| map_cancel(&e))?;

    if matches!(source, SourceKind::Screen | SourceKind::Virtual)
        && let Some(token) = response.restore_token()
    {
        save_restore_token(source, token);
    }

    let stream = response
        .streams()
        .first()
        .ok_or_else(|| anyhow!("portal returned no streams"))?;
    let node_id = stream.pipe_wire_node_id();

    // Trust the response over the request. The fixed source caps a virtual
    // monitor needs are unsatisfiable against a real monitor, whose size the
    // portal offers as one fixed value, so guessing wrong here would fail the
    // pipeline with "No supported formats found" rather than degrade.
    let is_virtual = match stream.source_type() {
        Some(kind) => kind == SourceType::Virtual,
        None => source == SourceKind::Virtual,
    };
    let source_size = if is_virtual { requested_size } else { None };

    let fd = proxy
        .open_pipe_wire_remote(&session, OpenPipeWireRemoteOptions::default())
        .await
        .context("opening the PipeWire remote")?;

    // `stream.size()` is always absent for a virtual stream (mutter reports no
    // parameters for one), so what we asked for is the only size worth logging.
    match source_size {
        Some((w, h)) => info!("portal capture ready, pipewire node {node_id}, virtual {w}x{h}"),
        None => info!("portal capture ready, pipewire node {node_id}"),
    }
    Ok(Capture {
        fd,
        node_id,
        source_size,
        session: Some(session),
    })
}

/// What the "choose what to cast" dialog may offer. A virtual monitor is only
/// listed where mutter can actually make one.
fn choose_source_types() -> BitFlags<SourceType> {
    let base = SourceType::Monitor | SourceType::Window;
    if std::env::var("XDG_SESSION_TYPE").as_deref() == Ok("x11") {
        base
    } else {
        base | SourceType::Virtual
    }
}

/// Maps a portal error, turning a user cancellation into the `Cancelled`
/// sentinel (so the session ends quietly) and anything else into a real error.
fn map_cancel(error: &ashpd::Error) -> anyhow::Error {
    if matches!(error, &ashpd::Error::Response(ResponseError::Cancelled)) {
        Cancelled.into()
    } else {
        anyhow!("portal request failed: {error}")
    }
}

/// Where a restore token lives, one file per source kind - a shared file would
/// restore a monitor when the user asked for a virtual one, and each save would
/// destroy the other kind's token. Delete a file to get its picker dialog back.
/// `None` for the kinds that deliberately ask every time.
fn restore_token_path(source: SourceKind) -> Option<PathBuf> {
    let name = match source {
        SourceKind::Screen => "screen-restore-token",
        SourceKind::Virtual => "virtual-restore-token",
        SourceKind::Window | SourceKind::Choose | SourceKind::Audio => return None,
    };
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))?;
    Some(base.join("gnome-shell-cast").join(name))
}

fn load_restore_token(source: SourceKind) -> Option<String> {
    let token = std::fs::read_to_string(restore_token_path(source)?).ok()?;
    let token = token.trim();
    (!token.is_empty()).then(|| token.to_owned())
}

fn save_restore_token(source: SourceKind, token: &str) {
    let Some(path) = restore_token_path(source) else {
        return;
    };
    let write = || -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&path, token)
    };
    if let Err(e) = write() {
        warn!("could not save portal restore token: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsRawFd;

    /// The one thing about a virtual monitor that cannot be settled by reading
    /// code: whether mutter creates the monitor at the size *we* negotiate, or
    /// picks its own and leaves us to scale. `RecordVirtual` takes no size, and
    /// `pipewiresrc` derives its `PipeWire` format from peer caps, so a fixed
    /// caps filter on the source pad should decide it.
    ///
    /// Ignored because it needs a live Wayland session, a click in the portal
    /// dialog, and it briefly adds a monitor to the desktop. Run it by hand:
    ///
    /// ```text
    /// cargo test --manifest-path daemon/Cargo.toml \
    ///     capture::tests::a_virtual_monitor_is_created_at_the_size_we_ask_for \
    ///     -- --ignored --nocapture
    /// ```
    #[tokio::test]
    #[ignore = "needs a Wayland session and a click in the portal dialog"]
    async fn a_virtual_monitor_is_created_at_the_size_we_ask_for() -> Result<()> {
        use gstreamer as gst;
        use gstreamer::prelude::*;

        const WANTED: (i32, i32) = (1920, 1080);

        gst::init()?;
        let before = monitor_count().await?;
        println!("monitors before: {before}");

        let capture = open(SourceKind::Virtual, Some(WANTED)).await?;
        let source = crate::pipeline::VideoSource::from(&capture);
        let desc = format!(
            "pipewiresrc fd={} path={} do-timestamp=true {}! videoconvert ! fakesink",
            capture.fd.as_raw_fd(),
            capture.node_id,
            source.source_caps(),
        );
        println!("probe pipeline: {desc}");
        let pipeline = gst::parse::launch(&desc)?
            .downcast::<gst::Pipeline>()
            .map_err(|_| anyhow!("parsed element is not a pipeline"))?;
        pipeline.set_state(gst::State::Playing)?;

        // The monitor only appears once the format is negotiated, which happens
        // on the first buffer rather than on the state change.
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;

        let sizes = monitor_sizes().await?;
        let after = sizes.len();
        println!("monitors after: {after} {sizes:?}");
        let _ = pipeline.set_state(gst::State::Null);
        capture.close().await;

        assert_eq!(after, before.saturating_add(1), "no monitor was created");
        assert!(
            sizes.contains(&WANTED),
            "monitor created, but not at {WANTED:?}: {sizes:?} - mutter chose its own size"
        );
        Ok(())
    }

    async fn monitor_count() -> Result<usize> {
        Ok(monitor_sizes().await?.len())
    }

    /// Every monitor's current mode size, from mutter's `DisplayConfig`.
    async fn monitor_sizes() -> Result<Vec<(i32, i32)>> {
        use zbus::zvariant::OwnedValue;

        type HashMapSv = std::collections::HashMap<String, OwnedValue>;
        type Connector = (String, String, String, String);
        // (id, width, height, refresh, preferred scale, supported scales, props)
        type Mode = (String, i32, i32, f64, f64, Vec<f64>, HashMapSv);
        type Monitor = (Connector, Vec<Mode>, HashMapSv);
        // (x, y, scale, transform, primary, monitors, props)
        type LogicalMonitor = (i32, i32, f64, u32, bool, Vec<Connector>, HashMapSv);

        let connection = zbus::Connection::session().await?;
        let reply = connection
            .call_method(
                Some("org.gnome.Mutter.DisplayConfig"),
                "/org/gnome/Mutter/DisplayConfig",
                Some("org.gnome.Mutter.DisplayConfig"),
                "GetCurrentState",
                &(),
            )
            .await?;
        let body = reply.body();
        let (_serial, monitors, _logical, _props): (
            u32,
            Vec<Monitor>,
            Vec<LogicalMonitor>,
            HashMapSv,
        ) = body.deserialize()?;
        Ok(monitors
            .iter()
            .filter_map(|(_id, modes, _props)| {
                modes
                    .iter()
                    .find(|(_, _, _, _, _, _, props)| props.contains_key("is-current"))
                    .map(|&(_, w, h, ..)| (w, h))
            })
            .collect())
    }
}
