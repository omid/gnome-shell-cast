use std::os::fd::OwnedFd;

use anyhow::{anyhow, Context, Result};
use ashpd::desktop::screencast::{CursorMode, Screencast, SourceType};
use ashpd::desktop::{PersistMode, Session};
use log::info;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    Screen,
    Window,
}

/// An open XDG ScreenCast portal session. The PipeWire stream stays alive for
/// as long as this struct (and in particular `_session`) is kept around.
pub struct Capture {
    pub fd: OwnedFd,
    pub node_id: u32,
    _session: Session<'static, Screencast<'static>>,
}

/// Asks the portal for a screen or window capture. GNOME shows its native
/// source picker dialog as part of this call.
pub async fn open(source: SourceKind) -> Result<Capture> {
    let proxy = Screencast::new()
        .await
        .context("connecting to the ScreenCast portal")?;
    let session = proxy.create_session().await?;

    let source_type = match source {
        SourceKind::Screen => SourceType::Monitor,
        SourceKind::Window => SourceType::Window,
    };
    proxy
        .select_sources(
            &session,
            CursorMode::Embedded,
            source_type.into(),
            false,
            None,
            PersistMode::DoNot,
        )
        .await
        .context("selecting capture sources")?;

    let response = proxy
        .start(&session, None)
        .await
        .context("starting the portal session")?
        .response()
        .map_err(|e| anyhow!("portal request cancelled or failed: {e}"))?;

    let stream = response
        .streams()
        .first()
        .ok_or_else(|| anyhow!("portal returned no streams"))?;
    let node_id = stream.pipe_wire_node_id();

    let fd = proxy
        .open_pipe_wire_remote(&session)
        .await
        .context("opening the PipeWire remote")?;

    info!("portal capture ready, pipewire node {node_id}");
    Ok(Capture {
        fd,
        node_id,
        _session: session,
    })
}
