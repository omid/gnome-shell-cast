//! The negotiated quality envelope: what the receiver will accept, what we are
//! willing to send, and the resolved values the pipeline is built from.
//!
//! Ported from openscreen's `capture_recommendations` and Chromium's
//! `mirror_settings`. Every user setting is optional; `None` means automatic,
//! and automatic means "whatever the receiver and these limits allow". A set
//! value is still clamped into the envelope - the receiver's ANSWER wins over a
//! preference, because exceeding it produces a stream it cannot decode.

use serde_json::Value;

/// What we are willing to produce regardless of what the receiver allows.
/// Chromium keeps the same table in `mirror_settings.cc`; the two are combined
/// by taking the stricter of the two, as `SetConstraints` does.
mod sender {
    pub const MIN_WIDTH: i32 = 180;
    pub const MIN_HEIGHT: i32 = 180;
    pub const MAX_WIDTH: i32 = 1920;
    pub const MAX_HEIGHT: i32 = 1080;
    pub const MAX_FPS: i32 = 30;
    pub const MIN_FPS: i32 = 5;
    /// Chromium starts at 5 Mbit/s and treats it as the ceiling too.
    pub const START_VIDEO_BITRATE_BPS: i32 = 5_000_000;
    pub const MAX_VIDEO_BITRATE_BPS: i32 = 5_000_000;
    pub const AUDIO_BITRATE_BPS: i32 = 128_000;
    pub const AUDIO_SAMPLE_RATE: i32 = 48_000;
    pub const AUDIO_CHANNELS: i32 = 2;
}

