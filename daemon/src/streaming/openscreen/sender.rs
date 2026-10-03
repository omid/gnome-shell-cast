//! The media sender: owns the UDP socket, encrypts and packetizes encoded
//! frames from the `GStreamer` appsinks, answers receiver RTCP (retransmits on
//! NACK, forwards keyframe requests), and emits periodic Sender Reports.
//! Ports the roles of openscreen's `sender.cc` + `sender_packet_router.cc`,
//! without the adaptive bandwidth machinery (LAN use, fixed bitrate).

use std::collections::VecDeque;
use std::io::ErrorKind;
use std::net::{SocketAddr, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use aes::cipher::StreamCipher;
use gstreamer::buffer::{MappedBuffer, Readable};
use log::{debug, info, warn};
use parking_lot::{Condvar, Mutex};

use super::bandwidth::{self, BandwidthEstimator};
use super::crypto::FrameCrypto;
use super::rtcp;
use super::rtp::{OutboundFrame, PacketizedFrame, Packetizer};
use crate::streaming::ladder::ResolutionLadder;

/// Matches openscreen's kMaxUnackedFrames.
const MAX_HISTORY_FRAMES: usize = 120;
const SENDER_REPORT_INTERVAL: Duration = Duration::from_millis(500);
/// How long video may flow with no RTCP coming back before we say so.
const FEEDBACK_TIMEOUT: Duration = Duration::from_secs(3);
/// How long a single packet may wait for room in the socket's send buffer.
const SEND_TIMEOUT: Duration = Duration::from_millis(100);
/// Pause between attempts while the send buffer is full.
const SEND_RETRY_DELAY: Duration = Duration::from_micros(200);
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
/// each). Within this depth, sends never allocate - unlike `std::sync::mpsc`,
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

/// Spaces RTP into the same bounded 10 ms bursts as Open Screen's
/// `SenderPacketRouter`. A successful local UDP send does not mean that a
/// receiver's much smaller input buffer can absorb an entire large keyframe.
struct PacketPacer {
    burst_started: Instant,
    packets_in_burst: u64,
}

impl PacketPacer {
    fn new(now: Instant) -> Self {
        Self {
            burst_started: now,
            packets_in_burst: 0,
        }
    }

    fn delay_for_next_packet(&mut self, now: Instant) -> Duration {
        let next_burst = self
            .burst_started
            .checked_add(bandwidth::BURST_INTERVAL)
            .unwrap_or(now);
        if now >= next_burst {
            self.burst_started = now;
            self.packets_in_burst = 1;
            return Duration::ZERO;
        }
        if self.packets_in_burst < bandwidth::MAX_PACKETS_PER_BURST {
            self.packets_in_burst = self.packets_in_burst.saturating_add(1);
            return Duration::ZERO;
        }

        self.burst_started = next_burst;
        self.packets_in_burst = 1;
        next_burst.saturating_duration_since(now)
    }

    fn wait_for_next_packet(&mut self) {
        let delay = self.delay_for_next_packet(Instant::now());
        if !delay.is_zero() {
            thread::sleep(delay);
        }
    }
}

struct Stream {
    kind: StreamKind,
    ssrc: u32,
    packetizer: Packetizer,
    crypto: FrameCrypto,
    next_frame_id: u64,
    /// Last frame the receiver has fully received; -1 until it acks anything,
    /// which is also how it reports "nothing yet" on the wire.
    checkpoint: i64,
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
            checkpoint: -1,
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
        peer: SocketAddr,
        streams: Vec<StreamConfig>,
        chunks: ChunkReceiver,
        request_keyframe: Box<dyn Fn() + Send>,
        rate_control: RateControl,
    ) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_flag = Arc::clone(&stop);
        #[allow(
            clippy::expect_used,
            reason = "spawning a thread only fails on OS resource exhaustion, which is unrecoverable"
        )]
        let handle = thread::Builder::new()
            .name("mirror-sender".into())
            .spawn(move || {
                run(
                    &socket,
                    peer,
                    &streams,
                    &chunks,
                    &*request_keyframe,
                    &rate_control,
                    &stop_flag,
                );
            })
            .expect("failed to spawn mirror-sender thread");
        Self {
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

/// Drains pending receiver RTCP: acknowledgements (which retire history and
/// feed the bandwidth estimate), NACKs and picture-loss reports.
#[allow(
    clippy::too_many_arguments,
    reason = "one call site, all of it per-loop state"
)]
fn service_rtcp(
    socket: &UdpSocket,
    peer: SocketAddr,
    streams: &mut [Stream],
    events: &mut rtcp::ReceiverEvents,
    receive_buffer: &mut [u8; 1500],
    estimator: &mut BandwidthEstimator,
    pacer: &mut PacketPacer,
    acked: &mut bool,
    request_keyframe: &dyn Fn(),
) {
    // 2. Receiver RTCP. The socket is unconnected: a receiver may answer
    // from a different port than the one it gave us in the ANSWER.
    while let Ok((size, _from)) = socket.recv_from(receive_buffer) {
        let Some(packet) = receive_buffer.get(..size) else {
            continue;
        };
        if let Some(rtt) = events.round_trip {
            estimator.on_round_trip(rtt);
        }
        for stream in streams.iter_mut() {
            rtcp::parse(packet, stream.ssrc, stream.checkpoint, events);
            if let Some(checkpoint) = events.checkpoint_frame_id {
                if !*acked && checkpoint >= 0 && stream.kind == StreamKind::Video {
                    info!("receiver acknowledged video up to frame {checkpoint}");
                    *acked = true;
                }
                stream.checkpoint = stream.checkpoint.max(checkpoint);
                while stream.history.front().is_some_and(|f| {
                    i64::try_from(f.frame_id).unwrap_or(i64::MAX) <= stream.checkpoint
                }) {
                    // Retiring a frame is the receiver confirming its bytes.
                    let confirmed = stream
                        .history
                        .front()
                        .map_or(0, |f| frame_bytes(&f.packets));
                    estimator.on_bytes_confirmed(confirmed, Instant::now());
                    stream.evict_oldest();
                }
            }
            for nack in &events.nacks {
                retransmit(socket, peer, stream, nack, pacer);
            }
            if events.picture_loss && stream.kind == StreamKind::Video {
                debug!("receiver reported picture loss, forcing a key frame");
                request_keyframe();
            }
        }
    }
}

