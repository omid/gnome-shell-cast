//! Cast Streaming RTP packetization, ported from openscreen
//! `cast/streaming/impl/rtp_defines.h` and `rtp_packetizer.cc`.
//!
//! Layout (all integers big-endian):
//!   bytes 0..12   standard RTP header (V=2; M = last packet of frame; PT;
//!                 sequence number; RTP timestamp; sender SSRC)
//!   byte  12      K|R|extension count (K = key frame, R = "reference frame
//!                 id present", always set by the sender)
//!   byte  13      frame id (low 8 bits)
//!   bytes 14..16  packet id within the frame
//!   bytes 16..18  max packet id of the frame
//!   byte  18      referenced frame id (low 8 bits)
//!   [ext]         optional Adaptive Latency extension (type 1, 2 data bytes)
//!   rest          AES-CTR encrypted payload chunk

/// Ethernet MTU minus IPv4 + UDP headers.
pub const MAX_PACKET_SIZE: usize = 1500 - 20 - 8;
/// Base Cast RTP header (no extensions): 12 + 7.
const BASE_HEADER_SIZE: usize = 19;
/// Header size with the Adaptive Latency extension reserved (openscreen's
/// kMaxRtpHeaderSize); payload chunking always assumes this worst case.
const MAX_HEADER_SIZE: usize = 23;
/// Usable payload bytes per packet.
pub const MAX_PAYLOAD_SIZE: usize = MAX_PACKET_SIZE - MAX_HEADER_SIZE;
/// Size of every packet except the first (which may carry the extension) and
/// the last (which carries the payload remainder).
const FULL_PACKET_SIZE: usize = BASE_HEADER_SIZE + MAX_PAYLOAD_SIZE;

const KEY_FRAME_BIT: u8 = 0b1000_0000;
const HAS_REFERENCE_FRAME_ID_BIT: u8 = 0b0100_0000;
const MARKER_BIT: u8 = 0b1000_0000;
const ADAPTIVE_LATENCY_EXTENSION_TYPE: u16 = 1;

/// A frame ready to be packetized. `data` is the plaintext payload; it is
/// encrypted on its way into the packets by the `process_payload` callback of
/// [`Packetizer::packetize`].
pub struct OutboundFrame<'a> {
    pub frame_id: u64,
    pub referenced_frame_id: u64,
    pub rtp_timestamp: u32,
    pub is_key_frame: bool,
    /// Set on the first frame to communicate the target playout delay.
    pub playout_delay_ms: Option<u16>,
    pub data: &'a [u8],
}

pub fn num_packets(payload_len: usize) -> usize {
    payload_len.div_ceil(MAX_PAYLOAD_SIZE).max(1)
}

/// All RTP packets of one frame, back to back in a single exactly-sized
/// buffer (one allocation per frame). Packet boundaries are arithmetic: every
/// packet except the first (optional extension) and the last (payload
/// remainder) is exactly `FULL_PACKET_SIZE` bytes.
pub struct PacketizedFrame {
    buffer: Vec<u8>,
    first_packet_len: usize,
    count: usize,
}

impl PacketizedFrame {
    pub fn packet(&self, packet_id: u16) -> Option<&[u8]> {
        let id = packet_id as usize;
        if id >= self.count {
            return None;
        }
        if id == 0 {
            return Some(&self.buffer[..self.first_packet_len]);
        }
        let start = self.first_packet_len + (id - 1) * FULL_PACKET_SIZE;
        let end = (start + FULL_PACKET_SIZE).min(self.buffer.len());
        Some(&self.buffer[start..end])
    }

    pub fn iter(&self) -> impl Iterator<Item = &[u8]> {
        (0..self.count).filter_map(|id| self.packet(id as u16))
    }

    /// Recovers the backing buffer for reuse by a later `packetize` call,
    /// keeping its capacity.
    pub fn into_buffer(self) -> Vec<u8> {
        self.buffer
    }
}