/// openscreen's defaults, used for whatever the ANSWER leaves out. Receivers
/// commonly send no constraints at all, so these are the usual path rather
/// than a rare fallback.
mod defaults {
    pub const VIDEO_MIN_BITRATE_BPS: i32 = 300_000;
    pub const VIDEO_MAX_BITRATE_BPS: i32 = 62_500_000;
    pub const AUDIO_MIN_BITRATE_BPS: i32 = 32_000;
    pub const AUDIO_MAX_BITRATE_BPS: i32 = 256_000;
    pub const AUDIO_MIN_SAMPLE_RATE: i32 = 16_000;
    pub const MAX_DELAY_MS: i32 = 400;
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct BitRateLimits {
    pub min_bps: i32,
    pub max_bps: i32,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct VideoConstraints {
    pub bit_rate: BitRateLimits,
    pub min: (i32, i32),
    pub max: (i32, i32),
    pub max_fps: i32,
    /// Caps width x height x fps independently of the dimensions, so 1080p and
    /// 30fps can each be allowed while their product is not.
    pub max_pixels_per_second: i64,
    pub max_delay_ms: i32,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct AudioConstraints {
    pub bit_rate: BitRateLimits,
    pub max_sample_rate: i32,
    pub min_sample_rate: i32,
    pub max_channels: i32,
    pub max_delay_ms: i32,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Constraints {
    pub video: VideoConstraints,
    pub audio: AudioConstraints,
}

impl Default for VideoConstraints {
    fn default() -> Self {
        Self {
            bit_rate: BitRateLimits {
                min_bps: defaults::VIDEO_MIN_BITRATE_BPS,
                max_bps: defaults::VIDEO_MAX_BITRATE_BPS,
            },
            min: (320, 180),
            max: (sender::MAX_WIDTH, sender::MAX_HEIGHT),
            max_fps: sender::MAX_FPS,
            max_pixels_per_second: i64::from(sender::MAX_WIDTH)
                .saturating_mul(i64::from(sender::MAX_HEIGHT))
                .saturating_mul(i64::from(sender::MAX_FPS)),
            max_delay_ms: defaults::MAX_DELAY_MS,
        }
    }
}

impl Default for AudioConstraints {
    fn default() -> Self {
        Self {
            bit_rate: BitRateLimits {
                min_bps: defaults::AUDIO_MIN_BITRATE_BPS,
                max_bps: defaults::AUDIO_MAX_BITRATE_BPS,
            },
            max_sample_rate: sender::AUDIO_SAMPLE_RATE,
            min_sample_rate: defaults::AUDIO_MIN_SAMPLE_RATE,
            max_channels: sender::AUDIO_CHANNELS,
            max_delay_ms: defaults::MAX_DELAY_MS,
        }
    }
}

fn as_i32(value: &Value) -> Option<i32> {
    // Cast sends some of these as strings ("30", "30000/1001").
    value
        .as_i64()
        .and_then(|v| i32::try_from(v).ok())
        .or_else(|| {
            let text = value.as_str()?;
            let head = text.split_once('/').map_or(text, |(num, _)| num);
            head.trim().parse().ok()
        })
}

/// Largest value we accept for `maxPixelsPerSecond`, well inside `i64`.
const MAX_PIXELS_PER_SECOND: f64 = 9.0e18;

#[allow(
    clippy::cast_possible_truncation,
    reason = "the JSON value is a float; it is bounds-checked before the cast"
)]
fn pixels_per_second(value: &Value) -> Option<i64> {
    let pixels = value.as_f64()?;
    (pixels.is_finite() && pixels > 0.0).then(|| pixels.min(MAX_PIXELS_PER_SECOND) as i64)
}

fn dimensions(value: &Value) -> Option<(i32, i32)> {
    Some((as_i32(value.get("width")?)?, as_i32(value.get("height")?)?))
}

/// Reads the optional `constraints` block of an ANSWER. Anything missing or
/// unparseable keeps its default, so a receiver that sends none - which is
/// common - still yields a usable envelope.
pub fn parse(answer: &Value) -> Constraints {
    let mut constraints = Constraints::default();
    let Some(block) = answer.get("constraints") else {
        return constraints;
    };

    if let Some(video) = block.get("video") {
        let v = &mut constraints.video;
        if let Some(min) = video.get("minBitRate").and_then(as_i32) {
            v.bit_rate.min_bps = min.max(1);
        }
        if let Some(max) = video.get("maxBitRate").and_then(as_i32) {
            v.bit_rate.max_bps = max.max(v.bit_rate.min_bps);
        }
        if let Some(min) = video.get("minDimensions").and_then(dimensions) {
            v.min = min;
        }
        if let Some(max) = video.get("maxDimensions").and_then(dimensions) {
            v.max = max;
        }
        if let Some(fps) = video
            .get("maxDimensions")
            .and_then(|d| d.get("frameRate"))
            .and_then(as_i32)
            .filter(|fps| *fps > 0)
        {
            v.max_fps = fps;
        }
        if let Some(pixels) = video.get("maxPixelsPerSecond").and_then(pixels_per_second) {
            v.max_pixels_per_second = pixels;
        }
        if let Some(delay) = video.get("maxDelay").and_then(as_i32) {
            v.max_delay_ms = delay;
        }
    }

    if let Some(audio) = block.get("audio") {
        let a = &mut constraints.audio;
        if let Some(min) = audio.get("minBitRate").and_then(as_i32) {
            a.bit_rate.min_bps = min.max(1);
        }
        if let Some(max) = audio.get("maxBitRate").and_then(as_i32) {
            a.bit_rate.max_bps = max.max(a.bit_rate.min_bps);
        }
        if let Some(rate) = audio
            .get("maxSampleRate")
            .and_then(as_i32)
            .filter(|r| *r > 0)
        {
            a.max_sample_rate = rate;
        }
        if let Some(channels) = audio.get("maxChannels").and_then(as_i32).filter(|c| *c > 0) {
            a.max_channels = channels;
        }
        if let Some(delay) = audio.get("maxDelay").and_then(as_i32) {
            a.max_delay_ms = delay;
        }
    }

    constraints
}

/// The values the pipeline is actually built from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Resolved {
    pub size: (i32, i32),
    pub fps: i32,
    pub video_bitrate_kbps: i32,
    pub audio_bitrate_bps: i32,
    /// The range the bitrate control loop may move within.
    pub video_min_bitrate_bps: i32,
    pub video_max_bitrate_bps: i32,
    pub sample_rate: i32,
    pub channels: i32,
    pub playout_delay_ms: i32,
}

/// Scales `size` down to fit inside `max` without changing its aspect ratio.
fn fit_inside(size: (i32, i32), max: (i32, i32)) -> (i32, i32) {
    let (w, h) = (size.0.max(2), size.1.max(2));
    let (max_w, max_h) = (max.0.max(2), max.1.max(2));
    if w <= max_w && h <= max_h {
        return (w, h);
    }
    // Integer maths throughout: the encoders want even dimensions anyway.
    let scale = |other: i32, to: i32, from: i32| -> i32 {
        i64::from(other)
            .saturating_mul(i64::from(to))
            .checked_div(i64::from(from))
            .and_then(|v| i32::try_from(v).ok())
            .unwrap_or(2)
    };
    let by_width = (max_w, scale(h, max_w, w));
    let by_height = (scale(w, max_h, h), max_h);
    let fitted = if by_width.1 <= max_h {
        by_width
    } else {
        by_height
    };
    (fitted.0.max(2) & !1, fitted.1.max(2) & !1)
}

/// Combines the user's settings (`None` meaning automatic), our own limits and
/// the receiver's constraints. The receiver always wins: a preference above
/// what it allows would produce a stream it cannot decode.
pub fn resolve(
    requested_size: Option<(i32, i32)>,
    requested_fps: Option<i32>,
    requested_video_kbps: Option<i32>,
    requested_audio_kbps: Option<i32>,
    captured: (i32, i32),
    constraints: &Constraints,
) -> Resolved {
    let v = &constraints.video;
    let max = (
        v.max.0.clamp(sender::MIN_WIDTH, sender::MAX_WIDTH),
        v.max.1.clamp(sender::MIN_HEIGHT, sender::MAX_HEIGHT),
    );

    // Automatic keeps the captured size, only shrinking it to fit the envelope.
    let size = fit_inside(requested_size.unwrap_or(captured), max);
    let size = (
        size.0.max(v.min.0.min(max.0)) & !1,
        size.1.max(v.min.1.min(max.1)) & !1,
    );

    let max_fps = v.max_fps.clamp(sender::MIN_FPS, sender::MAX_FPS);
    let mut fps = requested_fps
        .unwrap_or(max_fps)
        .clamp(sender::MIN_FPS, max_fps);

    // maxPixelsPerSecond is a separate limit from the dimensions; spend it on
    // frame rate first, which degrades more gracefully than resolution.
    let pixels = i64::from(size.0).saturating_mul(i64::from(size.1));
    if pixels > 0 && pixels.saturating_mul(i64::from(fps)) > v.max_pixels_per_second {
        let allowed = v
            .max_pixels_per_second
            .checked_div(pixels)
            .and_then(|v| i32::try_from(v).ok())
            .unwrap_or(sender::MIN_FPS);
        fps = allowed.clamp(sender::MIN_FPS, fps);
    }

    let video_max = v.bit_rate.max_bps.min(sender::MAX_VIDEO_BITRATE_BPS);
    let video_min = v.bit_rate.min_bps.min(video_max);
    let video_bps = requested_video_kbps
        .map_or(sender::START_VIDEO_BITRATE_BPS, |kbps| {
            kbps.saturating_mul(1000)
        })
        .clamp(video_min, video_max);

    let a = &constraints.audio;
    let audio_max = a.bit_rate.max_bps.min(defaults::AUDIO_MAX_BITRATE_BPS);
    let audio_min = a.bit_rate.min_bps.min(audio_max);
    let audio_bps = requested_audio_kbps
        .map_or(sender::AUDIO_BITRATE_BPS, |kbps| kbps.saturating_mul(1000))
        .clamp(audio_min, audio_max);

    Resolved {
        size,
        fps,
        video_bitrate_kbps: video_bps.checked_div(1000).unwrap_or(1).max(1),
        video_min_bitrate_bps: video_min,
        video_max_bitrate_bps: video_max,
        audio_bitrate_bps: audio_bps,
        sample_rate: sender::AUDIO_SAMPLE_RATE.clamp(a.min_sample_rate, a.max_sample_rate),
        channels: sender::AUDIO_CHANNELS.clamp(1, a.max_channels.max(1)),
        playout_delay_ms: v.max_delay_ms.min(a.max_delay_ms).max(1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn auto(captured: (i32, i32), constraints: &Constraints) -> Resolved {
        resolve(None, None, None, None, captured, constraints)
    }

    /// The device we test against sends none of these, so the defaults are the
    /// common path rather than a fallback.
    #[test]
    fn an_answer_without_constraints_keeps_every_default() {
        let answer = json!({"udpPort": 1234, "sendIndexes": [0, 1]});
        assert_eq!(parse(&answer), Constraints::default());
    }

    #[test]
    fn constraints_are_read_from_the_answer() {
        let answer = json!({"constraints": {
            "video": {
                "minBitRate": 300_000, "maxBitRate": 10_000_000,
                "maxDimensions": {"width": 1280, "height": 720, "frameRate": "24"},
                "maxPixelsPerSecond": 27_648_000.0, "maxDelay": 800,
            },
            "audio": {"maxBitRate": 96000, "maxChannels": 1, "maxSampleRate": 24000},
        }});
        let c = parse(&answer);
        assert_eq!(c.video.max, (1280, 720));
        assert_eq!(c.video.max_fps, 24);
        assert_eq!(c.video.bit_rate.max_bps, 10_000_000);
        assert_eq!(c.video.max_delay_ms, 800);
        assert_eq!(c.audio.bit_rate.max_bps, 96_000);
        assert_eq!(c.audio.max_channels, 1);
        assert_eq!(c.audio.max_sample_rate, 24_000);
    }

    #[test]
    fn automatic_keeps_the_captured_size_when_it_fits() {
        let r = auto((1280, 720), &Constraints::default());
        assert_eq!(r.size, (1280, 720));
        assert_eq!(r.fps, 30);
    }

    #[test]
    fn automatic_shrinks_an_oversized_capture_keeping_the_aspect_ratio() {
        let r = auto((3840, 2160), &Constraints::default());
        assert_eq!(r.size, (1920, 1080));
    }

    #[test]
    fn a_non_16_by_9_capture_keeps_its_aspect_ratio() {
        let r = auto((2560, 1600), &Constraints::default());
        assert_eq!(r.size.0, 1728);
        assert_eq!(r.size.1, 1080);
    }

    #[test]
    fn the_receiver_wins_over_a_larger_request() {
        let mut c = Constraints::default();
        c.video.max = (1280, 720);
        let r = resolve(Some((1920, 1080)), None, None, None, (1920, 1080), &c);
        assert_eq!(r.size, (1280, 720));
    }

    #[test]
    fn frame_rate_is_capped_by_max_pixels_per_second() {
        let mut c = Constraints::default();
        // 1080p at only 15 fps worth of pixels.
        c.video.max_pixels_per_second = 1920 * 1080 * 15;
        let r = resolve(Some((1920, 1080)), Some(30), None, None, (1920, 1080), &c);
        assert_eq!(r.fps, 15);
    }

    #[test]
    fn bitrates_are_clamped_into_the_receivers_range() {
        let mut c = Constraints::default();
        c.video.bit_rate = BitRateLimits {
            min_bps: 500_000,
            max_bps: 2_000_000,
        };
        c.audio.bit_rate = BitRateLimits {
            min_bps: 32_000,
            max_bps: 64_000,
        };
        let r = resolve(None, None, Some(9000), Some(128), (1280, 720), &c);
        assert_eq!(r.video_bitrate_kbps, 2000);
        assert_eq!(r.audio_bitrate_bps, 64_000);
    }

    #[test]
    fn automatic_bitrate_starts_at_the_chromium_default() {
        let r = auto((1280, 720), &Constraints::default());
        assert_eq!(r.video_bitrate_kbps, 5000);
        assert_eq!(r.audio_bitrate_bps, 128_000);
        assert_eq!(r.sample_rate, 48_000);
        assert_eq!(r.channels, 2);
    }

    #[test]
    fn a_mono_receiver_narrows_the_audio_format() {
        let mut c = Constraints::default();
        c.audio.max_channels = 1;
        c.audio.max_sample_rate = 24_000;
        let r = auto((1280, 720), &c);
        assert_eq!(r.channels, 1);
        assert_eq!(r.sample_rate, 24_000);
    }

    #[test]
    fn frame_rates_arrive_as_strings_and_fractions() {
        assert_eq!(as_i32(&json!("30")), Some(30));
        assert_eq!(as_i32(&json!("30000/1001")), Some(30000));
        assert_eq!(as_i32(&json!(24)), Some(24));
        assert_eq!(as_i32(&json!("nonsense")), None);
    }
}
