//! Video codec and encoder selection for the Cast Streaming (mirroring) path.
//!
//! The RTP/RTCP/crypto layer is codec-agnostic - it packetizes whole encrypted
//! frames - so the only codec-specific parts of mirroring are the `codecName`
//! advertised in the OFFER and the `GStreamer` encoder element. This module
//! owns both: which codecs we can encode locally, and the encoder for each,
//! **preferring hardware** (VA-API/NVENC) over software.
//!
//! Every candidate fragment is parse-checked before use, so a hardware encoder
//! that is present but mis-parametrised falls back to the next candidate (and
//! ultimately software) rather than failing the cast.

use gstreamer as gst;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum VideoCodec {
    Vp8,
    Vp9,
    Av1,
    H264,
}

impl VideoCodec {
    /// The `codecName` string used in the Cast OFFER.
    pub fn codec_name(self) -> &'static str {
        match self {
            Self::Vp8 => "vp8",
            Self::Vp9 => "vp9",
            Self::Av1 => "av1",
            Self::H264 => "h264",
        }
    }
}

/// Which encoders the user will accept. `Auto` keeps the hardware-first order
/// in `factories`; the other two are an escape hatch for a driver that is
/// present but misbehaving.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum EncoderPolicy {
    #[default]
    Auto,
    Hardware,
    Software,
}

/// The raw format fed to the encoder. Forcing one narrows the candidate list
/// rather than the caps, because an encoder that cannot take the format would
/// otherwise fail to link.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum FormatPolicy {
    #[default]
    Auto,
    Nv12,
    I420,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct EncodingPolicy {
    pub encoder: EncoderPolicy,
    pub format: FormatPolicy,
}

impl EncoderPolicy {
    /// Anything unrecognised means `Auto`: the extension sending the option can
    /// be a different version than the daemon reading it.
    pub fn parse(value: &str) -> Self {
        match value {
            "hardware" => Self::Hardware,
            "software" => Self::Software,
            _ => Self::Auto,
        }
    }
}

impl FormatPolicy {
    pub fn parse(value: &str) -> Self {
        match value {
            "nv12" => Self::Nv12,
            "i420" => Self::I420,
            _ => Self::Auto,
        }
    }

    /// The `format` field for the raw caps feeding the encoder. `Auto` offers
    /// both, letting negotiation pick NV12 for VA-API and I420 for the rest -
    /// leaving it out entirely would let videoconvert choose 4:4:4.
    pub fn caps_format(self) -> &'static str {
        match self {
            Self::Auto => "{NV12,I420}",
            Self::Nv12 => "NV12",
            Self::I420 => "I420",
        }
    }

    /// The format an encoder is required to accept, or `None` when unconstrained.
    fn required(self) -> Option<&'static str> {
        match self {
            Self::Auto => None,
            Self::Nv12 => Some("NV12"),
            Self::I420 => Some("I420"),
        }
    }
}

/// Efficiency order, best first - used to break ties among codecs at the same
/// hardware tier. VP8 is last and mandatory (every Cast-V2 receiver decodes
/// it), so it is the guaranteed fallback.
const EFFICIENCY_ORDER: [VideoCodec; 4] = [
    VideoCodec::Av1,
    VideoCodec::Vp9,
    VideoCodec::H264,
    VideoCodec::Vp8,
];

fn efficiency_rank(codec: VideoCodec) -> u8 {
    match codec {
        VideoCodec::Av1 => 0,
        VideoCodec::Vp9 => 1,
        VideoCodec::H264 => 2,
        VideoCodec::Vp8 => 3,
    }
}

