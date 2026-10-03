//! Network bandwidth estimation and the encoder bitrate control loop.
//!
//! Ported from openscreen's `bandwidth_estimator`: two ring buffers over a
//! recent history window, one counting the bytes we pushed at the socket and
//! one counting the bytes the receiver has confirmed. The control policy is
//! openscreen's, and deliberately asymmetric - drop fast, climb slowly.
//!
//! Feedback is attributed back to when the packets were *sent*, using the
//! round-trip time from Receiver Report LSR/DLSR, and throughput is measured
//! over the time actually spent transmitting rather than over the whole
//! window - both as upstream does, so a sender that is not saturating the
//! link is not mistaken for one that cannot go faster.

use std::time::{Duration, Instant};

/// Upstream's `kNumTimeslices`.
const NUM_TIMESLICES: usize = 256;
const NUM_TIMESLICES_U32: u32 = 256;
/// How much recent history the estimate is averaged over.
const HISTORY_WINDOW: Duration = Duration::from_secs(2);
/// Chromium keeps only this much of the estimate: "Don't ever try to use *all*
/// of the network bandwidth!" - 0.8, as a fraction to keep the maths integral.
const SAFETY_NUMERATOR: u64 = 4;
const SAFETY_DENOMINATOR: u64 = 5;
/// Chromium's `kConservativeIncrease` of 1.1.
const INCREASE_NUMERATOR: u64 = 11;
const INCREASE_DENOMINATOR: u64 = 10;
/// How often the loop may change the encoder bitrate.
pub const CONTROL_INTERVAL: Duration = Duration::from_millis(500);

/// Open Screen's default 24 Mbit/s burst ceiling, divided into 10 ms bursts
/// of our 1472-byte maximum UDP payloads.
pub const MAX_PACKETS_PER_BURST: u64 = 21;
pub const BURST_INTERVAL: Duration = Duration::from_millis(10);

/// A ring buffer of byte counts over `NUM_TIMESLICES` equal slices.
struct FlowTracker {
    slice: Duration,
    history: [u64; NUM_TIMESLICES],
    /// Index of the newest slice.
    head: usize,
    head_started: Instant,
    /// When this tracker first and last saw a sample, for the overlap check.
    first_sample: Option<Instant>,
    last_sample: Option<Instant>,
}

impl FlowTracker {
    fn new(now: Instant) -> Self {
        Self {
            slice: HISTORY_WINDOW
                .checked_div(NUM_TIMESLICES_U32)
                .unwrap_or(HISTORY_WINDOW),
            history: [0; NUM_TIMESLICES],
            head: 0,
            head_started: now,
            first_sample: None,
            last_sample: None,
        }
    }

    /// Rolls the window forward to `now`, zeroing the slices passed over.
    fn advance_to(&mut self, now: Instant) {
        let elapsed = now.saturating_duration_since(self.head_started);
        let Some(slices) = elapsed.as_nanos().checked_div(self.slice.as_nanos().max(1)) else {
            return;
        };
        let slices = usize::try_from(slices).unwrap_or(NUM_TIMESLICES);
        if slices == 0 {
            return;
        }
        if slices >= NUM_TIMESLICES {
            self.history = [0; NUM_TIMESLICES];
        } else {
            for step in 1..=slices {
                let index = self.head.wrapping_add(step) % NUM_TIMESLICES;
                if let Some(slot) = self.history.get_mut(index) {
                    *slot = 0;
                }
            }
        }
        self.head = self.head.wrapping_add(slices) % NUM_TIMESLICES;
        self.head_started = self
            .head_started
            .checked_add(
                self.slice
                    .saturating_mul(u32::try_from(slices).unwrap_or(1)),
            )
            .unwrap_or(now);
    }

    fn accumulate(&mut self, bytes: u64, now: Instant) {
        self.advance_to(now);
        if let Some(slot) = self.history.get_mut(self.head) {
            *slot = slot.saturating_add(bytes);
        }
        self.first_sample.get_or_insert(now);
        self.last_sample = Some(now);
    }

    fn sum(&self) -> u64 {
        self.history.iter().fold(0_u64, |a, b| a.saturating_add(*b))
    }
}

pub struct BandwidthEstimator {
    sent: FlowTracker,
    packets: FlowTracker,
    confirmed: FlowTracker,
    started: Instant,
    round_trip: Option<Duration>,
}

impl BandwidthEstimator {
    pub fn new(now: Instant) -> Self {
        Self {
            sent: FlowTracker::new(now),
            packets: FlowTracker::new(now),
            confirmed: FlowTracker::new(now),
            started: now,
            round_trip: None,
        }
    }

    /// `packets` is what the burst cost us; the byte count sizes the flow and
    /// the packet count sizes the time spent transmitting.
    pub fn on_burst_sent(&mut self, bytes: u64, packets: u64, now: Instant) {
        self.sent.accumulate(bytes, now);
        self.packets.accumulate(packets, now);
    }

