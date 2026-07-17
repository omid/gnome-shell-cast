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
use log::{info, warn};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    Screen,
    Window,
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
/// is being shared" indicator disappear.
pub struct Capture {
    pub fd: OwnedFd,
    pub node_id: u32,
    session: Option<Session<Screencast>>,
}

impl Drop for Capture {
    fn drop(&mut self) {
        // ashpd's `Session` has no `Drop` of its own, and this daemon outlives
        // the cast, so the portal session must be closed explicitly or the
        // compositor keeps showing the screen-sharing indicator. `close()` is
        // async, so hand it to the runtime we are being dropped on.
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
            Err(_) => warn!("no tokio runtime available to close the portal session"),
        }
    }
}

/// Asks the portal for a screen or window capture. GNOME shows its native
/// source picker dialog as part of this call - except for screen casts with a
/// saved restore token, which reuse the previous selection without a dialog.
pub async fn open(source: SourceKind) -> Result<Capture> {
    let proxy = Screencast::new()
        .await
        .context("connecting to the ScreenCast portal")?;
    let session = proxy
        .create_session(CreateSessionOptions::default())
        .await?;

    // Persist the selection for screen casts only: re-picking the same
    // monitor every time is pure friction, while window casts usually mean
    // a *different* window, so those should always show the picker.
    let (source_type, persist, restore_token) = match source {
        SourceKind::Screen => (
            SourceType::Monitor,
            PersistMode::ExplicitlyRevoked,
            load_restore_token(),
        ),
        SourceKind::Window => (SourceType::Window, PersistMode::DoNot, None),
        SourceKind::Audio => return Err(anyhow!("audio-only casts do not use the portal")),
    };
    proxy
        .select_sources(
            &session,
            SelectSourcesOptions::default()
                .set_cursor_mode(CursorMode::Embedded)
                .set_sources(BitFlags::from(source_type))
                .set_multiple(false)
                .set_persist_mode(persist)
                .set_restore_token(restore_token.as_deref()),
        )
        .await
        .map_err(map_cancel)?;

    let response = proxy
        .start(&session, None, StartCastOptions::default())
        .await
        .map_err(map_cancel)?
        .response()
        .map_err(map_cancel)?;

    if source == SourceKind::Screen
        && let Some(token) = response.restore_token()
    {
        save_restore_token(token);
    }

    let stream = response
        .streams()
        .first()
        .ok_or_else(|| anyhow!("portal returned no streams"))?;
    let node_id = stream.pipe_wire_node_id();

    let fd = proxy
        .open_pipe_wire_remote(&session, OpenPipeWireRemoteOptions::default())
        .await
        .context("opening the PipeWire remote")?;

    info!("portal capture ready, pipewire node {node_id}");
    Ok(Capture {
        fd,
        node_id,
        session: Some(session),
    })
}

/// Maps a portal error, turning a user cancellation into the `Cancelled`
/// sentinel (so the session ends quietly) and anything else into a real error.
fn map_cancel(error: ashpd::Error) -> anyhow::Error {
    if matches!(error, ashpd::Error::Response(ResponseError::Cancelled)) {
        Cancelled.into()
    } else {
        anyhow!("portal request failed: {error}")
    }
}

/// Where the screen-cast restore token lives. Delete this file to get the
/// monitor picker dialog back.
fn restore_token_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))?;
    Some(base.join("gnome-shell-cast").join("screen-restore-token"))
}

fn load_restore_token() -> Option<String> {
    let token = std::fs::read_to_string(restore_token_path()?).ok()?;
    let token = token.trim();
    (!token.is_empty()).then(|| token.to_string())
}

fn save_restore_token(token: &str) {
    let Some(path) = restore_token_path() else {
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
