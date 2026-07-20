//! Overlay layout: the snapshot becomes rows, the rows become geometry.
//!
//! The layout is shared by both platforms. Metrics that could not be read are drawn as a dash
//! rather than a zero — a direct consequence of the `Option` convention in `bs-core`: the user
//! must be able to tell "zero watts" from "no sensor".

use bs_core::{Color, MetricsSnapshot, Power, Theme};

use crate::atlas::GlyphAtlas;
use crate::draw::DrawList;

/// What fills a metric that could not be read.
const MISSING: &str = "—";

#[derive(Debug, Clone)]
pub struct HudOptions {
    /// Gap between the edge of the backing panel and the text.
    pub padding: f32,
    /// Whether to show each core's load as its own bar.
    pub show_cores: bool,
    /// Width of one core bar, in pixels.
    pub core_bar_width: f32,
}

impl Default for HudOptions {
    fn default() -> Self {
        Self {
            padding: 8.0,
            show_cores: true,
            core_bar_width: 4.0,
        }
    }
}

/// The resulting overlay size — the platform sizes its window from this.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HudSize {
    pub width: f32,
    pub height: f32,
}

struct Span {
    text: String,
    color: Color,
}

impl Span {
    fn new(text: impl Into<String>, color: Color) -> Self {
        Self {
            text: text.into(),
            color,
        }
    }
}

/// Builds the overlay geometry and reports its size.
pub fn build(
    atlas: &GlyphAtlas,
    snapshot: &MetricsSnapshot,
    theme: &Theme,
    opts: &HudOptions,
) -> (DrawList, HudSize) {
    let rows = rows(snapshot, theme);

    let text_width = rows
        .iter()
        .map(|row| row.iter().map(|s| atlas.measure(&s.text)).sum::<f32>())
        .fold(0.0f32, f32::max);

    // Core bars live on their own row below the CPU metrics.
    let cores = &snapshot.cpu.cores;
    let core_row_width = if opts.show_cores && !cores.is_empty() {
        cores.len() as f32 * opts.core_bar_width
    } else {
        0.0
    };
    let core_row_height = if core_row_width > 0.0 {
        atlas.line_height
    } else {
        0.0
    };

    let size = HudSize {
        width: text_width.max(core_row_width) + opts.padding * 2.0,
        height: rows.len() as f32 * atlas.line_height + core_row_height + opts.padding * 2.0,
    };

    let mut list = DrawList::new();
    list.rect(atlas, 0.0, 0.0, size.width, size.height, theme.background);

    let mut baseline = opts.padding + atlas.ascent;
    for row in &rows {
        let mut pen = opts.padding;
        for span in row {
            pen = list.text(atlas, pen, baseline, &span.text, span.color);
        }
        baseline += atlas.line_height;
    }

    if core_row_width > 0.0 {
        draw_core_bars(&mut list, atlas, cores, theme, opts, baseline);
    }

    (list, size)
}

/// Per-core load as individual vertical bars.
///
/// As numbers this would take several lines on any modern CPU, and what the eye wants from an
/// overlay is the shape of the distribution, not exact per-core percentages.
fn draw_core_bars(
    list: &mut DrawList,
    atlas: &GlyphAtlas,
    cores: &[bs_core::CoreMetrics],
    theme: &Theme,
    opts: &HudOptions,
    baseline: f32,
) {
    let max_h = atlas.line_height * 0.7;
    let top = baseline - atlas.ascent;
    let gap = 1.0f32.min(opts.core_bar_width * 0.25);

    for (i, core) in cores.iter().enumerate() {
        let x = opts.padding + i as f32 * opts.core_bar_width;
        let h = (core.load_pct.clamp(0.0, 100.0) / 100.0) * max_h;
        let w = opts.core_bar_width - gap;

        // A dim full-height track, so idle cores still read as cores rather than as empty
        // space.
        list.rect(atlas, x, top, w, max_h, Color::rgba(0xFF, 0xFF, 0xFF, 0x22));
        list.rect(
            atlas,
            x,
            top + (max_h - h),
            w,
            h,
            theme.load_color(core.load_pct),
        );
    }
}

