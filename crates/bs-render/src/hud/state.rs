//! What the panel carries between frames.
//!
//! The overlay samples hardware twice a second and draws at the display's rate. This holds the
//! difference: a smoothed copy of every number, a spring for the panel's size, and enough
//! memory of the last frame to know when a reading grew a digit.
//!
//! The central choice is what gets smoothed. Not the strings — smoothing text is not a
//! meaningful operation — and not the finished geometry either, which would blur the type.
//! What is smoothed is the **snapshot itself**: an ordinary [`MetricsSnapshot`] whose numbers
//! are eased copies of the real ones. The model is then rebuilt from it every frame by the
//! same code that builds it from a real sample, and reformatting an easing number is what
//! makes the digits walk — 142, 141, 140 — with no machinery for it anywhere.

use std::collections::HashMap;
use std::time::Duration;

use bs_core::{Config, CoreMetrics, MetricsSnapshot, Power};

use crate::atlas::Atlas;
use crate::draw::DrawList;

use super::anim::{Smoothed, Spring, Tween};
use super::model::HudModel;
use super::paint::{self, HudSize, HudStyle};

/// How long a reading takes to reach a new value. Comfortably shorter than the half second
/// between samples, so a value has settled before the next one arrives and the panel never
/// reads as lagging behind the machine.
pub const DEFAULT_VALUE_TAU: Duration = Duration::from_millis(120);

/// How long the panel takes to change size.
pub const DEFAULT_RESIZE: Duration = Duration::from_millis(200);

/// How long a newly appeared digit takes to fade in.
const DIGIT_FADE: Duration = Duration::from_millis(150);

/// How long the panel takes to unroll when a game starts, or roll away when one stops.
pub const DEFAULT_REVEAL: Duration = Duration::from_millis(320);

/// How much of the panel is left at the far end of rolling away.
///
/// Not zero: a box collapsing to a line and vanishing looks like a rendering fault, whereas one
/// that shrinks to a sliver and then goes reads as deliberate.
const COLLAPSED: f32 = 0.0;

/// Tuning, so the settings file can reach it without this module knowing about settings.
#[derive(Debug, Clone)]
pub struct Motion {
    pub value_tau: Duration,
    pub resize: Duration,
    pub reveal: Duration,
    /// When off, everything arrives instantly. The accessibility path, and the honest answer
    /// for anyone who simply does not want the panel moving.
    pub enabled: bool,
}

impl Default for Motion {
    fn default() -> Self {
        Self {
            value_tau: DEFAULT_VALUE_TAU,
            resize: DEFAULT_RESIZE,
            reveal: DEFAULT_REVEAL,
            enabled: true,
        }
    }
}

/// One reading's memory across frames.
#[derive(Debug)]
struct CellMemo {
    /// The widest this reading has been. Only ever grows within a session: a frame rate that
    /// touches four digits once should not make the panel flinch every time it drops back.
    reserve: usize,
    last_len: usize,
    fade: Tween,
}

#[derive(Debug, Default)]
struct Smoothing {
    cpu_load: Option<Smoothed>,
    cpu_clock: Option<Smoothed>,
    cpu_temp: Option<Smoothed>,
    cpu_power: Option<Smoothed>,
    cores: Vec<Smoothed>,
    gpu_load: Option<Smoothed>,
    gpu_clock: Option<Smoothed>,
    gpu_temp: Option<Smoothed>,
    gpu_hotspot: Option<Smoothed>,
    gpu_fan: Option<Smoothed>,
    gpu_power: Option<Smoothed>,
    vram_used: Option<Smoothed>,
    ram_used: Option<Smoothed>,
    ram_rate: Option<Smoothed>,
    fps: Option<Smoothed>,
    frametime: Option<Smoothed>,
    low_1pct: Option<Smoothed>,
    low_01pct: Option<Smoothed>,
}

