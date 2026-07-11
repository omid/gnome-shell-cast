use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use gstreamer as gst;
use gstreamer::prelude::*;
use log::{info, warn};
use tokio::sync::{mpsc, oneshot};

use crate::capture::{self, SourceKind};
use crate::discovery::Device;
use crate::pipeline::{self, StreamSettings, PLAYLIST_NAME};
use crate::{cast, http, SharedState};

/// Runs one cast session end to end: portal capture → GStreamer HLS encode →
/// HTTP serve → Chromecast playback, then cleans everything up when `stop_rx`
/// resolves (StopCast, a replacement session, or a device-side disconnect).
pub async fn run(
    state: Arc<SharedState>,
    generation: u64,
    device: Device,
    source: SourceKind,
    settings: StreamSettings,
    stop_rx: oneshot::Receiver<()>,
) {
    state.set_status("connecting", &device.id);

    match cast_session(&state, &device, source, settings, stop_rx).await {
        Ok(()) => state.set_status("idle", ""),
        Err(e) => {
            warn!("cast session failed: {e:#}");
            state.set_status("error", &device.id);
        }
    }

    // Only the newest session may clear the shared stop handle; an older
    // session finishing late must not tear down its successor.
    if state.generation.load(Ordering::SeqCst) == generation {
        state.active.lock().unwrap().take();
    }
}

async fn cast_session(
    state: &Arc<SharedState>,
    device: &Device,
    source: SourceKind,
    settings: StreamSettings,
    mut stop_rx: oneshot::Receiver<()>,
) -> Result<()> {
    // 1. Portal capture (GNOME shows the screen/window picker here).
    let capture = capture::open(source).await?;

    // 2. A private runtime directory for the HLS playlist and segments.
    let hls_dir = runtime_dir()?;
    tokio::fs::create_dir_all(&hls_dir)
        .await
        .with_context(|| format!("creating {}", hls_dir.display()))?;
    let _cleanup = DirCleanup(hls_dir.clone());

    // 3. Encode into the directory and serve it.
    let audio_monitor = pipeline::default_audio_monitor().await;
    if audio_monitor.is_none() {
        warn!("no audio monitor found, casting video only");
    }
    let pipeline = pipeline::build(
        capture.fd.as_raw_fd(),
        capture.node_id,
        &settings,
        &hls_dir,
        audio_monitor.as_deref(),
    )?;
    pipeline
        .set_state(gst::State::Playing)
        .context("starting the GStreamer pipeline")?;
    let _pipeline_stop = PipelineStop(pipeline.clone());

    let server = http::serve(hls_dir.clone())?;

    // 4. Wait for the first playable playlist before pointing the device at it.
    wait_for_playlist(&hls_dir).await?;

    let local_ip = http::local_ip_towards(device.addr)?;
    let url = format!("http://{local_ip}:{}/{PLAYLIST_NAME}", server.port);
    info!("stream ready at {url}");

    // 5. Tell the Chromecast to play it.
    let (cast_events_tx, mut cast_events) = mpsc::unbounded_channel();
    let control = cast::start(device.addr, device.port, url, cast_events_tx);

    // 6. Run until asked to stop, the device disconnects, or the pipeline dies.
    let bus = pipeline
        .bus()
        .ok_or_else(|| anyhow!("pipeline has no bus"))?;
    let mut bus_poll = tokio::time::interval(Duration::from_millis(500));
    loop {
        tokio::select! {
            _ = &mut stop_rx => {
                info!("stop requested");
                break;
            }
            event = cast_events.recv() => match event {
                Some(cast::CastEvent::Playing) => state.set_status("casting", &device.id),
                Some(cast::CastEvent::Ended(reason)) => {
                    info!("device ended the session: {reason}");
                    break;
                }
                None => break,
            },
            _ = bus_poll.tick() => {
                while let Some(message) = bus.pop() {
                    use gst::MessageView;
                    match message.view() {
                        MessageView::Error(e) => {
                            return Err(anyhow!("pipeline error: {}", e.error()));
                        }
                        MessageView::Eos(_) => return Err(anyhow!("pipeline reached EOS")),
                        _ => {}
                    }
                }
            }
        }
    }

    drop(control); // Stops the receiver app and joins the control thread.
    Ok(())
}

fn runtime_dir() -> Result<PathBuf> {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    Ok(base.join(format!("gnome-shell-cast-{}", std::process::id())))
}

async fn wait_for_playlist(dir: &std::path::Path) -> Result<()> {
    let playlist = dir.join(PLAYLIST_NAME);
    for _ in 0..60 {
        if let Ok(content) = tokio::fs::read_to_string(&playlist).await {
            if content.contains(".ts") {
                return Ok(());
            }
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    Err(anyhow!(
        "encoder produced no playable HLS playlist within 15s"
    ))
}

struct PipelineStop(gst::Pipeline);

impl Drop for PipelineStop {
    fn drop(&mut self) {
        let _ = self.0.set_state(gst::State::Null);
    }
}

struct DirCleanup(PathBuf);

impl Drop for DirCleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
