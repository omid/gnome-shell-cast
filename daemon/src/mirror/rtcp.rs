//! Minimal Cast Streaming RTCP: builds Sender Reports and parses the
//! receiver's compound packets (Cast Feedback = ACK checkpoint + NACKs, and
//! Picture Loss Indicator). Ported from openscreen
//! `cast/streaming/impl/rtp_defines.h`, `rtcp_common.cc`,
//! `sender_report_builder.cc` and `compound_rtcp_parser.cc`.

use std::time::{SystemTime, UNIX_EPOCH};

const PT_SENDER_REPORT: u8 = 200;
const PT_PAYLOAD_SPECIFIC: u8 = 206;

const SUBTYPE_PICTURE_LOSS: u8 = 1;
const SUBTYPE_FEEDBACK: u8 = 15;

const CAST: u32 = u32::from_be_bytes(*b"CAST");

/// "All packets of the frame lost" marker in NACK loss fields.
pub const ALL_PACKETS_LOST: u16 = 0xffff;

/// Seconds between the NTP epoch (1900) and the Unix epoch (1970).
const NTP_UNIX_OFFSET: u64 = 2_208_988_800;

pub fn ntp_now() -> u64 {
    let d = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let seconds = d.as_secs() + NTP_UNIX_OFFSET;
    let fraction = (u64::from(d.subsec_nanos()) << 32) / 1_000_000_000;
    (seconds << 32) | fraction
}

/// A 28-byte RTCP Sender Report (no report blocks): maps the stream's RTP
/// timeline onto the NTP wall clock, which the receiver needs for A/V sync
/// and lag estimation.
pub fn build_sender_report(
    sender_ssrc: u32,
    ntp_timestamp: u64,
    rtp_timestamp: u32,
    packet_count: u32,
    octet_count: u32,
) -> [u8; 28] {
    let mut p = [0_u8; 28];
    p[0] = 0b1000_0000; // V=2, P=0, report count 0
    p[1] = PT_SENDER_REPORT;
    p[2..4].copy_from_slice(&6_u16.to_be_bytes()); // length: 7 words - 1
    p[4..8].copy_from_slice(&sender_ssrc.to_be_bytes());
    p[8..16].copy_from_slice(&ntp_timestamp.to_be_bytes());
    p[16..20].copy_from_slice(&rtp_timestamp.to_be_bytes());
    p[20..24].copy_from_slice(&packet_count.to_be_bytes());
    p[24..28].copy_from_slice(&octet_count.to_be_bytes());
    p
}

/// One packet-level NACK: a frame (full frame id, bit-expanded) and a packet
/// within it, or `ALL_PACKETS_LOST` for the whole frame.
#[derive(Debug, PartialEq, Eq)]
pub struct Nack {
    pub frame_id: u64,
    pub packet_id: u16,
}

#[derive(Debug, Default)]
pub struct ReceiverEvents {
    /// All frames up to and including this one are fully received.
    pub checkpoint_frame_id: Option<u64>,
    pub nacks: Vec<Nack>,
    /// The receiver lost decoder state and needs a key frame.
    pub picture_loss: bool,
}

impl ReceiverEvents {
    fn clear(&mut self) {
        self.checkpoint_frame_id = None;
        self.nacks.clear();
        self.picture_loss = false;
    }
}

/// Parses a compound RTCP packet from the receiver into `events`, replacing
/// its previous contents (the caller reuses one instance to keep the NACK
/// buffer's allocation). `sender_ssrc` selects the stream this parser
/// instance cares about; feedback for other SSRCs is ignored.
/// `checkpoint_hint` is the last known checkpoint, used to bit-expand the
/// 8-bit frame ids on the wire.
pub fn parse(data: &[u8], sender_ssrc: u32, checkpoint_hint: u64, events: &mut ReceiverEvents) {
    events.clear();
    let mut rest = data;

    while rest.len() >= 4 {
        let byte0 = rest[0];
        if byte0 >> 6 != 2 {
            break; // not RTCP v2; corrupt
        }
        let count_or_subtype = byte0 & 0b0001_1111;
        let packet_type = rest[1];
        let length_words = u16::from_be_bytes([rest[2], rest[3]]) as usize;
        let total = 4 + length_words * 4;
        if total > rest.len() {
            break;
        }
        let body = &rest[4..total];

        match (packet_type, count_or_subtype) {
            (PT_PAYLOAD_SPECIFIC, SUBTYPE_FEEDBACK) => {
                parse_feedback(body, sender_ssrc, checkpoint_hint, events);
            }
            (PT_PAYLOAD_SPECIFIC, SUBTYPE_PICTURE_LOSS)
                if body.len() >= 8
                    && u32::from_be_bytes([body[4], body[5], body[6], body[7]]) == sender_ssrc =>
            {
                events.picture_loss = true;
            }
            // Receiver reports, extended reports, SDES, etc. carry nothing we
            // act on; only Cast Feedback and PLI matter to the sender.
            _ => {}
        }
        rest = &rest[total..];
    }
}

