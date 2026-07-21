//! The configurator window.
//!
//! Borderless, with its own close and minimise controls, and an opening that expands from a
//! small rounded rectangle. Layout is laid out from explicit rectangles rather than from
//! automatic stacking, because the design depends on exact positions and egui's automatic
//! layout would fight that at every step.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use bs_core::{Config, Corner, Vendor};
use egui::{
    Align2, Color32, CornerRadius, FontFamily, FontId, Pos2, Rect, Sense, Stroke, StrokeKind, Ui,
    UiBuilder, Vec2,
};

use crate::anim::Opening;
use crate::counter::Counter;
use crate::theme::{self, Accents};

/// The window at rest.
pub const WINDOW_SIZE: [f32; 2] = [580.0, 700.0];

const RADIUS: u8 = 18;
const PAD: f32 = 26.0;
const TITLEBAR_H: f32 = 38.0;
const HEADER_H: f32 = 82.0;
const STATUS_H: f32 = 46.0;
const FEATURES_W: f32 = 252.0;

/// The size the panel is designed at, so the slider can read as a percentage of it rather than
/// as a pixel count nobody has an intuition for.
const DEFAULT_FONT_PX: f32 = 16.0;

/// How long after the last edit the file is written.
///
/// Dragging a slider changes the value dozens of times a second; writing on every one of those
/// would hammer the disk and make the overlay reload constantly.
const SAVE_DELAY: Duration = Duration::from_millis(400);

/// How long "Saved" stays up.
const SAVED_NOTICE: Duration = Duration::from_secs(2);

pub struct ConfigApp {
    config: Config,
    path: PathBuf,
    accents: Accents,
    cpu_name: String,
    gpu_name: String,
    gpu_vendor: Vendor,
    /// Kept because the processor's power is a real reading on an AMD package and a model
    /// everywhere else, and the label beside it has to say which.
    cpu_vendor: Vendor,

    opening: Opening,
    /// Set when a control changes, cleared once the file is written.
    pending_save: Option<Instant>,
    saved_at: Option<Instant>,
    save_error: Option<String>,

    /// The counter process this window started, and can stop.
    counter: Counter,
}

impl ConfigApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        counter: Counter,
        path: PathBuf,
        config: Config,
    ) -> Self {
        install_font(&cc.egui_ctx);

        // One reading, purely to find out what hardware is here. The window colours itself
        // from the answer.
        let hardware = bs_telemetry::sample_once(&config);
        let cpu_vendor = hardware
            .cpu
            .name
            .as_deref()
            .map(Vendor::from_name)
            .unwrap_or_default();

        Self {
            accents: Accents::detect(cpu_vendor, hardware.gpu.vendor),
            cpu_name: hardware
                .cpu
                .name
                .unwrap_or_else(|| "Unknown processor".into()),
            gpu_name: hardware
                .gpu
                .name
                .unwrap_or_else(|| "Unknown graphics card".into()),
            gpu_vendor: hardware.gpu.vendor,
            cpu_vendor,
            config,
            path,
            opening: Opening::new(reduced_motion()),
            pending_save: None,
            saved_at: None,
            save_error: None,
            counter,
        }
    }

    /// Records that something changed, so it gets written shortly.
    ///
    /// The counter reads the file, so the write is what carries the change across. It is
    /// delayed rather than immediate because dragging a slider changes a value dozens of
    /// times a second.
    fn touch(&mut self) {
        self.pending_save = Some(Instant::now());
        self.saved_at = None;
    }

    fn save_if_due(&mut self) {
        let Some(at) = self.pending_save else { return };
        if at.elapsed() < SAVE_DELAY {
            return;
        }
        self.pending_save = None;
        match self.config.save(&self.path) {
            Ok(()) => {
                self.saved_at = Some(Instant::now());
                self.save_error = None;
            }
            Err(e) => {
                tracing::error!(path = %self.path.display(), error = %e, "could not save");
                self.save_error = Some(e.to_string());
            }
        }
    }
}

