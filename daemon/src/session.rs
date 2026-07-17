use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app::AppSink;
use log::{info, warn};
use tokio::sync::{mpsc, oneshot};

use crate::capture::{self, SourceKind};
use crate::discovery::Device;
use crate::pipeline::{self, PLAYLIST_NAME, StreamSettings};
use crate::{SharedState, cast, http, mirror};

/// Runs one cast session end to end: portal capture → `GStreamer` HLS encode →
/// HTTP serve → Chromecast playback, then cleans everything up when `stop_rx`
/// resolves (`StopCast`, a replacement session, or a device-side disconnect).
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
        Err(e) if e.downcast_ref::<capture::Cancelled>().is_some() => {
            info!("screen share cancelled by the user");
            state.set_status("idle", "");
        }
        Err(e) => {
            warn!("cast session failed: {e:#}");
            state.set_last_event("error", &format!("{e:#}"));
            state.set_status("error", &device.id);
        }
    }

    // Only the newest session may clear the shared stop handle and details; an
    // older session finishing late must not tear down its successor.
    if state.generation.load(Ordering::SeqCst) == generation {
        state.active.lock().take();
        state.clear_details();
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
    // Audio-only casts capture nothing on-screen and never touch the portal,
    // but they cannot work at all without the system audio monitor.
    let capture = match source {
        SourceKind::Audio => {
            if pipeline::default_audio_monitor().await.is_none() {
                return Err(anyhow!(
                    "no system audio monitor found (pactl get-default-sink)"
                ));
            }
            None
        }
        other => Some(capture::open(other).await?),
    };

    // 2. Prefer Chrome-style Cast Streaming (sub-second latency); fall back
    // to the HLS path below only when the receiver can't be negotiated with.
    match mirror::run(state, device, capture.as_ref(), &settings, &mut stop_rx).await {
        mirror::Outcome::Finished(result) => return result,
        mirror::Outcome::Unavailable(e) => {
            warn!("mirroring unavailable, falling back to HLS: {e:#}");
        }
    }

    // Audio-only receivers (speakers, smart clocks) run a Default Media
    // Receiver that rejects live HLS but plays a progressive HTTP audio stream
    // (internet-radio style), so give them that instead of the HLS path below.
    if source == SourceKind::Audio {
        return cast_audio_stream(state, device, stop_rx).await;
    }

    // 3. Connect to the Chromecast and launch its receiver app in parallel
    // with the encoder warm-up below; the URL is delivered once the first
    // HLS segment exists. Declared before `control` so an early error drops
    // `control` (whose join needs the poll loop to see a closed channel or
    // the stop flag) before the sender.
    let (url_tx, url_rx) = oneshot::channel();
    let (cast_events_tx, mut cast_events) = mpsc::unbounded_channel();
    let control = cast::start(device.addr, device.port, url_rx, cast_events_tx);

    // 3. A private runtime directory for the HLS playlist and segments.
    let hls_dir = runtime_dir();
    tokio::fs::create_dir_all(&hls_dir)
        .await
        .with_context(|| format!("creating {}", hls_dir.display()))?;
    let _cleanup = DirCleanup(hls_dir.clone());

    // 4. Encode into the directory and serve it.
    let audio_monitor = pipeline::default_audio_monitor().await;
    if audio_monitor.is_none() && capture.is_some() {
        warn!("no audio monitor found, casting video only");
    }
    let pipeline = pipeline::build(
        capture.as_ref().map(|c| (c.fd.as_raw_fd(), c.node_id)),
        &settings,
        &hls_dir,
        audio_monitor.as_deref(),
    )?;
    pipeline
        .set_state(gst::State::Playing)
        .context("starting the GStreamer pipeline")?;
    let _pipeline_stop = PipelineStop(pipeline.clone());

    let server = http::serve(hls_dir.clone())?;

    // 5. Wait for a full playlist window before pointing the device at it.
    // Loading earlier makes the player stall while segments trickle in, and
    // every stalled second becomes permanent lag behind live: the Default
    // Media Receiver never re-seeks to the live edge. With the whole window
    // available it fills its buffer instantly and starts one window (~3s)
    // behind.
    wait_for_playlist(&hls_dir).await?;

    let local_ip = http::local_ip_towards(device.addr)?;
    let url = format!("http://{local_ip}:{}/{PLAYLIST_NAME}", server.port);
    info!("stream ready at {url}");

    // 6. Hand the URL to the already-connected cast thread; a send error
    // means the connection died, which the event loop below will report.
    let _ = url_tx.send(cast::LoadMedia {
        url,
        content_type: "application/vnd.apple.mpegurl".to_string(),
        title: None,
        artist: None,
    });

    // 7. Run until asked to stop, the device disconnects, or the pipeline dies.
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
                Some(cast::CastEvent::Playing) => {
                    // HLS isn't negotiated, so there are no receiver codecs to
                    // list. Audio-only casts carry only AAC audio (no video).
                    let codec = if source == SourceKind::Audio { "aac" } else { "h264" };
                    state.set_details("hls", codec, Vec::new());
                    state.set_status("casting", &device.id);
                }
                Some(cast::CastEvent::Ended(reason)) => {
                    info!("device ended the session: {reason}");
                    state.set_last_event("ended", &reason);
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

/// Casts system audio to an audio-only receiver as a progressive HTTP stream
/// (MP3 or ADTS AAC) played by the Default Media Receiver — the HLS path these
/// devices reject is skipped entirely.
async fn cast_audio_stream(
    state: &Arc<SharedState>,
    device: &Device,
    mut stop_rx: oneshot::Receiver<()>,
) -> Result<()> {
    let monitor = pipeline::default_audio_monitor()
        .await
        .ok_or_else(|| anyhow!("no system audio monitor found (pactl get-default-sink)"))?;

    // Connect and launch the Default Media Receiver in parallel with the
    // encoder warm-up; the URL is delivered once audio is flowing.
    let (url_tx, url_rx) = oneshot::channel();
    let (cast_events_tx, mut cast_events) = mpsc::unbounded_channel();
    let control = cast::start(device.addr, device.port, url_rx, cast_events_tx);

    let (pipeline, content_type) = pipeline::build_audio_stream(&monitor)?;
    let broadcaster = http::AudioBroadcaster::new();
    attach_audio_sink(&pipeline, &broadcaster)?;
    pipeline
        .set_state(gst::State::Playing)
        .context("starting the audio pipeline")?;
    let _pipeline_stop = PipelineStop(pipeline.clone());

    let server = http::serve_audio(broadcaster, content_type)?;
    let codec = if content_type == "audio/mpeg" {
        "mp3"
    } else {
        "aac"
    };

    let local_ip = http::local_ip_towards(device.addr)?;
    // A filename with a plausible extension; the receiver keys playback off the
    // LOAD content type, but some are pickier when the URL has no extension.
    let url = format!("http://{local_ip}:{}/audio.{codec}", server.port);
    info!("audio stream ready at {url}");
    let _ = url_tx.send(cast::LoadMedia {
        url,
        content_type: content_type.to_string(),
        title: Some("GNOME Shell Cast".to_string()),
        artist: hostname(),
    });

    // Run until asked to stop, the device disconnects, or the pipeline dies.
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
                Some(cast::CastEvent::Playing) => {
                    state.set_details("audio", codec, Vec::new());
                    state.set_status("casting", &device.id);
                }
                Some(cast::CastEvent::Ended(reason)) => {
                    info!("device ended the session: {reason}");
                    state.set_last_event("ended", &reason);
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

/// Feeds every encoded audio buffer from the pipeline's `asink` appsink into
/// the broadcaster, which fans it out to the connected HTTP client.
fn attach_audio_sink(pipeline: &gst::Pipeline, broadcaster: &http::AudioBroadcaster) -> Result<()> {
    let sink = pipeline
        .by_name("asink")
        .and_then(|e| e.downcast::<AppSink>().ok())
        .ok_or_else(|| anyhow!("audio pipeline has no appsink named asink"))?;
    let broadcaster = broadcaster.clone();
    sink.set_callbacks(
        gstreamer_app::AppSinkCallbacks::builder()
            .new_sample(move |sink| {
                let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                let Some(buffer) = sample.buffer() else {
                    return Ok(gst::FlowSuccess::Ok);
                };
                let Ok(map) = buffer.map_readable() else {
                    return Err(gst::FlowError::Error);
                };
                broadcaster.push(std::sync::Arc::from(map.as_slice()));
                Ok(gst::FlowSuccess::Ok)
            })
            .build(),
    );
    Ok(())
}

/// The machine's hostname, shown as the cast's secondary line so a receiver
/// makes clear which computer it is playing from. `None` if it can't be read.
fn hostname() -> Option<String> {
    let name = std::fs::read_to_string("/proc/sys/kernel/hostname").ok()?;
    let name = name.trim();
    (!name.is_empty()).then(|| name.to_string())
}

fn runtime_dir() -> PathBuf {
    let base = std::env::var_os("XDG_RUNTIME_DIR").map_or_else(std::env::temp_dir, PathBuf::from);
    base.join(format!("gnome-shell-cast-{}", std::process::id()))
}

async fn wait_for_playlist(dir: &std::path::Path) -> Result<()> {
    let playlist = dir.join(PLAYLIST_NAME);
    for _ in 0..60 {
        if let Ok(content) = tokio::fs::read_to_string(&playlist).await {
            // One playlist window's worth of segments (playlist-length=3).
            if content.matches(".ts").count() >= 3 {
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
