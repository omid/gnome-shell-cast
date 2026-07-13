//! The media sender: owns the UDP socket, encrypts and packetizes encoded
//! frames from the `GStreamer` appsinks, answers receiver RTCP (retransmits on
//! NACK, forwards keyframe requests), and emits periodic Sender Reports.
//! Ports the roles of openscreen's `sender.cc` + `sender_packet_router.cc`,
//! without the adaptive bandwidth machinery (LAN use, fixed bitrate).

use std::collections::VecDeque;
use std::net::UdpSocket;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use aes::cipher::StreamCipher;
use gstreamer::buffer::{MappedBuffer, Readable};
use log::{debug, info, warn};
use parking_lot::{Condvar, Mutex};

use super::crypto::FrameCrypto;
use super::rtcp;
use super::rtp::{OutboundFrame, PacketizedFrame, Packetizer};

/// Matches openscreen's kMaxUnackedFrames.
const MAX_HISTORY_FRAMES: usize = 120;
const SENDER_REPORT_INTERVAL: Duration = Duration::from_millis(500);
/// How long we wait for a command before servicing the socket again.
const TICK: Duration = Duration::from_millis(2);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StreamKind {
    Audio,
    Video,
}

/// One encoded buffer from an appsink.
pub struct EncodedChunk {
    pub kind: StreamKind,
    pub is_key_frame: bool,
    pub rtp_timestamp: u32,
    /// NTP timestamp taken when the buffer left the encoder; paired with
    /// `rtp_timestamp` in Sender Reports so the receiver can sync A/V.
    pub ntp_timestamp: u64,
    /// The encoded bytes, still in the mapped `GStreamer` buffer: the frame
    /// crosses the thread boundary by reference count, not by copy.
    pub data: MappedBuffer<Readable>,
}

pub struct StreamConfig {
    pub kind: StreamKind,
    pub ssrc: u32,
    pub payload_type: u8,
    pub aes_key: [u8; 16],
    pub aes_iv_mask: [u8; 16],
}

/// Initial chunk-queue capacity: both appsinks fully backed up (max-buffers=32
/// each). Within this depth, sends never allocate — unlike `std::sync::mpsc`,
/// which boxes a queue node per message.
const CHUNK_QUEUE_CAPACITY: usize = 64;

struct ChunkQueue {
    queue: Mutex<VecDeque<EncodedChunk>>,
    available: Condvar,
    senders: AtomicUsize,
}

/// Producer half of the chunk queue; cloned into each appsink callback.
pub struct ChunkSender(Arc<ChunkQueue>);

impl ChunkSender {
    pub fn send(&self, chunk: EncodedChunk) {
        self.0.queue.lock().push_back(chunk);
        self.0.available.notify_one();
    }
}

impl Clone for ChunkSender {
    fn clone(&self) -> Self {
        self.0.senders.fetch_add(1, Ordering::Relaxed);
        Self(Arc::clone(&self.0))
    }
}

impl Drop for ChunkSender {
    fn drop(&mut self) {
        if self.0.senders.fetch_sub(1, Ordering::Release) == 1 {
            // Wake the receiver so it can observe the disconnect.
            self.0.available.notify_all();
        }
    }
}

/// Consumer half of the chunk queue; owned by the sender thread.
pub struct ChunkReceiver(Arc<ChunkQueue>);

enum RecvTimeoutError {
    Timeout,
    Disconnected,
}

impl ChunkReceiver {
    /// Waits up to `timeout` for a chunk. Buffered chunks are drained before
    /// a disconnect is reported (matching `std::sync::mpsc`). May time out
    /// early on a spurious wakeup; the caller's loop re-enters anyway.
    fn recv_timeout(&self, timeout: Duration) -> Result<EncodedChunk, RecvTimeoutError> {
        let mut queue = self.0.queue.lock();
        if let Some(chunk) = queue.pop_front() {
            return Ok(chunk);
        }
        if self.0.senders.load(Ordering::Acquire) == 0 {
            return Err(RecvTimeoutError::Disconnected);
        }
        let _ = self.0.available.wait_for(&mut queue, timeout);
        queue.pop_front().ok_or(RecvTimeoutError::Timeout)
    }
}

/// An mpsc channel for encoded chunks with a preallocated ring, so
/// steady-state sends from the appsink callbacks are allocation-free.
pub fn chunk_channel() -> (ChunkSender, ChunkReceiver) {
    let shared = Arc::new(ChunkQueue {
        queue: Mutex::new(VecDeque::with_capacity(CHUNK_QUEUE_CAPACITY)),
        available: Condvar::new(),
        senders: AtomicUsize::new(1),
    });
    (ChunkSender(Arc::clone(&shared)), ChunkReceiver(shared))
}