    /// Records the receiver's confirmation, attributed back to when those
    /// bytes were sent - one round trip ago.
    pub fn on_bytes_confirmed(&mut self, bytes: u64, now: Instant) {
        let sent_at = self
            .round_trip
            .and_then(|rtt| now.checked_sub(rtt))
            .unwrap_or(now);
        self.confirmed.accumulate(bytes, sent_at);
    }

    /// Whether the sending and confirming histories cover enough common time
    /// to compare. Upstream's threshold is half the history window.
    fn histories_overlap(&self, now: Instant) -> bool {
        let (Some(sent_first), Some(sent_last)) = (self.sent.first_sample, self.sent.last_sample)
        else {
            return false;
        };
        let (Some(conf_first), Some(conf_last)) =
            (self.confirmed.first_sample, self.confirmed.last_sample)
        else {
            return false;
        };
        // Only the part of each history still inside the window counts.
        let window_start = now.checked_sub(HISTORY_WINDOW).unwrap_or(now);
        let start = sent_first.max(conf_first).max(window_start);
        let end = sent_last.min(conf_last);
        if end <= start {
            return false;
        }
        end.saturating_duration_since(start)
            >= HISTORY_WINDOW.checked_div(2).unwrap_or(HISTORY_WINDOW)
    }

    pub fn on_round_trip(&mut self, rtt: Duration) {
        self.round_trip = Some(rtt);
    }

    /// Bits per second the receiver is confirming, or `None` until a full
    /// window has gone by - upstream likewise refuses to guess from a short
    /// history rather than reporting a wrong number.
    pub fn estimate_bps(&mut self, now: Instant) -> Option<u32> {
        if now.saturating_duration_since(self.started) < HISTORY_WINDOW {
            return None;
        }
        self.sent.advance_to(now);
        self.packets.advance_to(now);
        self.confirmed.advance_to(now);
        let confirmed = self.confirmed.sum();
        if confirmed == 0 {
            return None;
        }
        // Upstream refuses to estimate unless the two histories overlap for at
        // least half the window: a tick where acknowledgements briefly lag the
        // sending would otherwise read as a collapsed link.
        if !self.histories_overlap(now) {
            return None;
        }
        // Only the fraction of the window we were actually transmitting in
        // counts, or an idle sender looks like a slow link.
        let window_ms = u64::try_from(HISTORY_WINDOW.as_millis()).unwrap_or(2_000);
        let bursts = window_ms
            .checked_div(
                u64::try_from(BURST_INTERVAL.as_millis())
                    .unwrap_or(10)
                    .max(1),
            )
            .unwrap_or(1);
        let capacity = bursts.saturating_mul(MAX_PACKETS_PER_BURST).max(1);
        let sent_packets = self.packets.sum().min(capacity).max(1);
        let transmit_ms = window_ms
            .saturating_mul(sent_packets)
            .checked_div(capacity)?
            .max(1);
        let bits = confirmed.saturating_mul(8).saturating_mul(1000);
        bits.checked_div(transmit_ms).map(clamp_to_u32)
    }
}

fn clamp_to_u32(value: u64) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

fn scale(value: u32, numerator: u64, denominator: u64) -> u32 {
    u64::from(value)
        .saturating_mul(numerator)
        .checked_div(denominator.max(1))
        .map_or(value, clamp_to_u32)
}