pub struct HudState {
    motion: Motion,
    style: HudStyle,
    /// The last real sample, kept whole. Everything not smoothed — names, module lists,
    /// whether a sensor exists at all — is read straight from it.
    sample: MetricsSnapshot,
    config: Config,
    smooth: Smoothing,
    width: Spring,
    height: Spring,
    /// How far the panel is unrolled, 0 to 1.
    reveal: Spring,
    memo: HashMap<&'static str, CellMemo>,
    list: DrawList,
    /// False until the first sample, so readings appear at their values instead of easing in
    /// from zero.
    seeded: bool,
    /// False until the first paint. Separate from `seeded` because the size is not known at
    /// sampling time — it takes a font to measure — and without this the panel would unfold
    /// from a point on the frame after its first sample arrived.
    sized: bool,
}

impl HudState {
    pub fn new(config: Config, style: HudStyle, motion: Motion) -> Self {
        let reveal = Spring::new(COLLAPSED, motion.reveal);
        let style = HudStyle {
            horizontal: config.placement.orientation == bs_core::Orientation::Horizontal,
            ..style
        };
        Self {
            motion,
            style,
            sample: MetricsSnapshot::default(),
            config,
            smooth: Smoothing::default(),
            width: Spring::new(0.0, DEFAULT_RESIZE),
            height: Spring::new(0.0, DEFAULT_RESIZE),
            // Starts rolled away. The panel arrives by unrolling rather than by appearing,
            // even on the very first game of a session.
            reveal,
            memo: HashMap::new(),
            list: DrawList::new(),
            seeded: false,
            sized: false,
        }
    }

    pub fn set_config(&mut self, config: Config) {
        self.style.horizontal =
            config.placement.orientation == bs_core::Orientation::Horizontal;
        self.config = config;
    }

    pub fn set_motion(&mut self, motion: Motion) {
        self.motion = motion;
    }

    /// Asks the panel to unroll or to roll away.
    ///
    /// Separate from the window being shown or hidden, and deliberately so: the window has to
    /// stay up for the whole of the rolling-away, or there would be nothing on screen to
    /// animate. [`HudState::is_hidden`] says when it is finally safe to take it down.
    pub fn set_revealed(&mut self, revealed: bool) {
        let target = if revealed { 1.0 } else { COLLAPSED };
        if !self.motion.enabled {
            self.reveal.jump_to(target);
        } else {
            self.reveal.set_target(target);
        }
    }

