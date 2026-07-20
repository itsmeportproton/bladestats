//! bladestats on Windows: wiring, argument handling and the main loop.
//!
//! Everything substantial lives in the library next to this file; see its documentation for
//! why the split exists.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::Result;
use bs_core::{
    Config, CoreMetrics, FrameMetrics, GpuMetrics, LoadOutcome, MetricsSnapshot, Power,
    SnapshotHub, Vendor,
};
use bs_render::{GlyphAtlas, HudOptions, hud};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, MSG, PM_REMOVE, PeekMessageW, TranslateMessage, WM_QUIT,
};

use bs_windows::renderer::Renderer;
use bs_windows::window::{self, OverlayWindow};
use bs_windows::{etw, target};

/// How often the window is pushed back on top. Games reorder the window stack when they
/// activate, and without this the overlay eventually ends up underneath.
const TOPMOST_INTERVAL: Duration = Duration::from_secs(1);

/// How often the foreground window is re-examined to decide what to report on.
const TARGET_INTERVAL: Duration = Duration::from_millis(500);

/// How often the settings file is checked for changes.
///
/// Polling rather than a filesystem watch: one `stat` a second costs nothing next to the rest
/// of this program, and it avoids a dependency and a background thread for a file that changes
/// when somebody clicks a checkbox.
const CONFIG_POLL_INTERVAL: Duration = Duration::from_secs(1);

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("BLADESTATS_LOG")
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let demo = std::env::args().any(|a| a == "--demo");
    let selftest = std::env::args().any(|a| a == "--etw-selftest");

    let config_path = bs_core::config::default_path();
    let (mut config, outcome) = Config::load(&config_path);
    report_config(&config_path, &outcome);

    // Write the defaults out on a first run so there is something to edit. A portable program
    // that keeps its settings invisible until a separate tool creates them is a program whose
    // settings nobody finds. Failure here is not fatal: a read-only directory means no file,
    // not no overlay.
    if outcome == LoadOutcome::Missing
        && let Err(e) = config.save(&config_path)
    {
        tracing::warn!(path = %config_path.display(), error = %e, "could not write default settings");
    }
    let mut config_stamp = modified_at(&config_path);

    // A parse error has to reach the screen. Somebody who edited the file by hand and got it
    // wrong is not reading a console.
    let mut notice = match &outcome {
        LoadOutcome::Invalid(why) => Some(format!("config ignored: {why}")),
        _ => None,
    };

    let mut atlas = build_atlas(config.placement.font_size)?;
    let opts = HudOptions::default();
    let hub = SnapshotHub::new();
    let mut frames = None;

    if demo {
        hub.store(demo_snapshot());
    } else {
        // Sampling runs on its own thread so a slow PDH query can never stall a redraw.
        bs_telemetry::spawn(hub.clone());

        // Frame timing is optional by design. Without administrator rights the ETW session
        // cannot be created, and the right response is to carry on without a frame rate
        // rather than to refuse to start.
        match etw::FrameSource::start() {
            Ok(source) => frames = Some(source),
            Err(e) => {
                tracing::warn!(error = %e, "frame rate unavailable");
                notice = Some("no FPS: run as administrator".to_string());
            }
        }
    }

    // The layout drives the window size, not the other way round: the HUD knows how much room
    // it needs.
    let (_, size) = hud::build(&atlas, &hub.load(), &config, &opts);
    let (mut w, mut h) = (size.width.ceil() as i32, size.height.ceil() as i32);
    let (x, y) = window::corner_position(config.placement.corner, config.placement.margin, w, h);

    let overlay = OverlayWindow::new(x, y, w, h)?;
    let mut renderer = Renderer::new(overlay.hwnd, &atlas, w as u32, h as u32)?;

    let own_pid = std::process::id();
    if selftest && let Some(source) = &frames {
        // Our own present rate is known exactly, so the reported figure can be checked
        // against a number this program controls rather than against a guess.
        source.set_target(own_pid);
        tracing::info!(pid = own_pid, "self-test: tracing our own presents");
    }

    tracing::info!(width = w, height = h, "overlay running");

    let mut redraw_interval = refresh_interval(&config);
    let mut last_draw = Instant::now() - redraw_interval;
    let mut last_topmost = Instant::now();
    let mut last_target = Instant::now() - TARGET_INTERVAL;
    let mut last_config = Instant::now();
    let mut visible = true;

    loop {
        if pump_messages() {
            break;
        }

        // Settings changed underneath us, most likely because the configurator saved.
        if last_config.elapsed() >= CONFIG_POLL_INTERVAL {
            last_config = Instant::now();
            let stamp = modified_at(&config_path);
            if stamp != config_stamp {
                config_stamp = stamp;
                let (next, outcome) = Config::load(&config_path);
                report_config(&config_path, &outcome);

                if next.placement.font_size != config.placement.font_size {
                    atlas = build_atlas(next.placement.font_size)?;
                    renderer.set_atlas(&atlas)?;
                }
                notice = match &outcome {
                    LoadOutcome::Invalid(why) => Some(format!("config ignored: {why}")),
                    _ => notice.filter(|n| !n.starts_with("config ignored")),
                };
                config = next;
                redraw_interval = refresh_interval(&config);
                // Force the next tick to redraw and reposition.
                last_draw = Instant::now() - redraw_interval;
            }
        }

        if last_topmost.elapsed() >= TOPMOST_INTERVAL {
            overlay.reassert_topmost();
            last_topmost = Instant::now();
        }

        if !selftest && last_target.elapsed() >= TARGET_INTERVAL {
            last_target = Instant::now();
            if let Some(t) = target::current(own_pid) {
                if let Some(source) = &frames {
                    source.set_target(t.pid);
                }
                // Nothing can be composited over an exclusive-fullscreen swapchain, so the
                // overlay hides instead of sitting invisibly behind it.
                if t.overlay_possible() != visible {
                    visible = t.overlay_possible();
                    overlay.show(visible);
                    tracing::debug!(mode = ?t.mode, visible, "target changed");
                }
            }
        }

        if last_draw.elapsed() >= redraw_interval {
            last_draw = Instant::now();

            let mut snapshot = (*hub.load()).clone();
            if let Some(source) = &frames {
                snapshot.frames = source.metrics(etw::now_ns());
            }
            snapshot.notice = notice.clone();

            let (list, size) = hud::build(&atlas, &snapshot, &config, &opts);
            let (nw, nh) = (size.width.ceil() as i32, size.height.ceil() as i32);

            if (nw, nh) != (w, h) {
                (w, h) = (nw, nh);
                let (x, y) =
                    window::corner_position(config.placement.corner, config.placement.margin, w, h);
                overlay.set_bounds(x, y, w, h);
                renderer.resize(w as u32, h as u32)?;
            }
            renderer.render(&list)?;
        }

        // There is nothing to do between redraws, so sleep instead of spinning on the message
        // queue.
        std::thread::sleep(Duration::from_millis(5));
    }

    Ok(())
}

