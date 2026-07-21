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
use bs_core::{Config, GameWatch, LoadOutcome, Orientation, Presence, SnapshotHub};
use bs_render::{GlyphAtlas, HudOptions};
use bs_render::hud::{HudSize, HudState, Motion};
use windows::Win32::Foundation::HANDLE;
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, MSG, MWMO_INPUTAVAILABLE, MsgWaitForMultipleObjectsEx, PM_REMOVE,
    PeekMessageW, QS_ALLINPUT, TranslateMessage, WM_HOTKEY, WM_QUIT,
};

use crate::renderer::Renderer;
use crate::selfstat::{self, ProfileLog, SelfStat};
use crate::window::{self, OverlayWindow};
use crate::{etw, hotkeys, target};

/// How often the window is pushed back on top. Games reorder the window stack when they
/// activate, and without this the overlay eventually ends up underneath.
const TOPMOST_INTERVAL: Duration = Duration::from_secs(1);

/// How often the foreground window is re-examined to decide what to report on.
const TARGET_INTERVAL: Duration = Duration::from_millis(500);

/// How often the settings file is checked for changes.
const CONFIG_POLL_INTERVAL: Duration = Duration::from_secs(1);

/// How often what the counter costs reaches the log.
const COST_REPORT_INTERVAL: Duration = Duration::from_secs(60);

/// How often fresh readings are picked up from the sampling thread.
///
/// Faster than the hardware is sampled, because the frame rate is derived here rather than on
/// that thread and it is the one reading that visibly moves. Everything else simply repeats a
/// value it already had, which costs a comparison.
const SAMPLE_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// How often a covered window checks whether it has been uncovered.
const OCCLUSION_POLL_INTERVAL: Duration = Duration::from_millis(200);