/// `GStreamer` encoder factories to try for `codec`, best first: hardware
/// (VA-API, then NVENC) ahead of software.
fn factories(codec: VideoCodec) -> &'static [&'static str] {
    match codec {
        VideoCodec::H264 => &["vah264enc", "vah264lpenc", "nvh264enc", "x264enc"],
        VideoCodec::Vp8 => &["vavp8enc", "vp8enc"],
        VideoCodec::Vp9 => &["vavp9enc", "vp9enc"],
        // SVT-AV1 is far faster than aom's av1enc, so prefer it in software.
        VideoCodec::Av1 => &["vaav1enc", "nvav1enc", "svtav1enc", "av1enc"],
    }
}

/// VA-API / NVENC / V4L2 elements are hardware; everything else is software.
fn is_hardware(factory: &str) -> bool {
    factory.starts_with("va") || factory.starts_with("nv") || factory.starts_with("v4l2")
}

/// Whether `factory` accepts raw video in `format` on its sink pad, read from
/// the element's own template caps so it stays right across plugin versions.
/// The probe carries no caps features, so it matches only the system-memory
/// variant - which is the one the pipeline actually feeds.
fn accepts_format(factory: &str, format: &str) -> bool {
    let Some(found) = gst::ElementFactory::find(factory) else {
        return false;
    };
    let probe = gst::Caps::builder("video/x-raw")
        .field("format", format)
        .build();
    found
        .static_pad_templates()
        .iter()
        .filter(|template| template.direction() == gst::PadDirection::Sink)
        .any(|template| template.caps().can_intersect(&probe))
}

/// Whether `factory` may be used under `policy`. Shared with the HLS path so
/// both pipelines honour the setting the same way.
pub fn allowed(factory: &str, policy: EncodingPolicy) -> bool {
    let encoder_ok = match policy.encoder {
        EncoderPolicy::Auto => true,
        EncoderPolicy::Hardware => is_hardware(factory),
        EncoderPolicy::Software => !is_hardware(factory),
    };
    encoder_ok
        && policy
            .format
            .required()
            .is_none_or(|format| accepts_format(factory, format))
}

/// The launch fragment configuring `factory` for low-latency CBR at
/// `bitrate_bps`, producing an element named `venc` (so keyframe forcing can
/// find it). `fps` sizes the keyframe interval. Hardware params are kept
/// minimal - just bitrate and CBR - to maximise the chance they parse across
/// driver/plugin versions; the parse-check drops any that don't.
fn launch_for(factory: &str, bitrate_bps: u32, fps: u32) -> String {
    // svtav1/av1/VA/NVENC want kbit/s
    let kbps = bitrate_bps.checked_div(1000).unwrap_or(1).max(1);
    let key_int = fps.saturating_mul(2).max(1);
    match factory {
        // vp8enc and vp9enc share the VPX base and its properties (bit/s).
        "vp8enc" | "vp9enc" => format!(
            "{factory} name=venc deadline=1 cpu-used=8 end-usage=cbr \
             target-bitrate={bitrate_bps} keyframe-max-dist={key_int} lag-in-frames=0 \
             error-resilient=default threads=4"
        ),
        "svtav1enc" => {
            format!(
                "svtav1enc name=venc preset=12 target-bitrate={kbps} intra-period-length={key_int}"
            )
        }
        "av1enc" => format!(
            "av1enc name=venc usage-profile=realtime end-usage=cbr \
             target-bitrate={kbps} cpu-used=9 lag-in-frames=0 keyframe-max-dist={key_int} \
             threads=4"
        ),
        "x264enc" => format!(
            "x264enc name=venc tune=zerolatency speed-preset=veryfast bitrate={kbps} \
             key-int-max={key_int} bframes=0"
        ),
        // VA-API (GStreamer 'va' plugin): bitrate in kbit/s, CBR rate control.
        f if f.starts_with("va") => {
            format!("{factory} name=venc bitrate={kbps} rate-control=cbr")
        }
        // NVENC (GStreamer 'nvcodec' plugin).
        f if f.starts_with("nv") => {
            format!("{factory} name=venc bitrate={kbps} rc-mode=cbr")
        }
        other => format!("{other} name=venc"),
    }
}

