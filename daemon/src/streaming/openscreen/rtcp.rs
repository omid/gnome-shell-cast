//! Minimal Cast Streaming RTCP: builds Sender Reports and parses the
//! receiver's compound packets (Cast Feedback = ACK checkpoint + NACKs, and
//! Picture Loss Indicator). Ported from openscreen
//! `cast/streaming/impl/rtp_defines.h`, `rtcp_common.cc`,
//! `sender_report_builder.cc` and `compound_rtcp_parser.cc`.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

const PT_SENDER_REPORT: u8 = 200;
const PT_RECEIVER_REPORT: u8 = 201;
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
    let seconds = d.as_secs().saturating_add(NTP_UNIX_OFFSET);
    let fraction = u64::from(d.subsec_nanos())
        .wrapping_shl(32)
        .checked_div(1_000_000_000)
        .unwrap_or(0);
    seconds.wrapping_shl(32) | fraction
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
    // Written as one field sequence rather than by offset: the layout is the
    // order of the chain, and nothing can index past the end.
    let mut report = [0_u8; 28];
    let fields = [0b1000_0000, PT_SENDER_REPORT] // V=2, P=0, report count 0
        .into_iter()
        .chain(6_u16.to_be_bytes()) // length: 7 words - 1
        .chain(sender_ssrc.to_be_bytes())
        .chain(ntp_timestamp.to_be_bytes())
        .chain(rtp_timestamp.to_be_bytes())
        .chain(packet_count.to_be_bytes())
        .chain(octet_count.to_be_bytes());
    for (slot, byte) in report.iter_mut().zip(fields) {
        *slot = byte;
    }
    report
}

/// A big-endian `u32` at `at`, or `None` if the packet is too short.
fn be_u32(bytes: &[u8], at: usize) -> Option<u32> {
    let end = at.checked_add(4)?;
    let field: [u8; 4] = bytes.get(at..end)?.try_into().ok()?;
    Some(u32::from_be_bytes(field))
}

/// One packet-level NACK: a frame (full frame id, bit-expanded) and a packet
/// within it, or `ALL_PACKETS_LOST` for the whole frame.
#[derive(Debug, PartialEq, Eq)]
pub struct Nack {
    pub frame_id: i64,
    pub packet_id: u16,
}

#[derive(Debug, Default)]
pub struct ReceiverEvents {
    /// All frames up to and including this one are fully received. Signed:
    /// a receiver with nothing yet reports the frame before the first, -1.
    pub checkpoint_frame_id: Option<i64>,
    pub nacks: Vec<Nack>,
    /// The receiver lost decoder state and needs a key frame.
    pub picture_loss: bool,
    /// Round-trip time from a Receiver Report's LSR/DLSR, when one arrived.
    pub round_trip: Option<Duration>,
}

impl ReceiverEvents {
    fn clear(&mut self) {
        self.checkpoint_frame_id = None;
        self.nacks.clear();
        self.picture_loss = false;
        self.round_trip = None;
    }
}

/// Parses a compound RTCP packet from the receiver into `events`, replacing
/// its previous contents (the caller reuses one instance to keep the NACK
/// buffer's allocation). `sender_ssrc` selects the stream this parser
/// instance cares about; feedback for other SSRCs is ignored.
/// `checkpoint_hint` is the last known checkpoint, used to bit-expand the
/// 8-bit frame ids on the wire.
pub fn parse(data: &[u8], sender_ssrc: u32, checkpoint_hint: i64, events: &mut ReceiverEvents) {
    events.clear();
    let mut rest = data;

    while let Some(&[byte0, packet_type, len_hi, len_lo]) = rest.first_chunk::<4>() {
        if byte0 >> 6 != 2 {
            break; // not RTCP v2; corrupt
        }
        let count_or_subtype = byte0 & 0b0001_1111;
        let length_words = usize::from(u16::from_be_bytes([len_hi, len_lo]));
        let Some(total) = length_words.checked_mul(4).and_then(|n| n.checked_add(4)) else {
            break;
        };
        let Some(body) = rest.get(4..total) else {
            break;
        };

        match (packet_type, count_or_subtype) {
            (PT_PAYLOAD_SPECIFIC, SUBTYPE_FEEDBACK) => {
                parse_feedback(body, sender_ssrc, checkpoint_hint, events);
            }
            (PT_PAYLOAD_SPECIFIC, SUBTYPE_PICTURE_LOSS) if be_u32(body, 4) == Some(sender_ssrc) => {
                events.picture_loss = true;
            }
            (PT_RECEIVER_REPORT, _) => {
                if let Some(rtt) = parse_round_trip(body, count_or_subtype) {
                    events.round_trip = Some(rtt);
                }
            }
            // Extended reports, SDES, etc. carry nothing we act on.
            _ => {}
        }
        let Some(next) = rest.get(total..) else {
            break;
        };
        rest = next;
    }
}

/// RTT from the first report block: the receiver echoes the middle 32 bits of
/// our last Sender Report's NTP stamp (LSR) plus how long it sat on it (DLSR),
/// both in 1/65536 s. Anything implausible is discarded rather than trusted.
fn parse_round_trip(body: &[u8], block_count: u8) -> Option<Duration> {
    if block_count == 0 {
        return None;
    }
    // [sender ssrc][report block: ssrc, loss, ext seq, jitter, LSR, DLSR]
    let lsr = be_u32(body, 4 + 16)?;
    let dlsr = be_u32(body, 4 + 20)?;
    if lsr == 0 {
        return None;
    }
    let now = ntp_middle_32(ntp_now());
    let elapsed = now.wrapping_sub(lsr).wrapping_sub(dlsr);
    // A negative or absurd result means the stamps did not line up.
    if elapsed == 0 || elapsed > u32::from(u16::MAX) {
        return None;
    }
    let micros = u64::from(elapsed)
        .saturating_mul(1_000_000)
        .checked_div(65_536)?;
    log::debug!("receiver round trip {micros} us");
    Some(Duration::from_micros(micros))
}

