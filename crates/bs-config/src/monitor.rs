//! The monitor preview: a second window showing what the overlay draws.
//!
//! It sits on a synthetic game frame on purpose. Transparency cannot be judged against a flat
//! background &mdash; a half-transparent panel over a plain fill passes as opaque, and the
//! background slider would look like it does nothing.

use bs_core::{Config, Vendor};
use egui::{
    Align2, Color32, CornerRadius, FontFamily, FontId, Pos2, Rect, Stroke, StrokeKind, Ui, Vec2,
    ViewportBuilder, ViewportId,
};

use crate::theme::{self, Accents};

pub const SIZE: [f32; 2] = [380.0, 470.0];

/// Shows the preview in its own window. Clears `open` when the window is closed.
pub fn show(
    ctx: &egui::Context,
    config: &Config,
    accents: &Accents,
    gpu_name: &str,
    gpu_vendor: Vendor,
    open: &mut bool,
) {
    // No transparency requested, unlike the main window. This one fills itself edge to edge
    // with the stand-in game frame, so transparency would buy nothing but rounded outer
    // corners &mdash; and asking for it failed outright on this machine's OpenGL driver
    // ("the GL config does not support it"), which left black squares in those corners.
    let viewport = ViewportBuilder::default()
        .with_title("bladestats monitor")
        .with_inner_size(SIZE)
        .with_resizable(false)
        .with_decorations(false);

    let mut closed = false;
    ctx.show_viewport_immediate(ViewportId::from_hash_of("monitor"), viewport, |ctx, _| {
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ctx, |ui| {
                let rect = ui.max_rect();
                paint_scene(ui, rect);
                paint_panel(ui, rect.shrink(22.0), config, accents, gpu_name, gpu_vendor);
            });

        if ctx.input(|i| i.viewport().close_requested() || i.key_pressed(egui::Key::Escape)) {
            closed = true;
        }
    });

    if closed {
        *open = false;
    }
}

/// Stands in for a game running underneath. Deliberately busy: a plain fill would let a
/// half-transparent panel pass as opaque.
fn paint_scene(ui: &Ui, rect: Rect) {
    let p = ui.painter();
    // Square corners: the window itself is opaque, so rounding here would only reveal the
    // clear colour behind it.
    p.rect_filled(
        rect,
        CornerRadius::ZERO,
        Color32::from_rgb(0x10, 0x18, 0x21),
    );

    for (centre, radius, colour) in [
        (
            rect.lerp_inside(Vec2::new(0.22, 0.18)),
            0.55,
            Color32::from_rgb(0xC8, 0x7A, 0x2E),
        ),
        (
            rect.lerp_inside(Vec2::new(0.80, 0.30)),
            0.45,
            Color32::from_rgb(0x3A, 0x5C, 0xC8),
        ),
        (
            rect.lerp_inside(Vec2::new(0.60, 0.85)),
            0.50,
            Color32::from_rgb(0x1B, 0x8E, 0x74),
        ),
    ] {
        p.circle_filled(centre, rect.width() * radius, colour.gamma_multiply(0.45));
    }
}

