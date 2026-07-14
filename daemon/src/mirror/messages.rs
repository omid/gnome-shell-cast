//! OFFER/ANSWER JSON messages for the `urn:x-cast:com.google.cast.webrtc`
//! namespace, matching openscreen `cast/streaming/public/offer_messages.cc`,
//! `answer_messages.cc` and `message_fields.h`.

use anyhow::{Context, Result, anyhow};
use serde_json::{Value, json};

pub const WEBRTC_NAMESPACE: &str = "urn:x-cast:com.google.cast.webrtc";
/// The "Chrome Mirroring" receiver app.
pub const MIRRORING_APP_ID: &str = "0F5096E8";

/// Chrome's default end-to-end target playout delay for mirroring.
pub const TARGET_PLAYOUT_DELAY_MS: u16 = 400;

/// RTP payload types. Chrome always sends these legacy values ("the Android
/// TV hack" in openscreen): some receivers require audio=127 and video=96
/// regardless of codec.
pub const AUDIO_PAYLOAD_TYPE: u8 = 127;
pub const VIDEO_PAYLOAD_TYPE: u8 = 96;

pub const AUDIO_RTP_TIMEBASE: u32 = 48_000;
pub const VIDEO_RTP_TIMEBASE: u32 = 90_000;

pub struct AudioParams {
    pub index: u32,
    pub ssrc: u32,
    pub aes_key: [u8; 16],
    pub aes_iv_mask: [u8; 16],
    pub bit_rate: u32,
}

pub struct VideoParams {
    pub index: u32,
    pub ssrc: u32,
    pub aes_key: [u8; 16],
    pub aes_iv_mask: [u8; 16],
    /// Cast OFFER `codecName` (e.g. "vp8"/"vp9"/"av1"); one variant per codec.
    pub codec_name: &'static str,
    pub max_bit_rate: u32,
    pub max_fps: u32,
    pub width: u32,
    pub height: u32,
}