/// The middle 32 bits of an NTP timestamp, as RTCP report blocks carry them.
fn ntp_middle_32(ntp: u64) -> u32 {
    u32::try_from((ntp >> 16) & 0xFFFF_FFFF).unwrap_or(0)
}

fn parse_feedback(body: &[u8], sender_ssrc: u32, hint: i64, events: &mut ReceiverEvents) {
    // [receiver ssrc][sender ssrc]["CAST"][checkpoint u8][#loss u8][delay u16]
    let (Some(ssrc), Some(cast), Some(&checkpoint_byte), Some(&loss_count)) =
        (be_u32(body, 4), be_u32(body, 8), body.get(12), body.get(13))
    else {
        return;
    };
    if ssrc != sender_ssrc || cast != CAST {
        return;
    }
    let checkpoint = expand_frame_id(checkpoint_byte, hint);
    events.checkpoint_frame_id = Some(match events.checkpoint_frame_id {
        Some(existing) => existing.max(checkpoint),
        None => checkpoint,
    });

    let Some(loss_fields) = body.get(16..) else {
        return;
    };
    // [frame id u8][lost packet id u16][bit vector for the next 8 u8]
    // `as_chunks` drops a trailing partial field, which is what a short record
    // deserves; the fixed-size chunk is what lets this destructure infallibly.
    for &[id, packet_hi, packet_lo, bits] in loss_fields
        .as_chunks::<4>()
        .0
        .iter()
        .take(usize::from(loss_count))
    {
        let frame_id = expand_frame_id(id, checkpoint.saturating_add(1));
        let packet_id = u16::from_be_bytes([packet_hi, packet_lo]);
        events.nacks.push(Nack {
            frame_id,
            packet_id,
        });
        if packet_id == ALL_PACKETS_LOST {
            continue;
        }
        // Bit i marks the packet i+1 after `packet_id`. Masks rather than
        // shifts so nothing can shift past the width.
        for (offset, mask) in [1_u8, 2, 4, 8, 16, 32, 64, 128].into_iter().enumerate() {
            let Ok(offset) = u16::try_from(offset) else {
                continue;
            };
            if bits & mask != 0
                && let Some(packet_id) =
                    packet_id.checked_add(offset).and_then(|p| p.checked_add(1))
            {
                events.nacks.push(Nack {
                    frame_id,
                    packet_id,
                });
            }
        }
    }
    // An optional "CST2" frame-level ACK bit vector follows; the checkpoint
    // and NACKs are all we need, so it is deliberately not parsed.
}

/// Expands an 8-bit frame id to the value *nearest* the reference, i.e. in
/// [reference - 128, reference + 127] (openscreen's `ExpandedValueBase`). The
/// window must reach backwards: a receiver with nothing yet acks frame -1.
fn expand_frame_id(low8: u8, reference: i64) -> i64 {
    let delta = i64::from(low8).wrapping_sub(reference) & 0xff;
    let delta = if delta >= 128 {
        delta.wrapping_sub(256)
    } else {
        delta
    };
    reference.wrapping_add(delta)
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
        // "I have received nothing": the frame before the first one.
        assert_eq!(expand_frame_id(255, 0), -1);
    }

    fn feedback_packet(sender_ssrc: u32, checkpoint: u8, loss: &[(u8, u16, u8)]) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&0xEEEE_EEEE_u32.to_be_bytes()); // receiver ssrc
        body.extend_from_slice(&sender_ssrc.to_be_bytes());
        body.extend_from_slice(&CAST.to_be_bytes());
        body.push(checkpoint);
        body.push(u8::try_from(loss.len()).unwrap_or(u8::MAX));
        body.extend_from_slice(&400_u16.to_be_bytes());
        for (fid, pid, bits) in loss {
            body.push(*fid);
            body.extend_from_slice(&pid.to_be_bytes());
            body.push(*bits);
        }
        let mut p = vec![0x80 | SUBTYPE_FEEDBACK, PT_PAYLOAD_SPECIFIC];
        let words = u16::try_from(body.len().checked_div(4).unwrap_or(0)).unwrap_or(u16::MAX);
        p.extend_from_slice(&words.to_be_bytes());
        p.extend_from_slice(&body);
        p
    }

    fn parse_new(data: &[u8], sender_ssrc: u32, checkpoint_hint: i64) -> ReceiverEvents {
        // Pre-poison the events to prove parse() replaces previous contents.
        let mut events = ReceiverEvents {
            checkpoint_frame_id: Some(i64::MAX),
            nacks: vec![Nack {
                frame_id: i64::MAX,
                packet_id: 0,
            }],
            picture_loss: false,
            round_trip: Some(Duration::from_secs(99)),
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

    /// Read as a forward jump this retires the whole retransmit history, so
    /// the frames the receiver is about to NACK could never be resent.
    #[test]
    fn empty_receiver_checkpoint_is_before_the_first_frame() {
        let packet = feedback_packet(42, 255, &[]);
        let events = parse_new(&packet, 42, 0);
        assert_eq!(events.checkpoint_frame_id, Some(-1));
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
