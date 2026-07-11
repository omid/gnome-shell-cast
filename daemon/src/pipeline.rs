use std::collections::HashMap;
use std::os::fd::RawFd;
use std::path::Path;

use anyhow::{Context, Result};
use gstreamer as gst;
use gstreamer::prelude::*;
use log::{info, warn};
use zbus::zvariant::OwnedValue;

pub const PLAYLIST_NAME: &str = "stream.m3u8";

#[derive(Debug, Clone)]
pub struct StreamSettings {
    /// Cap the video size; None keeps the captured size.
    pub size: Option<(i32, i32)>,
    pub fps: i32,
    pub bitrate_kbps: i32,
}

impl Default for StreamSettings {
    fn default() -> Self {
        Self {
            size: None,
            fps: 30,
            bitrate_kbps: 6000,
        }
    }
}

impl StreamSettings {
    pub fn from_options(options: &HashMap<String, OwnedValue>) -> Self {
        let get_i32 = |key: &str| options.get(key).and_then(|v| i32::try_from(v).ok());

        let mut settings = Self::default();
        if let (Some(w), Some(h)) = (get_i32("width"), get_i32("height")) {
            if w > 0 && h > 0 {
                settings.size = Some((w, h));
            }
        }
        if let Some(fps) = get_i32("fps") {
            settings.fps = fps.clamp(10, 60);
        }
        if let Some(bitrate) = get_i32("bitrate-kbps") {
            settings.bitrate_kbps = bitrate.clamp(1000, 20_000);
        }
        settings
    }
}

/// AAC encoders in order of preference; which ones exist depends on the
/// installed GStreamer plugin packages (gst-plugins-bad/ugly, gst-libav, ...).
const AAC_ENCODERS: &[&str] = &["fdkaacenc", "avenc_aac", "voaacenc", "faac"];

/// Returns the first AAC encoder element available in the GStreamer registry.
pub fn find_aac_encoder() -> Option<&'static str> {
    AAC_ENCODERS
        .iter()
        .copied()
        .find(|name| gst::ElementFactory::find(name).is_some())
}

/// Builds the gst-launch description capturing the PipeWire node, encoding
/// H.264 (+ AAC system audio when a monitor device and encoder are known) and
/// writing a live HLS stream into `hls_dir`. `audio` is the pulse monitor
/// device to capture and the AAC encoder element to use.
pub fn launch_description(
    fd: RawFd,
    node_id: u32,
    settings: &StreamSettings,
    hls_dir: &Path,
    audio: Option<(&str, &str)>,
) -> String {
    let dir = hls_dir.display();
    let fps = settings.fps;
    // Keyframe every HLS segment so segments are independently decodable.
    let target_duration = 2;
    let key_int = (fps * target_duration).max(1);

    let size_caps = settings
        .size
        .map(|(w, h)| format!(",width={w},height={h},pixel-aspect-ratio=1/1"))
        .unwrap_or_default();

    let mut desc = format!(
        "pipewiresrc fd={fd} path={node_id} do-timestamp=true keepalive-time=1000 resend-last=true \
         ! queue ! videoconvert ! videoscale ! videorate \
         ! video/x-raw,framerate={fps}/1{size_caps} \
         ! x264enc tune=zerolatency speed-preset=veryfast bitrate={bitrate} key-int-max={key_int} bframes=0 \
         ! video/x-h264,profile=main ! h264parse ! queue \
         ! hls.video hlssink2 name=hls target-duration={target_duration} playlist-length=5 max-files=10 \
         playlist-location={dir}/{PLAYLIST_NAME} location={dir}/segment%05d.ts",
        bitrate = settings.bitrate_kbps,
    );

    if let Some((monitor, encoder)) = audio {
        desc.push_str(&format!(
            " pulsesrc device={monitor} provide-clock=false \
             ! queue ! audioconvert ! audioresample \
             ! {encoder} bitrate=128000 ! aacparse ! queue ! hls.audio"
        ));
    }

    desc
}

pub fn build(
    fd: RawFd,
    node_id: u32,
    settings: &StreamSettings,
    hls_dir: &Path,
    audio_monitor: Option<&str>,
) -> Result<gst::Pipeline> {
    let audio = audio_monitor.and_then(|monitor| match find_aac_encoder() {
        Some(encoder) => Some((monitor, encoder)),
        None => {
            warn!(
                "no AAC encoder found (install fdk-aac/gst-plugins-bad or gst-libav), \
                 casting video only"
            );
            None
        }
    });

    let desc = launch_description(fd, node_id, settings, hls_dir, audio);
    info!("pipeline: {desc}");

    let pipeline = gst::parse::launch(&desc)
        .context("building the GStreamer pipeline (are gst-plugins-good/bad/ugly and gst-libav installed?)")?
        .downcast::<gst::Pipeline>()
        .map_err(|_| anyhow::anyhow!("parsed element is not a pipeline"))?;
    Ok(pipeline)
}

/// Finds the PulseAudio/PipeWire monitor source of the default sink, used to
/// capture what the system is playing. Returns None (video-only cast) when it
/// cannot be determined.
pub async fn default_audio_monitor() -> Option<String> {
    let output = tokio::process::Command::new("pactl")
        .arg("get-default-sink")
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let sink = String::from_utf8(output.stdout).ok()?;
    let sink = sink.trim();
    if sink.is_empty() {
        return None;
    }
    Some(format!("{sink}.monitor"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn default_settings_from_empty_options() {
        let settings = StreamSettings::from_options(&HashMap::new());
        assert_eq!(settings.size, None);
        assert_eq!(settings.fps, 30);
        assert_eq!(settings.bitrate_kbps, 6000);
    }

    #[test]
    fn options_are_clamped() {
        let mut options = HashMap::new();
        options.insert("fps".to_string(), OwnedValue::from(500i32));
        options.insert("bitrate-kbps".to_string(), OwnedValue::from(1i32));
        let settings = StreamSettings::from_options(&options);
        assert_eq!(settings.fps, 60);
        assert_eq!(settings.bitrate_kbps, 1000);
    }

    #[test]
    fn description_scales_when_size_is_set() {
        let settings = StreamSettings {
            size: Some((1280, 720)),
            ..Default::default()
        };
        let desc = launch_description(3, 42, &settings, &PathBuf::from("/run/x"), None);
        assert!(desc.contains("width=1280,height=720"));
        assert!(desc.contains("fd=3 path=42"));
        assert!(desc.contains("/run/x/stream.m3u8"));
        assert!(!desc.contains("pulsesrc"));
    }

    #[test]
    fn description_includes_audio_branch() {
        let desc = launch_description(
            3,
            42,
            &StreamSettings::default(),
            &PathBuf::from("/run/x"),
            Some(("alsa_output.pci.monitor", "fdkaacenc")),
        );
        assert!(desc.contains("pulsesrc device=alsa_output.pci.monitor"));
        assert!(desc.contains("fdkaacenc bitrate=128000"));
    }
}
