//! Frame timing: FPS, frame time and low percentiles derived from a stream of present
//! timestamps.
//!
//! The timestamps come from ETW and the arithmetic lives here — it is also the one part of
//! the project that unit tests can cover properly.

use std::collections::VecDeque;

/// How many frames the ring buffer holds.
///
/// A 0.1% low needs at least a thousand frames to mean anything, so the capacity is sized for
/// several seconds at a high frame rate.
const DEFAULT_CAPACITY: usize = 4096;

/// If the game has not presented for longer than this, treat the stream as dead rather than
/// showing a frozen number from the previous scene.
const STALE_AFTER_NS: u64 = 1_000_000_000;

/// Below these sample counts the corresponding percentile is meaningless and is not computed.
const MIN_FRAMES_FOR_1PCT: usize = 100;
const MIN_FRAMES_FOR_01PCT: usize = 1000;

/// The span the displayed frame rate is measured over.
///
/// The rate has to be counted across a window rather than taken from the newest interval. A
/// present timestamp is when the call was made, not when the frame was shown, and the two
/// differ by a jittering amount: on a display locked to 144 Hz, individual intervals land
/// anywhere between six and eight milliseconds even though every frame is shown on time.
/// Inverting one of those reads 166, then 130, then 149 — a number that flickers wildly while
/// the game is in fact perfectly steady.
///
/// Half a second is long enough to average the jitter away and short enough that a real drop
/// is on screen almost at once.
const RATE_WINDOW_NS: u64 = 500_000_000;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FrameMetrics {
    /// Frames per second, counted over the last half second.
    pub fps: f32,
    /// Mean frame time over that same half second, in milliseconds.
    ///
    /// Deliberately the mean of the window rather than the newest interval, so that it and
    /// [`FrameMetrics::fps`] are two views of one measurement and cannot disagree on screen.
    pub frametime_ms: f32,
    /// Mean FPS across the whole buffer.
    pub avg_fps: f32,
    /// FPS corresponding to the 99th percentile frame time. `None` if there are too few frames.
    pub low_1pct: Option<f32>,
    /// FPS corresponding to the 99.9th percentile frame time. `None` if there are too few frames.
    pub low_01pct: Option<f32>,
    /// How many frames went into the calculation.
    pub sample_count: usize,
}

/// Ring buffer of present timestamps, in nanoseconds.
#[derive(Debug)]
pub struct FrameTimeline {
    times: VecDeque<u64>,
    capacity: usize,
}

impl Default for FrameTimeline {
    fn default() -> Self {
        Self::with_capacity(DEFAULT_CAPACITY)
    }
}

