//! The overlay itself: the window on top of the game and the loop that keeps it current.
//!
//! Runs as its own process, started by the settings window. That separation is the reason the
//! counter stays as cheap as it does: a games-long session holds this process and nothing
//! else, while the window that configures it exists only while somebody is looking at it.
//!
//! Settings therefore arrive through the file rather than through memory. It is polled once a
//! second, which costs nothing next to the rest of this program and needs no channel between
//! two processes that otherwise never speak.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::Result;
use bs_core::{Config, LoadOutcome, SnapshotHub};
use bs_render::{GlyphAtlas, HudOptions};
use bs_render::hud::{HudState, Motion};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, MSG, PM_REMOVE, PeekMessageW, TranslateMessage, WM_QUIT,
};

use crate::renderer::Renderer;
use crate::selfstat::{self, ProfileLog, SelfStat};
use crate::window::{self, OverlayWindow};
use crate::{etw, target};

/// How often the window is pushed back on top. Games reorder the window stack when they
/// activate, and without this the overlay eventually ends up underneath.
const TOPMOST_INTERVAL: Duration = Duration::from_secs(1);

/// How often the foreground window is re-examined to decide what to report on.
const TARGET_INTERVAL: Duration = Duration::from_millis(500);

/// How often the settings file is checked for changes.
const CONFIG_POLL_INTERVAL: Duration = Duration::from_secs(1);

/// How often what the counter costs reaches the log.
const COST_REPORT_INTERVAL: Duration = Duration::from_secs(60);