/// Moves the encoder bitrate towards what the link is actually carrying, and
/// the capture resolution when the bitrate alone cannot close the gap.
fn control_rate(rate_control: &RateControl, state: &mut RateState) {
    let now = Instant::now();
    if now.duration_since(state.last_control) < bandwidth::CONTROL_INTERVAL {
        return;
    }
    state.last_control = now;
    let Some(estimate) = state.estimator.estimate_bps(now) else {
        return;
    };
    debug!(
        "bandwidth estimate {estimate} bps at {} bps encoded",
        state.current_bps
    );

    if let Some(set_bitrate) = rate_control.set_bitrate.as_ref()
        && let Some(next) = bandwidth::next_bitrate(
            state.current_bps,
            estimate,
            rate_control.min_bps,
            rate_control.max_bps,
        )
    {
        debug!("link is carrying {estimate} bps; encoder bitrate -> {next} bps");
        set_bitrate(next);
        state.current_bps = next;
    }

    // The bitrate loop has run out of room in one direction or the other.
    let at_floor = state.current_bps <= rate_control.min_bps;
    let at_ceiling = state.current_bps >= rate_control.max_bps;
    if let Some(set_size) = rate_control.set_size.as_ref()
        && let Some(ladder) = state.ladder.as_mut()
        && let Some(size) = ladder.observe(at_floor, at_ceiling, now)
    {
        info!("capture resolution -> {}x{}", size.0, size.1);
        set_size(size);
    }
}

/// Total wire bytes of a packetized frame, counted when the receiver confirms it.
fn frame_bytes(packets: &PacketizedFrame) -> u64 {
    packets.iter().fold(0_u64, |sum, packet| {
        sum.saturating_add(u64::try_from(packet.len()).unwrap_or(0))
    })
}

/// How the bitrate control loop may move, and how to apply a new value.
/// `set_bitrate` is `None` when the user pinned a bitrate: an explicit setting
/// is honoured rather than adapted away.
/// Applies a new encoder bitrate, in bits per second.
pub type SetBitrate = Box<dyn Fn(u32) + Send>;
/// Applies a new capture size.
pub type SetSize = Box<dyn Fn((i32, i32)) + Send>;

pub struct RateControl {
    pub start_bps: u32,
    pub min_bps: u32,
    pub max_bps: u32,
    pub set_bitrate: Option<SetBitrate>,
    /// Applies a new capture size. `None` when the user pinned a resolution.
    pub set_size: Option<SetSize>,
    /// The rungs the capture may move between, smallest first.
    pub sizes: Vec<(i32, i32)>,
    pub start_size: (i32, i32),
}