fn parse_feedback(body: &[u8], sender_ssrc: u32, hint: u64, events: &mut ReceiverEvents) {
    // [receiver ssrc][sender ssrc]["CAST"][checkpoint u8][#loss u8][delay u16]
    if body.len() < 12 {
        return;
    }
    if u32::from_be_bytes([body[4], body[5], body[6], body[7]]) != sender_ssrc {
        return;
    }
    if u32::from_be_bytes([body[8], body[9], body[10], body[11]]) != CAST {
        return;
    }
    let checkpoint = expand_frame_id(body[12], hint);
    events.checkpoint_frame_id = Some(match events.checkpoint_frame_id {
        Some(existing) => existing.max(checkpoint),
        None => checkpoint,
    });
    let loss_fields = body[13] as usize;

    let mut pos = 16;
    for _ in 0..loss_fields {
        if pos + 4 > body.len() {
            return;
        }
        // [frame id u8][lost packet id u16][bit vector for the next 8 u8]
        let frame_id = expand_frame_id(body[pos], checkpoint + 1);
        let packet_id = u16::from_be_bytes([body[pos + 1], body[pos + 2]]);
        let bits = body[pos + 3];
        events.nacks.push(Nack {
            frame_id,
            packet_id,
        });
        if packet_id != ALL_PACKETS_LOST {
            for i in 0..8_u16 {
                if bits & (1 << i) != 0 {
                    events.nacks.push(Nack {
                        frame_id,
                        packet_id: packet_id + 1 + i,
                    });
                }
            }
        }
        pos += 4;
    }
    // An optional "CST2" frame-level ACK bit vector follows; the checkpoint
    // and NACKs are all we need, so it is deliberately not parsed.
}

/// Expands an 8-bit truncated frame id to the full value: the unique value
/// with those low 8 bits in the window [reference, reference + 255].
fn expand_frame_id(low8: u8, reference: u64) -> u64 {
    let candidate = (reference & !0xff) | u64::from(low8);
    if candidate < reference {
        candidate + 256
    } else {
        candidate
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sender_report_layout() {
        let sr = build_sender_report(0x0102_0304, 0xAABB_CCDD_1122_3344, 90_000, 5, 6_000);
        assert_eq!(sr.len(), 28);
        assert_eq!(sr[0], 0x80);
        assert_eq!(sr[1], 200);
        assert_eq!(u16::from_be_bytes([sr[2], sr[3]]), 6);
        assert_eq!(&sr[4..8], &0x0102_0304_u32.to_be_bytes());
        assert_eq!(&sr[8..16], &0xAABB_CCDD_1122_3344_u64.to_be_bytes());
        assert_eq!(&sr[16..20], &90_000_u32.to_be_bytes());
    }

    #[test]
    fn expand_frame_id_windows() {
        assert_eq!(expand_frame_id(5, 0), 5);
        assert_eq!(expand_frame_id(5, 250), 261); // wrapped past the low byte
        assert_eq!(expand_frame_id(250, 250), 250);
        assert_eq!(expand_frame_id(0x02, 0x100), 0x102);
    }

    fn feedback_packet(sender_ssrc: u32, checkpoint: u8, loss: &[(u8, u16, u8)]) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&0xEEEE_EEEE_u32.to_be_bytes()); // receiver ssrc
        body.extend_from_slice(&sender_ssrc.to_be_bytes());
        body.extend_from_slice(&CAST.to_be_bytes());
        body.push(checkpoint);
        body.push(loss.len() as u8);
        body.extend_from_slice(&400_u16.to_be_bytes());
        for (fid, pid, bits) in loss {
            body.push(*fid);
            body.extend_from_slice(&pid.to_be_bytes());
            body.push(*bits);
        }
        let mut p = vec![0x80 | SUBTYPE_FEEDBACK, PT_PAYLOAD_SPECIFIC];
        p.extend_from_slice(&((body.len() / 4) as u16).to_be_bytes());
        p.extend_from_slice(&body);
        p
    }

    fn parse_new(data: &[u8], sender_ssrc: u32, checkpoint_hint: u64) -> ReceiverEvents {
        // Pre-poison the events to prove parse() replaces previous contents.
        let mut events = ReceiverEvents {
            checkpoint_frame_id: Some(u64::MAX),
            nacks: vec![Nack {
                frame_id: u64::MAX,
                packet_id: 0,
            }],
            picture_loss: false,
        };
        parse(data, sender_ssrc, checkpoint_hint, &mut events);
        events
    }

    #[test]
    fn parses_checkpoint_and_nacks() {
        let packet = feedback_packet(42, 7, &[(9, 2, 0b0000_0101)]);
        let events = parse_new(&packet, 42, 0);
        assert_eq!(events.checkpoint_frame_id, Some(7));
        // packet 2 itself, then bits 0 and 2 → packets 3 and 5.
        assert_eq!(
            events.nacks,
            vec![
                Nack {
                    frame_id: 9,
                    packet_id: 2
                },
                Nack {
                    frame_id: 9,
                    packet_id: 3
                },
                Nack {
                    frame_id: 9,
                    packet_id: 5
                },
            ]
        );
    }

    #[test]
    fn ignores_feedback_for_other_ssrc() {
        let packet = feedback_packet(43, 7, &[]);
        let events = parse_new(&packet, 42, 0);
        assert_eq!(events.checkpoint_frame_id, None);
        assert!(events.nacks.is_empty());
    }

    #[test]
    fn parses_pli_in_compound_packet() {
        // Receiver report (empty, packet type 201) followed by PLI for our ssrc.
        let mut p = vec![0x80, 201, 0, 1];
        p.extend_from_slice(&0xEEEE_EEEE_u32.to_be_bytes());
        p.extend_from_slice(&[0x80 | SUBTYPE_PICTURE_LOSS, PT_PAYLOAD_SPECIFIC, 0, 2]);
        p.extend_from_slice(&0xEEEE_EEEE_u32.to_be_bytes());
        p.extend_from_slice(&42_u32.to_be_bytes());
        let events = parse_new(&p, 42, 0);
        assert!(events.picture_loss);
    }
}