/// Longest the loop will ever wait without looking around, so a missed wake-up costs a fraction
/// of a second rather than a session.
const IDLE_WAIT_CAP: Duration = Duration::from_millis(500);

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
    // The sampler set depends on the settings — a processor temperature is only read when the
    // user has asked for it — so the thread is given them at birth. Changing that choice takes
    // a restart of the counter, which the settings window can do with one button.
    bs_telemetry::spawn(hub.clone(), current.clone());

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
    let (_, size, settled) = state.paint(&atlas);
    let (mut w, mut h) = (size.width.ceil() as i32, size.height.ceil() as i32);
    let (x, y) = position(&current, w, h, settled);

    let overlay = OverlayWindow::new(x, y, w, h)?;
    let mut renderer = Renderer::new(overlay.hwnd, &atlas, w as u32, h as u32)?;

    let own_pid = std::process::id();
    tracing::info!(width = w, height = h, "overlay running");

    // While something is moving the display sets the pace. This is the floor underneath that:
    // how often the panel is redrawn when nothing is animating at all, which is what catches
    // the changes no animation reports — a device name arriving, a notice appearing, a block
    // switched on in the settings.
    let mut redraw_interval = refresh_interval(&current);
    let mut last_draw = Instant::now() - redraw_interval;
    let sample_interval = SAMPLE_POLL_INTERVAL;
    let mut last_sample = Instant::now() - sample_interval;
    let mut last_topmost = Instant::now();
    let mut last_target = Instant::now() - TARGET_INTERVAL;
    let mut last_config = Instant::now();
    // Whether the window is up. Lags `wanted_visible` on the way down by however long the
    // panel takes to roll shut.
    let mut visible = false;
    let mut wanted_visible = false;

    // Whether a game is on screen, and the user's override of that answer. `None` means follow
    // the detection; the override is dropped as soon as the detection changes its mind, so a
    // shortcut pressed during one game does not silently govern the next.
    let mut watch = GameWatch::new();
    let mut detected = false;
    let mut forced: Option<bool> = None;
    let _hotkeys = hotkeys::Hotkeys::register(&current.hotkeys);
    let mut watched_pid = None;
    let mut graphics_api = None;

    // What this process costs. Reported rather than assumed: the budget is one of the
    // project's headline claims and nothing else here would notice it being broken.
    let mut selfstat = SelfStat::new();
    let mut profile = ProfileLog::from_args();
    let mut last_cost_report = Instant::now();
    let mut context = selfstat::Context::default();

    loop {
        let pumped = pump_messages();
        if pumped.quit {
            break;
        }
        if pumped.toggle {
            // Flips whatever is on screen now and pins it, overriding the detection in
            // whichever direction that has got it wrong. This escape hatch is what makes
            // hiding by default safe to ship: a game the detection misses costs one keypress.
            forced = Some(!forced.unwrap_or(detected));
            tracing::info!(forced = forced.unwrap(), "toggled");
        }
        if pumped.reload {
            // Makes the next poll see a difference, whatever the file's timestamp says.
            config_stamp = None;
            last_config = Instant::now() - CONFIG_POLL_INTERVAL;
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
                // A different process in front means the previous one's history says nothing
                // about this one.
                if watched_pid != Some(t.pid) {
                    watched_pid = Some(t.pid);
                    watch.reset();
                    // Looked up once per game rather than on a timer: a process does not
                    // change which graphics API it renders with while it is running.
                    graphics_api = target::graphics_api(t.pid);
                    tracing::debug!(pid = t.pid, ?graphics_api, "new target");
                }

                let was = detected;
                detected = watch.update(
                    etw::now_ns(),
                    t.mode == target::DisplayMode::Borderless,
                    context.fps,
                ) == Presence::Playing;
                if detected != was {
                    // The detection has changed its mind, so an override from before it did is
                    // stale. Auto resumes on its own rather than needing to be switched back.
                    forced = None;
                    tracing::debug!(detected, "game detection");
                }

                // Nothing can be composited over an exclusive-fullscreen swapchain, so the
                // overlay hides there whatever else says otherwise.
                let wanted = if current.behaviour.only_in_games {
                    forced.unwrap_or(detected)
                } else {
                    forced.unwrap_or(true)
                };
                let show = wanted && t.overlay_possible();

                context.visible = show;
                if show != wanted_visible {
                    wanted_visible = show;
                    // The window is not taken down here. It has to stay up for the whole of
                    // the panel rolling shut, or there would be nothing on screen to animate;
                    // the loop below hides it once the animation has actually finished.
                    state.set_revealed(show);
                    if show {
                        visible = true;
                        overlay.show(true);
                    }
                    tracing::debug!(mode = ?t.mode, show, detected, "visibility");
                }

                // Keyed on the game rather than on whether the panel is up. Forced on at the
                // desktop, the panel is something to look at with a pointer still in hand.
                overlay.hide_cursor(detected && visible);
            }
        }

        // Readings arrive on their own schedule, far slower than frames. Keeping the two apart
        // is the whole point: the panel is redrawn to animate, not because anything new was
        // read.
        if last_sample.elapsed() >= sample_interval {
            last_sample = Instant::now();
            let mut snapshot = (*hub.load()).clone();
            if let Some(source) = &frames {
                snapshot.frames = source.metrics(etw::now_ns());
            }
            context.fps = snapshot.frames.as_ref().map(|f| f.fps);
            snapshot.graphics_api = graphics_api;
            snapshot.notice = notice.clone();
            state.on_sample(snapshot);
        }

        let dt = last_draw.elapsed();
        let animating = state.step(dt);
        let due = dt >= redraw_interval;

        if visible && !renderer.is_occluded() && (animating || due) {
            last_draw = Instant::now();
            let (list, size, settled) = state.paint(&atlas);
            let (nw, nh) = (size.width.ceil() as i32, size.height.ceil() as i32);

            if (nw, nh) != (w, h) {
                (w, h) = (nw, nh);
                let (x, y) = position(&current, w, h, settled);
                overlay.set_bounds(x, y, w, h);
                renderer.resize(w as u32, h as u32)?;
            }
            renderer.render(&list)?;
        }

        // Down only once the rolling has finished, so the last frame of the animation is seen.
        if visible && !wanted_visible && state.is_hidden() {
            visible = false;
            overlay.show(false);
            overlay.hide_cursor(false);
        }

        // Nothing here spins and nothing here sleeps blindly. When the panel is moving the
        // wait is on the swapchain's frame-latency object, which paces the loop to the display
        // exactly; when it is still, the wait is on the message queue with a timeout set by
        // whichever piece of housekeeping is due next. On a desktop with nothing animating
        // that comes to a handful of wakeups a second instead of two hundred.
        let now = Instant::now();
        let mut timeout = [
            remaining(now, last_config, CONFIG_POLL_INTERVAL),
            remaining(now, last_topmost, TOPMOST_INTERVAL),
            remaining(now, last_target, TARGET_INTERVAL),
            remaining(now, last_sample, sample_interval),
            remaining(now, last_draw, redraw_interval),
        ]
        .into_iter()
        .min()
        .unwrap_or(IDLE_WAIT_CAP)
        .min(IDLE_WAIT_CAP);

        let drawing = visible && !renderer.is_occluded();
        if renderer.is_occluded() {
            timeout = timeout.min(OCCLUSION_POLL_INTERVAL);
        }

        let waitable = (animating && drawing).then(|| renderer.frame_latency());
        wait_for_work(waitable, timeout);

        if renderer.is_occluded() {
            renderer.poll_occlusion();
        }
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