/// Per-run state for the bitrate and resolution loops.
struct RateState {
    estimator: BandwidthEstimator,
    current_bps: u32,
    last_control: Instant,
    ladder: Option<ResolutionLadder>,
}

fn run(
    socket: &UdpSocket,
    peer: SocketAddr,
    configs: &[StreamConfig],
    chunks: &ChunkReceiver,
    request_keyframe: &dyn Fn(),
    rate_control: &RateControl,
    stop: &AtomicBool,
) {
    let mut streams: Vec<Stream> = configs.iter().map(Stream::new).collect();
    if socket.set_nonblocking(true).is_err() {
        warn!("could not make the RTP socket non-blocking");
    }
    let mut receive_buffer = [0_u8; 1500];
    // Reused across RTCP packets so NACK parsing does not allocate per packet.
    let mut events = rtcp::ReceiverEvents::default();
    let mut video_started_at = None;
    // Tells "not getting our RTP" apart from "getting it but not decoding".
    let mut acked = false;
    let mut reported_silence = false;
    let mut reported_drops = false;
    let mut pacer = PacketPacer::new(Instant::now());
    let mut rate_state = RateState {
        estimator: BandwidthEstimator::new(Instant::now()),
        current_bps: rate_control.start_bps,
        last_control: Instant::now(),
        ladder: rate_control.set_size.as_ref().map(|_| {
            ResolutionLadder::new(
                rate_control.sizes.clone(),
                rate_control.start_size,
                Instant::now(),
            )
        }),
    };

    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }

        // 1. Media frames (block briefly so RTCP still gets serviced).
        match chunks.recv_timeout(TICK) {
            Ok(chunk) => {
                if let Some(stream) = streams.iter_mut().find(|s| s.kind == chunk.kind) {
                    if chunk.kind == StreamKind::Video && video_started_at.is_none() {
                        info!("sending first video frame to the receiver");
                        video_started_at = Some(Instant::now());
                    }
                    let before = (stream.octet_count, stream.packet_count);
                    let dropped = send_frame(socket, peer, stream, &chunk, &mut pacer);
                    rate_state.estimator.on_burst_sent(
                        u64::from(stream.octet_count.wrapping_sub(before.0)),
                        u64::from(stream.packet_count.wrapping_sub(before.1)),
                        Instant::now(),
                    );
                    if dropped > 0 && !reported_drops {
                        warn!(
                            "the network would not take {dropped} packet(s) of a frame; \
                             the picture will break up - try a lower bitrate"
                        );
                        reported_drops = true;
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }

        if !acked
            && !reported_silence
            && video_started_at.is_some_and(|t| t.elapsed() >= FEEDBACK_TIMEOUT)
        {
            warn!(
                "no RTCP feedback from the receiver after {}s of video; it is not receiving our RTP",
                FEEDBACK_TIMEOUT.as_secs()
            );
            reported_silence = true;
        }

        service_rtcp(
            socket,
            peer,
            &mut streams,
            &mut events,
            &mut receive_buffer,
            &mut rate_state.estimator,
            &mut pacer,
            &mut acked,
            request_keyframe,
        );

        // 3. Bitrate control: what the receiver is confirming decides what the
        // encoder is asked for next.
        control_rate(rate_control, &mut rate_state);

        // 4. Periodic Sender Reports.
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
                let _ = send_sender_report(socket, peer, &report);
                stream.last_report = Some(Instant::now());
            }
        }
    }
    info!("mirror sender stopped");
}

/// Sender reports share the unconnected RTP socket so receiver feedback comes
/// back to the same port. Unlike RTP's `send_to`, the old `send` call failed
/// with `ENOTCONN` after the socket was deliberately made unconnected.
fn send_sender_report(
    socket: &UdpSocket,
    peer: SocketAddr,
    report: &[u8],
) -> std::io::Result<usize> {
    socket.send_to(report, peer)
}