impl eframe::App for ConfigApp {
    /// Transparent, so the rounded window shape is ours to draw rather than the operating
    /// system's rectangle.
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0]
    }

    /// The given `Ui` arrives with no margin and no background: exactly right here, since the
    /// rounded window shape, its border and its controls are all ours to draw.
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.save_if_due();

        // The "saved" notice has to be forgotten once it has been up long enough, not merely
        // stop being drawn. Leaving it set kept the condition below true forever, which held
        // the window repainting at full rate for the rest of the session &mdash; four percent
        // of a core spent redrawing an unchanged window.
        if self.saved_at.is_some_and(|at| at.elapsed() >= SAVED_NOTICE) {
            self.saved_at = None;
        }

        // Repaint only while something is moving. An idle window should cost nothing.
        if !self.opening.finished() || self.pending_save.is_some() || self.saved_at.is_some() {
            ctx.request_repaint();
        }

        let size: Vec2 = self.opening.size(WINDOW_SIZE).into();
        let rect = Rect::from_center_size(ui.max_rect().center(), size);
        let radius = CornerRadius::same(RADIUS);

        let fade = self.opening.seed_opacity();
        ui.painter()
            .rect_filled(rect, radius, theme::GROUND.gamma_multiply(fade));
        ui.painter().rect_stroke(
            rect,
            radius,
            Stroke::new(1.0, theme::EDGE.gamma_multiply(fade)),
            StrokeKind::Inside,
        );

        let opacity = self.opening.content_opacity();
        if opacity > 0.0 {
            let mut content = ui.new_child(UiBuilder::new().max_rect(rect));
            content.set_opacity(opacity);
            content.set_clip_rect(rect);
            self.contents(&mut content, rect);
        }
    }
}

impl ConfigApp {
    fn contents(&mut self, ui: &mut Ui, rect: Rect) {
        let titlebar = Rect::from_min_size(rect.min, Vec2::new(rect.width(), TITLEBAR_H));
        self.title_bar(ui, titlebar);

        let header = Rect::from_min_size(
            Pos2::new(rect.left(), titlebar.bottom()),
            Vec2::new(rect.width(), HEADER_H),
        );
        self.header(ui, header);

        let status = Rect::from_min_size(
            Pos2::new(rect.left(), rect.bottom() - STATUS_H),
            Vec2::new(rect.width(), STATUS_H),
        );
        self.status_bar(ui, status);

        let body = Rect::from_min_max(
            Pos2::new(rect.left() + PAD, header.bottom()),
            Pos2::new(rect.right() - PAD, status.top()),
        );
        let features = Rect::from_min_size(body.min, Vec2::new(FEATURES_W, body.height()));
        let log = Rect::from_min_max(Pos2::new(features.right() + 22.0, body.top()), body.max);

        self.features(ui, features);
        self.release_log(ui, log);
    }