pub struct Packetizer {
    payload_type: u8,
    ssrc: u32,
    sequence_number: u16,
}

impl Packetizer {
    pub fn new(payload_type: u8, ssrc: u32) -> Self {
        Self {
            payload_type,
            ssrc,
            // Random start, per the Cast Streaming spec.
            sequence_number: rand::random(),
        }
    }

    /// Splits `frame` into ready-to-send RTP packets, built inside `buffer`
    /// (recycled from an earlier frame via [`PacketizedFrame::into_buffer`],
    /// or empty; contents are discarded, capacity is reused). Each payload
    /// chunk is copied into the packet and then handed to
    /// `process_payload(packet_payload)` - the caller encrypts in place
    /// there, so the frame is never held in an intermediate encrypted copy.
    /// Chunks are processed in payload order, so a streaming cipher can be
    /// carried across calls.
    ///
    /// Sequence numbers are consumed even on retransmission, so retransmits
    /// must resend the stored packets rather than re-packetizing (matches
    /// openscreen).
    pub fn packetize<F>(
        &mut self,
        frame: &OutboundFrame<'_>,
        mut buffer: Vec<u8>,
        mut process_payload: F,
    ) -> PacketizedFrame
    where
        F: FnMut(&mut [u8]),
    {
        let count = num_packets(frame.data.len());
        let max_packet_id = (count - 1) as u16;
        let extension_len = if frame.playout_delay_ms.is_some() {
            4
        } else {
            0
        };
        buffer.clear();
        buffer.reserve(count * BASE_HEADER_SIZE + extension_len + frame.data.len());
        let mut first_packet_len = 0;

        for packet_id in 0..count as u16 {
            let is_last = packet_id == max_packet_id;
            let chunk_start = MAX_PAYLOAD_SIZE * packet_id as usize;
            let chunk_end = if is_last {
                frame.data.len()
            } else {
                chunk_start + MAX_PAYLOAD_SIZE
            };
            let latency_change = if packet_id == 0 {
                frame.playout_delay_ms
            } else {
                None
            };

            // RTP header.
            buffer.push(0b1000_0000);
            buffer.push(if is_last { MARKER_BIT } else { 0 } | self.payload_type);
            buffer.extend_from_slice(&self.sequence_number.to_be_bytes());
            self.sequence_number = self.sequence_number.wrapping_add(1);
            buffer.extend_from_slice(&frame.rtp_timestamp.to_be_bytes());
            buffer.extend_from_slice(&self.ssrc.to_be_bytes());
            // Cast header.
            buffer.push(
                if frame.is_key_frame { KEY_FRAME_BIT } else { 0 }
                    | HAS_REFERENCE_FRAME_ID_BIT
                    | u8::from(latency_change.is_some()),
            );
            buffer.push(frame.frame_id as u8);
            buffer.extend_from_slice(&packet_id.to_be_bytes());
            buffer.extend_from_slice(&max_packet_id.to_be_bytes());
            buffer.push(frame.referenced_frame_id as u8);
            if let Some(delay) = latency_change {
                // 6-bit type, 10-bit data size, then the data.
                buffer.extend_from_slice(
                    &((ADAPTIVE_LATENCY_EXTENSION_TYPE << 10) | 2).to_be_bytes(),
                );
                buffer.extend_from_slice(&delay.to_be_bytes());
            }
            let payload_at = buffer.len();
            buffer.extend_from_slice(&frame.data[chunk_start..chunk_end]);
            process_payload(&mut buffer[payload_at..]);
            if packet_id == 0 {
                first_packet_len = buffer.len();
            }
        }

        PacketizedFrame {
            buffer,
            first_packet_len,
            count,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta() -> (u64, u64, u32) {
        (0x102, 0x101, 987)
    }

    fn packetize(pk: &mut Packetizer, frame: &OutboundFrame<'_>) -> PacketizedFrame {
        pk.packetize(frame, Vec::new(), |_| {})
    }

    #[test]
    fn single_packet_frame_layout() {
        let (frame_id, referenced_frame_id, rtp_timestamp) = meta();
        let data: Vec<u8> = (0..10).collect();
        let mut pk = Packetizer::new(96, 0xAABB_CCDD);
        let f = OutboundFrame {
            frame_id,
            referenced_frame_id,
            rtp_timestamp,
            is_key_frame: true,
            playout_delay_ms: Some(400),
            data: &data,
        };
        let packets = packetize(&mut pk, &f);
        let p = packets.packet(0).unwrap();
        assert!(packets.packet(1).is_none());
        assert_eq!(p.len(), BASE_HEADER_SIZE + 4 + 10);
        assert_eq!(p[0], 0x80);
        assert_eq!(p[1], MARKER_BIT | 96); // last packet → marker set
        assert_eq!(&p[4..8], &987_u32.to_be_bytes());
        assert_eq!(&p[8..12], &0xAABB_CCDD_u32.to_be_bytes());
        // key | has-ref | 1 extension
        assert_eq!(p[12], KEY_FRAME_BIT | HAS_REFERENCE_FRAME_ID_BIT | 1);
        assert_eq!(p[13], 0x02); // frame id truncated to 8 bits
        assert_eq!(&p[14..16], &[0, 0]); // packet id
        assert_eq!(&p[16..18], &[0, 0]); // max packet id
        assert_eq!(p[18], 0x01); // referenced frame id truncated
        // Adaptive latency extension: type 1, size 2, 400ms.
        assert_eq!(&p[19..21], &((1_u16 << 10) | 2).to_be_bytes());
        assert_eq!(&p[21..23], &400_u16.to_be_bytes());
        assert_eq!(&p[23..], &data[..]);
    }

    #[test]
    fn multi_packet_frame_split_and_sequence() {
        let (frame_id, referenced_frame_id, rtp_timestamp) = meta();
        let data: Vec<u8> = (0..MAX_PAYLOAD_SIZE + 5).map(|i| i as u8).collect();
        let mut pk = Packetizer::new(127, 1);
        let f = OutboundFrame {
            frame_id,
            referenced_frame_id,
            rtp_timestamp,
            is_key_frame: false,
            playout_delay_ms: None,
            data: &data,
        };
        let packets = packetize(&mut pk, &f);
        assert_eq!(packets.iter().count(), 2);
        let p0 = packets.packet(0).unwrap();
        let p1 = packets.packet(1).unwrap();
        assert!(packets.packet(2).is_none());
        // First packet: full chunk, no marker.
        assert_eq!(p0.len(), BASE_HEADER_SIZE + MAX_PAYLOAD_SIZE);
        assert_eq!(p0[1], 127);
        // Second packet: remainder, marker set, packet id 1 of max 1.
        assert_eq!(p1.len(), BASE_HEADER_SIZE + 5);
        assert_eq!(p1[1], MARKER_BIT | 127);
        assert_eq!(&p1[14..16], &1_u16.to_be_bytes());
        assert_eq!(&p1[16..18], &1_u16.to_be_bytes());
        // Consecutive sequence numbers across packets.
        let s0 = u16::from_be_bytes([p0[2], p0[3]]);
        let s1 = u16::from_be_bytes([p1[2], p1[3]]);
        assert_eq!(s1, s0.wrapping_add(1));
        // Payload continuity across the split.
        assert_eq!(p1[BASE_HEADER_SIZE], data[MAX_PAYLOAD_SIZE]);
    }

    #[test]
    fn multi_packet_frame_with_extension_boundaries() {
        // The first packet carries the 4-byte extension; the arithmetic
        // packet lookup must account for its different size.
        let (frame_id, referenced_frame_id, rtp_timestamp) = meta();
        let data = vec![0xAB_u8; 2 * MAX_PAYLOAD_SIZE + 7];
        let mut pk = Packetizer::new(96, 1);
        let f = OutboundFrame {
            frame_id,
            referenced_frame_id,
            rtp_timestamp,
            is_key_frame: true,
            playout_delay_ms: Some(400),
            data: &data,
        };
        let packets = packetize(&mut pk, &f);
        assert_eq!(packets.iter().count(), 3);
        assert_eq!(
            packets.packet(0).unwrap().len(),
            BASE_HEADER_SIZE + 4 + MAX_PAYLOAD_SIZE
        );
        assert_eq!(
            packets.packet(1).unwrap().len(),
            BASE_HEADER_SIZE + MAX_PAYLOAD_SIZE
        );
        assert_eq!(packets.packet(2).unwrap().len(), BASE_HEADER_SIZE + 7);
        for (id, p) in packets.iter().enumerate() {
            assert!(p.len() <= MAX_PACKET_SIZE);
            assert_eq!(&p[14..16], &(id as u16).to_be_bytes());
            assert_eq!(&p[16..18], &2_u16.to_be_bytes());
        }
    }

    #[test]
    fn empty_payload_still_yields_one_packet() {
        let (frame_id, referenced_frame_id, rtp_timestamp) = meta();
        let mut pk = Packetizer::new(127, 1);
        let f = OutboundFrame {
            frame_id,
            referenced_frame_id,
            rtp_timestamp,
            is_key_frame: false,
            playout_delay_ms: None,
            data: &[],
        };
        let packets = packetize(&mut pk, &f);
        assert_eq!(packets.iter().count(), 1);
        assert_eq!(packets.packet(0).unwrap().len(), BASE_HEADER_SIZE);
    }

    #[test]
    fn process_payload_transforms_in_place_and_in_order() {
        let data = vec![1_u8; 2 * MAX_PAYLOAD_SIZE + 9];
        let mut pk = Packetizer::new(96, 1);
        let (frame_id, referenced_frame_id, rtp_timestamp) = meta();
        let f = OutboundFrame {
            frame_id,
            referenced_frame_id,
            rtp_timestamp,
            is_key_frame: false,
            playout_delay_ms: None,
            data: &data,
        };
        let mut seen = 0_usize;
        let packets = pk.packetize(&f, Vec::new(), |payload| {
            seen += payload.len();
            for b in payload.iter_mut() {
                *b ^= 0xFF;
            }
        });
        // The callback saw every payload byte exactly once...
        assert_eq!(seen, data.len());
        assert_eq!(
            packets.iter().map(<[u8]>::len).sum::<usize>(),
            data.len() + 3 * BASE_HEADER_SIZE
        );
        // ...and its in-place writes are what the packets carry.
        for p in packets.iter() {
            assert!(p[BASE_HEADER_SIZE..].iter().all(|&b| b == 0xFE));
        }
    }

    #[test]
    fn recycled_buffer_leaves_no_stale_bytes() {
        let (frame_id, referenced_frame_id, rtp_timestamp) = meta();
        let mut pk = Packetizer::new(96, 1);
        let big = vec![0xAA_u8; 2 * MAX_PAYLOAD_SIZE];
        let first = packetize(
            &mut pk,
            &OutboundFrame {
                frame_id,
                referenced_frame_id,
                rtp_timestamp,
                is_key_frame: true,
                playout_delay_ms: None,
                data: &big,
            },
        );
        let buffer = first.into_buffer();
        let capacity = buffer.capacity();

        let small = vec![0xBB_u8; 10];
        let second = pk.packetize(
            &OutboundFrame {
                frame_id: frame_id + 1,
                referenced_frame_id: frame_id,
                rtp_timestamp,
                is_key_frame: false,
                playout_delay_ms: None,
                data: &small,
            },
            buffer,
            |_| {},
        );
        assert_eq!(second.iter().count(), 1);
        let p = second.packet(0).unwrap();
        assert_eq!(p.len(), BASE_HEADER_SIZE + 10);
        assert!(p[BASE_HEADER_SIZE..].iter().all(|&b| b == 0xBB));
        // The smaller frame reused the capacity instead of reallocating.
        assert_eq!(second.into_buffer().capacity(), capacity);
    }
}
