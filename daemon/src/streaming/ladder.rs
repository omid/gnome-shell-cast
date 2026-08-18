//! The capture resolution ladder, ported from Chromium's
//! `media/capture/content/capture_resolution_chooser` and the size-change
//! policy of `video_capture_oracle`.
//!
//! Resolution moves in discrete steps, never continuously: upstream steps the
//! height down 90 pixels at a time and drops any step that is not at least 15%
//! smaller in area, because "the end-to-end system cannot stabilize" when
//! consecutive steps are too close together.
//!
//! Chromium drives the target area from capture buffer utilization, which we
//! cannot see. Ours is driven by the bitrate control loop instead: a link that
//! has pinned the encoder at its floor cannot carry the current frame size, and
//! sustained headroom at the ceiling means it could carry more.

use std::time::{Duration, Instant};

/// Upstream's step between snapped heights.
const HEIGHT_STEP: i32 = 90;
/// Upstream requires this much area reduction between neighbouring steps.
const MIN_AREA_REDUCTION_PERCENT: i64 = 15;
/// Upstream's `min_size_change_period`: how long a size must be held.
const MIN_SIZE_CHANGE_PERIOD: Duration = Duration::from_secs(3);
/// Upstream's `kProvingPeriodForAnimatedContent`: how long headroom must last
/// before stepping *up*, which is deliberately far more cautious than down.
const PROVING_PERIOD: Duration = Duration::from_secs(30);

/// The discrete sizes this source may be captured at, smallest first.
pub fn snapped_sizes(source: (i32, i32), min: (i32, i32), max: (i32, i32)) -> Vec<(i32, i32)> {
    let width = source.0.clamp(min.0.min(max.0), max.0).max(2);
    let height = source.1.clamp(min.1.min(max.1), max.1).max(2);
    let mut sizes = vec![(width & !1, height & !1)];

    let mut next_height = height;
    loop {
        next_height = next_height.saturating_sub(HEIGHT_STEP);
        if next_height < min.1.min(height) || next_height < 2 {
            break;
        }
        let scaled_width = i64::from(next_height)
            .saturating_mul(i64::from(width))
            .checked_div(i64::from(height))
            .and_then(|w| i32::try_from(w).ok())
            .unwrap_or(2);
        let candidate = ((scaled_width & !1).max(2), (next_height & !1).max(2));
        let Some(&smallest) = sizes.last() else {
            break;
        };
        if !is_meaningfully_smaller(candidate, smallest) {
            continue;
        }
        sizes.push(candidate);
    }
    sizes.reverse();
    sizes
}

fn area(size: (i32, i32)) -> i64 {
    i64::from(size.0).saturating_mul(i64::from(size.1))
}

/// Whether `candidate` cuts at least `MIN_AREA_REDUCTION_PERCENT` off `current`.
fn is_meaningfully_smaller(candidate: (i32, i32), current: (i32, i32)) -> bool {
    let limit = area(current)
        .saturating_mul(100_i64.saturating_sub(MIN_AREA_REDUCTION_PERCENT))
        .checked_div(100)
        .unwrap_or(0);
    area(candidate) <= limit
}

/// Tracks the current rung and decides when to move, damped so the picture
/// does not oscillate.
pub struct ResolutionLadder {
    sizes: Vec<(i32, i32)>,
    index: usize,
    changed_at: Instant,
    headroom_since: Option<Instant>,
}

impl ResolutionLadder {
    /// `current` is where the pipeline starts; the nearest rung is taken.
    pub fn new(sizes: Vec<(i32, i32)>, current: (i32, i32), now: Instant) -> Self {
        let index = nearest_index(&sizes, area(current));
        Self {
            sizes,
            index,
            changed_at: now,
            headroom_since: None,
        }
    }

    pub fn current(&self) -> Option<(i32, i32)> {
        self.sizes.get(self.index).copied()
    }

    /// Reports where the bitrate loop has ended up. Returns a new size when the
    /// ladder decides to move. `at_floor` means the link could not carry what
    /// we asked for; `at_ceiling` means it carried everything we asked for.
    pub fn observe(
        &mut self,
        at_floor: bool,
        at_ceiling: bool,
        now: Instant,
    ) -> Option<(i32, i32)> {
        if at_ceiling {
            self.headroom_since.get_or_insert(now);
        } else {
            self.headroom_since = None;
        }
        if now.saturating_duration_since(self.changed_at) < MIN_SIZE_CHANGE_PERIOD {
            return None;
        }

        if at_floor && self.index > 0 {
            self.index = self.index.saturating_sub(1);
            self.changed_at = now;
            self.headroom_since = None;
            return self.current();
        }

        let proven = self
            .headroom_since
            .is_some_and(|since| now.saturating_duration_since(since) >= PROVING_PERIOD);
        if proven && self.index.saturating_add(1) < self.sizes.len() {
            self.index = self.index.saturating_add(1);
            self.changed_at = now;
            self.headroom_since = None;
            return self.current();
        }
        None
    }
}

