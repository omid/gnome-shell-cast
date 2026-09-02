use std::collections::HashMap;
use std::os::fd::RawFd;
use std::path::Path;

use anyhow::{Context, Result};
use gstreamer as gst;
use gstreamer::prelude::*;
use log::{info, warn};
use zbus::zvariant::OwnedValue;

use crate::streaming::encoder::{self, EncoderPolicy, EncodingPolicy, FormatPolicy};

pub const PLAYLIST_NAME: &str = "stream.m3u8";

#[derive(Debug, Clone, Default)]
pub struct StreamSettings {
    /// Every field is a request: `None` means automatic, which defers to the
    /// receiver's constraints and our own limits (see `streaming::quality`).
    pub size: Option<(i32, i32)>,
    pub fps: Option<i32>,
    pub bitrate_kbps: Option<i32>,
    pub audio_bitrate_kbps: Option<i32>,
    /// Which encoder and raw format the user will accept; `Auto` by default.
    pub encoding: EncodingPolicy,
}

impl StreamSettings {
    /// The values to build a pipeline from when there is no receiver to
    /// negotiate with (the HLS fallback), or before an ANSWER has arrived.
    pub fn resolve_local(&self) -> crate::streaming::quality::Resolved {
        self.resolve(&crate::streaming::quality::Constraints::default())
    }

    /// Combines this request with `constraints`; automatic fields are filled
    /// in from the envelope. `size` doubles as the captured size when set.
    pub fn resolve(
        &self,
        constraints: &crate::streaming::quality::Constraints,
    ) -> crate::streaming::quality::Resolved {
        crate::streaming::quality::resolve(
            self.size,
            self.fps,
            self.bitrate_kbps,
            self.audio_bitrate_kbps,
            self.size.unwrap_or((1920, 1080)),
            constraints,
        )
    }

    pub fn from_options(mut options: HashMap<String, OwnedValue>) -> Self {
        let mut get_i32 = |key: &str| options.remove(key).and_then(|v| i32::try_from(&v).ok());

        let mut settings = Self::default();
        if let (Some(w), Some(h)) = (get_i32("width"), get_i32("height"))
            && w > 0
            && h > 0
        {
            // Capped at 8K so a bad request can't ask for an absurd frame size.
            settings.size = Some((w.min(7680), h.min(4320)));
        }
        // 0 is how the extension spells "automatic" over D-Bus.
        if let Some(fps) = get_i32("fps").filter(|fps| *fps > 0) {
            settings.fps = Some(fps.clamp(1, 60));
        }
        if let Some(bitrate) = get_i32("bitrate-kbps").filter(|b| *b > 0) {
            settings.bitrate_kbps = Some(bitrate.clamp(100, 60_000));
        }
        if let Some(bitrate) = get_i32("audio-bitrate-kbps").filter(|b| *b > 0) {
            settings.audio_bitrate_kbps = Some(bitrate.clamp(16, 512));
        }

        let mut get_string = |key: &str| options.remove(key).and_then(|v| String::try_from(v).ok());
        if let Some(encoder) = get_string("video-encoder") {
            settings.encoding.encoder = EncoderPolicy::parse(&encoder);
        }
        if let Some(format) = get_string("video-format") {
            settings.encoding.format = FormatPolicy::parse(&format);
        }
        settings
    }
}

/// Applies a new target bitrate to the running encoder. The property and its
/// unit differ per element, so this maps by factory name; anything unknown is
/// left alone rather than guessed at.
pub fn set_encoder_bitrate(pipeline: &gst::Pipeline, bits_per_second: u32) {
    let Some(venc) = pipeline.by_name("venc") else {
        return;
    };
    let factory = venc
        .factory()
        .map(|f| f.name().to_string())
        .unwrap_or_default();
    let kbps = bits_per_second.checked_div(1000).unwrap_or(1).max(1);
    match factory.as_str() {
        // The VPX base takes bit/s.
        "vp8enc" | "vp9enc" => venc.set_property(
            "target-bitrate",
            i32::try_from(bits_per_second).unwrap_or(i32::MAX),
        ),
        "svtav1enc" | "av1enc" => venc.set_property("target-bitrate", kbps),
        "x264enc" => venc.set_property("bitrate", kbps),
        // A V4L2 encoder has no bitrate property; the control carries bit/s,
        // and the existing fields are kept so the GOP size set at launch stays.
        other if other.starts_with("v4l2") => {
            let mut controls = venc
                .property::<Option<gst::Structure>>("extra-controls")
                .unwrap_or_else(|| gst::Structure::new_empty("controls"));
            controls.set(
                "video_bitrate",
                i32::try_from(bits_per_second).unwrap_or(i32::MAX),
            );
            venc.set_property("extra-controls", controls);
        }
        other if other.starts_with("va") || other.starts_with("nv") => {
            venc.set_property("bitrate", kbps);
        }
        _ => {}
    }
}

