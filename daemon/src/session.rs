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
use crate::pipeline::{self, PLAYLIST_NAME, PipelineStop, StreamSettings};
use crate::{SharedState, cast, http, streaming, volume};

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
            state.set_last_event("error", &user_message(&e));
            state.set_status("error", &device.id);
        }
    }

    // Only the newest session may clear the shared stop handle and details; an
    // older session finishing late must not tear down its successor.
    if state.generation.load(Ordering::SeqCst) == generation {
        state.active.lock().take();
        state.clear_details();
        state.set_volume_channel(None);
    }
}

/// What to show the user: the chain carries crate detail (rustls doc URLs,
/// `os error` numbers) that belongs in the journal, not in a notification.
fn user_message(error: &anyhow::Error) -> String {
    use std::io::ErrorKind;

    if let Some(io) = error
        .chain()
        .find_map(|cause| cause.downcast_ref::<std::io::Error>())
    {
        return match io.kind() {
            ErrorKind::NetworkUnreachable | ErrorKind::HostUnreachable => {
                "Could not reach the device. Check that it is switched on and on the same network."
            }
            ErrorKind::ConnectionRefused => "The device refused the connection.",
            ErrorKind::TimedOut | ErrorKind::ConnectionReset | ErrorKind::BrokenPipe => {
                "Lost the connection to the device."
            }
            _ => "Could not reach the device over the network.",
        }
        .to_string();
    }
    format!("{error}")
}

async fn cast_session(
    state: &Arc<SharedState>,
    device: &Device,
    source: SourceKind,
    settings: StreamSettings,
    stop_rx: oneshot::Receiver<()>,
) -> Result<()> {
    setup_volume(state, device).await;

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

    let result =
        cast_with_capture(state, device, source, settings, stop_rx, capture.as_ref()).await;

    // Awaited here, not left to `Drop`: the next cast must not race it.
    if let Some(capture) = capture {
        capture.close().await;
    }
    result
}

async fn cast_with_capture(
    state: &Arc<SharedState>,
    device: &Device,
    source: SourceKind,
    settings: StreamSettings,
    mut stop_rx: oneshot::Receiver<()>,
    capture: Option<&capture::Capture>,
) -> Result<()> {
    // 2. Prefer Chrome-style Cast Streaming (sub-second latency); fall back
    // to the HLS path below only when the receiver can't be negotiated with.
    match streaming::run(state, device, capture, &settings, &mut stop_rx).await {
        streaming::Outcome::Finished(result) => return result,
        streaming::Outcome::Unavailable(e) => {
            warn!("mirroring unavailable, falling back to HLS: {e:#}");
        }
    }

    // Audio-only receivers (speakers, smart clocks) reject live HLS but play a
    // progressive audio stream, so route them there instead of the HLS path.
    if source == SourceKind::Audio {
        return cast_audio_stream(state, device, stop_rx).await;
    }

    // 3. Build HLS pipeline and serve it.
    let hls_dir = runtime_dir();
    tokio::fs::create_dir_all(&hls_dir)
        .await
        .with_context(|| format!("creating {}", hls_dir.display()))?;
    let _cleanup = DirCleanup(hls_dir.clone());

    let audio_monitor = pipeline::default_audio_monitor().await;
    if audio_monitor.is_none() {
        warn!("no audio monitor found, casting video only");
    }
    let pipeline = pipeline::build(
        capture.map(|c| (c.fd.as_raw_fd(), c.node_id)),
        &settings,
        &hls_dir,
        audio_monitor.as_deref(),
    )?;
    pipeline
        .set_state(gst::State::Playing)
        .context("starting the GStreamer pipeline")?;
    let _pipeline_stop = PipelineStop(pipeline.clone());

    let server = http::serve(hls_dir.clone())?;
    wait_for_playlist(&hls_dir).await?;

    let local_ip = http::local_ip_towards(device.addr)?;
    let url = format!(
        "http://{local_ip}:{}/{}/{PLAYLIST_NAME}",
        server.port, server.token
    );
    info!("stream ready at {url}");

    run_cast_to_device(
        state,
        device,
        &mut stop_rx,
        &pipeline,
        cast::LoadMedia {
            url,
            content_type: "application/vnd.apple.mpegurl".to_string(),
            title: None,
            artist: None,
        },
        "hls",
        "h264",
    )
    .await
}