/// The next encoder bitrate given the measured throughput, following
/// openscreen: below target, drop straight to what the link is carrying;
/// at or above it, edge up. `None` leaves the bitrate alone.
pub fn next_bitrate(
    current_bps: u32,
    estimate_bps: u32,
    min_bps: u32,
    max_bps: u32,
) -> Option<u32> {
    let usable = scale(estimate_bps, SAFETY_NUMERATOR, SAFETY_DENOMINATOR);
    let target = if usable < current_bps {
        usable
    } else {
        scale(current_bps, INCREASE_NUMERATOR, INCREASE_DENOMINATOR)
    };
    let target = target.clamp(min_bps.min(max_bps), max_bps);
    // Ignore changes too small to be worth reconfiguring the encoder for.
    let delta = target.abs_diff(current_bps);
    let threshold = current_bps.checked_div(20).unwrap_or(0);
    (delta > threshold).then_some(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(base: Instant, ms: u64) -> Instant {
        base.checked_add(Duration::from_millis(ms)).unwrap()
    }

    #[test]
    fn no_estimate_before_a_full_window() {
        let now = Instant::now();
        let mut e = BandwidthEstimator::new(now);
        e.on_bytes_confirmed(100_000, now);
        assert_eq!(e.estimate_bps(at(now, 100)), None);
    }

    #[test]
    fn silence_yields_no_estimate() {
        let now = Instant::now();
        let mut e = BandwidthEstimator::new(now);
        e.on_burst_sent(100_000, 64, now);
        assert_eq!(e.estimate_bps(at(now, 2100)), None);
    }

    /// Saturating the sender for the whole window: throughput is simply the
    /// confirmed bytes over the window.
    #[test]
    fn confirmed_bytes_become_bits_per_second() {
        let now = Instant::now();
        let mut e = BandwidthEstimator::new(now);
        // Far more packets than a full window can carry saturates the model.
        for step in 0..20 {
            e.on_burst_sent(12_500, 640, at(now, step * 100));
            e.on_bytes_confirmed(12_500, at(now, step * 100));
        }
        let estimate = e.estimate_bps(at(now, 2000)).unwrap();
        assert!(
            (900_000..=1_100_000).contains(&estimate),
            "unexpected estimate {estimate}"
        );
    }

    /// The point of the burst-duration model: a sender that only transmitted
    /// for a tenth of the window is carrying far more than its average.
    #[test]
    fn an_idle_sender_is_not_mistaken_for_a_slow_link() {
        let now = Instant::now();
        let mut e = BandwidthEstimator::new(now);
        for step in 0..20 {
            e.on_burst_sent(12_500, MAX_PACKETS_PER_BURST, at(now, step * 100));
            e.on_bytes_confirmed(12_500, at(now, step * 100));
        }
        let estimate = e.estimate_bps(at(now, 2000)).unwrap();
        // Same bytes as above but a tenth of the transmit time: ~10x.
        assert!(estimate > 5_000_000, "unexpected estimate {estimate}");
    }

    /// A round trip long enough to push every confirmation out of the window
    /// leaves nothing to measure, which is how the shift proves it happened.
    #[test]
    fn feedback_is_attributed_back_to_when_it_was_sent() {
        let now = Instant::now();
        let mut shifted = BandwidthEstimator::new(now);
        shifted.on_round_trip(Duration::from_secs(5));
        let mut unshifted = BandwidthEstimator::new(now);
        for step in 0..30 {
            let when = at(now, step * 100);
            shifted.on_burst_sent(12_500, 640, when);
            shifted.on_bytes_confirmed(12_500, when);
            unshifted.on_burst_sent(12_500, 640, when);
            unshifted.on_bytes_confirmed(12_500, when);
        }
        let at_end = at(now, 3_000);
        assert!(unshifted.estimate_bps(at_end).is_some());
        assert_eq!(shifted.estimate_bps(at_end), None);
    }

    /// The guard that stops one lagging tick of acknowledgements from reading
    /// as a collapsed link - which it did, on a real cast, before this existed.
    #[test]
    fn a_short_burst_of_feedback_is_not_enough_to_estimate() {
        let now = Instant::now();
        let mut e = BandwidthEstimator::new(now);
        for step in 0..30 {
            e.on_burst_sent(12_500, 640, at(now, step * 100));
        }
        // Confirmations covering only the last 200ms of a 2s window.
        e.on_bytes_confirmed(12_500, at(now, 2_800));
        e.on_bytes_confirmed(12_500, at(now, 3_000));
        assert_eq!(e.estimate_bps(at(now, 3_000)), None);
    }

    #[test]
    fn old_samples_age_out_of_the_window() {
        let now = Instant::now();
        let mut e = BandwidthEstimator::new(now);
        e.on_bytes_confirmed(250_000, now);
        // Long after the window has rolled past, nothing is left.
        assert_eq!(e.estimate_bps(at(now, 10_000)), None);
    }

    #[test]
    fn a_drop_in_throughput_cuts_the_bitrate_immediately() {
        // Carrying 1 Mbit/s while sending 4 Mbit/s.
        let next = next_bitrate(4_000_000, 1_000_000, 300_000, 5_000_000).unwrap();
        assert_eq!(next, 800_000);
    }

    #[test]
    fn headroom_raises_the_bitrate_only_gradually() {
        let next = next_bitrate(1_000_000, 50_000_000, 300_000, 5_000_000).unwrap();
        assert_eq!(next, 1_100_000);
    }

    #[test]
    fn the_bitrate_stays_inside_the_negotiated_range() {
        // The 1.1x step would overshoot the ceiling, so it lands on it.
        assert_eq!(
            next_bitrate(4_000_000, 50_000_000, 300_000, 4_300_000),
            Some(4_300_000)
        );
        // A collapse cannot push it below the receiver's floor.
        assert_eq!(
            next_bitrate(5_000_000, 10_000, 300_000, 5_000_000),
            Some(300_000)
        );
    }

    /// The dead-band only suppresses tiny *reductions*: a climb is always a
    /// full 10% step, which clears the 5% threshold by construction.
    #[test]
    fn a_small_reduction_is_not_worth_reconfiguring_the_encoder() {
        // usable = 992 kbit/s against a 1 Mbit/s target: under 5% down.
        assert_eq!(next_bitrate(1_000_000, 1_240_000, 300_000, 5_000_000), None);
    }
}