    /// Whether the panel has finished rolling away and has nothing left to draw.
    pub fn is_hidden(&self) -> bool {
        self.reveal.target() <= COLLAPSED && self.reveal.settled()
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Takes a fresh reading. Called at the sampling rate, not the frame rate.
    pub fn on_sample(&mut self, snapshot: MetricsSnapshot) {
        let tau = self.motion.value_tau;
        let animate = self.motion.enabled && self.seeded;

        track(&mut self.smooth.cpu_load, snapshot.cpu.load_pct, tau, animate);
        track(&mut self.smooth.cpu_clock, peak_core_mhz(&snapshot), tau, animate);
        track(&mut self.smooth.cpu_temp, snapshot.cpu.temp_c, tau, animate);
        track(
            &mut self.smooth.cpu_power,
            snapshot.cpu.power.map(Power::watts),
            tau,
            animate,
        );
        track(&mut self.smooth.gpu_load, snapshot.gpu.load_pct, tau, animate);
        track(
            &mut self.smooth.gpu_clock,
            snapshot.gpu.core_clock_mhz,
            tau,
            animate,
        );
        track(&mut self.smooth.gpu_temp, snapshot.gpu.temp_c, tau, animate);
        track(
            &mut self.smooth.gpu_hotspot,
            snapshot.gpu.hotspot_c,
            tau,
            animate,
        );
        track(&mut self.smooth.gpu_fan, snapshot.gpu.fan_rpm, tau, animate);
        track(
            &mut self.smooth.gpu_power,
            snapshot.gpu.power.map(Power::watts),
            tau,
            animate,
        );
        track(
            &mut self.smooth.vram_used,
            snapshot.gpu.vram_used_bytes.map(|b| b as f32),
            tau,
            animate,
        );
        track(
            &mut self.smooth.ram_used,
            snapshot.memory.used_bytes.map(|b| b as f32),
            tau,
            animate,
        );

        track(
            &mut self.smooth.ram_rate,
            snapshot.memory.live_mts,
            tau,
            animate,
        );

        let frames = snapshot.frames.as_ref();
        track(&mut self.smooth.fps, frames.map(|f| f.fps), tau, animate);
        track(
            &mut self.smooth.frametime,
            frames.map(|f| f.frametime_ms),
            tau,
            animate,
        );
        track(
            &mut self.smooth.low_1pct,
            frames.and_then(|f| f.low_1pct),
            tau,
            animate,
        );
        track(
            &mut self.smooth.low_01pct,
            frames.and_then(|f| f.low_01pct),
            tau,
            animate,
        );

        // A core count change means a different processor's worth of bars; there is nothing
        // to carry over, so they start where they are.
        if self.smooth.cores.len() != snapshot.cpu.cores.len() {
            self.smooth.cores = snapshot
                .cpu
                .cores
                .iter()
                .map(|c| Smoothed::new(c.load_pct, tau))
                .collect();
        } else {
            for (s, c) in self.smooth.cores.iter_mut().zip(&snapshot.cpu.cores) {
                s.set_target(c.load_pct);
                if !animate {
                    s.jump_to(c.load_pct);
                }
            }
        }

        self.sample = snapshot;
        self.seeded = true;
    }

    /// Advances every animation. Returns whether anything is still moving.
    ///
    /// The render loop uses the answer to decide whether to present another frame at all, so a
    /// false here has to genuinely mean "nothing will change if you do nothing".
    pub fn step(&mut self, dt: Duration) -> bool {
        let mut moving = false;
        for s in self.smooth.all_mut() {
            moving |= s.step(dt);
        }
        for s in &mut self.smooth.cores {
            moving |= s.step(dt);
        }
        moving |= self.width.step(dt);
        moving |= self.height.step(dt);
        moving |= self.reveal.step(dt);
        for memo in self.memo.values_mut() {
            moving |= memo.fade.step(dt);
        }
        moving
    }

    /// The snapshot as it should be drawn this instant: real where nothing moves, eased where
    /// something does.
    fn displayed(&self) -> MetricsSnapshot {
        let mut s = self.sample.clone();

        s.cpu.load_pct = read(&self.smooth.cpu_load);
        s.cpu.temp_c = read(&self.smooth.cpu_temp);
        s.cpu.power = with_provenance(self.sample.cpu.power, read(&self.smooth.cpu_power));
        s.gpu.load_pct = read(&self.smooth.gpu_load);
        s.gpu.core_clock_mhz = read(&self.smooth.gpu_clock);
        s.gpu.temp_c = read(&self.smooth.gpu_temp);
        s.gpu.hotspot_c = read(&self.smooth.gpu_hotspot);
        s.gpu.fan_rpm = read(&self.smooth.gpu_fan);
        s.gpu.power = with_provenance(self.sample.gpu.power, read(&self.smooth.gpu_power));
        s.gpu.vram_used_bytes = read(&self.smooth.vram_used).map(|v| v.max(0.0) as u64);
        s.memory.used_bytes = read(&self.smooth.ram_used).map(|v| v.max(0.0) as u64);
        s.memory.live_mts = read(&self.smooth.ram_rate);

        if let Some(f) = &mut s.frames {
            if let Some(v) = read(&self.smooth.fps) {
                f.fps = v;
            }
            if let Some(v) = read(&self.smooth.frametime) {
                f.frametime_ms = v;
            }
            f.low_1pct = read(&self.smooth.low_1pct);
            f.low_01pct = read(&self.smooth.low_01pct);
        }

        // The clock shown is the peak across cores, so it is smoothed as one number and put
        // back on every core rather than smoothed sixteen times over.
        let clock = read(&self.smooth.cpu_clock);
        s.cpu.cores = self
            .smooth
            .cores
            .iter()
            .zip(&self.sample.cpu.cores)
            .map(|(sm, real)| CoreMetrics {
                load_pct: sm.value(),
                freq_mhz: real.freq_mhz.and(clock),
            })
            .collect();

        s
    }

    /// Builds this frame's geometry and reports the box it should be drawn in.
    ///
    /// Two sizes come back. The first is the animating one the window is resized to; the second
    /// is the size the panel is heading towards, which the caller needs to place the window: a
    /// strip opens from its middle, and a middle can only be worked out from where the finished
    /// panel will sit, not from how much of it exists this frame.
    pub fn paint(&mut self, atlas: &Atlas) -> (&DrawList, HudSize, HudSize) {
        let mut model = HudModel::new(&self.displayed(), &self.config);
        self.apply_memory(&mut model);

        let settled = paint::measure(&model, atlas, &self.style);
        if !self.sized || !self.motion.enabled {
            self.width.jump_to(settled.width);
            self.height.jump_to(settled.height);
            self.sized = true;
        } else {
            self.width.set_target(settled.width);
            self.height.set_target(settled.height);
        }

        // The window is the full width but only as tall as the panel is unrolled. Everything
        // below that line is still painted and simply never reaches the screen: the swapchain
        // is the window, so the rasteriser does the clipping for free, and rolling the panel
        // open costs nothing beyond the geometry that was going to be drawn anyway.
        // Opened along the axis the panel runs: a stack unrolls downwards, a strip runs out
        // from its middle in both directions, which is what the window's own width does for it.
        let reveal = self.reveal.value().clamp(0.0, 1.0);
        let size = if self.style.horizontal {
            HudSize {
                width: (self.width.value() * reveal).round().max(1.0),
                height: self.height.value().round(),
            }
        } else {
            HudSize {
                width: self.width.value().round(),
                height: (self.height.value() * reveal).round().max(1.0),
            }
        };

        self.list.clear();
        paint::paint(
            &mut self.list,
            &model,
            atlas,
            &self.config.theme,
            &self.style,
            // The contents are laid out for the size the panel is heading towards, not the one
            // it currently has. Laying out for the moving size would reflow the text on every
            // frame of a resize, which looks far worse than a panel briefly wider than its
            // contents.
            settled,
            // ...but the backing plate follows the window, so a half-open panel is a proper
            // rounded box with its contents sliding out of it, rather than a full-height box
            // with its bottom sheared off.
            size,
        );
        (&self.list, size, settled)
    }

    /// Applies remembered widths, and notices readings that just grew a digit.
    fn apply_memory(&mut self, model: &mut HudModel) {
        for block in &mut model.blocks {
            for row in &mut block.rows {
                let super::model::Row::Readout { cells, .. } = row else {
                    continue;
                };
                for cell in cells {
                    let len = cell.value.chars().count();
                    let memo = self.memo.entry(cell.id).or_insert_with(|| CellMemo {
                        reserve: cell.reserve.max(len),
                        last_len: len,
                        fade: Tween::new(DIGIT_FADE),
                    });

                    memo.reserve = memo.reserve.max(cell.reserve).max(len);
                    if len != memo.last_len {
                        if len > memo.last_len && self.motion.enabled && self.seeded {
                            memo.fade.restart();
                        }
                        memo.last_len = len;
                    }

                    cell.reserve = memo.reserve;
                    cell.lead_alpha = memo.fade.eased();
                }
            }
        }
    }
}

/// Follows a reading, without inventing motion where there is none.
///
/// A sensor that disappears takes its value with it immediately rather than easing to zero: a
/// number sliding down to nothing is a claim about the hardware, and the honest answer is the
/// dash. A sensor that appears starts at its real value for the same reason.
fn track(slot: &mut Option<Smoothed>, value: Option<f32>, tau: Duration, animate: bool) {
    match (slot.as_mut(), value) {
        (_, None) => *slot = None,
        (None, Some(v)) => *slot = Some(Smoothed::new(v, tau)),
        (Some(s), Some(v)) => {
            s.set_target(v);
            if !animate {
                s.jump_to(v);
            }
        }
    }
}

fn read(slot: &Option<Smoothed>) -> Option<f32> {
    slot.as_ref().map(Smoothed::value)
}

/// Keeps a wattage's provenance while replacing its number.
///
/// An estimate that eased into a measurement would drop the tilde and quietly promote a model
/// to a sensor reading, which is the one thing the `Power` type exists to prevent.
fn with_provenance(original: Option<Power>, watts: Option<f32>) -> Option<Power> {
    match (original, watts) {
        (Some(Power::Measured(_)), Some(w)) => Some(Power::Measured(w)),
        (Some(Power::Estimated(_)), Some(w)) => Some(Power::Estimated(w)),
        _ => None,
    }
}

fn peak_core_mhz(s: &MetricsSnapshot) -> Option<f32> {
    s.cpu
        .cores
        .iter()
        .filter_map(|c| c.freq_mhz)
        .fold(None, |acc: Option<f32>, f| {
            Some(acc.map_or(f, |a| a.max(f)))
        })
}

impl Smoothing {
    fn all_mut(&mut self) -> impl Iterator<Item = &mut Smoothed> {
        [
            self.cpu_load.as_mut(),
            self.cpu_clock.as_mut(),
            self.cpu_temp.as_mut(),
            self.cpu_power.as_mut(),
            self.gpu_load.as_mut(),
            self.gpu_clock.as_mut(),
            self.gpu_temp.as_mut(),
            self.gpu_hotspot.as_mut(),
            self.gpu_fan.as_mut(),
            self.gpu_power.as_mut(),
            self.vram_used.as_mut(),
            self.ram_used.as_mut(),
            self.ram_rate.as_mut(),
            self.fps.as_mut(),
            self.frametime.as_mut(),
            self.low_1pct.as_mut(),
            self.low_01pct.as_mut(),
        ]
        .into_iter()
        .flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::atlas::GlyphAtlas;
    use bs_core::FrameMetrics;

    fn atlas() -> Option<GlyphAtlas> {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../assets/fonts/JetBrainsMono-Regular.ttf"
        );
        GlyphAtlas::new(&std::fs::read(path).ok()?, 16.0).ok()
    }