/// A parse-only check that `fragment` names a real element with valid
/// properties/enum values, without disturbing the real pipeline.
fn fragment_parses(fragment: &str) -> bool {
    gst::parse::launch(fragment).is_ok()
}

/// The encoder fragment for `codec` and whether it is hardware, or `None` when
/// no encoder for it is installed **and** permitted by `policy`. Returns the
/// first candidate that actually parses.
pub fn video_encoder(
    codec: VideoCodec,
    bitrate_bps: u32,
    fps: u32,
    policy: EncodingPolicy,
) -> Option<(String, bool)> {
    factories(codec)
        .iter()
        .filter(|&&factory| allowed(factory, policy))
        .find_map(|&factory| {
            let fragment = launch_for(factory, bitrate_bps, fps);
            fragment_parses(&fragment).then(|| (fragment, is_hardware(factory)))
        })
}

/// Why no encoder was usable, phrased for the user - this reaches them verbatim
/// through `user_message()`, so it names the setting to change rather than the
/// `GStreamer` element that was missing.
pub fn policy_failure_message(policy: EncodingPolicy) -> String {
    match (policy.encoder, policy.format) {
        (EncoderPolicy::Auto, FormatPolicy::Auto) => {
            "No video encoder is installed. Install the GStreamer encoder plugins for your system."
                .to_owned()
        }
        (EncoderPolicy::Hardware, _) => {
            "No hardware video encoder can be used for this device. Set the video encoder \
             preference back to automatic."
                .to_owned()
        }
        (EncoderPolicy::Software, _) => {
            "No software video encoder can be used for this device. Set the video encoder \
             preference back to automatic."
                .to_owned()
        }
        (EncoderPolicy::Auto, _) => {
            "No video encoder accepts the selected pixel format. Set the pixel format \
             preference back to automatic."
                .to_owned()
        }
    }
}