/// Sends one packet, waiting out a full send buffer rather than dropping it:
/// the socket is non-blocking (so RTCP can be polled), so a frame's packet
/// burst hits `WouldBlock`, and a dropped packet costs the receiver the whole
/// frame. Waiting paces us to what the link accepts. False if it never went.
fn send_packet(
    socket: &UdpSocket,
    peer: SocketAddr,
    packet: &[u8],
    pacer: &mut PacketPacer,
) -> bool {
    pacer.wait_for_next_packet();
    let deadline = Instant::now().checked_add(SEND_TIMEOUT);
    loop {
        match socket.send_to(packet, peer) {
            Ok(_) => return true,
            Err(e) if e.kind() == ErrorKind::WouldBlock => {
                if deadline.is_none_or(|deadline| Instant::now() >= deadline) {
                    debug!("RTP send buffer stayed full for {SEND_TIMEOUT:?}, dropping a packet");
                    return false;
                }
                thread::sleep(SEND_RETRY_DELAY);
            }
            Err(e) => {
                debug!("RTP send failed: {e}");
                return false;
            }
        }
    }
}

/// Returns how many packets of the frame could not be sent.
fn send_frame(
    socket: &UdpSocket,
    peer: SocketAddr,
    stream: &mut Stream,
    chunk: &EncodedChunk,
    pacer: &mut PacketPacer,
) -> u32 {
    let frame_id = stream.next_frame_id;
    stream.next_frame_id = stream.next_frame_id.wrapping_add(1);

    let frame = OutboundFrame {
        frame_id,
        referenced_frame_id: if chunk.is_key_frame {
            frame_id
        } else {
            frame_id.saturating_sub(1)
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

    if chunk.kind == StreamKind::Video && chunk.is_key_frame {
        debug!(
            "video key frame {frame_id}: {} bytes in {} packets",
            chunk.data.len(),
            packets.iter().count()
        );
    }

    let mut dropped: u32 = 0;
    for packet in packets.iter() {
        stream.packet_count = stream.packet_count.wrapping_add(1);
        stream.octet_count = stream
            .octet_count
            .wrapping_add(u32::try_from(packet.len()).unwrap_or(u32::MAX));
        if !send_packet(socket, peer, packet, pacer) {
            dropped = dropped.saturating_add(1);
        }
    }
    stream.last_timestamps = Some((chunk.rtp_timestamp, chunk.ntp_timestamp));

    stream.history.push_back(SentFrame { frame_id, packets });
    if stream.history.len() > MAX_HISTORY_FRAMES {
        stream.evict_oldest();
    }
    dropped
}

fn retransmit(
    socket: &UdpSocket,
    peer: SocketAddr,
    stream: &Stream,
    nack: &rtcp::Nack,
    pacer: &mut PacketPacer,
) {
    let Some(frame) = stream
        .history
        .iter()
        .find(|f| i64::try_from(f.frame_id).unwrap_or(i64::MAX) == nack.frame_id)
    else {
        return;
    };
    if nack.packet_id == rtcp::ALL_PACKETS_LOST {
        for packet in frame.packets.iter() {
            send_packet(socket, peer, packet, pacer);
        }
    } else if let Some(packet) = frame.packets.packet(nack.packet_id) {
        send_packet(socket, peer, packet, pacer);
    } else {
        // The receiver NACKed a packet id we never produced; nothing to resend.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packet_pacer_splits_packets_into_ten_millisecond_bursts() {
        let mut now = Instant::now();
        let mut pacer = PacketPacer::new(now);
        for _ in 0..bandwidth::MAX_PACKETS_PER_BURST {
            assert_eq!(pacer.delay_for_next_packet(now), Duration::ZERO);
        }

        let delay = pacer.delay_for_next_packet(now);
        assert_eq!(delay, bandwidth::BURST_INTERVAL);
        now += delay;
        for _ in 1..bandwidth::MAX_PACKETS_PER_BURST {
            assert_eq!(pacer.delay_for_next_packet(now), Duration::ZERO);
        }
        assert_eq!(pacer.delay_for_next_packet(now), bandwidth::BURST_INTERVAL);
    }

    #[test]
    fn sender_report_works_on_the_unconnected_rtp_socket() {
        let receiver = UdpSocket::bind("127.0.0.1:0").unwrap();
        receiver
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        let sender = UdpSocket::bind("127.0.0.1:0").unwrap();
        assert!(sender.peer_addr().is_err());

        let report = [0x80, 200, 0, 0];
        send_sender_report(&sender, receiver.local_addr().unwrap(), &report).unwrap();

        let mut received = [0; 4];
        let (size, from) = receiver.recv_from(&mut received).unwrap();
        assert_eq!(size, report.len());
        assert_eq!(received, report);
        assert_eq!(from, sender.local_addr().unwrap());
    }

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