struct SentFrame {
    frame_id: u64,
    packets: PacketizedFrame,
}

struct Stream {
    kind: StreamKind,
    ssrc: u32,
    packetizer: Packetizer,
    crypto: FrameCrypto,
    next_frame_id: u64,
    checkpoint: u64,
    history: VecDeque<SentFrame>,
    /// Packet buffers recycled from evicted history frames. Sends and
    /// evictions pair up one-to-one in steady state, so `history` +
    /// `spare_buffers` never hold more buffers than the history cap and no
    /// separate bound is needed. After warm-up, packetizing only grows a
    /// recycled buffer when a frame exceeds its capacity.
    spare_buffers: Vec<Vec<u8>>,
    packet_count: u32,
    octet_count: u32,
    last_report: Option<Instant>,
    /// Latest (rtp, ntp) pair, reported in Sender Reports.
    last_timestamps: Option<(u32, u64)>,
}

impl Stream {
    fn new(config: &StreamConfig) -> Self {
        Self {
            kind: config.kind,
            ssrc: config.ssrc,
            packetizer: Packetizer::new(config.payload_type, config.ssrc),
            crypto: FrameCrypto::new(config.aes_key, config.aes_iv_mask),
            next_frame_id: 0,
            checkpoint: 0,
            history: VecDeque::with_capacity(MAX_HISTORY_FRAMES + 1),
            spare_buffers: Vec::new(),
            packet_count: 0,
            octet_count: 0,
            last_report: None,
            last_timestamps: None,
        }
    }

    /// Drops the oldest frame from the retransmit history, recycling its
    /// packet buffer for a future frame.
    fn evict_oldest(&mut self) {
        if let Some(frame) = self.history.pop_front() {
            self.spare_buffers.push(frame.packets.into_buffer());
        }
    }
}

pub struct MediaSender {
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl MediaSender {
    /// `request_keyframe` is invoked (from the sender thread) when the
    /// receiver reports picture loss.
    pub fn spawn(
        socket: UdpSocket,
        streams: Vec<StreamConfig>,
        chunks: ChunkReceiver,
        request_keyframe: Box<dyn Fn() + Send>,
    ) -> MediaSender {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_flag = stop.clone();
        #[allow(
            clippy::expect_used,
            reason = "spawning a thread only fails on OS resource exhaustion, which is unrecoverable"
        )]
        let handle = thread::Builder::new()
            .name("mirror-sender".into())
            .spawn(move || run(socket, streams, chunks, request_keyframe, stop_flag))
            .expect("failed to spawn mirror-sender thread");
        MediaSender {
            stop,
            handle: Some(handle),
        }
    }
}