/// The codecs we can encode on this host, **hardware-encodable ones first**,
/// then by efficiency. Used to build the OFFER - we advertise only codecs we
/// can produce, in the order we prefer to use them.
pub fn available_video_codecs(policy: EncodingPolicy) -> Vec<VideoCodec> {
    let mut avail: Vec<(VideoCodec, bool)> = EFFICIENCY_ORDER
        .into_iter()
        .filter_map(|codec| video_encoder(codec, 4_000_000, 30, policy).map(|(_, hw)| (codec, hw)))
        .collect();
    avail.sort_by_key(|&(codec, hw)| (!hw, efficiency_rank(codec)));
    avail.into_iter().map(|(codec, _)| codec).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codec_names_match_the_cast_offer_strings() {
        assert_eq!(VideoCodec::Vp8.codec_name(), "vp8");
        assert_eq!(VideoCodec::Vp9.codec_name(), "vp9");
        assert_eq!(VideoCodec::Av1.codec_name(), "av1");
        assert_eq!(VideoCodec::H264.codec_name(), "h264");
    }

    #[test]
    fn vpx_bitrate_is_bits_per_second() {
        let f = launch_for("vp9enc", 4_000_000, 30);
        assert!(f.starts_with("vp9enc name=venc"));
        assert!(f.contains("end-usage=cbr"));
        assert!(f.contains("target-bitrate=4000000"));
    }

    /// A receiver that lost the first key frame shows black until the next.
    #[test]
    fn every_fragment_keys_about_every_two_seconds() {
        for codec in EFFICIENCY_ORDER {
            for factory in factories(codec) {
                let f = launch_for(factory, 2_000_000, 40);
                if let Some(rest) = f.split("keyframe-max-dist=").nth(1) {
                    assert_eq!(rest.split_whitespace().next(), Some("80"), "{factory}");
                }
            }
        }
        assert!(launch_for("x264enc", 2_000_000, 40).contains("key-int-max=80"));
        assert!(launch_for("svtav1enc", 2_000_000, 40).contains("intra-period-length=80"));
    }

    #[test]
    fn av1_bitrate_is_kilobits_per_second() {
        assert!(launch_for("svtav1enc", 4_000_000, 30).contains("target-bitrate=4000"));
        assert!(launch_for("av1enc", 4_000_000, 30).contains("target-bitrate=4000"));
        assert!(launch_for("svtav1enc", 4_000_000, 30).contains("intra-period-length=60"));
    }

    #[test]
    fn hardware_encoders_are_detected_and_kilobit_rated() {
        assert!(is_hardware("vah264enc"));
        assert!(is_hardware("nvav1enc"));
        assert!(!is_hardware("x264enc"));
        assert!(!is_hardware("svtav1enc"));
        assert!(!is_hardware("vp9enc"));
        // 4 Mbit/s -> 4000 kbit/s for VA/NVENC.
        assert!(launch_for("vah264enc", 4_000_000, 30).contains("bitrate=4000"));
        assert!(launch_for("nvh264enc", 4_000_000, 30).contains("rc-mode=cbr"));
    }

    /// The format lookup has to agree with what the pipeline can actually link:
    /// VA-API H.264 takes only NV12, the VPX/AV1 encoders only I420. Each
    /// assertion is skipped when the plugin is not installed, so this passes on
    /// a CI box without gst-plugin-va.
    #[test]
    fn accepted_formats_match_the_encoders() {
        gst::init().unwrap();
        let installed = |factory| gst::ElementFactory::find(factory).is_some();

        if installed("vah264enc") {
            assert!(accepts_format("vah264enc", "NV12"));
            assert!(!accepts_format("vah264enc", "I420"));
        }
        if installed("vp8enc") {
            assert!(accepts_format("vp8enc", "I420"));
            assert!(!accepts_format("vp8enc", "NV12"));
        }
        if installed("x264enc") {
            assert!(accepts_format("x264enc", "I420"));
            assert!(accepts_format("x264enc", "NV12"));
        }
        assert!(!accepts_format("no-such-encoder", "I420"));
    }

    #[test]
    fn policy_filters_the_candidate_list() {
        gst::init().unwrap();
        let auto = EncodingPolicy::default();
        let software = EncodingPolicy {
            encoder: EncoderPolicy::Software,
            ..auto
        };
        let hardware = EncodingPolicy {
            encoder: EncoderPolicy::Hardware,
            ..auto
        };

        assert!(allowed("x264enc", auto));
        assert!(allowed("x264enc", software));
        assert!(!allowed("x264enc", hardware));
        assert!(allowed("vah264enc", hardware));
        assert!(!allowed("vah264enc", software));

        // A forced format rules out encoders that cannot take it, which is what
        // keeps a forced value from producing a pipeline that will not link.
        if gst::ElementFactory::find("vah264enc").is_some() {
            let i420 = EncodingPolicy {
                format: FormatPolicy::I420,
                ..auto
            };
            assert!(!allowed("vah264enc", i420));
            assert!(allowed("x264enc", i420));
        }
    }

    #[test]
    fn unknown_option_values_fall_back_to_auto() {
        assert_eq!(EncoderPolicy::parse("software"), EncoderPolicy::Software);
        assert_eq!(EncoderPolicy::parse("nonsense"), EncoderPolicy::Auto);
        assert_eq!(EncoderPolicy::parse(""), EncoderPolicy::Auto);
        assert_eq!(FormatPolicy::parse("nv12"), FormatPolicy::Nv12);
        assert_eq!(FormatPolicy::parse("yuv"), FormatPolicy::Auto);
        assert_eq!(FormatPolicy::Auto.caps_format(), "{NV12,I420}");
        assert_eq!(FormatPolicy::I420.caps_format(), "I420");
    }

    #[test]
    fn every_fragment_names_the_encoder_venc() {
        for codec in EFFICIENCY_ORDER {
            for factory in factories(codec) {
                assert!(launch_for(factory, 2_000_000, 24).contains("name=venc"));
            }
        }
    }
}
