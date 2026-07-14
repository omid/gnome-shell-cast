//! Video codec selection for the Cast Streaming (mirroring) path.
//!
//! The RTP/RTCP/crypto layer is codec-agnostic — it packetizes whole encrypted
//! frames — so the only codec-specific parts of mirroring are the `codecName`
//! advertised in the OFFER and the `GStreamer` encoder element. This module
//! owns both: which codecs we can encode locally, and the encoder launch
//! fragment for each. Hardware encoders (VA-API/NVENC) will slot in ahead of
//! the software ones in [`factories`] without touching callers.

use gstreamer as gst;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum VideoCodec {
    Vp8,
    Vp9,
    Av1,
}

impl VideoCodec {
    /// The `codecName` string used in the Cast OFFER.
    pub fn codec_name(self) -> &'static str {
        match self {
            VideoCodec::Vp8 => "vp8",
            VideoCodec::Vp9 => "vp9",
            VideoCodec::Av1 => "av1",
        }
    }
}

/// Preference order, best first. VP8 is last and mandatory (every Cast-V2
/// receiver implements it), so it is the guaranteed fallback. Higher-efficiency
/// codecs are only realtime-practical with a hardware encoder; on a
/// software-only host they may not sustain framerate at high resolutions.
pub const VIDEO_CODEC_PRIORITY: [VideoCodec; 3] =
    [VideoCodec::Av1, VideoCodec::Vp9, VideoCodec::Vp8];

/// `GStreamer` encoder element factories to try for `codec`, best first.
/// Step 2 prepends hardware encoders (e.g. `vavp9enc`, `nvav1enc`) here.
fn factories(codec: VideoCodec) -> &'static [&'static str] {
    match codec {
        VideoCodec::Vp8 => &["vp8enc"],
        VideoCodec::Vp9 => &["vp9enc"],
        // SVT-AV1 is far faster than aom's av1enc, so prefer it for realtime.
        VideoCodec::Av1 => &["svtav1enc", "av1enc"],
    }
}

/// The launch fragment configuring `factory` for low-latency CBR at
/// `bitrate_bps`, producing an element named `venc` (so keyframe forcing can
/// find it). `fps` sizes the AV1 keyframe interval.
fn launch_for(factory: &str, bitrate_bps: u32, fps: u32) -> String {
    // svtav1enc/av1enc want kbit/s; the VPX encoders want bit/s.
    let kbps = (bitrate_bps / 1000).max(1);
    match factory {
        // vp8enc and vp9enc share the VPX base and its properties.
        "vp8enc" | "vp9enc" => format!(
            "{factory} name=venc deadline=1 cpu-used=8 end-usage=cbr \
             target-bitrate={bitrate_bps} keyframe-max-dist=3000 lag-in-frames=0 \
             error-resilient=default threads=4"
        ),
        "svtav1enc" => format!(
            // preset 12 trades quality for speed; a ~2s intra period lets a
            // receiver recover even between the keyframes we force on demand.
            "svtav1enc name=venc preset=12 target-bitrate={kbps} \
             intra-period-length={}",
            (fps * 2).max(1)
        ),
        "av1enc" => format!(
            "av1enc name=venc usage-profile=realtime end-usage=cbr \
             target-bitrate={kbps} cpu-used=9 lag-in-frames=0 keyframe-max-dist=3000 \
             threads=4"
        ),
        other => format!("{other} name=venc"),
    }
}

/// The encoder launch fragment for `codec`, or `None` when no encoder for it is
/// installed. Picks the first available factory from [`factories`].
pub fn video_encoder(codec: VideoCodec, bitrate_bps: u32, fps: u32) -> Option<String> {
    factories(codec)
        .iter()
        .find(|f| gst::ElementFactory::find(f).is_some())
        .map(|f| launch_for(f, bitrate_bps, fps))
}

/// The codecs we can actually encode on this host, in preference order. Used
/// to build the OFFER — we only advertise codecs we can produce.
pub fn available_video_codecs() -> Vec<VideoCodec> {
    VIDEO_CODEC_PRIORITY
        .into_iter()
        .filter(|&codec| {
            factories(codec)
                .iter()
                .any(|f| gst::ElementFactory::find(f).is_some())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codec_names_match_the_cast_offer_strings() {
        assert_eq!(VideoCodec::Vp8.codec_name(), "vp8");
        assert_eq!(VideoCodec::Vp9.codec_name(), "vp9");
        assert_eq!(VideoCodec::Av1.codec_name(), "av1");
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
        // svtav1enc/av1enc take kbit/s, so 4 Mbit/s -> 4000.
        assert!(launch_for("svtav1enc", 4_000_000, 30).contains("target-bitrate=4000"));
        assert!(launch_for("av1enc", 4_000_000, 30).contains("target-bitrate=4000"));
        // ~2s intra period at 30fps.
        assert!(launch_for("svtav1enc", 4_000_000, 30).contains("intra-period-length=60"));
    }

    #[test]
    fn every_fragment_names_the_encoder_venc() {
        for codec in VIDEO_CODEC_PRIORITY {
            for factory in factories(codec) {
                assert!(launch_for(factory, 2_000_000, 24).contains("name=venc"));
            }
        }
    }
}