impl Drop for MediaSender {
    fn drop(&mut self) {
        // Signal the thread explicitly: it cannot rely on the chunk channel
        // disconnecting, because the appsink callbacks that hold the senders
        // outlive this struct (they live as long as the pipeline object).
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn run(
    socket: UdpSocket,
    configs: Vec<StreamConfig>,
    chunks: ChunkReceiver,
    request_keyframe: Box<dyn Fn() + Send>,
    stop: Arc<AtomicBool>,
) {
    let mut streams: Vec<Stream> = configs.iter().map(Stream::new).collect();
    if socket.set_nonblocking(true).is_err() {
        warn!("could not make the RTP socket non-blocking");
    }
    let mut receive_buffer = [0_u8; 1500];
    // Reused across RTCP packets so NACK parsing does not allocate per packet.
    let mut events = rtcp::ReceiverEvents::default();
    let mut first_video_frame = true;

    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }

        // 1. Media frames (block briefly so RTCP still gets serviced).
        match chunks.recv_timeout(TICK) {
            Ok(chunk) => {
                if let Some(stream) = streams.iter_mut().find(|s| s.kind == chunk.kind) {
                    if chunk.kind == StreamKind::Video && first_video_frame {
                        info!("sending first video frame to the receiver");
                        first_video_frame = false;
                    }
                    send_frame(&socket, stream, &chunk);
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }

        // 2. Receiver RTCP.
        while let Ok(size) = socket.recv(&mut receive_buffer) {
            let packet = &receive_buffer[..size];
            for stream in &mut streams {
                rtcp::parse(packet, stream.ssrc, stream.checkpoint, &mut events);
                if let Some(checkpoint) = events.checkpoint_frame_id {
                    stream.checkpoint = stream.checkpoint.max(checkpoint);
                    while stream
                        .history
                        .front()
                        .is_some_and(|f| f.frame_id <= stream.checkpoint)
                    {
                        stream.evict_oldest();
                    }
                }
                for nack in &events.nacks {
                    retransmit(&socket, stream, nack);
                }
                if events.picture_loss && stream.kind == StreamKind::Video {
                    debug!("receiver reported picture loss, forcing a key frame");
                    request_keyframe();
                }
            }
        }

        // 3. Periodic Sender Reports.
        for stream in &mut streams {
            let due = stream
                .last_report
                .is_none_or(|t| t.elapsed() >= SENDER_REPORT_INTERVAL);
            if due && let Some((rtp_ts, ntp_ts)) = stream.last_timestamps {
                let report = rtcp::build_sender_report(
                    stream.ssrc,
                    ntp_ts,
                    rtp_ts,
                    stream.packet_count,
                    stream.octet_count,
                );
                let _ = socket.send(&report);
                stream.last_report = Some(Instant::now());
            }
        }
    }
    info!("mirror sender stopped");
}

fn send_frame(socket: &UdpSocket, stream: &mut Stream, chunk: &EncodedChunk) {
    let frame_id = stream.next_frame_id;
    stream.next_frame_id += 1;

    let frame = OutboundFrame {
        frame_id,
        referenced_frame_id: if chunk.is_key_frame || frame_id == 0 {
            frame_id
        } else {
            frame_id - 1
        },
        rtp_timestamp: chunk.rtp_timestamp,
        is_key_frame: chunk.is_key_frame,
        // Communicate the fixed target playout delay with the first frame.
        playout_delay_ms: (frame_id == 0).then_some(super::messages::TARGET_PLAYOUT_DELAY_MS),
        data: &chunk.data,
    };
    // Encrypt on the way from the mapped encoder buffer into the packet
    // buffer: the chunks arrive in payload order, so one CTR keystream pass
    // covers the whole frame and no encrypted intermediate copy exists.
    let buffer = stream.spare_buffers.pop().unwrap_or_default();
    let mut cipher = stream.crypto.cipher(frame_id);
    let packets = stream
        .packetizer
        .packetize(&frame, buffer, |payload| cipher.apply_keystream(payload));

    for packet in packets.iter() {
        stream.packet_count = stream.packet_count.wrapping_add(1);
        stream.octet_count = stream.octet_count.wrapping_add(packet.len() as u32);
        if let Err(e) = socket.send(packet) {
            debug!("RTP send failed: {e}");
        }
    }
    stream.last_timestamps = Some((chunk.rtp_timestamp, chunk.ntp_timestamp));

    stream.history.push_back(SentFrame { frame_id, packets });
    if stream.history.len() > MAX_HISTORY_FRAMES {
        stream.evict_oldest();
    }
}

fn retransmit(socket: &UdpSocket, stream: &Stream, nack: &rtcp::Nack) {
    let Some(frame) = stream.history.iter().find(|f| f.frame_id == nack.frame_id) else {
        return;
    };
    if nack.packet_id == rtcp::ALL_PACKETS_LOST {
        for packet in frame.packets.iter() {
            let _ = socket.send(packet);
        }
    } else if let Some(packet) = frame.packets.packet(nack.packet_id) {
        let _ = socket.send(packet);
    } else {
        // The receiver NACKed a packet id we never produced; nothing to resend.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk() -> EncodedChunk {
        gstreamer::init().unwrap();
        EncodedChunk {
            kind: StreamKind::Video,
            is_key_frame: false,
            rtp_timestamp: 0,
            ntp_timestamp: 0,
            data: gstreamer::Buffer::from_slice(*b"x")
                .into_mapped_buffer_readable()
                .unwrap(),
        }
    }

    #[test]
    fn chunk_channel_delivers_then_times_out() {
        let (tx, rx) = chunk_channel();
        tx.send(chunk());
        assert!(rx.recv_timeout(Duration::ZERO).is_ok());
        assert!(matches!(
            rx.recv_timeout(Duration::ZERO),
            Err(RecvTimeoutError::Timeout)
        ));
    }

    #[test]
    fn chunk_channel_drains_before_reporting_disconnect() {
        let (tx, rx) = chunk_channel();
        let tx2 = tx.clone();
        drop(tx);
        tx2.send(chunk());
        drop(tx2);
        assert!(rx.recv_timeout(Duration::ZERO).is_ok());
        assert!(matches!(
            rx.recv_timeout(Duration::ZERO),
            Err(RecvTimeoutError::Disconnected)
        ));
    }
}