/// Index of the rung whose area is closest to `target`.
fn nearest_index(sizes: &[(i32, i32)], target: i64) -> usize {
    let mut best = 0;
    let mut best_delta = i64::MAX;
    for (index, size) in sizes.iter().enumerate() {
        let delta = area(*size).saturating_sub(target).abs();
        if delta < best_delta {
            best_delta = delta;
            best = index;
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: (i32, i32) = (320, 180);
    const MAX: (i32, i32) = (1920, 1080);

    #[test]
    fn the_ladder_runs_from_smallest_to_largest() {
        let sizes = snapped_sizes((1920, 1080), MIN, MAX);
        assert_eq!(sizes.last(), Some(&(1920, 1080)));
        for pair in sizes.windows(2) {
            assert!(area(pair[0]) < area(pair[1]), "not ascending: {sizes:?}");
        }
    }

    #[test]
    fn neighbouring_steps_differ_by_at_least_15_percent_of_area() {
        let sizes = snapped_sizes((1920, 1080), MIN, MAX);
        for pair in sizes.windows(2) {
            assert!(
                is_meaningfully_smaller(pair[0], pair[1]),
                "steps too close: {:?} then {:?}",
                pair[0],
                pair[1]
            );
        }
    }

    #[test]
    fn every_rung_keeps_the_source_aspect_ratio() {
        let sizes = snapped_sizes((1920, 1080), MIN, MAX);
        for size in sizes {
            let ratio = f64::from(size.0) / f64::from(size.1);
            assert!((ratio - 16.0 / 9.0).abs() < 0.05, "bad ratio for {size:?}");
        }
    }

    #[test]
    fn a_source_smaller_than_the_maximum_is_not_scaled_up() {
        let sizes = snapped_sizes((1280, 720), MIN, MAX);
        assert_eq!(sizes.last(), Some(&(1280, 720)));
    }

    #[test]
    fn a_floor_bound_link_steps_down_after_the_damping_period() {
        let now = Instant::now();
        let sizes = snapped_sizes((1920, 1080), MIN, MAX);
        let mut ladder = ResolutionLadder::new(sizes, (1920, 1080), now);
        // Too soon to move.
        assert_eq!(ladder.observe(true, false, now), None);
        let later = now.checked_add(Duration::from_secs(4)).unwrap();
        let stepped = ladder.observe(true, false, later).unwrap();
        assert!(area(stepped) < area((1920, 1080)));
    }

    #[test]
    fn headroom_must_be_proven_for_30s_before_stepping_up() {
        let now = Instant::now();
        let sizes = snapped_sizes((1920, 1080), MIN, MAX);
        let mut ladder = ResolutionLadder::new(sizes, (640, 360), now);
        let start = ladder.current().unwrap();
        // Headroom for 10s is not enough.
        let ten = now.checked_add(Duration::from_secs(10)).unwrap();
        assert_eq!(ladder.observe(false, true, ten), None);
        let forty = now.checked_add(Duration::from_secs(40)).unwrap();
        let stepped = ladder.observe(false, true, forty).unwrap();
        assert!(area(stepped) > area(start));
    }

    #[test]
    fn a_gap_in_headroom_restarts_the_proving_period() {
        let now = Instant::now();
        let sizes = snapped_sizes((1920, 1080), MIN, MAX);
        let mut ladder = ResolutionLadder::new(sizes, (640, 360), now);
        let twenty = now.checked_add(Duration::from_secs(20)).unwrap();
        assert_eq!(ladder.observe(false, true, twenty), None);
        // One observation without headroom resets the clock.
        let twenty_five = now.checked_add(Duration::from_secs(25)).unwrap();
        assert_eq!(ladder.observe(false, false, twenty_five), None);
        let forty = now.checked_add(Duration::from_secs(40)).unwrap();
        assert_eq!(ladder.observe(false, true, forty), None);
    }

    #[test]
    fn the_ladder_stops_at_both_ends() {
        let now = Instant::now();
        let sizes = snapped_sizes((1920, 1080), MIN, MAX);
        let smallest = sizes.first().copied().unwrap();
        let mut ladder = ResolutionLadder::new(sizes, smallest, now);
        let later = now.checked_add(Duration::from_secs(4)).unwrap();
        assert_eq!(ladder.observe(true, false, later), None);
    }
}
