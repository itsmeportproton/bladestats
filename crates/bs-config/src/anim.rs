//! The opening sequence: a small rounded rectangle appears, holds, then expands outward to
//! the whole window.
//!
//! Kept as pure arithmetic over elapsed time so it can be tested. The drawing code asks it
//! what fraction of the way open the window is and how visible the contents should be; it
//! knows nothing about egui.

use std::time::{Duration, Instant};

/// The rectangle the window grows from, in points.
pub const SEED_SIZE: [f32; 2] = [96.0, 118.0];

const SEED_IN: Duration = Duration::from_millis(280);
/// The pause before expanding. Deliberate: the small rectangle has to register as a thing in
/// its own right before it flies apart, and without it the whole move reads as one blur.
const HOLD_UNTIL: Duration = Duration::from_millis(620);
const EXPAND_UNTIL: Duration = Duration::from_millis(1260);
const CONTENT_FROM: Duration = Duration::from_millis(1180);
const CONTENT_UNTIL: Duration = Duration::from_millis(1500);

#[derive(Debug, Clone)]
pub struct Opening {
    started: Instant,
    /// When the system asks for less motion the window still arrives, just without the
    /// journey. Removing the animation outright leaves it appearing from nowhere, which reads
    /// as a glitch rather than as a considered absence.
    reduced: bool,
}

impl Opening {
    pub fn new(reduced: bool) -> Self {
        Self {
            started: Instant::now(),
            reduced,
        }
    }

    /// Plays the sequence again from the start.
    pub fn restart(&mut self) {
        self.started = Instant::now();
    }

    fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    /// How far the window has opened, 0 at the seed rectangle and 1 at full size.
    pub fn expansion(&self) -> f32 {
        if self.reduced {
            return 1.0;
        }
        let t = self.elapsed();
        if t < HOLD_UNTIL {
            return 0.0;
        }
        let span = (EXPAND_UNTIL - HOLD_UNTIL).as_secs_f32();
        ease_out((t - HOLD_UNTIL).as_secs_f32() / span)
    }

    /// Opacity of the seed rectangle itself as it fades in.
    pub fn seed_opacity(&self) -> f32 {
        if self.reduced {
            return 1.0;
        }
        ease_out(self.elapsed().as_secs_f32() / SEED_IN.as_secs_f32())
    }

    /// Opacity of the window contents, which arrive once the frame has settled.
    pub fn content_opacity(&self) -> f32 {
        if self.reduced {
            // A short fade rather than an instant appearance, so it still arrives.
            return ease_out(self.elapsed().as_secs_f32() / 0.26);
        }
        let t = self.elapsed();
        if t < CONTENT_FROM {
            return 0.0;
        }
        let span = (CONTENT_UNTIL - CONTENT_FROM).as_secs_f32();
        ((t - CONTENT_FROM).as_secs_f32() / span).clamp(0.0, 1.0)
    }

    /// Whether the sequence has finished, so the caller can stop asking for repaints.
    pub fn finished(&self) -> bool {
        self.elapsed()
            >= if self.reduced {
                Duration::from_millis(260)
            } else {
                CONTENT_UNTIL
            }
    }

    /// The window size at this moment, growing from the seed to `full`.
    pub fn size(&self, full: [f32; 2]) -> [f32; 2] {
        let t = self.expansion();
        [
            SEED_SIZE[0] + (full[0] - SEED_SIZE[0]) * t,
            SEED_SIZE[1] + (full[1] - SEED_SIZE[1]) * t,
        ]
    }
}

/// Decelerating ease. Fast departure, soft arrival: the window should look like it was let go
/// rather than driven.
fn ease_out(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An `Opening` that started a given time ago.
    fn aged(ms: u64, reduced: bool) -> Opening {
        let mut opening = Opening::new(reduced);
        opening.started = Instant::now() - Duration::from_millis(ms);
        opening
    }

    #[test]
    fn the_window_holds_at_the_seed_before_expanding() {
        // The whole point of the pause: at 400 ms the small rectangle is fully visible and has
        // not started growing.
        let held = aged(400, false);
        assert_eq!(held.expansion(), 0.0);
        assert!(
            held.seed_opacity() > 0.99,
            "the seed has finished fading in by then"
        );
        assert_eq!(held.size([580.0, 700.0]), SEED_SIZE);
    }

    #[test]
    fn expansion_runs_from_the_seed_to_the_full_size() {
        assert_eq!(aged(0, false).expansion(), 0.0);
        assert!(aged(900, false).expansion() > 0.0);
        assert!(aged(900, false).expansion() < 1.0);
        assert_eq!(aged(2000, false).expansion(), 1.0);
    }

    #[test]
    fn size_interpolates_between_the_seed_and_the_full_window() {
        let full = [580.0, 700.0];
        let mid = aged(900, false).size(full);

        assert!(mid[0] > SEED_SIZE[0] && mid[0] < full[0]);
        assert!(mid[1] > SEED_SIZE[1] && mid[1] < full[1]);
        assert_eq!(aged(2000, false).size(full), full);
    }

    #[test]
    fn contents_wait_until_the_frame_has_settled() {
        // Fading the contents in during the growth would smear them across a moving frame.
        assert_eq!(aged(600, false).content_opacity(), 0.0);
        assert_eq!(aged(1000, false).content_opacity(), 0.0);
        assert!(aged(1300, false).content_opacity() > 0.0);
        assert_eq!(aged(1600, false).content_opacity(), 1.0);
    }

    #[test]
    fn reduced_motion_still_arrives_rather_than_appearing_from_nowhere() {
        let reduced = aged(0, true);
        assert_eq!(reduced.expansion(), 1.0, "no growth");
        assert!(reduced.content_opacity() < 1.0, "but it does fade");
        assert!(aged(300, true).content_opacity() >= 1.0);
    }

    #[test]
    fn finishing_lets_the_caller_stop_repainting() {
        assert!(!aged(500, false).finished());
        assert!(aged(1600, false).finished());
        // Reduced motion is done far sooner, and holding repaints open would waste power for
        // an animation nobody asked to see.
        assert!(!aged(100, true).finished());
        assert!(aged(300, true).finished());
    }

    #[test]
    fn restarting_plays_it_again() {
        let mut opening = aged(2000, false);
        assert!(opening.finished());
        opening.restart();
        assert!(!opening.finished());
        assert_eq!(opening.expansion(), 0.0);
    }

    #[test]
    fn easing_is_bounded_and_decelerating() {
        assert_eq!(ease_out(0.0), 0.0);
        assert_eq!(ease_out(1.0), 1.0);
        assert_eq!(ease_out(-5.0), 0.0, "clamped, not extrapolated");
        assert_eq!(ease_out(5.0), 1.0);
        // More distance covered in the first half than the second.
        assert!(ease_out(0.5) > 0.5);
    }
}
