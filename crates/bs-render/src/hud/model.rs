//! The snapshot, resolved into what the panel actually shows.
//!
//! Kept apart from the geometry on purpose. This layer decides *what* appears — which blocks
//! exist, which readings are on, what each one reads, what colour it takes — and knows nothing
//! about pixels. The geometry pass then knows nothing about hardware. Every rule worth testing
//! lives here, and it is testable without a font.

use bs_core::{Color, Config, MetricsSnapshot, Theme, Vendor};

use crate::atlas::TextStyle;

use super::text::{self, MISSING};

/// One reading: a number and the unit that follows it, drawn in different colours.
#[derive(Debug, Clone, PartialEq)]
pub struct Cell {
    /// Stable across frames and across settings changes, so the state can remember this
    /// particular reading's width and its transitions without depending on where it happens
    /// to sit in the model this frame.
    pub id: &'static str,
    pub value: String,
    /// Drawn in the label colour, immediately after the value. Usually starts with a space.
    pub unit: String,
    pub color: Color,
    pub style: TextStyle,
    /// Character cells the value is right-aligned within.
    ///
    /// This is what stops the panel breathing. The font is monospaced, so reserving the widest
    /// form a reading can take means 99 becoming 100 fills a slot that was already there
    /// instead of shoving the unit — and everything after it — sideways.
    pub reserve: usize,
    /// Opacity of the leading character, for the frame or two after a reading grows a digit.
    /// One at rest.
    pub lead_alpha: f32,
}

impl Cell {
    fn new(id: &'static str, value: impl Into<String>, unit: &str, color: Color, reserve: usize) -> Self {
        let value = value.into();
        // A dash carries no unit. "—°C" reads as a temperature that failed to render, where a
        // bare dash reads as what it is: no sensor.
        let unit = if value == MISSING { "" } else { unit };
        Self {
            id,
            value,
            unit: unit.into(),
            color,
            style: TextStyle::Readout,
            reserve,
            lead_alpha: 1.0,
        }
    }

    fn big(id: &'static str, value: impl Into<String>, unit: &str, color: Color, reserve: usize) -> Self {
        Self {
            style: TextStyle::Big,
            ..Self::new(id, value, unit, color, reserve)
        }
    }

    fn small(id: &'static str, value: impl Into<String>, unit: &str, color: Color, reserve: usize) -> Self {
        Self {
            style: TextStyle::Small,
            ..Self::new(id, value, unit, color, reserve)
        }
    }

    /// How many character cells the value actually occupies — never less than it needs.
    ///
    /// A reservation is a floor, not a cap: a frame rate that runs past a thousand widens the
    /// panel rather than being clipped or, worse, drawn over the unit beside it.
    pub fn slots(&self) -> usize {
        self.reserve.max(self.value.chars().count())
    }
}

/// One line inside a block.
#[derive(Debug, Clone, PartialEq)]
pub enum Row {
    /// Readings laid left to right. Cells from `right_from` onward are pushed to the right
    /// edge instead, which is how the frame time and the memory rate sit opposite their
    /// headline number.
    Readout {
        cells: Vec<Cell>,
        right_from: Option<usize>,
    },
    /// Per-core load, as a strip of bars.
    Cores(Vec<f32>),
    /// How full something is, 0.0..=1.0.
    Bar(f32),
    /// The small grey sentence under the memory block.
    Spec(String),
}