    macro_rules! atlas_or_skip {
        () => {
            match atlas() {
                Some(a) => a,
                None => return,
            }
        };
    }

    fn state() -> HudState {
        HudState::new(Config::default(), HudStyle::default(), Motion::default())
    }

    fn with_fps(fps: f32) -> MetricsSnapshot {
        let mut s = MetricsSnapshot::default();
        s.frames = Some(FrameMetrics {
            fps,
            frametime_ms: 1000.0 / fps,
            avg_fps: fps,
            low_1pct: None,
            low_01pct: None,
            sample_count: 500,
        });
        s
    }

    fn drain(state: &mut HudState, atlas: &Atlas, total: Duration) {
        let mut left = total;
        while !left.is_zero() {
            let dt = Duration::from_millis(16).min(left);
            state.step(dt);
            state.paint(atlas);
            left -= dt;
        }
    }

    #[test]
    fn the_first_sample_appears_at_its_value_rather_than_easing_up_from_zero() {
        let atlas = atlas_or_skip!();
        let mut s = state();
        s.on_sample(with_fps(142.0));
        let displayed = s.displayed();
        assert_eq!(displayed.frames.unwrap().fps, 142.0);

        // And the panel is already its full size, not growing into it.
        let (_, size, _) = s.paint(&atlas);
        assert!(size.width > 0.0 && size.height > 0.0);
        assert!(!s.step(Duration::from_millis(16)), "nothing to animate yet");
    }