/// Runs the overlay until its window is closed.
///
/// Returns an error only if the window or its renderer could not be created at all. Anything
/// the overlay can carry on without &mdash; frame timing, a sensor, a whole backend &mdash; is
/// drawn as a dash and never stops it.
pub fn run(config_path: PathBuf) -> Result<()> {
    let (mut current, outcome) = Config::load(&config_path);
    let mut config_stamp = modified_at(&config_path);
    tracing::info!(path = %config_path.display(), ?outcome, "settings");

    let mut atlas = build_atlas(current.placement.font_size)?;
    let opts = HudOptions::default();

    let hub = SnapshotHub::new();
    bs_telemetry::spawn(hub.clone());

    // Frame timing is optional by design. Without administrator rights the ETW session cannot
    // be created, and the right response is to carry on without a frame rate rather than to
    // refuse to start.
    let mut frames = None;
    let mut notice = match &outcome {
        LoadOutcome::Invalid(why) => Some(format!("config ignored: {why}")),
        _ => None,
    };
    match etw::FrameSource::start() {
        Ok(source) => frames = Some(source),
        Err(e) => {
            tracing::warn!(error = %e, "frame rate unavailable");
            notice = Some("no FPS: run as administrator".to_string());
        }
    }

    // The panel carries its own animation between frames, so it outlives any one of them.
    let mut state = HudState::new(current.clone(), opts, Motion::default());
    state.on_sample((*hub.load()).clone());
    let (_, size) = state.paint(&atlas);
    let (mut w, mut h) = (size.width.ceil() as i32, size.height.ceil() as i32);
    let (x, y) = window::corner_position(current.placement.corner, current.placement.margin, w, h);

    let overlay = OverlayWindow::new(x, y, w, h)?;
    let mut renderer = Renderer::new(overlay.hwnd, &atlas, w as u32, h as u32)?;

    let own_pid = std::process::id();
    tracing::info!(width = w, height = h, "overlay running");

    let mut redraw_interval = refresh_interval(&current);
    let mut last_draw = Instant::now() - redraw_interval;
    let mut last_topmost = Instant::now();
    let mut last_target = Instant::now() - TARGET_INTERVAL;
    let mut last_config = Instant::now();
    let mut visible = true;

    // What this process costs. Reported rather than assumed: the budget is one of the
    // project's headline claims and nothing else here would notice it being broken.
    let mut selfstat = SelfStat::new();
    let mut profile = ProfileLog::from_args();
    let mut last_cost_report = Instant::now();
    let mut context = selfstat::Context::default();

    loop {
        if pump_messages() {
            break;
        }

        if let Some(usage) = selfstat.sample() {
            profile.record(&usage, context);
            // Once a minute in the log, because this is a background fact, not an event. The
            // profile file gets every reading; the log gets enough to spot a drift.
            if last_cost_report.elapsed() >= COST_REPORT_INTERVAL || profile.is_on() {
                last_cost_report = Instant::now();
                tracing::info!(
                    cpu_pct = usage.cpu_pct,
                    private_mb = usage.private_bytes / (1024 * 1024),
                    fps = context.fps,
                    target = context.target,
                    visible = context.visible,
                    "cost"
                );
            }
        }

        // Settings changed underneath us, most likely because the settings window saved.
        if last_config.elapsed() >= CONFIG_POLL_INTERVAL {
            last_config = Instant::now();
            let stamp = modified_at(&config_path);
            if stamp != config_stamp {
                config_stamp = stamp;
                let (next, outcome) = Config::load(&config_path);
                if next.placement.font_size != current.placement.font_size {
                    atlas = build_atlas(next.placement.font_size)?;
                    renderer.set_atlas(&atlas)?;
                }
                notice = match &outcome {
                    LoadOutcome::Invalid(why) => Some(format!("config ignored: {why}")),
                    _ => notice.filter(|n| !n.starts_with("config ignored")),
                };
                current = next;
                // Handed over rather than rebuilt: the panel keeps every value where it is,
                // so dragging a slider in the settings window does not make the readings
                // re-animate from nothing on every save.
                state.set_config(current.clone());
                redraw_interval = refresh_interval(&current);
                last_draw = Instant::now() - redraw_interval;
            }
        }

        if last_topmost.elapsed() >= TOPMOST_INTERVAL {
            overlay.reassert_topmost();
            last_topmost = Instant::now();
        }

        if last_target.elapsed() >= TARGET_INTERVAL {
            last_target = Instant::now();
            if let Some(t) = target::current(own_pid) {
                context.target = Some(t.pid);
                if let Some(source) = &frames {
                    source.set_target(t.pid);
                }
                // Nothing can be composited over an exclusive-fullscreen swapchain, so the
                // overlay hides instead of sitting invisibly behind it.
                context.visible = t.overlay_possible();
                if t.overlay_possible() != visible {
                    visible = t.overlay_possible();
                    overlay.show(visible);
                    tracing::debug!(mode = ?t.mode, visible, "target changed");
                }
            }
        }

        if last_draw.elapsed() >= redraw_interval {
            let dt = last_draw.elapsed();
            last_draw = Instant::now();

            let mut snapshot = (*hub.load()).clone();
            if let Some(source) = &frames {
                snapshot.frames = source.metrics(etw::now_ns());
            }
            context.fps = snapshot.frames.as_ref().map(|f| f.fps);
            snapshot.notice = notice.clone();

            state.on_sample(snapshot);
            state.step(dt);
            let (list, size) = state.paint(&atlas);
            let (nw, nh) = (size.width.ceil() as i32, size.height.ceil() as i32);

            if (nw, nh) != (w, h) {
                (w, h) = (nw, nh);
                let (x, y) = window::corner_position(
                    current.placement.corner,
                    current.placement.margin,
                    w,
                    h,
                );
                overlay.set_bounds(x, y, w, h);
                renderer.resize(w as u32, h as u32)?;
            }
            renderer.render(&list)?;
        }

        std::thread::sleep(Duration::from_millis(5));
    }

    tracing::info!("overlay stopped");
    Ok(())
}

/// Modification time, or `None` when the file is absent or unreadable.
///
/// Both cases compare equal to themselves, so a missing config does not look like a change on
/// every poll.
fn modified_at(path: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path).ok()?.modified().ok()
}

fn refresh_interval(config: &Config) -> Duration {
    Duration::from_secs_f32(1.0 / config.placement.refresh_hz.max(1) as f32)
}

fn build_atlas(font_size: f32) -> Result<GlyphAtlas> {
    GlyphAtlas::new(bs_render::EMBEDDED_FONT, font_size)
        .map_err(|e| anyhow::anyhow!("could not build the glyph atlas: {e}"))
}

/// Drains this thread's messages. Returns `true` when it is time to stop.
fn pump_messages() -> bool {
    unsafe {
        let mut msg = MSG::default();
        while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
            if msg.message == WM_QUIT {
                return true;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    false
}