    fn title_bar(&mut self, ui: &mut Ui, rect: Rect) {
        // The whole strip drags the window, the way a title bar should.
        let drag = ui.interact(rect, ui.id().with("drag"), Sense::click_and_drag());
        if drag.drag_started() {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
        }

        let y = rect.center().y;
        if light(ui, Pos2::new(rect.left() + 20.0, y), theme::CLOSE, "Close").clicked() {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }
        if light(
            ui,
            Pos2::new(rect.left() + 40.0, y),
            theme::MINIMISE,
            "Minimise",
        )
        .clicked()
        {
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::Minimized(true));
        }
    }

    fn header(&mut self, ui: &mut Ui, rect: Rect) {
        let p = ui.painter();
        let left = rect.left() + PAD;

        // The overlay draws labels muted and values bright; the wordmark repeats that split.
        let brand = FontId::new(27.0, FontFamily::Monospace);
        let blade_w = p
            .layout_no_wrap("blade".into(), brand.clone(), theme::TEXT)
            .size()
            .x;
        p.text(
            Pos2::new(left, rect.top() + 6.0),
            Align2::LEFT_TOP,
            "blade",
            brand.clone(),
            theme::TEXT,
        );
        p.text(
            Pos2::new(left + blade_w, rect.top() + 6.0),
            Align2::LEFT_TOP,
            "stats",
            brand,
            theme::MUTED,
        );

        // Detected hardware, each part in its own vendor's colour. This is the legend for
        // every other colour in the window.
        let small = FontId::new(12.0, FontFamily::Proportional);
        let cpu_w = p
            .layout_no_wrap(self.cpu_name.clone(), small.clone(), theme::TEXT)
            .size()
            .x;
        let y = rect.top() + 46.0;
        p.text(
            Pos2::new(left, y),
            Align2::LEFT_TOP,
            &self.cpu_name,
            small.clone(),
            self.accents.cpu.ink,
        );
        p.text(
            Pos2::new(left + cpu_w + 8.0, y),
            Align2::LEFT_TOP,
            "/",
            small.clone(),
            theme::LINE,
        );
        p.text(
            Pos2::new(left + cpu_w + 18.0, y),
            Align2::LEFT_TOP,
            &self.gpu_name,
            small,
            self.accents.gpu.ink,
        );

        // Version, right-aligned.
        let right = rect.right() - PAD;
        p.text(
            Pos2::new(right, rect.top() + 8.0),
            Align2::RIGHT_TOP,
            env!("CARGO_PKG_VERSION"),
            FontId::new(13.0, FontFamily::Monospace),
            theme::TEXT,
        );
        let chip = Rect::from_min_max(
            Pos2::new(right - 78.0, rect.top() + 30.0),
            Pos2::new(right, rect.top() + 48.0),
        );
        p.rect_filled(
            chip,
            CornerRadius::same(9),
            theme::dim(self.accents.chrome.fill, 0.12),
        );
        p.rect_stroke(
            chip,
            CornerRadius::same(9),
            Stroke::new(1.0, theme::dim(self.accents.chrome.fill, 0.5)),
            StrokeKind::Inside,
        );
        p.text(
            chip.center(),
            Align2::CENTER_CENTER,
            "PRE-RELEASE",
            FontId::new(9.0, FontFamily::Monospace),
            self.accents.chrome.ink,
        );
    }

    fn features(&mut self, ui: &mut Ui, rect: Rect) {
        label(ui, Pos2::new(rect.left(), rect.top()), "FEATURES");

        let panel = Rect::from_min_max(Pos2::new(rect.left(), rect.top() + 22.0), rect.max);
        ui.painter()
            .rect_filled(panel, CornerRadius::same(14), theme::PANEL);
        ui.painter().rect_stroke(
            panel,
            CornerRadius::same(14),
            Stroke::new(1.0, theme::LINE),
            StrokeKind::Inside,
        );

        let inner = panel.shrink(1.0);
        let mut child = ui.new_child(UiBuilder::new().max_rect(inner));
        child.set_clip_rect(inner);

        egui::ScrollArea::vertical()
            // Both columns scroll, and egui derives a scroll area's identity from its
            // position in the tree. Two siblings created the same way collide, and egui says
            // so loudly across the window. Naming them keeps their scroll positions apart.
            .id_salt("features")
            .auto_shrink([false, false])
            .show(&mut child, |ui| {
                ui.add_space(4.0);
                let (cpu, gpu, chrome) = (
                    self.accents.cpu.fill,
                    self.accents.gpu.fill,
                    self.accents.chrome.fill,
                );

                group(ui, "FRAME RATE", chrome);
                let m = &mut self.config.metrics;
                let mut changed = false;
                changed |= row(ui, &mut m.fps, "FPS", "", chrome);
                changed |= row(ui, &mut m.frame_time, "Frame time", "", chrome);
                changed |= row(ui, &mut m.low_1pct, "1% low", "", chrome);
                changed |= row(ui, &mut m.low_01pct, "0.1% low", "1000 fr", chrome);

                group(ui, "PROCESSOR", cpu);
                changed |= row(ui, &mut m.cpu_name, "Model name", "", cpu);
                changed |= row(ui, &mut m.cpu_load, "Load", "", cpu);
                changed |= row(ui, &mut m.cpu_cores, "Per-core bars", "", cpu);
                changed |= row(ui, &mut m.cpu_clock, "Clock speed", "", cpu);
                // Processor temperature is the one reading with no path that avoids a kernel
                // driver, so it is the only one still labelled as needing help.
                changed |= row(ui, &mut m.cpu_temp, "Temperature", "needs monitor", cpu);
                // Power is a real reading on an AMD package and a model everywhere else, and
                // the label says which — a tilde on screen means the same thing.
                let cpu_power_note = match self.cpu_vendor {
                    Vendor::Amd => "",
                    _ => "estimated",
                };
                changed |= row(ui, &mut m.cpu_power, "Power draw", cpu_power_note, cpu);

                group(ui, "GRAPHICS", gpu);
                // The note follows the card rather than pretending every vendor is equal.
                // AMD and NVIDIA have their sensor libraries wired up; Intel does not yet.
                let sensors = match self.gpu_vendor {
                    Vendor::Nvidia | Vendor::Amd => "",
                    Vendor::Intel => "no Intel",
                    Vendor::Unknown => "no driver",
                };
                changed |= row(ui, &mut m.gpu_name, "Model name", "", gpu);
                changed |= row(ui, &mut m.gpu_load, "Load", "", gpu);
                changed |= row(ui, &mut m.gpu_clock, "Clock speed", "", gpu);
                changed |= row(ui, &mut m.gpu_vram, "VRAM", "", gpu);
                changed |= row(ui, &mut m.gpu_temp, "Temperature", sensors, gpu);
                changed |= row(ui, &mut m.gpu_hotspot, "Hotspot", sensors, gpu);
                changed |= row(ui, &mut m.gpu_fan, "Fan speed", sensors, gpu);
                changed |= row(ui, &mut m.gpu_power, "Power draw", sensors, gpu);

                group(ui, "MEMORY", chrome);
                changed |= row(ui, &mut m.ram_usage, "Usage", "", chrome);
                changed |= row(ui, &mut m.ram_spec, "Module spec", "", chrome);

                group(ui, "BEHAVIOUR", chrome);
                note(
                    ui,
                    "The panel unrolls when a game is on screen and rolls away when one \
                     stops. Ctrl+Alt+B overrides that either way when the detection is wrong.",
                );
                changed |= row(
                    ui,
                    &mut self.config.behaviour.only_in_games,
                    "Only in games",
                    "",
                    chrome,
                );

                group(ui, "EXPERIMENTAL", gpu);
                note(
                    ui,
                    "Read from what the game has loaded, not from the driver. These name what \
                     is present and cannot say how much: the scaling ratio and the count of \
                     generated frames live inside the engine. A dash means not visible, which \
                     is not the same as off.",
                );
                let x = &mut self.config.experimental;
                changed |= row(ui, &mut x.graphics_api, "Graphics API", "", gpu);
                changed |= row(
                    ui,
                    &mut x.generated_frames,
                    "Frame generation",
                    frame_gen(self.gpu_vendor),
                    gpu,
                );
                changed |= row(ui, &mut x.render_scale, "Upscaler", "name only", gpu);
                changed |= row(ui, &mut x.ram_live_rate, "Live transfer rate", "", chrome);

                group(ui, "APPEARANCE", chrome);
                changed |= self.appearance(ui, chrome);

                ui.add_space(8.0);
                if changed {
                    self.touch();
                }
            });
    }

    fn appearance(&mut self, ui: &mut Ui, tint: Color32) -> bool {
        let mut changed = false;

        // Background transparency. Only the panel fill moves; the readings stay opaque, so
        // they survive being laid over a bright frame.
        let mut alpha = self.config.theme.background.a as f32 / 255.0;
        ui.horizontal(|ui| {
            ui.add_space(14.0);
            ui.label(
                egui::RichText::new("Monitor background")
                    .color(theme::TEXT)
                    .size(13.0),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(14.0);
                ui.label(
                    egui::RichText::new(format!("{:.0}%", alpha * 100.0))
                        .color(theme::MUTED)
                        .family(FontFamily::Monospace)
                        .size(11.0),
                );
            });
        });
        ui.horizontal(|ui| {
            ui.add_space(14.0);
            ui.spacing_mut().slider_width = FEATURES_W - 44.0;
            ui.visuals_mut().selection.bg_fill = tint;
            if ui
                .add(egui::Slider::new(&mut alpha, 0.0..=1.0).show_value(false))
                .changed()
            {
                self.config.theme.background.a = (alpha * 255.0).round() as u8;
                changed = true;
            }
        });
        ui.add_space(6.0);

        // Size. One slider for the whole panel rather than one for the type: every spacing,
        // every bar and the corner radius are all stated relative to this, so the panel grows
        // as a piece instead of the text outgrowing its padding.
        let mut size = self.config.placement.font_size;
        ui.horizontal(|ui| {
            ui.add_space(14.0);
            ui.label(
                egui::RichText::new("Monitor size")
                    .color(theme::TEXT)
                    .size(13.0),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(14.0);
                ui.label(
                    egui::RichText::new(format!("{:.0}%", size / DEFAULT_FONT_PX * 100.0))
                        .color(theme::MUTED)
                        .family(FontFamily::Monospace)
                        .size(11.0),
                );
            });
        });
        ui.horizontal(|ui| {
            ui.add_space(14.0);
            ui.spacing_mut().slider_width = FEATURES_W - 44.0;
            ui.visuals_mut().selection.bg_fill = tint;
            if ui
                .add(egui::Slider::new(&mut size, 10.0..=32.0).show_value(false))
                .changed()
            {
                // Whole pixels only. The glyphs are rasterised at exactly this size, and a
                // fractional one is not sharper than the integer below it, only muddier.
                self.config.placement.font_size = size.round();
                changed = true;
            }
        });
        ui.add_space(6.0);

        changed |= row(
            ui,
            &mut self.config.theme.vendor_colors,
            "Vendor colours",
            "",
            tint,
        );

        // Where the overlay sits. Four corners, so a segmented row rather than a dropdown:
        // the whole choice is visible at once.
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            ui.add_space(14.0);
            for corner in Corner::ALL {
                let selected = self.config.placement.corner == corner;
                let fill = if selected {
                    theme::dim(tint, 0.9)
                } else {
                    theme::PANEL_HOVER
                };
                let text = if selected {
                    theme::GROUND
                } else {
                    theme::MUTED
                };
                let button = egui::Button::new(
                    egui::RichText::new(corner.label())
                        .size(11.0)
                        .family(FontFamily::Monospace)
                        .color(text),
                )
                .fill(fill)
                .corner_radius(CornerRadius::same(7));

                if ui.add(button).clicked() && !selected {
                    self.config.placement.corner = corner;
                    changed = true;
                }
            }
        });
        ui.add_space(4.0);

        changed
    }

    fn release_log(&mut self, ui: &mut Ui, rect: Rect) {
        label(ui, Pos2::new(rect.left(), rect.top()), "RELEASE LOG");

        let area = Rect::from_min_max(Pos2::new(rect.left(), rect.top() + 22.0), rect.max);
        let mut child = ui.new_child(UiBuilder::new().max_rect(area));
        child.set_clip_rect(area);

        egui::ScrollArea::vertical()
            .id_salt("release-log")
            .auto_shrink([false, false])
            .show(&mut child, |ui| {
                ui.spacing_mut().item_spacing.y = 7.0;

                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(env!("CARGO_PKG_VERSION"))
                            .family(FontFamily::Monospace)
                            .size(12.0)
                            .color(theme::TEXT),
                    );
                    ui.label(
                        egui::RichText::new("2026-07-20")
                            .family(FontFamily::Monospace)
                            .size(10.0)
                            .color(theme::FAINT),
                    );
                });

                for (lead, rest, fix) in crate::log::ENTRIES {
                    bullet(ui, lead, rest, *fix);
                }
            });
    }

    fn status_bar(&mut self, ui: &mut Ui, rect: Rect) {
        let p = ui.painter();
        p.line_segment(
            [
                Pos2::new(rect.left() + PAD, rect.top()),
                Pos2::new(rect.right() - PAD, rect.top()),
            ],
            Stroke::new(1.0, theme::LINE),
        );

        let y = rect.center().y + 2.0;
        let mono = FontId::new(11.0, FontFamily::Monospace);

        // Left: whether the counter is up. Checked each frame rather than remembered, so one
        // that has died is reported instead of assumed to be fine.
        let running = self.counter.running();
        let (dot, state, colour) = match (running, self.counter.error()) {
            (true, _) => (theme::GOOD, "Counter running".to_string(), theme::MUTED),
            (false, Some(e)) => (theme::BAD, format!("counter failed: {e}"), theme::BAD),
            (false, None) => (theme::WARN, "Counter stopped".into(), theme::MUTED),
        };
        p.circle_filled(Pos2::new(rect.left() + PAD + 3.0, y - 1.0), 3.0, dot);
        p.text(
            Pos2::new(rect.left() + PAD + 14.0, y),
            Align2::LEFT_CENTER,
            state,
            mono.clone(),
            colour,
        );

        // Right: whether the last change reached disk. Somebody who just clicked something
        // wants to know it landed.
        let (text, colour) = match (&self.save_error, self.pending_save, self.saved_at) {
            (Some(e), _, _) => (format!("not saved: {e}"), theme::BAD),
            (None, Some(_), _) => ("saving...".into(), theme::FAINT),
            (None, None, Some(at)) if at.elapsed() < SAVED_NOTICE => {
                ("saved".to_string(), theme::MUTED)
            }
            // Nothing pending, so the space says the one thing about this window that is not
            // obvious: closing it does not take the counter with it.
            _ => ("close: counter keeps running".into(), theme::FAINT),
        };
        p.text(
            Pos2::new(rect.right() - PAD, y),
            Align2::RIGHT_CENTER,
            text,
            mono,
            colour,
        );

        // Starting and stopping the counter needs somewhere to live, and it is not the red
        // light: that closes this window, which is the common action and must not take the
        // counter with it. One button that changes what it says, because there is only ever
        // one sensible thing to do to a counter you can already see the state of.
        let button = Rect::from_min_max(
            Pos2::new(rect.right() - PAD - 240.0, rect.top() + 12.0),
            Pos2::new(rect.right() - PAD - 150.0, rect.top() + 34.0),
        );
        let response = ui.interact(button, ui.id().with("counter-toggle"), Sense::click());
        let (verb, hot) = if running {
            ("stop counter", theme::BAD)
        } else {
            ("start counter", theme::GOOD)
        };
        let tint = if response.hovered() { hot } else { theme::FAINT };

        let p = ui.painter();
        p.rect_stroke(
            button,
            CornerRadius::same(6),
            Stroke::new(1.0, theme::dim(tint, 0.5)),
            StrokeKind::Inside,
        );
        p.text(
            button.center(),
            Align2::CENTER_CENTER,
            verb,
            FontId::new(10.0, FontFamily::Monospace),
            tint,
        );

        if response.clicked() {
            if running {
                self.counter.stop();
            } else {
                self.counter = crate::counter::Counter::start(crate::counter::COUNTER_FLAG);
            }
        }
        if response.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
    }
}