    #[test]
    fn a_changed_reading_walks_to_its_new_value_instead_of_jumping() {
        let atlas = atlas_or_skip!();
        let mut s = state();
        s.on_sample(with_fps(60.0));
        s.paint(&atlas);

        s.on_sample(with_fps(142.0));
        s.step(Duration::from_millis(16));
        let mid = s.displayed().frames.unwrap().fps;
        assert!(
            mid > 60.0 && mid < 142.0,
            "the value teleported instead of moving: {mid}"
        );

        drain(&mut s, &atlas, Duration::from_millis(1200));
        assert!((s.displayed().frames.unwrap().fps - 142.0).abs() < 0.5);
    }

    #[test]
    fn a_sensor_that_disappears_shows_a_dash_at_once_and_never_slides_to_zero() {
        let mut s = state();
        let mut snapshot = MetricsSnapshot::default();
        snapshot.gpu.temp_c = Some(68.0);
        s.on_sample(snapshot);
        assert_eq!(s.displayed().gpu.temp_c, Some(68.0));

        // A number easing down to nothing would be the overlay claiming the card cooled off.
        s.on_sample(MetricsSnapshot::default());
        assert_eq!(s.displayed().gpu.temp_c, None);
    }

    #[test]
    fn an_estimate_stays_an_estimate_while_it_moves() {
        let mut s = state();
        let mut a = MetricsSnapshot::default();
        a.cpu.power = Some(Power::Estimated(40.0));
        s.on_sample(a);

        let mut b = MetricsSnapshot::default();
        b.cpu.power = Some(Power::Estimated(90.0));
        s.on_sample(b);
        s.step(Duration::from_millis(16));

        let shown = s.displayed().cpu.power.unwrap();
        assert!(shown.is_estimated(), "the tilde must survive the animation");
        assert!(shown.watts() > 40.0 && shown.watts() < 90.0);
    }