impl FrameTimeline {
    pub fn with_capacity(capacity: usize) -> Self {
        assert!(
            capacity >= 2,
            "a frame buffer smaller than two is meaningless"
        );
        Self {
            times: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    /// Records a frame timestamp.
    ///
    /// Non-increasing values are dropped: ETW delivers events in batches with no ordering
    /// guarantee inside a batch, and a negative frame time would corrupt every statistic
    /// downstream.
    pub fn push(&mut self, timestamp_ns: u64) {
        if let Some(&last) = self.times.back()
            && timestamp_ns <= last
        {
            return;
        }
        if self.times.len() == self.capacity {
            self.times.pop_front();
        }
        self.times.push_back(timestamp_ns);
    }

    /// Discards all history — when the focused game changes, for example.
    pub fn clear(&mut self) {
        self.times.clear();
    }

    pub fn is_empty(&self) -> bool {
        self.times.is_empty()
    }

    /// Computes metrics as of `now_ns`.
    ///
    /// Returns `None` with fewer than two frames, or when the last frame is too old — showing
    /// nothing beats showing the frame rate of a game that has already been minimised.
    pub fn metrics(&self, now_ns: u64) -> Option<FrameMetrics> {
        if self.times.len() < 2 {
            return None;
        }
        let last = *self.times.back()?;
        if now_ns.saturating_sub(last) > STALE_AFTER_NS {
            return None;
        }

        let first = *self.times.front()?;
        let span_ns = last.saturating_sub(first);
        if span_ns == 0 {
            return None;
        }

        let intervals = self.times.len() - 1;
        let avg_fps = intervals as f64 * 1e9 / span_ns as f64;

        let mut frametimes_ms: Vec<f32> = self
            .times
            .iter()
            .zip(self.times.iter().skip(1))
            .map(|(a, b)| (b - a) as f32 / 1e6)
            .collect();

        let (fps, frametime_ms) = self.recent_rate(last);

        // Percentiles are taken over frame times, not over FPS: a "1% low" is the slowest one
        // percent of frames, and averaging reciprocals would be wrong. These stay over the
        // whole buffer and over individual intervals — a stutter is exactly the outlier the
        // windowed rate is built to hide, and here it is the thing being looked for.
        frametimes_ms.sort_unstable_by(f32::total_cmp);
        let low_1pct = percentile_fps(&frametimes_ms, 0.99, MIN_FRAMES_FOR_1PCT);
        let low_01pct = percentile_fps(&frametimes_ms, 0.999, MIN_FRAMES_FOR_01PCT);

        Some(FrameMetrics {
            fps,
            frametime_ms,
            avg_fps: avg_fps as f32,
            low_1pct,
            low_01pct,
            sample_count: self.times.len(),
        })
    }

    /// Frames per second and mean frame time over the last [`RATE_WINDOW_NS`].
    ///
    /// Falls back to the newest interval when the window holds too little to count — right
    /// after a game launches, or once a second has passed with barely any frames in it. In
    /// both cases one interval is genuinely all there is to report.
    fn recent_rate(&self, last: u64) -> (f32, f32) {
        let cutoff = last.saturating_sub(RATE_WINDOW_NS);
        // Timestamps are strictly increasing, since `push` drops anything that is not.
        let start = self.times.partition_point(|&t| t < cutoff);
        let intervals = self.times.len().saturating_sub(start.max(1));

        if intervals >= 2 {
            let first = self.times[start.max(1) - 1];
            let span = last.saturating_sub(first);
            if span > 0 {
                let fps = intervals as f64 * 1e9 / span as f64;
                let frametime = span as f64 / intervals as f64 / 1e6;
                return (fps as f32, frametime as f32);
            }
        }

        let a = self.times[self.times.len() - 2];
        let frametime = (last - a) as f32 / 1e6;
        (1000.0 / frametime, frametime)
    }
}

/// Takes the `p`th percentile of ascending-sorted frame times and converts it to FPS.
///
/// `min_samples` guards against meaningless figures: a "0.1% low" over three frames is just
/// the worst frame, and presenting it under that name would be dishonest.
fn percentile_fps(sorted_frametimes_ms: &[f32], p: f32, min_samples: usize) -> Option<f32> {
    if sorted_frametimes_ms.len() < min_samples {
        return None;
    }
    let idx = ((sorted_frametimes_ms.len() - 1) as f32 * p).floor() as usize;
    let ft = sorted_frametimes_ms[idx];
    (ft > 0.0).then(|| 1000.0 / ft)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: u64 = 1_000_000;

    /// A steady stream of frames at a fixed interval.
    fn steady(count: usize, interval_ms: u64) -> FrameTimeline {
        let mut t = FrameTimeline::default();
        for i in 0..count {
            t.push(i as u64 * interval_ms * MS);
        }
        t
    }

    /// A display-locked stream: every frame is shown on time, but the timestamp of the present
    /// *call* wanders either side of the interval.
    ///
    /// This is not a contrived input. A present timestamp records when the game asked for the
    /// frame, not when the display showed it, and the gap between those is what varies.
    fn jittery(count: usize, interval_ns: u64, jitter_ns: u64) -> FrameTimeline {
        let mut t = FrameTimeline::default();
        // Deterministic wobble, so the test cannot pass or fail depending on the day.
        let pattern = [0.0f64, 0.8, -0.9, 0.35, -0.5, 0.95, -0.3, 0.1];
        for i in 0..count {
            let base = i as u64 * interval_ns;
            let offset = (pattern[i % pattern.len()] * jitter_ns as f64) as i64;
            t.push((base as i64 + offset).max(0) as u64);
        }
        t
    }

    #[test]
    fn a_vsynced_game_reports_its_refresh_rate_and_not_the_jitter() {
        // 144 Hz with nearly a millisecond of wobble either way. Taken from the newest
        // interval this reads anywhere from 130 to 166 and never settles; counted across a
        // window it is simply 144.
        const HZ_144_NS: u64 = 6_944_444;
        let timeline = jittery(600, HZ_144_NS, 900_000);
        let last = 599 * HZ_144_NS;
        let m = timeline.metrics(last + MS).expect("frames were recorded");

        assert!(
            (m.fps - 144.0).abs() < 2.0,
            "a locked 144 Hz game reported {} fps",
            m.fps
        );
        assert!(
            m.fps < 150.0,
            "the reading overshot the refresh rate: {}",
            m.fps
        );
        // And the two halves of the same measurement must agree on screen.
        assert!(
            (1000.0 / m.frametime_ms - m.fps).abs() < 0.5,
            "{} fps against {} ms",
            m.fps,
            m.frametime_ms
        );
    }

    #[test]
    fn a_single_slow_frame_does_not_throw_the_headline_rate() {
        // What the percentiles do with a lone stutter is settled elsewhere; this is only about
        // the big number staying readable through one.
        let mut t = steady(1200, 7);
        let last = 1199 * 7 * MS + 60 * MS;
        t.push(last);

        let m = t.metrics(last + MS).unwrap();
        // The rate does dip, and should: sixty milliseconds of nothing inside a half-second
        // window is a real loss of frames, and hiding it would be the wrong kind of smoothing.
        // What must not happen is the headline collapsing to the stutter's own 16 fps, which
        // is exactly what inverting the newest interval used to do.
        assert!(
            (110.0..145.0).contains(&m.fps),
            "one slow frame threw the rate to {}",
            m.fps
        );
        // Inverting the newest interval, as this used to, would have read 16 fps here.
        assert!(
            m.fps > 100.0,
            "the headline collapsed to the stutter itself: {}",
            m.fps
        );
    }

    #[test]
    fn a_game_that_has_only_just_started_still_reports_something() {
        // Two frames is not a window, but it is a measurement, and a blank frame rate for the
        // first half second would read as the overlay being broken.
        let mut t = FrameTimeline::default();
        t.push(0);
        t.push(10 * MS);
        let m = t.metrics(11 * MS).expect("two frames are enough");
        assert!((m.fps - 100.0).abs() < 1.0, "{}", m.fps);
    }

    #[test]
    fn empty_and_single_frame_yield_nothing() {
        let empty = FrameTimeline::default();
        assert!(empty.metrics(0).is_none());

        let mut one = FrameTimeline::default();
        one.push(1000 * MS);
        assert!(
            one.metrics(1000 * MS).is_none(),
            "a single frame has no frame time"
        );
    }

    #[test]
    fn steady_stream_reports_matching_fps() {
        // 200 frames at 10 ms is exactly 100 FPS.
        let t = steady(200, 10);
        let m = t.metrics(199 * 10 * MS).unwrap();

        assert!((m.fps - 100.0).abs() < 0.01);
        assert!((m.avg_fps - 100.0).abs() < 0.01);
        assert!((m.frametime_ms - 10.0).abs() < 0.001);
        assert_eq!(m.sample_count, 200);
    }

    #[test]
    fn lows_require_enough_samples_to_mean_anything() {
        let few = steady(50, 10).metrics(49 * 10 * MS).unwrap();
        assert!(
            few.low_1pct.is_none(),
            "a 1% low over 50 frames is just the worst frame"
        );
        assert!(few.low_01pct.is_none());

        let some = steady(200, 10).metrics(199 * 10 * MS).unwrap();
        assert!(some.low_1pct.is_some());
        assert!(
            some.low_01pct.is_none(),
            "a 0.1% low needs at least 1000 frames"
        );

        let many = steady(1500, 10).metrics(1499 * 10 * MS).unwrap();
        assert!(many.low_01pct.is_some());
    }

    /// `count` frames of 10 ms, where every `stutter_every`th one takes 100 ms instead.
    fn with_stutters(count: usize, stutter_every: usize) -> (FrameTimeline, u64) {
        let mut t = FrameTimeline::with_capacity(count + 1);
        let mut now = 0u64;
        for i in 0..count {
            now += if i > 0 && i % stutter_every == 0 {
                100 * MS
            } else {
                10 * MS
            };
            t.push(now);
        }
        (t, now)
    }

    #[test]
    fn stutters_drag_down_the_1pct_low_but_barely_touch_the_average() {
        // 2% of frames stutter, comfortably above the 1% threshold.
        let (t, now) = with_stutters(1000, 50);
        let m = t.metrics(now).unwrap();

        assert!(m.avg_fps > 70.0, "the mean barely suffers: {}", m.avg_fps);
        let low = m.low_1pct.unwrap();
        assert!(
            low < 20.0,
            "the 1% low must expose the 100 ms frames, got {low}"
        );
    }

    /// This is the definition of the metric, not a bug, and it is worth pinning down.
    ///
    /// A "1% low" is the 99th percentile frame time. One stutter in two hundred frames is half
    /// a percent of the sample, below the threshold, so the percentile does not move. Making
    /// isolated events like that visible is the job of a frame time graph, which is planned
    /// separately.
    #[test]
    fn a_lone_stutter_below_the_one_percent_threshold_does_not_move_the_1pct_low() {
        let mut t = FrameTimeline::default();
        let mut now = 0u64;
        for i in 0..200 {
            now += if i == 100 { 100 * MS } else { 10 * MS };
            t.push(now);
        }
        let m = t.metrics(now).unwrap();

        let low = m.low_1pct.unwrap();
        assert!(
            (low - 100.0).abs() < 1.0,
            "99% of frames still fit in 10 ms: {low}"
        );
    }

    #[test]
    fn out_of_order_timestamps_are_dropped_not_treated_as_negative_frametime() {
        let mut t = FrameTimeline::default();
        t.push(100 * MS);
        t.push(110 * MS);
        t.push(105 * MS); // arrived late, ignore it
        t.push(120 * MS);

        let m = t.metrics(120 * MS).unwrap();
        assert_eq!(m.sample_count, 3);
        assert!(m.frametime_ms > 0.0);
    }

    #[test]
    fn duplicate_timestamps_are_dropped() {
        let mut t = FrameTimeline::default();
        t.push(100 * MS);
        t.push(100 * MS);
        assert!(
            t.metrics(100 * MS).is_none(),
            "two identical timestamps describe one frame"
        );
    }

    #[test]
    fn stale_frames_report_nothing_rather_than_a_frozen_number() {
        let t = steady(200, 10);
        let last = 199 * 10 * MS;
        assert!(
            t.metrics(last + 500 * MS).is_some(),
            "half a second is still live"
        );
        assert!(
            t.metrics(last + 3_000 * MS).is_none(),
            "after three silent seconds the old FPS must not be shown"
        );
    }

    #[test]
    fn ring_buffer_evicts_oldest_and_keeps_reporting() {
        let mut t = FrameTimeline::with_capacity(8);
        for i in 0..100 {
            t.push(i as u64 * 10 * MS);
        }
        let m = t.metrics(99 * 10 * MS).unwrap();
        assert_eq!(m.sample_count, 8);
        assert!((m.avg_fps - 100.0).abs() < 0.01);
    }

    #[test]
    fn clear_forgets_everything() {
        let mut t = steady(200, 10);
        t.clear();
        assert!(t.is_empty());
        assert!(t.metrics(0).is_none());
    }
}
