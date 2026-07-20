//! Motion primitives.
//!
//! The overlay samples hardware twice a second and draws sixty times a second. Everything in
//! this file exists to bridge that gap: without it the panel would be a slideshow of five
//! hundred millisecond steps, and with a naive interpolation it would lag behind reality by
//! however long the interpolation takes.
//!
//! All three are framerate-independent, which is not a nicety. The overlay's frame rate follows
//! whatever display it is on and drops whenever the game is busy, so anything that advanced by
//! a fixed step per frame would move at a different speed on a 60 Hz and a 240 Hz monitor, and
//! change speed whenever the game stuttered.

use std::time::Duration;

/// The longest step any of these will take in one go.
///
/// A window that was occluded, a machine that went to sleep, a game that hitched for a second:
/// the elapsed time can be arbitrarily large, and integrating that in one step would fling a
/// spring across the screen. Past this point motion is simply skipped — the value arrives
/// where it was going, which is what the user would see anyway.
pub const MAX_STEP: Duration = Duration::from_millis(100);

/// A value that eases towards a target.
///
/// Exponential rather than a spring, deliberately: this is used for readings, and a reading
/// that overshoots is a reading that displays a number the sensor never reported. A processor
/// at 100% must never be drawn as 103%.
#[derive(Debug, Clone, Copy)]
pub struct Smoothed {
    value: f32,
    target: f32,
    /// Time constant: after this long, roughly 63% of the distance is covered.
    tau: f32,
}

impl Smoothed {
    pub fn new(value: f32, tau: Duration) -> Self {
        Self {
            value,
            target: value,
            tau: tau.as_secs_f32().max(1e-4),
        }
    }

    pub fn set_target(&mut self, target: f32) {
        self.target = target;
    }

    /// Puts the value at the target immediately, with no motion.
    pub fn jump_to(&mut self, target: f32) {
        self.value = target;
        self.target = target;
    }

    pub fn value(&self) -> f32 {
        self.value
    }

    pub fn target(&self) -> f32 {
        self.target
    }

    /// Advances by `dt`. Returns whether it is still moving.
    ///
    /// The exponential form is exact rather than an approximation of one, so stepping once by
    /// 32 ms and stepping twice by 16 ms land in the same place. A naive `value += (target -
    /// value) * rate` does not have that property, and produces visibly different speeds on
    /// different monitors.
    pub fn step(&mut self, dt: Duration) -> bool {
        if self.settled() {
            self.value = self.target;
            return false;
        }
        let dt = dt.min(MAX_STEP).as_secs_f32();
        self.value += (self.target - self.value) * (1.0 - (-dt / self.tau).exp());
        !self.settled()
    }

    /// Close enough that another step would not change a pixel.
    ///
    /// The threshold is relative to the target because these hold everything from a percentage
    /// to a byte count, and one absolute epsilon cannot serve both. A thousandth is the point
    /// where the last displayed digit has certainly stopped moving — chasing further would
    /// keep the overlay redrawing for another half second with nothing to show for it.
    pub fn settled(&self) -> bool {
        let scale = self.target.abs().max(1.0);
        (self.target - self.value).abs() <= scale * 1e-3
    }
}

/// A critically damped spring, for geometry.
///
/// Used where [`Smoothed`] is not, and for the opposite reason: the panel's size should feel
/// like it has weight, but must not overshoot, because a panel that overshoots needs a
/// swapchain larger than its final size for a few frames.
#[derive(Debug, Clone, Copy)]
pub struct Spring {
    value: f32,
    velocity: f32,
    target: f32,
    /// Angular frequency. Critically damped, so this alone sets the response.
    omega: f32,
}

/// Fixed integration step. Small enough that the spring is stable at any stiffness the settings
/// allow, and cheap enough that the substepping never shows up in a profile.
const SUBSTEP: f32 = 0.004;

/// How close, in pixels, counts as arrived. The idle path in the render loop depends on springs
/// actually stopping rather than creeping forever.
const SETTLE_PX: f32 = 0.25;