/// One macOS-style window control.
fn light(ui: &mut Ui, centre: Pos2, colour: Color32, tooltip: &str) -> egui::Response {
    let rect = Rect::from_center_size(centre, Vec2::splat(14.0));
    let response = ui
        .interact(rect, ui.id().with(tooltip), Sense::click())
        .on_hover_text(tooltip);
    ui.painter().circle_filled(centre, 6.0, colour);

    // The glyph appears on hover, as it does on macOS.
    if response.hovered() {
        let ink = Color32::from_black_alpha(150);
        let s = 3.0;
        if tooltip == "Close" {
            let stroke = Stroke::new(1.4, ink);
            ui.painter()
                .line_segment([centre - Vec2::splat(s), centre + Vec2::splat(s)], stroke);
            ui.painter().line_segment(
                [centre + Vec2::new(-s, s), centre + Vec2::new(s, -s)],
                stroke,
            );
        } else {
            ui.painter().line_segment(
                [centre + Vec2::new(-s, 0.0), centre + Vec2::new(s, 0.0)],
                Stroke::new(1.4, ink),
            );
        }
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response
}

fn label(ui: &Ui, at: Pos2, text: &str) {
    ui.painter().text(
        at,
        Align2::LEFT_TOP,
        text,
        FontId::new(11.0, FontFamily::Monospace),
        theme::MUTED,
    );
}

/// A section heading with the marker that says which device it belongs to.
fn group(ui: &mut Ui, text: &str, tint: Color32) {
    ui.add_space(10.0);
    ui.horizontal(|ui| {
        ui.add_space(14.0);
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(6.0), Sense::hover());
        ui.painter().rect_filled(rect, CornerRadius::same(2), tint);
        ui.add_space(1.0);
        ui.label(
            egui::RichText::new(text)
                .family(FontFamily::Monospace)
                .size(10.0)
                .color(theme::FAINT),
        );
    });
    ui.add_space(2.0);
}