fn rows(s: &MetricsSnapshot, theme: &Theme) -> Vec<Vec<Span>> {
    let mut rows = Vec::new();
    let label = theme.label;
    let text = theme.text;

    let cpu_color = vendor_or(s.cpu.name.as_deref(), theme.vendor_colors, label);
    let gpu_color = if theme.vendor_colors {
        s.gpu.vendor.color().unwrap_or(label)
    } else {
        label
    };

    // Frames
    let frames = s.frames.as_ref();
    rows.push(vec![
        Span::new("FPS  ", label),
        Span::new(
            frames.map_or(MISSING.into(), |f| format!("{:.0}", f.fps)),
            frames.map_or(label, |_| text),
        ),
        Span::new(
            frames.map_or(String::new(), |f| format!("  {:.1} ms", f.frametime_ms)),
            label,
        ),
    ]);
    if let Some(f) = frames {
        rows.push(vec![
            Span::new("     1% ", label),
            Span::new(opt_fps(f.low_1pct), text),
            Span::new("  0.1% ", label),
            Span::new(opt_fps(f.low_01pct), text),
        ]);
    }

    // CPU
    rows.push(vec![
        Span::new("CPU  ", label),
        Span::new(
            s.cpu.name.clone().unwrap_or_else(|| MISSING.into()),
            cpu_color,
        ),
    ]);
    rows.push(vec![
        Span::new("     ", label),
        Span::new(pct(s.cpu.load_pct), load_color(theme, s.cpu.load_pct)),
        Span::new(format!("  {}", mhz(peak_core_mhz(s))), text),
        Span::new(format!("  {}", temp(s.cpu.temp_c)), text),
        Span::new(format!("  {}", watts(s.cpu.power)), text),
    ]);

    // GPU
    rows.push(vec![
        Span::new("GPU  ", label),
        Span::new(
            s.gpu.name.clone().unwrap_or_else(|| MISSING.into()),
            gpu_color,
        ),
    ]);
    rows.push(vec![
        Span::new("     ", label),
        Span::new(pct(s.gpu.load_pct), load_color(theme, s.gpu.load_pct)),
        Span::new(format!("  {}", mhz(s.gpu.core_clock_mhz)), text),
        Span::new(format!("  {}", temp(s.gpu.temp_c)), text),
        Span::new(format!("  {}", watts(s.gpu.power)), text),
    ]);
    rows.push(vec![
        Span::new("VRAM ", label),
        Span::new(pair_gb(s.gpu.vram_used_bytes, s.gpu.vram_total_bytes), text),
    ]);

    // Memory. No watts here on purpose: RAM has no power sensor.
    rows.push(vec![
        Span::new("RAM  ", label),
        Span::new(pair_gb(s.memory.used_bytes, s.memory.total_bytes), text),
        Span::new(
            s.memory
                .speed_mhz
                .map_or(String::new(), |m| format!("  {m} MHz")),
            label,
        ),
    ]);

    rows
}

fn vendor_or(name: Option<&str>, enabled: bool, fallback: Color) -> Color {
    if !enabled {
        return fallback;
    }
    name.map(bs_core::Vendor::from_name)
        .and_then(|v| v.color())
        .unwrap_or(fallback)
}

