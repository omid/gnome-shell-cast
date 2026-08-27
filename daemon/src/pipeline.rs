use std::collections::HashMap;
use std::os::fd::{AsRawFd, RawFd};
use std::path::Path;

use anyhow::{Context, Result};
use gstreamer as gst;
use gstreamer::prelude::*;
use log::{info, warn};
use zbus::zvariant::OwnedValue;

use crate::streaming::encoder::{
    self, Api, EncoderPolicy, EncodingPolicy, FormatPolicy, VideoCodec,
};

pub const PLAYLIST_NAME: &str = "stream.m3u8";

/// The captured video a pipeline is built around.
#[derive(Debug, Clone, Copy)]
pub struct VideoSource {
    pub fd: RawFd,
    pub node_id: u32,
    /// `Some` only for a virtual monitor. `PipeWire` negotiation is what fixes
    /// such a monitor's resolution, and the negotiation takes it from the
    /// *consumer*, so the size has to be named in caps on the source pad.
    pub capture_size: Option<(i32, i32)>,
}

impl From<&crate::capture::Capture> for VideoSource {
    fn from(capture: &crate::capture::Capture) -> Self {
        Self {
            fd: capture.fd.as_raw_fd(),
            node_id: capture.node_id,
            capture_size: capture.source_size,
        }
    }
}

impl VideoSource {
    /// The launch fragment that pins the source size, empty unless this is a
    /// virtual monitor.
    ///
    /// It must sit ahead of `videoscale`, which reports width and height back
    /// upstream as open ranges and so hides the encoder-side caps from
    /// `pipewiresrc` entirely. Width and height only: mutter offers
    /// `framerate = 0/1` for a virtual stream, so naming a rate here makes every
    /// format fail to intersect, and a filter that names fewer fields is the
    /// permissive direction anyway.
    pub fn source_caps(self) -> String {
        self.capture_size
            .map(|(w, h)| {
                format!("! capsfilter name={SOURCE_FILTER} caps=video/x-raw,width={w},height={h} ")
            })
            .unwrap_or_default()
    }
}

/// Names the source-pad caps filter so nothing else has to guess which
/// `video/x-raw` filter is which.
const SOURCE_FILTER: &str = "srccaps";

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
        other => match encoder::api_of(other) {
            // A V4L2 encoder has no bitrate property; the control carries bit/s,
            // and the existing fields are kept so the GOP size set at launch stays.
            Some(Api::V4l2) => {
                let mut controls = venc
                    .property::<Option<gst::Structure>>("extra-controls")
                    .unwrap_or_else(|| gst::Structure::new_empty("controls"));
                controls.set(
                    "video_bitrate",
                    i32::try_from(bits_per_second).unwrap_or(i32::MAX),
                );
                venc.set_property("extra-controls", controls);
            }
            Some(Api::Va | Api::Nvenc) => venc.set_property("bitrate", kbps),
            None => {}
        },
    }
}

/// Retargets the encoder's input size, for the mirroring path's resolution
/// ladder. The filter is found through `venc`, which the launch strings always
/// link it straight into, rather than by walking the bin: a virtual-monitor
/// pipeline has a second `video/x-raw` filter pinned to the source pad, and
/// retargeting *that* one would resize the monitor the user's windows are on.
pub fn set_capture_size(pipeline: &gst::Pipeline, (width, height): (i32, i32)) {
    let Some(filter) = encoder_input_capsfilter(pipeline) else {
        return;
    };
    let Some(caps) = filter.property::<Option<gst::Caps>>("caps") else {
        return;
    };
    let Some(structure) = caps.structure(0) else {
        return;
    };
    let mut updated = structure.to_owned();
    updated.set("width", width);
    updated.set("height", height);
    filter.set_property("caps", gst::Caps::builder_full().structure(updated).build());
}