/// Drives a launched cast until stop, device disconnect, or a pipeline error,
/// reporting `casting` (with the given transport/codec) once playback starts.
async fn run_cast_loop(
    state: &SharedState,
    device: &Device,
    stop_rx: &mut oneshot::Receiver<()>,
    cast_events: &mut mpsc::UnboundedReceiver<cast::CastEvent>,
    bus: &gst::Bus,
    transport: &str,
    codec: &str,
) -> Result<()> {
    let mut bus_poll = tokio::time::interval(Duration::from_millis(500));
    loop {
        tokio::select! {
            _ = &mut *stop_rx => {
                info!("stop requested");
                return Ok(());
            }
            event = cast_events.recv() => match event {
                Some(cast::CastEvent::Playing) => {
                    state.set_details(transport, codec, Vec::new());
                    state.set_status("casting", &device.id);
                }
                Some(cast::CastEvent::Ended(reason)) => {
                    info!("device ended the session: {reason}");
                    state.set_last_event("ended", &reason);
                    return Ok(());
                }
                None => return Ok(()),
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
}

async fn setup_volume(state: &Arc<SharedState>, device: &Device) {
    let volume = volume::VolumeControl::start(device.addr, device.port, {
        let state = state.clone();
        move |level| state.set_cast_volume(f64::from(level))
    });
    state.set_volume_channel(Some(volume.sender()));
}

async fn run_cast_to_device(
    state: &Arc<SharedState>,
    device: &Device,
    stop_rx: &mut oneshot::Receiver<()>,
    pipeline: &gst::Pipeline,
    media: cast::LoadMedia,
    transport: &str,
    codec: &str,
) -> Result<()> {
    let (url_tx, url_rx) = oneshot::channel();
    let (cast_events_tx, mut cast_events) = mpsc::unbounded_channel();
    let control = cast::start(device.addr, device.port, url_rx, cast_events_tx);

    let _ = url_tx.send(media);

    let bus = pipeline
        .bus()
        .ok_or_else(|| anyhow!("pipeline has no bus"))?;
    let result = run_cast_loop(
        state,
        device,
        stop_rx,
        &mut cast_events,
        &bus,
        transport,
        codec,
    )
    .await;

    drop(control);
    result
}

/// Casts system audio to an audio-only receiver as a progressive HTTP stream
/// (MP3 or ADTS AAC), which its Default Media Receiver plays where HLS fails.
async fn cast_audio_stream(
    state: &Arc<SharedState>,
    device: &Device,
    mut stop_rx: oneshot::Receiver<()>,
) -> Result<()> {
    let monitor = pipeline::default_audio_monitor()
        .await
        .ok_or_else(|| anyhow!("no system audio monitor found (pactl get-default-sink)"))?;

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
    let url = format!(
        "http://{local_ip}:{}/{}/audio.{codec}",
        server.port, server.token
    );
    info!("audio stream ready at {url}");

    run_cast_to_device(
        state,
        device,
        &mut stop_rx,
        &pipeline,
        cast::LoadMedia {
            url,
            content_type: content_type.to_string(),
            title: Some("GNOME Shell Cast".to_string()),
            artist: hostname(),
        },
        "audio",
        codec,
    )
    .await
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

struct DirCleanup(PathBuf);

impl Drop for DirCleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn network_errors_lose_their_technical_detail() {
        let io = std::io::Error::from(std::io::ErrorKind::NetworkUnreachable);
        let message = user_message(&anyhow::Error::new(io).context("probing route to device"));
        assert!(
            message.starts_with("Could not reach the device."),
            "{message}"
        );
        assert!(!message.contains("os error"), "{message}");
    }

    #[test]
    fn other_errors_keep_their_top_level_context() {
        let error = anyhow!("no video encoder is installed").context("building the pipeline");
        assert_eq!(user_message(&error), "building the pipeline");
    }
}