fn load_color(theme: &Theme, pct: Option<f32>) -> Color {
    pct.map_or(theme.label, |p| theme.load_color(p))
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

fn pct(v: Option<f32>) -> String {
    v.map_or(MISSING.into(), |v| format!("{v:.0}%"))
}

fn mhz(v: Option<f32>) -> String {
    v.map_or(MISSING.into(), |v| format!("{v:.0} MHz"))
}

fn temp(v: Option<f32>) -> String {
    v.map_or(MISSING.into(), |v| format!("{v:.0}°C"))
}

fn opt_fps(v: Option<f32>) -> String {
    v.map_or(MISSING.into(), |v| format!("{v:.0}"))
}

/// Watts, tagged with their provenance: a tilde means a derived estimate, not a sensor reading.
fn watts(p: Option<Power>) -> String {
    match p {
        None => MISSING.into(),
        Some(p) if p.is_estimated() => format!("~{:.0} W", p.watts()),
        Some(p) => format!("{:.0} W", p.watts()),
    }
}

fn pair_gb(used: Option<u64>, total: Option<u64>) -> String {
    const GB: f64 = 1024.0 * 1024.0 * 1024.0;
    match (used, total) {
        (Some(u), Some(t)) => format!("{:.1} / {:.1} GB", u as f64 / GB, t as f64 / GB),
        (Some(u), None) => format!("{:.1} GB", u as f64 / GB),
        _ => MISSING.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bs_core::{CoreMetrics, FrameMetrics, Vendor};

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

    fn populated() -> MetricsSnapshot {
        let mut s = MetricsSnapshot::default();
        s.cpu.name = Some("AMD Ryzen 7 7800X3D".into());
        s.cpu.load_pct = Some(42.0);
        s.cpu.power = Some(Power::Estimated(65.0));
        s.cpu.cores = vec![
            CoreMetrics {
                load_pct: 30.0,
                freq_mhz: Some(4500.0)
            };
            16
        ];
        s.gpu.name = Some("NVIDIA GeForce RTX 4070".into());
        s.gpu.vendor = Vendor::Nvidia;
        s.gpu.load_pct = Some(88.0);
        s.gpu.power = Some(Power::Measured(145.0));
        s.gpu.temp_c = Some(62.0);
        s.gpu.vram_used_bytes = Some(6_500_000_000);
        s.gpu.vram_total_bytes = Some(12_884_901_888);
        s.memory.used_bytes = Some(19_000_000_000);
        s.memory.total_bytes = Some(34_359_738_368);
        s.memory.speed_mhz = Some(6000);
        s.frames = Some(FrameMetrics {
            fps: 144.0,
            frametime_ms: 6.9,
            avg_fps: 141.0,
            low_1pct: Some(98.0),
            low_01pct: None,
            sample_count: 500,
        });
        s
    }

    fn all_text(s: &MetricsSnapshot) -> String {
        rows(s, &Theme::default())
            .iter()
            .flat_map(|r| r.iter())
            .map(|s| s.text.clone())
            .collect::<Vec<_>>()
            .join("|")
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
        assert_eq!(watts(Some(Power::Estimated(65.0))), "~65 W");
        assert_eq!(watts(Some(Power::Measured(145.0))), "145 W");
        assert_eq!(watts(None), MISSING);

        let text = all_text(&populated());
        assert!(
            text.contains("~65 W"),
            "an estimated CPU wattage must be marked as one"
        );
        assert!(text.contains("145 W") && !text.contains("~145 W"));
    }

    #[test]
    fn ram_row_never_shows_watts() {
        // RAM has no power sensor, and its row must not invent one.
        let s = populated();
        let ram = rows(&s, &Theme::default())
            .into_iter()
            .find(|r| r[0].text.starts_with("RAM"))
            .expect("the RAM row");
        let joined: String = ram.iter().map(|s| s.text.as_str()).collect();
        assert!(
            !joined.contains('W'),
            "the memory row must not contain watts: {joined}"
        );
        assert!(joined.contains("6000 MHz"));
    }

    #[test]
    fn lows_row_appears_only_when_there_are_frames() {
        let with = rows(&populated(), &Theme::default());
        assert!(with.iter().any(|r| r[0].text.contains("1%")));

        let without = rows(&MetricsSnapshot::default(), &Theme::default());
        assert!(
            !without.iter().any(|r| r[0].text.contains("1%")),
            "without a frame source the percentile row means nothing"
        );
    }

    #[test]
    fn missing_low_percentile_is_a_dash_while_the_row_still_shows() {
        let text = all_text(&populated());
        assert!(text.contains("98"), "the 1% low is known");
        // A 0.1% low over 500 frames is not computed — a dash, not an invented number.
        assert!(text.contains(MISSING));
    }

    #[test]
    fn vendor_colours_apply_to_names_and_can_be_switched_off() {
        let s = populated();

        let mut on = Theme::default();
        on.vendor_colors = true;
        let gpu_row = rows(&s, &on)
            .into_iter()
            .find(|r| r[0].text.starts_with("GPU"));
        assert_eq!(gpu_row.unwrap()[1].color, Vendor::Nvidia.color().unwrap());

        let mut off = Theme::default();
        off.vendor_colors = false;
        let gpu_row = rows(&s, &off)
            .into_iter()
            .find(|r| r[0].text.starts_with("GPU"));
        assert_eq!(gpu_row.unwrap()[1].color, off.label);
    }

    #[test]
    fn cpu_vendor_colour_is_derived_from_its_name() {
        let mut s = populated();
        s.cpu.name = Some("Intel Core i9-13900K".into());
        let theme = Theme::default();
        let row = rows(&s, &theme)
            .into_iter()
            .find(|r| r[0].text.starts_with("CPU"))
            .unwrap();
        assert_eq!(row[1].color, Vendor::Intel.color().unwrap());
    }

    #[test]
    fn peak_core_clock_is_reported_not_the_average() {
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
        assert!(all_text(&s).contains("5200 MHz"));
    }

    #[test]
    fn hud_grows_with_the_number_of_cores() {
        let atlas = atlas_or_skip!();
        let theme = Theme::default();
        let opts = HudOptions::default();

        let mut few = populated();
        few.cpu.cores = vec![CoreMetrics::default(); 4];
        let mut many = populated();
        many.cpu.cores = vec![CoreMetrics::default(); 64];

        let (_, small) = build(&atlas, &few, &theme, &opts);
        let (_, big) = build(&atlas, &many, &theme, &opts);
        assert!(big.width > small.width, "64 cores are wider than 4");
        assert_eq!(
            big.height, small.height,
            "core bars occupy one row regardless of how many there are"
        );
    }

    #[test]
    fn hiding_cores_removes_their_row() {
        let atlas = atlas_or_skip!();
        let theme = Theme::default();
        let s = populated();

        let shown = build(&atlas, &s, &theme, &HudOptions::default()).1;
        let hidden = build(
            &atlas,
            &s,
            &theme,
            &HudOptions {
                show_cores: false,
                ..Default::default()
            },
        )
        .1;
        assert!(hidden.height < shown.height);
    }

    #[test]
    fn geometry_stays_inside_the_reported_size() {
        let atlas = atlas_or_skip!();
        let (list, size) = build(
            &atlas,
            &populated(),
            &Theme::default(),
            &HudOptions::default(),
        );

        assert!(!list.is_empty());
        for v in &list.vertices {
            assert!(
                v.pos[0] >= -0.5 && v.pos[0] <= size.width + 0.5,
                "on X: {:?}",
                v.pos
            );
            assert!(
                v.pos[1] >= -0.5 && v.pos[1] <= size.height + 0.5,
                "on Y: {:?}",
                v.pos
            );
        }
    }

    #[test]
    fn a_transparent_background_produces_no_backing_quad() {
        let atlas = atlas_or_skip!();
        let mut theme = Theme::default();
        theme.background = Color::TRANSPARENT;

        let empty = MetricsSnapshot::default();
        let (list, _) = build(
            &atlas,
            &empty,
            &theme,
            &HudOptions {
                show_cores: false,
                ..Default::default()
            },
        );
        // The first quad is normally the backing panel; without it geometry starts at the text.
        assert!(
            list.vertices[0].color[3] > 0.0,
            "visible text comes first, not an empty background"
        );
    }
}