fn paint_panel(
    ui: &mut Ui,
    rect: Rect,
    config: &Config,
    accents: &Accents,
    gpu_name: &str,
    gpu_vendor: Vendor,
) {
    let alpha = config.theme.background.a as f32 / 255.0;
    let p = ui.painter();

    // Only the fill carries the alpha. Text stays opaque so it survives being laid over a
    // bright frame &mdash; which is the whole reason the slider is safe to drag to zero.
    p.rect_filled(
        rect,
        CornerRadius::same(12),
        Color32::from_rgba_unmultiplied(0x08, 0x09, 0x0B, (alpha * 255.0) as u8),
    );
    p.rect_stroke(
        rect,
        CornerRadius::same(12),
        Stroke::new(1.0, Color32::from_white_alpha(24)),
        StrokeKind::Inside,
    );

    let mono = |size: f32| FontId::new(size, FontFamily::Monospace);
    let m = &config.metrics;
    let x = rect.left() + 16.0;
    let mut y = rect.top() + 16.0;

    let block = |ui: &Ui, key: &str, name: &str, tint: Color32, y: &mut f32| {
        let p = ui.painter();
        p.text(
            Pos2::new(x, *y),
            Align2::LEFT_TOP,
            key,
            mono(10.0),
            theme::FAINT,
        );
        p.text(
            Pos2::new(rect.right() - 16.0, *y),
            Align2::RIGHT_TOP,
            name,
            mono(11.0),
            tint,
        );
        *y += 16.0;
    };

    // Frames
    if m.fps || m.frame_time {
        block(
            ui,
            "FRAMES",
            if config.experimental.graphics_api {
                "D3D12"
            } else {
                ""
            },
            accents.chrome.ink,
            &mut y,
        );
        let p = ui.painter();
        if m.fps {
            p.text(
                Pos2::new(x, y),
                Align2::LEFT_TOP,
                "142",
                mono(21.0),
                theme::TEXT,
            );
            p.text(
                Pos2::new(x + 46.0, y + 9.0),
                Align2::LEFT_TOP,
                "fps",
                mono(11.0),
                theme::MUTED,
            );
        }
        if m.frame_time {
            p.text(
                Pos2::new(rect.right() - 16.0, y + 9.0),
                Align2::RIGHT_TOP,
                "7.0 ms",
                mono(11.0),
                theme::MUTED,
            );
        }
        y += 28.0;
        if m.low_1pct || m.low_01pct {
            let mut lows = String::new();
            if m.low_1pct {
                lows.push_str("1% 118  ");
            }
            if m.low_01pct {
                lows.push_str("0.1% 96");
            }
            ui.painter().text(
                Pos2::new(x, y),
                Align2::LEFT_TOP,
                lows,
                mono(11.0),
                theme::MUTED,
            );
            y += 18.0;
        }
        y = rule(ui, rect, y);
    }

    // Processor
    if m.cpu_name || m.cpu_load || m.cpu_clock || m.cpu_temp || m.cpu_power {
        block(
            ui,
            "CPU",
            if m.cpu_name { "Ryzen 7 9700X" } else { "" },
            accents.cpu.ink,
            &mut y,
        );
        let mut parts = Vec::new();
        if m.cpu_load {
            parts.push("42%".to_string());
        }
        if m.cpu_clock {
            parts.push("5.21 GHz".into());
        }
        if m.cpu_temp {
            parts.push("—".into());
        }
        if m.cpu_power {
            parts.push("~78 W".into());
        }
        ui.painter().text(
            Pos2::new(x, y),
            Align2::LEFT_TOP,
            parts.join("   "),
            mono(12.5),
            theme::TEXT,
        );
        y += 20.0;

        if m.cpu_cores {
            y = cores(ui, x, y, rect.width() - 32.0, accents.cpu.fill);
        }
        y = rule(ui, rect, y);
    }

    // Graphics
    if m.gpu_name || m.gpu_load || m.gpu_clock || m.gpu_temp || m.gpu_power || m.gpu_vram {
        block(
            ui,
            "GPU",
            if m.gpu_name { gpu_name } else { "" },
            accents.gpu.ink,
            &mut y,
        );
        let has_sensors = gpu_vendor == Vendor::Nvidia;
        let mut parts = Vec::new();
        if m.gpu_load {
            parts.push("97%".to_string());
        }
        if m.gpu_clock {
            parts.push("2.43 GHz".into());
        }
        if m.gpu_temp {
            parts.push(if has_sensors {
                "68°C".into()
            } else {
                "—".to_string()
            });
        }
        if m.gpu_power {
            parts.push(if has_sensors {
                "231 W".into()
            } else {
                "—".to_string()
            });
        }
        ui.painter().text(
            Pos2::new(x, y),
            Align2::LEFT_TOP,
            parts.join("   "),
            mono(12.5),
            theme::TEXT,
        );
        y += 20.0;

        if m.gpu_vram {
            ui.painter().text(
                Pos2::new(x, y),
                Align2::LEFT_TOP,
                "VRAM  11.2 / 16.0 GB",
                mono(12.0),
                theme::TEXT,
            );
            y += 18.0;
            y = bar(ui, x, y, rect.width() - 32.0, 0.70, accents.gpu.fill);
        }
        y = rule(ui, rect, y);
    }

    // Memory
    if m.ram_usage || m.ram_spec {
        block(
            ui,
            "RAM",
            if m.ram_spec { "DDR5-5800" } else { "" },
            accents.chrome.ink,
            &mut y,
        );
        if m.ram_usage {
            ui.painter().text(
                Pos2::new(x, y),
                Align2::LEFT_TOP,
                "18.4 / 32.0 GB",
                mono(12.5),
                theme::TEXT,
            );
        }
        if config.experimental.ram_live_rate {
            ui.painter().text(
                Pos2::new(rect.right() - 16.0, y),
                Align2::RIGHT_TOP,
                "2576 MT/s",
                mono(12.5),
                theme::TEXT,
            );
        }
        y += 20.0;
        if m.ram_usage {
            y = bar(ui, x, y, rect.width() - 32.0, 0.58, accents.chrome.fill);
        }
        if m.ram_spec {
            ui.painter().text(
                Pos2::new(x, y),
                Align2::LEFT_TOP,
                "32768 MB · 2 × 16384 · rated 5800 MT/s",
                mono(10.5),
                theme::MUTED,
            );
        }
    }
}

fn rule(ui: &Ui, rect: Rect, y: f32) -> f32 {
    ui.painter().line_segment(
        [
            Pos2::new(rect.left() + 16.0, y + 4.0),
            Pos2::new(rect.right() - 16.0, y + 4.0),
        ],
        Stroke::new(1.0, Color32::from_white_alpha(18)),
    );
    y + 14.0
}

/// A usage bar. A number alone does not show how close to full something is.
fn bar(ui: &Ui, x: f32, y: f32, width: f32, fraction: f32, tint: Color32) -> f32 {
    let track = Rect::from_min_size(Pos2::new(x, y), Vec2::new(width, 4.0));
    let p = ui.painter();
    p.rect_filled(track, CornerRadius::same(2), Color32::from_white_alpha(26));
    p.rect_filled(
        Rect::from_min_size(track.min, Vec2::new(width * fraction, 4.0)),
        CornerRadius::same(2),
        tint,
    );
    y + 12.0
}

/// Per-core bars. Uneven on purpose: a game loads a couple of cores hard and leaves the rest
/// idling, and a flat row of equal bars would misrepresent that.
fn cores(ui: &Ui, x: f32, y: f32, width: f32, tint: Color32) -> f32 {
    const LOADS: [f32; 16] = [
        0.88, 0.34, 0.96, 0.21, 0.63, 0.18, 0.91, 0.27, 0.44, 0.15, 0.72, 0.19, 0.38, 0.12, 0.55,
        0.24,
    ];
    let step = width / LOADS.len() as f32;
    let max_h = 16.0;
    let p = ui.painter();
    for (i, load) in LOADS.iter().enumerate() {
        let left = x + i as f32 * step;
        let h = max_h * load;
        p.rect_filled(
            Rect::from_min_size(Pos2::new(left, y + max_h - h), Vec2::new(step - 1.5, h)),
            CornerRadius::same(1),
            tint,
        );
    }
    y + max_h + 8.0
}