/// The caps filter feeding the encoder: the peer of `venc`'s sink pad.
fn encoder_input_capsfilter(pipeline: &gst::Pipeline) -> Option<gst::Element> {
    let element = pipeline
        .by_name("venc")?
        .static_pad("sink")?
        .peer()?
        .parent_element()?;
    (element.factory()?.name() == "capsfilter").then_some(element)
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

/// The H.264 encoder for the HLS path. The candidates and their order are the
/// mirroring path's (`encoder::factories`); only the launch parameters differ,
/// because HLS wants one keyframe per segment. `None` when the user's encoder or
/// pixel-format choice rules every one of them out.
fn find_h264_encoder(bitrate_kbps: i32, key_int: i32, policy: EncodingPolicy) -> Option<String> {
    let software = format!(
        "x264enc name=venc tune=zerolatency speed-preset=veryfast bitrate={bitrate_kbps} key-int-max={key_int} bframes=0"
    );
    for &f in encoder::factories(VideoCodec::H264) {
        if !encoder::allowed(f, policy) {
            continue;
        }
        let fragment = match f {
            "x264enc" => software.clone(),
            _ if encoder::api_of(f) == Some(Api::Nvenc) => {
                format!(
                    "{f} name=venc bitrate={bitrate_kbps} rc-mode=cbr gop-size={key_int} bframes=0"
                )
            }
            _ if encoder::api_of(f) == Some(Api::V4l2) => format!(
                "{f} name=venc {}",
                encoder::v4l2_controls(
                    u32::try_from(bitrate_kbps)
                        .unwrap_or(0)
                        .saturating_mul(1000),
                    u32::try_from(key_int).unwrap_or(1),
                )
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
/// `hls_dir`: H.264 from the captured `PipeWire` node named by `video`, plus AAC
/// system audio when `audio` names the pulse monitor device and the AAC encoder
/// element. Audio-only casts pass `video: None` and produce audio-only TS
/// segments.
pub fn launch_description(
    video: Option<VideoSource>,
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
    if let Some(source) = video {
        let (fd, node_id) = (source.fd, source.node_id);
        let source_caps = source.source_caps();
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
             {source_caps}\
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
    video: Option<VideoSource>,
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
            Some(VideoSource {
                fd: 3,
                node_id: 42,
                capture_size: None,
            }),
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
            Some(VideoSource {
                fd: 3,
                node_id: 42,
                capture_size: None,
            }),
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
            Some(VideoSource {
                fd: 3,
                node_id: 42,
                capture_size: None,
            }),
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

    #[test]
    /// The pinned size has to reach `pipewiresrc`, which means ahead of
    /// videoscale, and must name nothing but width and height.
    fn a_virtual_capture_pins_the_source_size() {
        let desc = launch_description(
            Some(VideoSource {
                fd: 3,
                node_id: 42,
                capture_size: Some((1920, 1080)),
            }),
            &StreamSettings::default(),
            &PathBuf::from("/run/x"),
            None,
            "x264enc bitrate=4000",
        );
        assert!(
            desc.contains("capsfilter name=srccaps caps=video/x-raw,width=1920,height=1080"),
            "{desc}"
        );
        let pin = desc.find("srccaps").unwrap_or(usize::MAX);
        assert!(pin < desc.find("videoscale").unwrap_or(0), "{desc}");
        // A framerate here would never intersect what mutter offers for a
        // virtual stream, and a pixel-aspect-ratio is not ours to demand.
        let fragment = desc
            .get(pin..desc.find("! queue").unwrap_or(pin))
            .unwrap_or_default();
        assert!(!fragment.contains("framerate"), "{fragment}");
        assert!(!fragment.contains("pixel-aspect-ratio"), "{fragment}");
    }

    #[test]
    /// A real monitor or window sizes itself, so nothing is pinned and the
    /// description stays exactly what it was before virtual monitors existed.
    fn a_monitor_capture_has_no_source_capsfilter() {
        let desc = launch_description(
            Some(VideoSource {
                fd: 3,
                node_id: 42,
                capture_size: None,
            }),
            &StreamSettings::default(),
            &PathBuf::from("/run/x"),
            None,
            "x264enc bitrate=4000",
        );
        assert!(!desc.contains("srccaps"), "{desc}");
        assert!(desc.contains("resend-last=true ! queue"), "{desc}");
    }

    #[test]
    /// The resolution ladder must retarget the encoder's filter, never the one
    /// pinning a virtual monitor's size.
    fn set_capture_size_leaves_the_source_filter_alone() {
        gst::init().unwrap_or_default();
        let Ok(element) = gst::parse::launch(
            "videotestsrc ! capsfilter name=srccaps caps=video/x-raw,width=1920,height=1080 \
             ! videoscale ! capsfilter caps=video/x-raw,width=1920,height=1080 \
             ! identity name=venc ! fakesink",
        ) else {
            return; // videotestsrc/videoscale absent; nothing to assert against
        };
        let Ok(pipeline) = element.downcast::<gst::Pipeline>() else {
            return;
        };
        set_capture_size(&pipeline, (1280, 720));

        let size_of = |name: &str| -> Option<(i32, i32)> {
            let caps = pipeline
                .by_name(name)?
                .property::<Option<gst::Caps>>("caps")?;
            let s = caps.structure(0)?;
            Some((s.get("width").ok()?, s.get("height").ok()?))
        };
        assert_eq!(size_of("srccaps"), Some((1920, 1080)));
        assert_eq!(
            encoder_input_capsfilter(&pipeline)
                .and_then(|f| f.property::<Option<gst::Caps>>("caps"))
                .and_then(|c| c
                    .structure(0)
                    .map(|s| (s.get("width").ok(), s.get("height").ok()))),
            Some((Some(1280), Some(720)))
        );
    }
}