impl Spring {
    /// `duration` is roughly how long it takes to arrive.
    pub fn new(value: f32, duration: Duration) -> Self {
        Self {
            value,
            velocity: 0.0,
            target: value,
            omega: 5.0 / duration.as_secs_f32().max(1e-3),
        }
    }

    pub fn set_target(&mut self, target: f32) {
        self.target = target;
    }

    pub fn jump_to(&mut self, target: f32) {
        self.value = target;
        self.target = target;
        self.velocity = 0.0;
    }

    pub fn value(&self) -> f32 {
        self.value
    }

    pub fn target(&self) -> f32 {
        self.target
    }

    pub fn settled(&self) -> bool {
        (self.target - self.value).abs() <= SETTLE_PX && self.velocity.abs() <= SETTLE_PX
    }

    pub fn step(&mut self, dt: Duration) -> bool {
        if self.settled() {
            self.value = self.target;
            self.velocity = 0.0;
            return false;
        }

        let mut remaining = dt.min(MAX_STEP).as_secs_f32();
        while remaining > 0.0 {
            let h = remaining.min(SUBSTEP);
            // Critical damping: the damping coefficient is exactly 2ω, which is the boundary
            // between overshooting and crawling.
            let accel =
                self.omega * self.omega * (self.target - self.value) - 2.0 * self.omega * self.velocity;
            self.velocity += accel * h;
            self.value += self.velocity * h;
            remaining -= h;
        }

        if self.settled() {
            self.value = self.target;
            self.velocity = 0.0;
            return false;
        }
        true
    }
}

/// A one-shot 0..1 ramp, for transitions that have a beginning and an end.
#[derive(Debug, Clone, Copy)]
pub struct Tween {
    t: f32,
    duration: f32,
}

impl Tween {
    pub fn new(duration: Duration) -> Self {
        Self {
            // Starts finished: nothing is in transition until something changes.
            t: 1.0,
            duration: duration.as_secs_f32().max(1e-4),
        }
    }

    pub fn restart(&mut self) {
        self.t = 0.0;
    }

    pub fn finish(&mut self) {
        self.t = 1.0;
    }

    pub fn running(&self) -> bool {
        self.t < 1.0
    }

    pub fn step(&mut self, dt: Duration) -> bool {
        if self.t >= 1.0 {
            return false;
        }
        self.t = (self.t + dt.min(MAX_STEP).as_secs_f32() / self.duration).min(1.0);
        self.t < 1.0
    }