/// Where the window belongs for a panel this size.
///
/// `settled` is the size the panel is heading towards, and it is what a strip has to be placed
/// against: the two sketches this mode came from show the bar opening outwards from its centre,
/// which only holds if the centre stays put while the width changes.
fn position(config: &Config, w: i32, h: i32, settled: HudSize) -> (i32, i32) {
    let p = &config.placement;
    match p.orientation {
        Orientation::Horizontal => {
            window::strip_position(p.corner, p.margin, w, h, settled.width.ceil() as i32)
        }
        Orientation::Vertical => window::corner_position(p.corner, p.margin, w, h),
    }
}

fn refresh_interval(config: &Config) -> Duration {
    Duration::from_secs_f32(1.0 / config.placement.refresh_hz.max(1) as f32)
}

fn build_atlas(font_size: f32) -> Result<GlyphAtlas> {
    GlyphAtlas::new(bs_render::EMBEDDED_FONT, font_size)
        .map_err(|e| anyhow::anyhow!("could not build the glyph atlas: {e}"))
}

/// How long until `last + period`, or nothing if it is already due.
fn remaining(now: Instant, last: Instant, period: Duration) -> Duration {
    period.saturating_sub(now.saturating_duration_since(last))
}

/// Waits for the next thing worth waking up for.
///
/// `handle`, when given, is the swapchain's frame-latency object: waiting on it is what paces
/// the overlay to the display. The alternative — presenting with a vsync interval — parks the
/// thread inside the graphics driver, where it cannot answer a window message, put itself back
/// on top, or notice the settings file changing. The message queue is watched either way, so
/// the window stays responsive to the system even while nothing is being drawn.
fn wait_for_work(handle: Option<HANDLE>, timeout: Duration) {
    let ms = timeout.as_millis().min(u32::MAX as u128) as u32;
    let handles = handle.map(|h| [h]);
    unsafe {
        MsgWaitForMultipleObjectsEx(
            handles.as_ref().map(|h| h.as_slice()),
            ms,
            QS_ALLINPUT,
            MWMO_INPUTAVAILABLE,
        );
    }
}

/// What came out of the message queue this time round.
#[derive(Debug, Default)]
struct Pumped {
    quit: bool,
    toggle: bool,
    reload: bool,
}

/// Drains this thread's messages, collecting the shortcuts among them.
fn pump_messages() -> Pumped {
    let mut out = Pumped::default();
    unsafe {
        let mut msg = MSG::default();
        while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
            match msg.message {
                WM_QUIT => {
                    out.quit = true;
                    return out;
                }
                // Thread-targeted, so it arrives here rather than at the window procedure —
                // which is where it needs to be, since acting on it means touching the loop's
                // own state.
                WM_HOTKEY => match hotkeys::Hotkeys::action(msg.wParam.0 as i32) {
                    Some(hotkeys::Action::Toggle) => out.toggle = true,
                    Some(hotkeys::Action::Reload) => out.reload = true,
                    None => {}
                },
                _ => {}
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    out
}