fn note(ui: &mut Ui, text: &str) {
    ui.horizontal(|ui| {
        ui.add_space(14.0);
        ui.allocate_ui(Vec2::new(FEATURES_W - 30.0, 0.0), |ui| {
            ui.label(egui::RichText::new(text).size(11.0).color(theme::FAINT));
        });
    });
    ui.add_space(4.0);
}

/// One switchable reading. Returns whether it changed.
fn row(ui: &mut Ui, on: &mut bool, name: &str, hint: &str, tint: Color32) -> bool {
    let height = 26.0;
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, height), Sense::click());

    if response.hovered() {
        ui.painter()
            .rect_filled(rect, CornerRadius::ZERO, theme::PANEL_HOVER);
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }

    let box_rect = Rect::from_center_size(
        Pos2::new(rect.left() + 21.0, rect.center().y),
        Vec2::splat(15.0),
    );
    let p = ui.painter();
    if *on {
        p.rect_filled(box_rect, CornerRadius::same(4), tint);
        // A tick drawn rather than typed, so it does not depend on a glyph existing.
        let c = box_rect.center();
        let stroke = Stroke::new(2.0, theme::GROUND);
        p.line_segment([c + Vec2::new(-3.5, 0.0), c + Vec2::new(-1.0, 2.5)], stroke);
        p.line_segment([c + Vec2::new(-1.0, 2.5), c + Vec2::new(3.5, -2.5)], stroke);
    } else {
        p.rect_stroke(
            box_rect,
            CornerRadius::same(4),
            Stroke::new(1.0, Color32::from_rgb(0x3A, 0x3F, 0x47)),
            StrokeKind::Inside,
        );
    }

    p.text(
        Pos2::new(box_rect.right() + 10.0, rect.center().y),
        Align2::LEFT_CENTER,
        name,
        FontId::new(13.0, FontFamily::Proportional),
        if *on { theme::TEXT } else { theme::MUTED },
    );
    if !hint.is_empty() {
        p.text(
            Pos2::new(rect.right() - 14.0, rect.center().y),
            Align2::RIGHT_CENTER,
            hint,
            FontId::new(10.0, FontFamily::Monospace),
            theme::FAINT,
        );
    }

    if response.clicked() {
        *on = !*on;
        return true;
    }
    false
}

