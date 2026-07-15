//! Video codec and encoder selection for the Cast Streaming (mirroring) path.
//!
//! The RTP/RTCP/crypto layer is codec-agnostic — it packetizes whole encrypted
//! frames — so the only codec-specific parts of mirroring are the `codecName`
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
            VideoCodec::Vp8 => "vp8",
            VideoCodec::Vp9 => "vp9",
            VideoCodec::Av1 => "av1",
            VideoCodec::H264 => "h264",
        }
    }
}

/// Efficiency order, best first — used to break ties among codecs at the same
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

/// The launch fragment configuring `factory` for low-latency CBR at
/// `bitrate_bps`, producing an element named `venc` (so keyframe forcing can
/// find it). `fps` sizes the keyframe interval. Hardware params are kept
/// minimal — just bitrate and CBR — to maximise the chance they parse across
/// driver/plugin versions; the parse-check drops any that don't.
fn launch_for(factory: &str, bitrate_bps: u32, fps: u32) -> String {
    let kbps = (bitrate_bps / 1000).max(1); // svtav1/av1/VA/NVENC want kbit/s
    let key_int = (fps * 2).max(1);
    match factory {
        // vp8enc and vp9enc share the VPX base and its properties (bit/s).
        "vp8enc" | "vp9enc" => format!(
            "{factory} name=venc deadline=1 cpu-used=8 end-usage=cbr \
             target-bitrate={bitrate_bps} keyframe-max-dist=3000 lag-in-frames=0 \
             error-resilient=default threads=4"
        ),
        "svtav1enc" => {
            format!(
                "svtav1enc name=venc preset=12 target-bitrate={kbps} intra-period-length={key_int}"
            )
        }
        "av1enc" => format!(
            "av1enc name=venc usage-profile=realtime end-usage=cbr \
             target-bitrate={kbps} cpu-used=9 lag-in-frames=0 keyframe-max-dist=3000 \
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
/// no working encoder for it is installed. Returns the first candidate that
/// actually parses.
pub fn video_encoder(codec: VideoCodec, bitrate_bps: u32, fps: u32) -> Option<(String, bool)> {
    factories(codec).iter().find_map(|&factory| {
        let fragment = launch_for(factory, bitrate_bps, fps);
        fragment_parses(&fragment).then(|| (fragment, is_hardware(factory)))
    })
}

/// The codecs we can encode on this host, **hardware-encodable ones first**,
/// then by efficiency. Used to build the OFFER — we advertise only codecs we
/// can produce, in the order we prefer to use them.
pub fn available_video_codecs() -> Vec<VideoCodec> {
    let mut avail: Vec<(VideoCodec, bool)> = EFFICIENCY_ORDER
        .into_iter()
        .filter_map(|codec| video_encoder(codec, 4_000_000, 30).map(|(_, hw)| (codec, hw)))
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

    #[test]
    fn every_fragment_names_the_encoder_venc() {
        for codec in EFFICIENCY_ORDER {
            for factory in factories(codec) {
                assert!(launch_for(factory, 2_000_000, 24).contains("name=venc"));
            }
        }
    }
}