    /// Eased progress, 0..=1. Decelerating, so a thing arriving settles rather than stops dead.
    pub fn eased(&self) -> f32 {
        let inv = 1.0 - self.t;
        1.0 - inv * inv * inv
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    /// Runs exactly `total` of simulated time in steps of at most `step`.
    ///
    /// The final step is shortened rather than overshooting. Getting this wrong silently makes
    /// two frame rates cover different amounts of time, which then looks exactly like the
    /// framerate dependence these tests exist to rule out.
    fn run(f: &mut impl FnMut(Duration) -> bool, step: Duration, total: Duration) {
        let mut remaining = total;
        while !remaining.is_zero() {
            let dt = step.min(remaining);
            f(dt);
            remaining -= dt;
        }
    }

    #[test]
    fn a_smoothed_value_arrives_and_then_stops_reporting_motion() {
        let mut s = Smoothed::new(0.0, ms(120));
        s.set_target(100.0);
        assert!(s.step(ms(16)), "it has somewhere to be");

        run(&mut |dt| s.step(dt), ms(16), ms(2000));
        assert!((s.value() - 100.0).abs() < 0.1, "{}", s.value());
        assert!(!s.step(ms(16)), "an arrived value must stop asking to be redrawn");
    }

    #[test]
    fn the_same_elapsed_time_lands_in_the_same_place_at_any_frame_rate() {
        // This is the whole reason for the exponential form. A 240 Hz monitor and a 60 Hz one
        // must show the same motion, and a stuttering game must not animate in slow motion.
        let mut fast = Smoothed::new(0.0, ms(120));
        let mut slow = Smoothed::new(0.0, ms(120));
        fast.set_target(100.0);
        slow.set_target(100.0);

        run(&mut |dt| fast.step(dt), Duration::from_micros(4166), ms(200));
        run(&mut |dt| slow.step(dt), ms(33), ms(200));

        assert!(
            (fast.value() - slow.value()).abs() < 0.5,
            "240 Hz reached {} where 30 Hz reached {}",
            fast.value(),
            slow.value()
        );
    }

    #[test]
    fn a_reading_never_overshoots_what_the_sensor_said() {
        // A processor at 100% drawn as 103% would be the overlay inventing a reading.
        let mut s = Smoothed::new(0.0, ms(120));
        s.set_target(100.0);
        for _ in 0..200 {
            s.step(ms(16));
            assert!(s.value() <= 100.0 + 1e-3, "overshot to {}", s.value());
        }
    }

    #[test]
    fn a_huge_gap_in_time_does_not_fling_anything() {
        // Alt-tabbing away for a minute and back.
        let mut s = Smoothed::new(0.0, ms(120));
        s.set_target(50.0);
        s.step(Duration::from_secs(60));
        assert!((0.0..=50.0).contains(&s.value()), "{}", s.value());

        let mut spring = Spring::new(0.0, ms(200));
        spring.set_target(400.0);
        spring.step(Duration::from_secs(60));
        assert!((0.0..=400.0).contains(&spring.value()), "{}", spring.value());
    }

    #[test]
    fn a_spring_arrives_without_overshooting() {
        let mut s = Spring::new(100.0, ms(200));
        s.set_target(400.0);

        let mut peak = 0.0f32;
        run(
            &mut |dt| {
                let moving = s.step(dt);
                peak = peak.max(s.value());
                moving
            },
            ms(8),
            ms(600),
        );

        // Critically damped: it must not sail past the target and come back, because a panel
        // that does needs a swapchain bigger than its final size.
        assert!(peak <= 400.0 + SETTLE_PX, "overshot to {peak}");
        assert_eq!(s.value(), 400.0, "and it must actually arrive");
        assert!(!s.settled() || s.value() == s.target());
    }

    #[test]
    fn a_spring_stops_rather_than_creeping_forever() {
        // The idle path in the render loop stops presenting when nothing is moving, so a
        // spring that never quite settles would keep the overlay awake indefinitely.
        let mut s = Spring::new(0.0, ms(200));
        s.set_target(300.0);
        let mut frames = 0;
        while s.step(ms(16)) {
            frames += 1;
            assert!(frames < 200, "the spring never settled");
        }
        assert_eq!(s.value(), 300.0);
    }

    #[test]
    fn jumping_skips_the_motion_entirely() {
        // What `animation.enabled = false` and the first sample after startup both need: the
        // panel should appear correct, not fly in from zero.
        let mut s = Smoothed::new(0.0, ms(120));
        s.jump_to(42.0);
        assert_eq!(s.value(), 42.0);
        assert!(!s.step(ms(16)));

        let mut spring = Spring::new(0.0, ms(200));
        spring.jump_to(300.0);
        assert_eq!(spring.value(), 300.0);
        assert!(!spring.step(ms(16)));
    }

    #[test]
    fn a_tween_starts_finished_so_nothing_animates_until_something_changes() {
        let mut t = Tween::new(ms(150));
        assert!(!t.running());
        assert_eq!(t.eased(), 1.0);

        t.restart();
        assert!(t.running());
        assert_eq!(t.eased(), 0.0);
    }

    #[test]
    fn a_tween_runs_once_and_decelerates() {
        let mut t = Tween::new(ms(150));
        t.restart();

        t.step(ms(75));
        let half = t.eased();
        assert!(
            half > 0.5,
            "an ease-out covers most of its distance early: {half}"
        );

        run(&mut |dt| t.step(dt), ms(16), ms(200));
        assert_eq!(t.eased(), 1.0);
        assert!(!t.running());
    }
}