fn refresh_interval(config: &Config) -> Duration {
    Duration::from_secs_f32(1.0 / config.placement.refresh_hz.max(1) as f32)
}

fn build_atlas(font_size: f32) -> Result<GlyphAtlas> {
    GlyphAtlas::new(bs_render::EMBEDDED_FONT, font_size)
        .map_err(|e| anyhow::anyhow!("could not build the glyph atlas: {e}"))
}

/// Modification time, or `None` when the file is absent or unreadable.
///
/// Both cases compare equal to themselves, so a missing config does not look like a change on
/// every poll.
fn modified_at(path: &std::path::Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path).ok()?.modified().ok()
}

fn report_config(path: &PathBuf, outcome: &LoadOutcome) {
    match outcome {
        LoadOutcome::Loaded => tracing::info!(path = %path.display(), "settings loaded"),
        LoadOutcome::Missing => {
            tracing::info!(path = %path.display(), "no settings file yet; using defaults")
        }
        LoadOutcome::Invalid(why) => {
            tracing::warn!(path = %path.display(), error = %why, "settings ignored")
        }
    }
}

/// Drains pending messages. Returns `true` when it is time to quit.
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

/// Fabricated metrics for checking the overlay's appearance without hardware access.
fn demo_snapshot() -> MetricsSnapshot {
    let mut s = MetricsSnapshot::default();
    s.cpu.name = Some("AMD Ryzen 7 7800X3D 8-Core Processor".into());
    s.cpu.load_pct = Some(42.0);
    s.cpu.power = Some(Power::Estimated(65.0));
    s.cpu.cores = (0..16)
        .map(|i| CoreMetrics {
            load_pct: [12.0, 88.0, 34.0, 95.0][i % 4],
            freq_mhz: Some(4200.0 + i as f32 * 40.0),
        })
        .collect();
    s.gpu = GpuMetrics {
        name: Some("NVIDIA GeForce RTX 4070".into()),
        vendor: Vendor::Nvidia,
        load_pct: Some(88.0),
        vram_used_bytes: Some(6_500_000_000),
        vram_total_bytes: Some(12_884_901_888),
        temp_c: Some(62.0),
        core_clock_mhz: Some(2610.0),
        power: Some(Power::Measured(145.0)),
    };
    s.memory.used_bytes = Some(19_000_000_000);
    s.memory.total_bytes = Some(34_359_738_368);
    s.memory.speed_mhz = Some(6000);
    s.frames = Some(FrameMetrics {
        fps: 144.0,
        frametime_ms: 6.9,
        avg_fps: 141.0,
        low_1pct: Some(98.0),
        low_01pct: Some(72.0),
        sample_count: 2000,
    });
    s
}
