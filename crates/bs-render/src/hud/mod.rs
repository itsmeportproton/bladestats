//! Overlay layout: the snapshot becomes blocks, the blocks become geometry.
//!
//! The layout is shared by both platforms. Metrics that could not be read are drawn as a dash
//! rather than a zero — a direct consequence of the `Option` convention in `bs-core`: the user
//! must be able to tell "zero watts" from "no sensor".
//!
//! Three layers, and the split is what keeps any of it testable. [`model`] decides what the
//! panel says and knows nothing about pixels; [`paint`] turns that into quads and knows nothing
//! about hardware; [`text`] holds the formatting both would otherwise duplicate. Only the
//! middle one needs a font, so most of the rules can be tested without one.

pub mod anim;
pub mod model;
pub mod paint;
pub mod state;
pub mod text;

pub use model::HudModel;
pub use paint::{HudSize, HudStyle};
pub use state::{HudState, Motion};

use bs_core::{Config, MetricsSnapshot};

use crate::atlas::Atlas;
use crate::draw::DrawList;

/// The former name of [`HudStyle`], from when it held two numbers.
pub type HudOptions = HudStyle;

/// Builds the overlay geometry and reports its size.
///
/// A single settled frame, with no animation state: the preview renders with this, and so does
/// every test. The overlay itself drives the same two passes through a state that carries
/// values between frames.
pub fn build(
    atlas: &Atlas,
    snapshot: &MetricsSnapshot,
    config: &Config,
    style: &HudStyle,
) -> (DrawList, HudSize) {
    let model = HudModel::new(snapshot, config);
    let size = paint::measure(&model, atlas, style);
    let mut list = DrawList::new();
    paint::paint(&mut list, &model, atlas, &config.theme, style, size, size);
    (list, size)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::atlas::GlyphAtlas;
    use bs_core::{CoreMetrics, FrameMetrics, Metrics, Power, Vendor};

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
        s.gpu.vram_used_bytes = Some(12_025_908_838);
        s.gpu.vram_total_bytes = Some(17_179_869_184);
        s.memory.used_bytes = Some(19_756_101_632);
        s.memory.total_bytes = Some(34_359_738_368);
        s.memory.speed_mhz = Some(2576);
        s.memory.kind = Some("DDR5");
        s.memory.rated_mhz = Some(5800);
        s.memory.modules = vec![16384, 16384];
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
        config
    }

    #[test]
    fn geometry_stays_inside_the_reported_size() {
        let atlas = atlas_or_skip!();
        let (list, size) = build(&atlas, &populated(), &everything(), &HudStyle::default());

        assert!(!list.is_empty());
        for v in &list.vertices {
            assert!(
                v.pos[0] >= -0.5 && v.pos[0] <= size.width + 0.5,
                "on X: {:?} in a panel {} wide",
                v.pos,
                size.width
            );
            assert!(
                v.pos[1] >= -0.5 && v.pos[1] <= size.height + 0.5,
                "on Y: {:?} in a panel {} tall",
                v.pos,
                size.height
            );
        }
    }

    #[test]
    fn the_panel_does_not_widen_with_the_core_count() {
        let atlas = atlas_or_skip!();
        let config = everything();
        let style = HudStyle::default();

        let mut few = populated();
        few.cpu.cores = vec![CoreMetrics::default(); 4];
        let mut many = populated();
        many.cpu.cores = vec![CoreMetrics::default(); 64];

        // The old layout gave every core a fixed width and grew sideways. The strip now
        // divides whatever width the text already needed, so a 64-thread part does not make
        // the overlay twice as wide as a 4-thread one.
        let small = build(&atlas, &few, &config, &style).1;
        let big = build(&atlas, &many, &config, &style).1;
        assert_eq!(big.width, small.width);
        assert_eq!(big.height, small.height, "the strip is one row regardless");
    }

    #[test]
    fn hiding_a_section_makes_the_panel_shorter() {
        let atlas = atlas_or_skip!();
        let s = populated();
        let style = HudStyle::default();

        let shown = build(&atlas, &s, &everything(), &style).1;
        let mut hidden = everything();
        hidden.metrics.cpu_cores = false;
        let without_cores = build(&atlas, &s, &hidden, &style).1;
        assert!(without_cores.height < shown.height);

        let mut no_gpu = everything();
        no_gpu.metrics.gpu_load = false;
        no_gpu.metrics.gpu_clock = false;
        no_gpu.metrics.gpu_temp = false;
        no_gpu.metrics.gpu_power = false;
        no_gpu.metrics.gpu_vram = false;
        let without_gpu = build(&atlas, &s, &no_gpu, &style).1;
        assert!(
            without_gpu.height < without_cores.height,
            "a whole section switched off must take its heading with it"
        );
    }

    #[test]
    fn the_panel_never_collapses_below_a_readable_width() {
        let atlas = atlas_or_skip!();
        let mut bare = Config::default();
        bare.metrics = Metrics {
            fps: true,
            ..everything().metrics
        };
        bare.metrics.frame_time = false;
        bare.metrics.low_1pct = false;
        bare.metrics.low_01pct = false;
        bare.metrics.cpu_name = false;
        bare.metrics.cpu_load = false;
        bare.metrics.cpu_cores = false;
        bare.metrics.cpu_clock = false;
        bare.metrics.cpu_temp = false;
        bare.metrics.cpu_power = false;
        bare.metrics.gpu_name = false;
        bare.metrics.gpu_load = false;
        bare.metrics.gpu_clock = false;
        bare.metrics.gpu_vram = false;
        bare.metrics.gpu_temp = false;
        bare.metrics.gpu_power = false;
        bare.metrics.ram_usage = false;
        bare.metrics.ram_spec = false;

        let (_, size) = build(&atlas, &populated(), &bare, &HudStyle::default());
        // Three digits alone would make a panel a couple of centimetres wide, which reads as a
        // glitch rather than as a deliberate minimal overlay.
        assert!(size.width >= HudStyle::default().min_width * atlas.scale);
    }

    #[test]
    fn a_transparent_background_produces_no_backing_quad() {
        let atlas = atlas_or_skip!();
        let mut config = everything();
        config.theme.background = bs_core::Color::TRANSPARENT;

        let (list, _) = build(
            &atlas,
            &MetricsSnapshot::default(),
            &config,
            &HudStyle::default(),
        );
        assert!(
            list.vertices.iter().all(|v| v.color[3] > 0.0),
            "an invisible fill must not reach the vertex buffer at all"
        );
    }

    #[test]
    fn the_notice_is_drawn_even_when_every_reading_is_off() {
        let atlas = atlas_or_skip!();
        let mut s = MetricsSnapshot::default();
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

        let (list, size) = build(&atlas, &s, &nothing, &HudStyle::default());
        assert!(!list.is_empty(), "the reason must still be on screen");
        assert!(size.height > 0.0);
    }
}