/// Retargets the running capture to `size`. The caps between videoscale and
/// the encoder come from the launch string, so the filter is unnamed; it is
/// found by factory and its existing fields are preserved.
pub fn set_capture_size(pipeline: &gst::Pipeline, (width, height): (i32, i32)) {
    let mut elements = pipeline.iterate_elements();
    while let Ok(Some(element)) = elements.next() {
        let is_capsfilter = element
            .factory()
            .is_some_and(|factory| factory.name() == "capsfilter");
        if !is_capsfilter {
            continue;
        }
        let caps = element.property::<Option<gst::Caps>>("caps");
        let Some(caps) = caps else { continue };
        let Some(structure) = caps.structure(0) else {
            continue;
        };
        if structure.name() != "video/x-raw" {
            continue;
        }
        let mut updated = structure.to_owned();
        updated.set("width", width);
        updated.set("height", height);
        element.set_property("caps", gst::Caps::builder_full().structure(updated).build());
        return;
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
/// that is present but mis-parametrised falls back to the next one. `None` when
/// the user's encoder or pixel-format choice rules every one of them out.
const H264_ENCODERS: &[&str] = &[
    "vah264enc",
    "vah264lpenc",
    "nvh264enc",
    "v4l2h264enc",
    "x264enc",
];

fn find_h264_encoder(bitrate_kbps: i32, key_int: i32, policy: EncodingPolicy) -> Option<String> {
    let software = format!(
        "x264enc name=venc tune=zerolatency speed-preset=veryfast bitrate={bitrate_kbps} key-int-max={key_int} bframes=0"
    );
    for &f in H264_ENCODERS {
        if !encoder::allowed(f, policy) {
            continue;
        }
        let fragment = match f {
            "x264enc" => software.clone(),
            _ if f.starts_with("nv") => {
                format!(
                    "{f} name=venc bitrate={bitrate_kbps} rc-mode=cbr gop-size={key_int} bframes=0"
                )
            }
            // V4L2 takes bit/s through controls rather than properties.
            _ if f.starts_with("v4l2") => format!(
                "{f} name=venc extra-controls=\"controls,video_bitrate={},video_gop_size={key_int}\"",
                i64::from(bitrate_kbps).saturating_mul(1000)
            ),
            _ => format!(
                "{f} name=venc bitrate={bitrate_kbps} rate-control=cbr key-int-max={key_int}"
            ),
        };
        // Hardware candidates have to open their device, not just parse: the
        // HLS path would otherwise fail a whole cast on a phantom encoder.
        if encoder::fragment_usable(f, &fragment) {
            return Some(fragment);
        }
    }
    None
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
    let resolved = settings.resolve_local();
    let fps = resolved.fps;
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
        // NV12 for the VA-API encoders, I420 for x264enc; unconstrained,
        // videoconvert picks Y444 and x264enc emits 4:4:4 no receiver decodes.
        let format = settings.encoding.format.caps_format();
        let _ = write!(
            desc,
            "pipewiresrc fd={fd} path={node_id} do-timestamp=true keepalive-time=1000 resend-last=true \
             ! queue leaky=downstream max-size-buffers=3 max-size-bytes=0 max-size-time=0 \
             ! videoconvert ! videoscale ! videorate \
             ! video/x-raw,format={format},framerate={fps}/1{size_caps} \
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

/// Builds a progressive (non-HLS) audio pipeline for audio-only receivers,
/// encoding the system audio monitor onto an appsink named `asink`. Prefers MP3
/// (most widely supported on cheap Cast receivers), falling back to ADTS AAC.
/// Returns the pipeline and the HTTP content type to advertise.
pub fn build_audio_stream(monitor: &str) -> Result<(gst::Pipeline, &'static str)> {
    let (encode, content_type) = if gst::ElementFactory::find("lamemp3enc").is_some() {
        (
            "lamemp3enc target=bitrate bitrate=128 cbr=true".to_owned(),
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
    // Video casts (this path) degrade to video-only with a warning when AAC
    // encoding or the audio monitor is unavailable.
    let audio = match (audio_monitor, find_aac_encoder()) {
        (Some(monitor), Some(encoder)) => Some((monitor, encoder)),
        (Some(_), None) => {
            warn!(
                "no AAC encoder found (install fdk-aac/gst-plugins-bad or gst-libav), \
             casting video only"
            );
            None
        }
        (None, _) => None,
    };

    // Keyframe every segment (target-duration = 1s) so segments decode alone.
    let key_int = settings.resolve_local().fps.max(1);
    // A forced encoder or pixel format can rule out every candidate; fail with
    // the reason rather than quietly ignoring the user's choice.
    let video_encoder = match video {
        Some(_) => Some(
            find_h264_encoder(
                settings.resolve_local().video_bitrate_kbps,
                key_int,
                settings.encoding,
            )
            .ok_or_else(|| anyhow::anyhow!(encoder::policy_failure_message(settings.encoding)))?,
        ),
        None => None,
    };
    let desc = launch_description(
        video,
        settings,
        hls_dir,
        audio,
        video_encoder.as_deref().unwrap_or_default(),
    );
    info!("pipeline: {desc}");

    let pipeline = gst::parse::launch(&desc)
        .context("building the GStreamer pipeline (are gst-plugins-good/bad/ugly and gst-libav installed?)")?
        .downcast::<gst::Pipeline>()
        .map_err(|_| anyhow::anyhow!("parsed element is not a pipeline"))?;
    Ok(pipeline)
}

/// The `GStreamer` encoder element a built pipeline actually uses (e.g.
/// "vah264enc"), read back from the pipeline so it cannot drift from the
/// fragment that was chosen.
pub fn encoder_element(pipeline: &gst::Pipeline) -> Option<String> {
    Some(pipeline.by_name("venc")?.factory()?.name().to_string())
}

/// The raw video format the encoder negotiated, once the pipeline has
/// prerolled. With the pixel format preference on automatic this is the only
/// way to know whether NV12 or I420 was picked - the caps offered both.
pub fn negotiated_format(pipeline: &gst::Pipeline) -> Option<String> {
    let caps = pipeline
        .by_name("venc")?
        .static_pad("sink")?
        .current_caps()?;
    caps.structure(0)?.get::<String>("format").ok()
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

/// Stops a pipeline when it goes out of scope, on every path out of a session.
pub struct PipelineStop(pub gst::Pipeline);

impl Drop for PipelineStop {
    fn drop(&mut self) {
        let _ = self.0.set_state(gst::State::Null);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    /// Nothing requested means automatic everywhere; the values come from the
    /// receiver's envelope later, not from a hardcoded default here.
    fn default_settings_from_empty_options() {
        let settings = StreamSettings::from_options(HashMap::new());
        assert_eq!(settings.size, None);
        assert_eq!(settings.fps, None);
        assert_eq!(settings.bitrate_kbps, None);
        assert_eq!(settings.audio_bitrate_kbps, None);
    }

    #[test]
    fn zero_means_automatic() {
        let mut options = HashMap::new();
        options.insert("fps".to_owned(), OwnedValue::from(0_i32));
        options.insert("bitrate-kbps".to_owned(), OwnedValue::from(0_i32));
        let settings = StreamSettings::from_options(options);
        assert_eq!(settings.fps, None);
        assert_eq!(settings.bitrate_kbps, None);
    }

    #[test]
    fn options_are_clamped() {
        let mut options = HashMap::new();
        options.insert("fps".to_owned(), OwnedValue::from(500_i32));
        options.insert("bitrate-kbps".to_owned(), OwnedValue::from(1_i32));
        let settings = StreamSettings::from_options(options);
        assert_eq!(settings.fps, Some(60));
        assert_eq!(settings.bitrate_kbps, Some(100));
    }

    #[test]
    fn high_resolution_and_bitrate_pass_through() {
        let mut options = HashMap::new();
        options.insert("width".to_owned(), OwnedValue::from(3840_i32));
        options.insert("height".to_owned(), OwnedValue::from(2160_i32));
        options.insert("bitrate-kbps".to_owned(), OwnedValue::from(30_000_i32));
        let settings = StreamSettings::from_options(options);
        assert_eq!(settings.size, Some((3840, 2160)));
        assert_eq!(settings.bitrate_kbps, Some(30_000));
    }

    #[test]
    fn absurd_size_and_bitrate_are_capped() {
        let mut options = HashMap::new();
        options.insert("width".to_owned(), OwnedValue::from(100_000_i32));
        options.insert("height".to_owned(), OwnedValue::from(100_000_i32));
        options.insert("bitrate-kbps".to_owned(), OwnedValue::from(999_999_i32));
        let settings = StreamSettings::from_options(options);
        assert_eq!(settings.size, Some((7680, 4320)));
        assert_eq!(settings.bitrate_kbps, Some(60_000));
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
        assert!(desc.contains("format={NV12,I420}"));
        assert!(desc.contains("fd=3 path=42"));
        assert!(desc.contains("x264enc bitrate=4000 ! h264parse"));
        assert!(desc.contains("/run/x/stream.m3u8"));
        assert!(!desc.contains("pulsesrc"));
    }

    #[test]
    fn forced_pixel_format_reaches_the_caps() {
        let settings = StreamSettings {
            encoding: EncodingPolicy {
                format: FormatPolicy::Nv12,
                ..Default::default()
            },
            ..Default::default()
        };
        let desc = launch_description(
            Some((3, 42)),
            &settings,
            &PathBuf::from("/run/x"),
            None,
            "x264enc bitrate=4000",
        );
        assert!(desc.contains("format=NV12,"), "{desc}");
        assert!(!desc.contains("{NV12,I420}"));
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