fn hex(bytes: &[u8; 16]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::with_capacity(32), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// The OFFER message body: `{"type": "OFFER", "seqNum": .., "offer": {..}}`.
/// Audio is optional (video-only cast when no monitor source exists). `videos`
/// carries one variant per video codec we can encode — the receiver selects
/// one via the ANSWER's `sendIndexes`; it is empty for an audio-only cast.
/// Callers pass at least one stream in total.
pub fn offer(seq_num: u32, audio: Option<&AudioParams>, videos: &[VideoParams]) -> Value {
    let mut streams = Vec::new();

    if let Some(a) = audio {
        streams.push(json!({
            "index": a.index,
            "type": "audio_source",
            "codecName": "opus",
            "rtpProfile": "cast",
            "rtpPayloadType": AUDIO_PAYLOAD_TYPE,
            "ssrc": a.ssrc,
            "targetDelay": TARGET_PLAYOUT_DELAY_MS,
            "aesKey": hex(&a.aes_key),
            "aesIvMask": hex(&a.aes_iv_mask),
            "timeBase": format!("1/{AUDIO_RTP_TIMEBASE}"),
            "channels": 2,
            "bitRate": a.bit_rate,
            "receiverRtcpEventLog": false,
        }));
    }

    for v in videos {
        streams.push(json!({
            "index": v.index,
            "type": "video_source",
            "codecName": v.codec_name,
            "rtpProfile": "cast",
            "rtpPayloadType": VIDEO_PAYLOAD_TYPE,
            "ssrc": v.ssrc,
            "targetDelay": TARGET_PLAYOUT_DELAY_MS,
            "aesKey": hex(&v.aes_key),
            "aesIvMask": hex(&v.aes_iv_mask),
            "timeBase": format!("1/{VIDEO_RTP_TIMEBASE}"),
            "channels": 1,
            "maxFrameRate": format!("{}/1", v.max_fps),
            "maxBitRate": v.max_bit_rate,
            "profile": "",
            "level": "",
            "resolutions": [{"width": v.width, "height": v.height}],
            "receiverRtcpEventLog": false,
        }));
    }

    json!({
        "type": "OFFER",
        "seqNum": seq_num,
        "offer": {
            "castMode": "mirroring",
            "supportedStreams": streams,
        },
    })
}

#[derive(Debug)]
pub struct Answer {
    pub udp_port: u16,
    /// Offer stream indexes the receiver accepted.
    pub send_indexes: Vec<u32>,
}

/// Parses an ANSWER message body (already matched on seqNum by the caller).
pub fn parse_answer(message: &Value) -> Result<Answer> {
    let result = message["result"].as_str().unwrap_or("error");
    if result != "ok" {
        return Err(anyhow!("receiver rejected the offer: {}", message["error"]));
    }
    let answer = &message["answer"];
    let udp_port = answer["udpPort"]
        .as_u64()
        .filter(|p| (1..=65535).contains(p))
        .context("ANSWER has no valid udpPort")? as u16;
    let send_indexes = answer["sendIndexes"]
        .as_array()
        .context("ANSWER has no sendIndexes")?
        .iter()
        .filter_map(|v| v.as_u64().map(|i| i as u32))
        .collect();
    Ok(Answer {
        udp_port,
        send_indexes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn video() -> VideoParams {
        VideoParams {
            index: 1,
            ssrc: 50001,
            aes_key: [0xab; 16],
            aes_iv_mask: [0x12; 16],
            codec_name: "vp8",
            max_bit_rate: 5_000_000,
            max_fps: 30,
            width: 1920,
            height: 1080,
        }
    }

    fn audio() -> AudioParams {
        AudioParams {
            index: 0,
            ssrc: 1001,
            aes_key: [1; 16],
            aes_iv_mask: [2; 16],
            bit_rate: 128_000,
        }
    }

    #[test]
    fn offer_has_expected_shape() {
        let audio = audio();
        let o = offer(7, Some(&audio), &[video()]);
        assert_eq!(o["type"], "OFFER");
        assert_eq!(o["seqNum"], 7);
        assert_eq!(o["offer"]["castMode"], "mirroring");
        let streams = o["offer"]["supportedStreams"].as_array().unwrap();
        assert_eq!(streams.len(), 2);
        assert_eq!(streams[0]["type"], "audio_source");
        assert_eq!(streams[0]["rtpPayloadType"], 127);
        assert_eq!(streams[0]["aesKey"].as_str().unwrap().len(), 32);
        assert_eq!(streams[1]["type"], "video_source");
        assert_eq!(streams[1]["codecName"], "vp8");
        assert_eq!(streams[1]["rtpPayloadType"], 96);
        assert_eq!(streams[1]["timeBase"], "1/90000");
        assert_eq!(streams[1]["resolutions"][0]["width"], 1920);
    }

    #[test]
    fn offer_lists_each_video_codec_as_its_own_stream() {
        let audio = audio();
        let videos = [
            VideoParams {
                index: 1,
                codec_name: "av1",
                ..video()
            },
            VideoParams {
                index: 2,
                codec_name: "vp9",
                ..video()
            },
            VideoParams {
                index: 3,
                codec_name: "vp8",
                ..video()
            },
        ];
        let o = offer(1, Some(&audio), &videos);
        let streams = o["offer"]["supportedStreams"].as_array().unwrap();
        assert_eq!(streams.len(), 4); // 1 audio + 3 video variants
        assert_eq!(streams[1]["codecName"], "av1");
        assert_eq!(streams[1]["index"], 1);
        assert_eq!(streams[2]["codecName"], "vp9");
        assert_eq!(streams[3]["codecName"], "vp8");
        // Each video variant carries its own index and payload type 96.
        for s in &streams[1..] {
            assert_eq!(s["type"], "video_source");
            assert_eq!(s["rtpPayloadType"], 96);
        }
    }

    #[test]
    fn audio_only_offer_has_no_video_stream() {
        let audio = audio();
        let o = offer(3, Some(&audio), &[]);
        let streams = o["offer"]["supportedStreams"].as_array().unwrap();
        assert_eq!(streams.len(), 1);
        assert_eq!(streams[0]["type"], "audio_source");
        assert_eq!(streams[0]["codecName"], "opus");
    }

    #[test]
    fn answer_roundtrip() {
        let msg = serde_json::json!({
            "type": "ANSWER",
            "seqNum": 7,
            "result": "ok",
            "answer": {"udpPort": 2345, "sendIndexes": [0, 1], "ssrcs": [1002, 50002]},
        });
        let a = parse_answer(&msg).unwrap();
        assert_eq!(a.udp_port, 2345);
        assert_eq!(a.send_indexes, vec![0, 1]);
    }

    #[test]
    fn rejected_answer_is_error() {
        let msg = serde_json::json!({"type": "ANSWER", "seqNum": 7, "result": "error",
            "error": {"code": 123, "description": "bad offer"}});
        assert!(parse_answer(&msg).is_err());
    }
}