/// One section of the panel.
#[derive(Debug, Clone, PartialEq)]
pub struct Block {
    /// `FRAMES`, `CPU`, `GPU`, `RAM`.
    pub key: &'static str,
    /// The device, right-aligned in the heading. `None` leaves the heading to the key alone.
    pub name: Option<String>,
    /// The vendor accent this block is coloured by.
    pub tint: Color,
    pub rows: Vec<Row>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct HudModel {
    pub blocks: Vec<Block>,
    /// Why something the user expected to see is missing. Never suppressed by settings.
    pub notice: Option<String>,
}

impl HudModel {
    /// Resolves a snapshot against the settings.
    pub fn new(s: &MetricsSnapshot, config: &Config) -> Self {
        // Sections that describe no particular device — the frame rate, memory — follow the
        // graphics card. bladestats is a graphics tool, the card is the headline part, and
        // when both vendors match this resolves to a single colour for the whole panel anyway.
        let accent = vendor_tint(Some(s.gpu.vendor), &config.theme, config.theme.text);
        let mut blocks = Vec::with_capacity(4);

        if let Some(b) = frames_block(s, config, accent) {
            blocks.push(b);
        }
        if let Some(b) = cpu_block(s, config) {
            blocks.push(b);
        }
        if let Some(b) = gpu_block(s, config) {
            blocks.push(b);
        }
        if let Some(b) = ram_block(s, config, accent) {
            blocks.push(b);
        }

        Self {
            blocks,
            notice: s.notice.clone(),
        }
    }