    #[test]
    fn the_panel_does_not_resize_when_a_reading_changes_width() {
        let atlas = atlas_or_skip!();
        let mut s = state();
        s.on_sample(with_fps(99.0));
        let (_, narrow, _) = s.paint(&atlas);
        let narrow = narrow.width;

        s.on_sample(with_fps(142.0));
        drain(&mut s, &atlas, Duration::from_millis(1500));
        let (_, wide, _) = s.paint(&atlas);

        // This is the whole point of reserving character cells. Two digits becoming three used
        // to widen the panel, and a frame rate crossing 100 does that several times a second.
        assert_eq!(
            wide.width, narrow,
            "the panel moved because a number grew a digit"
        );
    }

    #[test]
    fn a_reading_that_grew_a_digit_fades_the_new_one_in_and_then_stops() {
        let atlas = atlas_or_skip!();
        let mut s = state();
        s.on_sample(with_fps(99.0));
        s.paint(&atlas);

        s.on_sample(with_fps(142.0));
        // Far enough for the smoothed value to have crossed 100.
        drain(&mut s, &atlas, Duration::from_millis(200));

        let memo = s.memo.get("fps").expect("the frame rate is remembered");
        assert_eq!(memo.last_len, 3);

        drain(&mut s, &atlas, Duration::from_millis(600));
        assert!(
            !s.memo["fps"].fade.running(),
            "the transition must end rather than shimmer forever"
        );
    }

    #[test]
    fn a_reservation_grows_but_never_shrinks_within_a_session() {
        let atlas = atlas_or_skip!();
        let mut s = state();
        s.on_sample(with_fps(1200.0));
        s.paint(&atlas);
        assert_eq!(s.memo["fps"].reserve, 4);

        s.on_sample(with_fps(60.0));
        drain(&mut s, &atlas, Duration::from_millis(1500));
        // Dropping back must not make the panel flinch: a menu that runs at four digits and a
        // level that runs at two would otherwise resize the overlay every time it loaded.
        assert_eq!(s.memo["fps"].reserve, 4);
    }

    #[test]
    fn switching_motion_off_makes_everything_arrive_at_once() {
        let atlas = atlas_or_skip!();
        let mut s = HudState::new(
            Config::default(),
            HudStyle::default(),
            Motion {
                enabled: false,
                ..Motion::default()
            },
        );
        s.on_sample(with_fps(60.0));
        s.paint(&atlas);
        s.on_sample(with_fps(142.0));

        assert_eq!(s.displayed().frames.unwrap().fps, 142.0);
        assert!(
            !s.step(Duration::from_millis(16)),
            "nothing should be moving with motion switched off"
        );
    }

    #[test]
    fn painting_repeatedly_does_not_keep_growing_the_vertex_buffer() {
        let atlas = atlas_or_skip!();
        let mut s = state();
        s.on_sample(with_fps(142.0));
        s.paint(&atlas);

        let settled = s.list.vertices.capacity();
        drain(&mut s, &atlas, Duration::from_millis(2000));
        assert_eq!(
            s.list.vertices.capacity(),
            settled,
            "the draw list is meant to be reused, not reallocated every frame"
        );
    }