fn bullet(ui: &mut Ui, lead: &str, rest: &str, fix: bool) {
    ui.horizontal_top(|ui| {
        let (dot, _) = ui.allocate_exact_size(Vec2::new(9.0, 14.0), Sense::hover());
        ui.painter().circle_filled(
            Pos2::new(dot.left() + 1.0, dot.top() + 8.0),
            1.6,
            if fix {
                theme::WARN
            } else {
                Color32::from_rgb(0x3F, 0x44, 0x4C)
            },
        );
        ui.allocate_ui(Vec2::new(ui.available_width(), 0.0), |ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.horizontal_wrapped(|ui| {
                if !lead.is_empty() {
                    ui.label(egui::RichText::new(lead).size(12.5).color(theme::TEXT));
                    ui.label(egui::RichText::new(" ").size(12.5));
                }
                ui.label(egui::RichText::new(rest).size(12.5).color(theme::MUTED));
            });
        });
    });
}

fn frame_gen(vendor: Vendor) -> &'static str {
    match vendor {
        Vendor::Amd => "AFMF",
        Vendor::Nvidia => "DLSS-FG",
        Vendor::Intel => "XeSS-FG",
        Vendor::Unknown => "",
    }
}

/// Uses the overlay's own typeface for anything monospaced, so the two programs agree on what
/// a number looks like.
fn install_font(ctx: &egui::Context) {
    const JETBRAINS_MONO: &[u8] = include_bytes!("../../../assets/fonts/JetBrainsMono-Regular.ttf");

    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "jetbrains".into(),
        std::sync::Arc::new(egui::FontData::from_static(JETBRAINS_MONO)),
    );
    fonts
        .families
        .entry(FontFamily::Monospace)
        .or_default()
        .insert(0, "jetbrains".into());
    ctx.set_fonts(fonts);
}

/// Whether the system has asked for less animation.
///
/// Queried from the OS rather than assumed. Getting this wrong in the permissive direction
/// means shipping motion at somebody who explicitly asked for none; getting it wrong in the
/// other direction hides the thing the window was designed around.
#[cfg(windows)]
fn reduced_motion() -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{
        SPI_GETCLIENTAREAANIMATION, SystemParametersInfoW,
    };

    // Win32 writes a BOOL here, which is a 32-bit integer: non-zero means animations are on.
    let mut animations: i32 = 1;
    let ok = unsafe {
        SystemParametersInfoW(
            SPI_GETCLIENTAREAANIMATION,
            0,
            Some(&mut animations as *mut i32 as *mut _),
            Default::default(),
        )
    }
    .is_ok();

    // If the setting cannot be read, assume animations are wanted: that is the Windows
    // default, and the query failing says nothing about the user's preference.
    ok && animations == 0
}

#[cfg(not(windows))]
fn reduced_motion() -> bool {
    false
}