    /// Everything the model would draw, joined — for tests that assert on content rather than
    /// on layout.
    #[cfg(test)]
    pub fn debug_text(&self) -> String {
        let mut out = Vec::new();
        for b in &self.blocks {
            out.push(b.key.to_string());
            if let Some(n) = &b.name {
                out.push(n.clone());
            }
            for row in &b.rows {
                match row {
                    Row::Readout { cells, .. } => {
                        for c in cells {
                            out.push(format!("{}{}", c.value, c.unit));
                        }
                    }
                    Row::Spec(s) => out.push(s.clone()),
                    Row::Cores(c) => out.push(format!("{}cores", c.len())),
                    Row::Bar(f) => out.push(format!("bar{f:.2}")),
                }
            }
        }
        if let Some(n) = &self.notice {
            out.push(n.clone());
        }
        out.join("|")
    }
}

/// A block with nothing but its heading is dropped.
///
/// Switching every reading in a section off should shrink the panel, not leave a labelled gap
/// where the section used to be.
fn finish(key: &'static str, name: Option<String>, tint: Color, rows: Vec<Row>) -> Option<Block> {
    if rows.is_empty() {
        return None;
    }
    Some(Block {
        key,
        name,
        tint,
        rows,
    })
}

fn frames_block(s: &MetricsSnapshot, config: &Config, accent: Color) -> Option<Block> {
    let theme = &config.theme;
    let m = &config.metrics;
    let f = s.frames.as_ref();
    let mut rows = Vec::new();

    if m.fps || m.frame_time {
        let mut cells = Vec::new();
        let mut right_from = None;
        if m.fps {
            cells.push(Cell::big(
                "fps",
                f.map_or(MISSING.into(), |f| format!("{:.0}", f.fps)),
                " fps",
                f.map_or(theme.label, |_| theme.text),
                3,
            ));
        }
        if m.frame_time && let Some(f) = f {
            right_from = Some(cells.len());
            cells.push(Cell::small(
                "frametime",
                format!("{:.1}", f.frametime_ms),
                " ms",
                theme.label,
                4,
            ));
        }
        if !cells.is_empty() {
            rows.push(Row::Readout { cells, right_from });
        }
    }

    // The percentile lows only exist once there are frames to take percentiles of.
    if let Some(f) = f
        && (m.low_1pct || m.low_01pct)
    {
        let mut cells = Vec::new();
        if m.low_1pct {
            cells.push(Cell::small("low1.label", "1%", "", theme.label, 0));
            cells.push(Cell::small(
                "low1",
                text::opt_fps(f.low_1pct),
                "",
                theme.text,
                3,
            ));
        }
        if m.low_01pct {
            cells.push(Cell::small("low01.label", "0.1%", "", theme.label, 0));
            cells.push(Cell::small(
                "low01",
                text::opt_fps(f.low_01pct),
                "",
                theme.text,
                3,
            ));
        }
        if !cells.is_empty() {
            rows.push(Row::Readout {
                cells,
                right_from: None,
            });
        }
    }

    let name = config
        .experimental
        .graphics_api
        .then_some(s.graphics_api)
        .flatten()
        .map(str::to_owned);

    finish("FRAMES", name, accent, rows)
}

fn cpu_block(s: &MetricsSnapshot, config: &Config) -> Option<Block> {
    let theme = &config.theme;
    let m = &config.metrics;
    let tint = vendor_tint(
        s.cpu.name.as_deref().map(Vendor::from_name),
        theme,
        theme.text,
    );
    let mut rows = Vec::new();

    let mut cells = Vec::new();
    if m.cpu_load {
        cells.push(Cell::new(
            "cpu.load",
            text::pct(s.cpu.load_pct),
            "%",
            load_color(theme, s.cpu.load_pct),
            3,
        ));
    }
    if m.cpu_clock {
        cells.push(Cell::new(
            "cpu.clock",
            text::ghz(peak_core_mhz(s)),
            " GHz",
            theme.text,
            4,
        ));
    }
    if m.cpu_temp {
        cells.push(Cell::new(
            "cpu.temp",
            text::temp(s.cpu.temp_c),
            "°C",
            temp_color(theme, s.cpu.temp_c),
            2,
        ));
    }
    if m.cpu_power {
        cells.push(Cell::new(
            "cpu.power",
            text::watts(s.cpu.power),
            " W",
            theme.text,
            4,
        ));
    }
    if !cells.is_empty() {
        rows.push(Row::Readout {
            cells,
            right_from: None,
        });
    }

    if m.cpu_cores && !s.cpu.cores.is_empty() {
        rows.push(Row::Cores(
            s.cpu.cores.iter().map(|c| c.load_pct / 100.0).collect(),
        ));
    }

    let name = m.cpu_name.then(|| s.cpu.name.clone()).flatten();
    finish("CPU", name, tint, rows)
}

fn gpu_block(s: &MetricsSnapshot, config: &Config) -> Option<Block> {
    let theme = &config.theme;
    let m = &config.metrics;
    let tint = vendor_tint(Some(s.gpu.vendor), theme, theme.text);
    let mut rows = Vec::new();

    let mut cells = Vec::new();
    if m.gpu_load {
        cells.push(Cell::new(
            "gpu.load",
            text::pct(s.gpu.load_pct),
            "%",
            load_color(theme, s.gpu.load_pct),
            3,
        ));
    }
    if m.gpu_clock {
        cells.push(Cell::new(
            "gpu.clock",
            text::ghz(s.gpu.core_clock_mhz),
            " GHz",
            theme.text,
            4,
        ));
    }
    if m.gpu_temp {
        cells.push(Cell::new(
            "gpu.temp",
            text::temp(s.gpu.temp_c),
            "°C",
            temp_color(theme, s.gpu.temp_c),
            2,
        ));
    }
    if m.gpu_hotspot {
        cells.push(Cell::new(
            "gpu.hotspot",
            text::temp(s.gpu.hotspot_c),
            "°C hot",
            temp_color(theme, s.gpu.hotspot_c),
            2,
        ));
    }
    if m.gpu_power {
        cells.push(Cell::new(
            "gpu.power",
            text::watts(s.gpu.power),
            " W",
            theme.text,
            4,
        ));
    }
    if m.gpu_fan {
        cells.push(Cell::new(
            "gpu.fan",
            s.gpu
                .fan_rpm
                .map_or(MISSING.into(), |r| format!("{r:.0}")),
            " rpm",
            theme.label,
            4,
        ));
    }
    if !cells.is_empty() {
        rows.push(Row::Readout {
            cells,
            right_from: None,
        });
    }

    if m.gpu_vram {
        rows.push(Row::Readout {
            cells: vec![
                Cell::small("vram.label", "VRAM", "", theme.label, 0),
                Cell::new(
                    "vram",
                    text::pair_gb(s.gpu.vram_used_bytes, s.gpu.vram_total_bytes),
                    " GB",
                    theme.text,
                    text::pair_gb_reserve(s.gpu.vram_total_bytes),
                ),
            ],
            right_from: None,
        });
        if let Some(f) = text::fraction(s.gpu.vram_used_bytes, s.gpu.vram_total_bytes) {
            rows.push(Row::Bar(f));
        }
    }

    let name = m.gpu_name.then(|| s.gpu.name.clone()).flatten();
    finish("GPU", name, tint, rows)
}

fn ram_block(s: &MetricsSnapshot, config: &Config, accent: Color) -> Option<Block> {
    let theme = &config.theme;
    let m = &config.metrics;
    let mem = &s.memory;
    let mut rows = Vec::new();

    if m.ram_usage {
        let mut cells = vec![Cell::new(
            "ram.usage",
            text::pair_gb(mem.used_bytes, mem.total_bytes),
            " GB",
            theme.text,
            text::pair_gb_reserve(mem.total_bytes),
        )];
        let mut right_from = None;
        // The live rate sits opposite the capacity, the way the design has it: two facts
        // about the same hardware, not a list.
        // The live rate when the controller can be asked, and the configured one otherwise.
        // They are the same number at rest and diverge under load, which is the point: memory
        // clocks down when nothing is asking of it.
        let rate = if config.experimental.ram_live_rate {
            mem.live_mts
                .map(|r| r as u32)
                .or(mem.speed_mhz)
        } else {
            mem.speed_mhz
        };
        if m.ram_spec && let Some(speed) = rate {
            right_from = Some(cells.len());
            cells.push(Cell::new(
                "ram.rate",
                speed.to_string(),
                " MT/s",
                theme.text,
                4,
            ));
        }
        rows.push(Row::Readout { cells, right_from });

        if let Some(f) = text::fraction(mem.used_bytes, mem.total_bytes) {
            rows.push(Row::Bar(f));
        }
    }

    if m.ram_spec && let Some(spec) = text::memory_spec(&mem.modules, mem.rated_mhz) {
        rows.push(Row::Spec(spec));
    }

    let name = m
        .ram_spec
        .then(|| text::memory_name(mem.kind, mem.speed_mhz))
        .flatten();
    finish("RAM", name, accent, rows)
}

/// The brand colour for a device, when the user asked for vendor colours at all.
fn vendor_tint(vendor: Option<Vendor>, theme: &Theme, fallback: Color) -> Color {
    if !theme.vendor_colors {
        return fallback;
    }
    vendor.and_then(Vendor::color).unwrap_or(fallback)
}

fn load_color(theme: &Theme, pct: Option<f32>) -> Color {
    pct.map_or(theme.label, |p| theme.load_color(p))
}

/// Temperature on the same three-step scale as load, and for the same reason: the number
/// matters far less than which of the three bands it is in.
fn temp_color(theme: &Theme, c: Option<f32>) -> Color {
    match c {
        None => theme.label,
        Some(c) if c < 70.0 => theme.text,
        Some(c) if c < 85.0 => theme.warn,
        Some(_) => theme.bad,
    }
}

/// The highest clock across cores: under load the boost clock is what matters, not an average
/// dragged down by idle cores.
fn peak_core_mhz(s: &MetricsSnapshot) -> Option<f32> {
    s.cpu
        .cores
        .iter()
        .filter_map(|c| c.freq_mhz)
        .fold(None, |acc: Option<f32>, f| {
            Some(acc.map_or(f, |a| a.max(f)))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use bs_core::{CoreMetrics, FrameMetrics, Metrics, Power};

    fn populated() -> MetricsSnapshot {
        let mut s = MetricsSnapshot::default();
        s.cpu.name = Some("AMD Ryzen 7 9700X".into());
        s.cpu.load_pct = Some(42.0);
        s.cpu.power = Some(Power::Estimated(78.0));
        s.cpu.cores = vec![
            CoreMetrics {
                load_pct: 30.0,
                freq_mhz: Some(5210.0)
            };
            16
        ];
        s.gpu.name = Some("AMD Radeon RX 7800 XT".into());
        s.gpu.vendor = Vendor::Amd;
        s.gpu.load_pct = Some(97.0);
        s.gpu.core_clock_mhz = Some(2430.0);
        s.gpu.power = Some(Power::Measured(231.0));
        s.gpu.temp_c = Some(68.0);
        s.gpu.vram_used_bytes = Some(12_025_908_838);
        s.gpu.vram_total_bytes = Some(17_179_869_184);
        s.memory.used_bytes = Some(19_756_101_632);
        s.memory.total_bytes = Some(34_359_738_368);
        s.memory.speed_mhz = Some(2576);
        s.memory.kind = Some("DDR5");
        s.memory.rated_mhz = Some(5800);
        s.memory.modules = vec![16384, 16384];
        s.graphics_api = Some("D3D12");
        s.frames = Some(FrameMetrics {
            fps: 142.0,
            frametime_ms: 7.0,
            avg_fps: 141.0,
            low_1pct: Some(118.0),
            low_01pct: Some(96.0),
            sample_count: 5000,
        });
        s
    }

    /// Everything on, so a test that switches one reading off is testing that switch alone.
    fn everything() -> Config {
        let mut config = Config::default();
        config.metrics = Metrics {
            fps: true,
            frame_time: true,
            low_1pct: true,
            low_01pct: true,
            cpu_name: true,
            cpu_load: true,
            cpu_cores: true,
            cpu_clock: true,
            cpu_temp: true,
            cpu_power: true,
            gpu_name: true,
            gpu_load: true,
            gpu_clock: true,
            gpu_vram: true,
            gpu_temp: true,
            gpu_hotspot: true,
            gpu_fan: true,
            gpu_power: true,
            ram_usage: true,
            ram_spec: true,
        };
        config.experimental.graphics_api = true;
        config
    }

    fn all_text(s: &MetricsSnapshot) -> String {
        HudModel::new(s, &everything()).debug_text()
    }

    #[test]
    fn the_four_blocks_appear_in_the_order_the_design_states_them() {
        let model = HudModel::new(&populated(), &everything());
        let keys: Vec<&str> = model.blocks.iter().map(|b| b.key).collect();
        assert_eq!(keys, ["FRAMES", "CPU", "GPU", "RAM"]);
    }

    #[test]
    fn an_empty_snapshot_renders_dashes_not_zeroes() {
        let text = all_text(&MetricsSnapshot::default());
        assert!(text.contains(MISSING));
        assert!(
            !text.contains("0%") && !text.contains("0 W"),
            "an unread metric must not look like a genuine zero: {text}"
        );
    }

    #[test]
    fn estimated_watts_are_marked_with_a_tilde_and_measured_ones_are_not() {
        let text = all_text(&populated());
        assert!(
            text.contains("~78 W"),
            "an estimated CPU wattage must be marked: {text}"
        );
        assert!(text.contains("231 W") && !text.contains("~231 W"));
    }

    #[test]
    fn the_memory_block_never_shows_watts() {
        // Consumer boards have no sensor for it, and inventing one would be fiction.
        let model = HudModel::new(&populated(), &everything());
        let ram = model.blocks.iter().find(|b| b.key == "RAM").unwrap();
        for row in &ram.rows {
            if let Row::Readout { cells, .. } = row {
                for c in cells {
                    assert!(!c.unit.contains('W'), "watts in the memory block: {c:?}");
                }
            }
        }
    }

    #[test]
    fn switching_a_reading_off_removes_it() {
        let s = populated();
        let mut config = everything();
        assert!(HudModel::new(&s, &config).debug_text().contains("68°C"));

        config.metrics.gpu_temp = false;
        assert!(
            !HudModel::new(&s, &config).debug_text().contains("68°C"),
            "a reading switched off must leave nothing behind"
        );
    }

    #[test]
    fn a_block_with_nothing_left_in_it_disappears_entirely() {
        let s = populated();
        let mut config = everything();
        config.metrics.cpu_load = false;
        config.metrics.cpu_clock = false;
        config.metrics.cpu_temp = false;
        config.metrics.cpu_power = false;
        config.metrics.cpu_cores = false;

        // The name alone is not a section: without readings there is nothing to head.
        let model = HudModel::new(&s, &config);
        assert!(
            !model.blocks.iter().any(|b| b.key == "CPU"),
            "an empty block must not linger as a heading"
        );
        assert!(model.blocks.iter().all(|b| !b.rows.is_empty()));
    }

    #[test]
    fn the_lows_row_appears_only_when_there_are_frames() {
        let config = everything();
        assert!(HudModel::new(&populated(), &config).debug_text().contains("1%"));
        assert!(
            !HudModel::new(&MetricsSnapshot::default(), &config)
                .debug_text()
                .contains("1%"),
            "without a frame source a percentile means nothing"
        );
    }

    #[test]
    fn vendor_colours_apply_to_the_heading_and_can_be_switched_off() {
        let s = populated();

        let mut on = everything();
        on.theme.vendor_colors = true;
        let gpu = HudModel::new(&s, &on)
            .blocks
            .into_iter()
            .find(|b| b.key == "GPU")
            .unwrap();
        assert_eq!(gpu.tint, Vendor::Amd.color().unwrap());

        let mut off = everything();
        off.theme.vendor_colors = false;
        let gpu = HudModel::new(&s, &off)
            .blocks
            .into_iter()
            .find(|b| b.key == "GPU")
            .unwrap();
        assert_eq!(gpu.tint, off.theme.text);
    }

    #[test]
    fn the_processor_takes_its_accent_from_its_name() {
        let mut s = populated();
        s.cpu.name = Some("Intel Core i9-13900K".into());
        let cpu = HudModel::new(&s, &everything())
            .blocks
            .into_iter()
            .find(|b| b.key == "CPU")
            .unwrap();
        assert_eq!(cpu.tint, Vendor::Intel.color().unwrap());
    }

    #[test]
    fn the_peak_core_clock_is_reported_not_the_average() {
        let mut s = populated();
        s.cpu.cores = vec![
            CoreMetrics {
                load_pct: 5.0,
                freq_mhz: Some(400.0),
            },
            CoreMetrics {
                load_pct: 99.0,
                freq_mhz: Some(5200.0),
            },
        ];
        assert_eq!(peak_core_mhz(&s), Some(5200.0));
        assert!(all_text(&s).contains("5.20 GHz"));
    }

    #[test]
    fn the_notice_survives_every_setting() {
        // It exists to explain why something is missing, so settings must not silence it.
        let mut s = populated();
        s.notice = Some("no FPS: run as administrator".into());

        let mut nothing = Config::default();
        nothing.metrics = Metrics {
            fps: false,
            frame_time: false,
            low_1pct: false,
            low_01pct: false,
            cpu_name: false,
            cpu_load: false,
            cpu_cores: false,
            cpu_clock: false,
            cpu_temp: false,
            cpu_power: false,
            gpu_name: false,
            gpu_load: false,
            gpu_clock: false,
            gpu_vram: false,
            gpu_temp: false,
            gpu_hotspot: false,
            gpu_fan: false,
            gpu_power: false,
            ram_usage: false,
            ram_spec: false,
        };

        let model = HudModel::new(&s, &nothing);
        assert!(model.blocks.is_empty(), "everything off leaves no blocks");
        assert_eq!(model.notice.as_deref(), Some("no FPS: run as administrator"));
    }

    #[test]
    fn the_graphics_api_is_only_named_when_it_was_asked_for() {
        let s = populated();
        assert!(all_text(&s).contains("D3D12"));

        let mut off = everything();
        off.experimental.graphics_api = false;
        assert!(
            !HudModel::new(&s, &off).debug_text().contains("D3D12"),
            "an experimental reading must stay off until it is switched on"
        );
    }

    #[test]
    fn a_bar_is_only_drawn_when_both_ends_of_it_are_known() {
        let mut s = populated();
        s.gpu.vram_total_bytes = None;
        let gpu = HudModel::new(&s, &everything())
            .blocks
            .into_iter()
            .find(|b| b.key == "GPU")
            .unwrap();
        assert!(
            !gpu.rows.iter().any(|r| matches!(r, Row::Bar(_))),
            "a bar drawn against a guessed total looks like a measurement"
        );
    }

    #[test]
    fn hot_readings_change_colour_and_cool_ones_do_not() {
        let theme = Theme::default();
        assert_eq!(temp_color(&theme, Some(45.0)), theme.text);
        assert_eq!(temp_color(&theme, Some(78.0)), theme.warn);
        assert_eq!(temp_color(&theme, Some(97.0)), theme.bad);
        assert_eq!(temp_color(&theme, None), theme.label);
    }
}