    #[test]
    fn the_panel_unrolls_rather_than_appearing_and_rolls_away_before_it_goes() {
        let atlas = atlas_or_skip!();
        let mut s = state();
        s.on_sample(with_fps(142.0));

        // Nothing on screen until it is asked for, and the window is safe to take down.
        assert!(s.is_hidden());
        let (_, closed, _) = s.paint(&atlas);
        assert!(closed.height <= 1.0, "{}", closed.height);

        s.set_revealed(true);
        drain(&mut s, &atlas, Duration::from_millis(80));
        let (_, opening, _) = s.paint(&atlas);
        assert!(
            opening.height > 1.0,
            "it should be on its way open by now: {}",
            opening.height
        );
        assert!(!s.is_hidden());

        drain(&mut s, &atlas, Duration::from_millis(1200));
        let (_, open, _) = s.paint(&atlas);

        // Rolling away has to take time too, or there is nothing to see. The window must stay
        // up throughout, which is what `is_hidden` is asked before taking it down.
        s.set_revealed(false);
        drain(&mut s, &atlas, Duration::from_millis(80));
        let (_, closing, _) = s.paint(&atlas);
        assert!(closing.height < open.height);
        assert!(!s.is_hidden(), "the window is still needed to draw this");

        drain(&mut s, &atlas, Duration::from_millis(1200));
        assert!(s.is_hidden(), "and now it is not");
    }

    #[test]
    fn the_contents_keep_their_place_while_the_panel_rolls() {
        // The box moves; the text does not. Laying the contents out against the animating
        // height would reflow every line on every frame of the animation.
        let atlas = atlas_or_skip!();
        let mut s = state();
        s.on_sample(with_fps(142.0));
        s.set_revealed(true);
        drain(&mut s, &atlas, Duration::from_millis(1500));

        let settled: Vec<[f32; 2]> = s.paint(&atlas).0.vertices.iter().map(|v| v.pos).collect();

        s.set_revealed(false);
        drain(&mut s, &atlas, Duration::from_millis(60));
        let midway: Vec<[f32; 2]> = s.paint(&atlas).0.vertices.iter().map(|v| v.pos).collect();

        // The backing plate is the first two rounded rectangles — fourteen quads, and those do
        // move. Everything after them is content and must be untouched.
        let content = 14 * 4;
        assert_eq!(settled.len(), midway.len());
        assert_eq!(
            settled[content..],
            midway[content..],
            "the contents shifted while the panel was rolling"
        );
    }

    #[test]
    fn a_strip_reports_its_finished_width_while_it_is_still_opening() {
        // The window is placed against this second size, not the first. Without it the strip
        // would be positioned from its current width, which pins the left edge to the corner
        // and sends only the right edge travelling — the bar slides out sideways instead of
        // opening from its middle.
        let atlas = atlas_or_skip!();
        let mut config = Config::default();
        config.placement.orientation = bs_core::Orientation::Horizontal;
        let mut s = HudState::new(config, HudStyle::default(), Motion::default());
        s.on_sample(with_fps(142.0));
        s.set_revealed(true);
        drain(&mut s, &atlas, Duration::from_millis(1500));
        let (_, open, _) = s.paint(&atlas);

        s.set_revealed(false);
        drain(&mut s, &atlas, Duration::from_millis(60));
        let (_, closing, settled) = s.paint(&atlas);
        assert!(closing.width < open.width, "it should be closing by now");
        assert_eq!(settled.width, open.width);
    }

    #[test]
    fn motion_stops_once_everything_has_arrived() {
        // The render loop stops presenting when this returns false, so it has to become false.
        let atlas = atlas_or_skip!();
        let mut s = state();
        s.on_sample(with_fps(60.0));
        s.paint(&atlas);
        s.on_sample(with_fps(142.0));

        let mut frames = 0;
        while s.step(Duration::from_millis(16)) {
            s.paint(&atlas);
            frames += 1;
            assert!(frames < 300, "the panel never came to rest");
        }
    }
}
