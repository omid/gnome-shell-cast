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
            size: Some((1280, 720)),
            fps: 20,
            bitrate_kbps: 4000,
        }
    }
}

impl StreamSettings {
    pub fn from_options(options: &HashMap<String, OwnedValue>) -> Self {
        let get_i32 = |key: &str| options.get(key).and_then(|v| i32::try_from(v).ok());

        let mut settings = Self::default();
        if let (Some(w), Some(h)) = (get_i32("width"), get_i32("height"))
            && w > 0
            && h > 0
        {
            settings.size = Some((w, h));
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
/// installed `GStreamer` plugin packages (gst-plugins-bad/ugly, gst-libav, ...).
const AAC_ENCODERS: &[&str] = &["fdkaacenc", "avenc_aac", "voaacenc", "faac"];

/// Returns the first AAC encoder element available in the `GStreamer` registry.
pub fn find_aac_encoder() -> Option<&'static str> {
    AAC_ENCODERS
        .iter()
        .copied()
        .find(|name| gst::ElementFactory::find(name).is_some())
}

/// H.264 encoders for the HLS path, hardware first (VA-API, then NVENC), then
/// software `x264enc`. Each candidate is parse-checked, so a hardware encoder
/// that is present but mis-parametrised falls back to the next one.
const H264_ENCODERS: &[&str] = &["vah264enc", "vah264lpenc", "nvh264enc", "x264enc"];

fn find_h264_encoder(bitrate_kbps: i32, key_int: i32) -> String {
    let software = format!(
        "x264enc tune=zerolatency speed-preset=veryfast bitrate={bitrate_kbps} key-int-max={key_int} bframes=0"
    );
    for &f in H264_ENCODERS {
        let fragment = match f {
            "x264enc" => software.clone(),
            _ if f.starts_with("nv") => {
                format!("{f} bitrate={bitrate_kbps} rc-mode=cbr gop-size={key_int} bframes=0")
            }
            _ => format!("{f} bitrate={bitrate_kbps} rate-control=cbr key-int-max={key_int}"),
        };
        if gst::parse::launch(&fragment).is_ok() {
            return fragment;
        }
    }
    software
}

/// Builds the gst-launch description writing a live HLS stream into
/// `hls_dir`: H.264 from the captured `PipeWire` node when `video` carries
/// the (fd, node id) pair, plus AAC system audio when `audio` names the pulse
/// monitor device and the AAC encoder element. Audio-only casts pass
/// `video: None` and produce audio-only TS segments.
pub fn launch_description(
    video: Option<(RawFd, u32)>,
    settings: &StreamSettings,
    hls_dir: &Path,
    audio: Option<(&str, &str)>,
    video_encoder: &str,
) -> String {
    use std::fmt::Write as _;

    let dir = hls_dir.display();
    let fps = settings.fps;
    // Short segments keep both startup and live lag low: the player is
    // roughly 3 target-durations behind the encoder. Keyframe every segment
    // so segments are independently decodable.
    let target_duration = 1;

    let mut desc = String::new();
    if let Some((fd, node_id)) = video {
        let size_caps = settings
            .size
            .map(|(w, h)| format!(",width={w},height={h},pixel-aspect-ratio=1/1"))
            .unwrap_or_default();

        // The source queue is small and leaky: when the encoder can't keep up
        // with raw frames the pipeline drops the oldest instead of buffering
        // them, so the stream falls in quality rather than further behind live.
        // `video_encoder` is the chosen H.264 element (hardware if available).
        let _ = write!(
            desc,
            "pipewiresrc fd={fd} path={node_id} do-timestamp=true keepalive-time=1000 resend-last=true \
             ! queue leaky=downstream max-size-buffers=3 max-size-bytes=0 max-size-time=0 \
             ! videoconvert ! videoscale ! videorate \
             ! video/x-raw,framerate={fps}/1{size_caps} \
             ! {video_encoder} ! h264parse ! queue \
             ! hls.video "
        );
    }

    let _ = write!(
        desc,
        "hlssink2 name=hls target-duration={target_duration} playlist-length=3 max-files=6 \
         playlist-location={dir}/{PLAYLIST_NAME} location={dir}/segment%05d.ts"
    );

    if let Some((monitor, encoder)) = audio {
        let _ = write!(
            desc,
            " pulsesrc device={monitor} provide-clock=false \
             ! queue ! audioconvert ! audioresample \
             ! {encoder} bitrate=128000 ! aacparse ! queue ! hls.audio"
        );
    }

    desc
}

/// Builds a progressive (non-HLS) audio pipeline that encodes the system audio
/// monitor to a continuous byte stream on an appsink named `asink`, for
/// audio-only receivers (speakers, smart clocks) whose Default Media Receiver
/// rejects live HLS but plays an internet-radio-style HTTP stream. Prefers MP3
/// (the most widely supported audio type on cheap Cast receivers), falling back
/// to ADTS AAC. Returns the pipeline and the HTTP content type to advertise.
pub fn build_audio_stream(monitor: &str) -> Result<(gst::Pipeline, &'static str)> {
    let (encode, content_type) = if gst::ElementFactory::find("lamemp3enc").is_some() {
        (
            "lamemp3enc target=bitrate bitrate=128 cbr=true".to_string(),
            "audio/mpeg",
        )
    } else {
        let aac = find_aac_encoder().context(
            "no MP3 or AAC encoder found (install gst-plugins-ugly, fdk-aac/gst-plugins-bad, or gst-libav)",
        )?;
        (
            format!(
                "{aac} bitrate=128000 ! aacparse ! audio/mpeg,mpegversion=4,stream-format=adts"
            ),
            "audio/aac",
        )
    };

    let desc = format!(
        "pulsesrc device={monitor} provide-clock=false \
         ! queue ! audioconvert ! audioresample ! audio/x-raw,rate=44100,channels=2 \
         ! {encode} ! appsink name=asink sync=false max-buffers=64 drop=false"
    );
    info!("audio stream pipeline: {desc}");

    let pipeline = gst::parse::launch(&desc)
        .context("building the progressive audio pipeline")?
        .downcast::<gst::Pipeline>()
        .map_err(|_| anyhow::anyhow!("parsed element is not a pipeline"))?;
    Ok((pipeline, content_type))
}

pub fn build(
    video: Option<(RawFd, u32)>,
    settings: &StreamSettings,
    hls_dir: &Path,
    audio_monitor: Option<&str>,
) -> Result<gst::Pipeline> {
    // Audio-only casts hard-require the audio branch; video casts degrade to
    // video-only with a warning when AAC encoding is unavailable.
    let audio = match (audio_monitor, find_aac_encoder()) {
        (Some(monitor), Some(encoder)) => Some((monitor, encoder)),
        (Some(_), None) if video.is_none() => {
            return Err(anyhow::anyhow!(
                "no AAC encoder found (install fdk-aac/gst-plugins-bad or gst-libav)"
            ));
        }
        (Some(_), None) => {
            warn!(
                "no AAC encoder found (install fdk-aac/gst-plugins-bad or gst-libav), \
             casting video only"
            );
            None
        }
        (None, _) if video.is_none() => {
            return Err(anyhow::anyhow!(
                "audio-only cast but no system audio monitor was found"
            ));
        }
        (None, _) => None,
    };

    // Keyframe every segment (target-duration = 1s) so segments decode alone.
    let key_int = settings.fps.max(1);
    let video_encoder = find_h264_encoder(settings.bitrate_kbps, key_int);
    let desc = launch_description(video, settings, hls_dir, audio, &video_encoder);
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
        assert_eq!(settings.size, Some((1280, 720)));
        assert_eq!(settings.fps, 20);
        assert_eq!(settings.bitrate_kbps, 4000);
    }

    #[test]
    fn options_are_clamped() {
        let mut options = HashMap::new();
        options.insert("fps".to_string(), OwnedValue::from(500_i32));
        options.insert("bitrate-kbps".to_string(), OwnedValue::from(1_i32));
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
        let desc = launch_description(
            Some((3, 42)),
            &settings,
            &PathBuf::from("/run/x"),
            None,
            "x264enc bitrate=4000",
        );
        assert!(desc.contains("width=1280,height=720"));
        assert!(desc.contains("fd=3 path=42"));
        assert!(desc.contains("x264enc bitrate=4000 ! h264parse"));
        assert!(desc.contains("/run/x/stream.m3u8"));
        assert!(!desc.contains("pulsesrc"));
    }

    #[test]
    fn description_includes_audio_branch() {
        let desc = launch_description(
            Some((3, 42)),
            &StreamSettings::default(),
            &PathBuf::from("/run/x"),
            Some(("alsa_output.pci.monitor", "fdkaacenc")),
            "x264enc bitrate=4000",
        );
        assert!(desc.contains("hls.video"));
        assert!(desc.contains("pulsesrc device=alsa_output.pci.monitor"));
        assert!(desc.contains("fdkaacenc bitrate=128000"));
    }

    #[test]
    fn audio_only_description_has_no_video_branch() {
        let desc = launch_description(
            None,
            &StreamSettings::default(),
            &PathBuf::from("/run/x"),
            Some(("alsa_output.pci.monitor", "fdkaacenc")),
            "x264enc bitrate=4000",
        );
        assert!(!desc.contains("pipewiresrc"));
        assert!(!desc.contains("x264enc"));
        assert!(!desc.contains("hls.video"));
        assert!(desc.starts_with("hlssink2 name=hls"));
        assert!(desc.contains("/run/x/stream.m3u8"));
        assert!(desc.contains("pulsesrc device=alsa_output.pci.monitor"));
        assert!(desc.contains("hls.audio"));
    }
}
