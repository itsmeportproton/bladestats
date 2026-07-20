//! bladestats on Windows.
//!
//! The overlay lives in its own window on top of the game and **injects nothing**: it loads no
//! code into the game, hooks no `Present`, reads no foreign memory. That is also where its one
//! limitation comes from — it only works in borderless ("fullscreen windowed") mode.

mod renderer;
mod window;

use std::time::{Duration, Instant};

use anyhow::Result;
use bs_core::{
    CoreMetrics, FrameMetrics, GpuMetrics, MetricsSnapshot, Power, SnapshotHub, Theme, Vendor,
};
use bs_render::{GlyphAtlas, HudOptions, hud};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, MSG, PM_REMOVE, PeekMessageW, TranslateMessage, WM_QUIT,
};

use crate::renderer::Renderer;
use crate::window::OverlayWindow;

/// Overlay font size. Becomes a setting once the config lands.
const FONT_PX: f32 = 16.0;

/// Redraw rate. 10 Hz: numbers on screen cannot be read faster than that anyway, and every
/// extra overlay frame is time taken from the game.
const REDRAW_INTERVAL: Duration = Duration::from_millis(100);

/// How often the window is pushed back on top. Games reorder the window stack when they
/// activate, and without this the overlay eventually ends up underneath.
const TOPMOST_INTERVAL: Duration = Duration::from_secs(1);

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("BLADESTATS_LOG")
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let demo = std::env::args().any(|a| a == "--demo");
    if demo {
        tracing::info!("demo mode: the metrics shown are fabricated");
    }

    let atlas = GlyphAtlas::new(bs_render::EMBEDDED_FONT, FONT_PX)
        .map_err(|e| anyhow::anyhow!("could not build the glyph atlas: {e}"))?;
    let theme = Theme::default();
    let opts = HudOptions::default();
    let hub = SnapshotHub::new();
    if demo {
        hub.store(demo_snapshot());
    }

    // The layout drives the window size, not the other way round: the HUD knows how much room
    // it needs.
    let (_, size) = hud::build(&atlas, &hub.load(), &theme, &opts);
    let overlay = OverlayWindow::new(32, 32, size.width.ceil() as i32, size.height.ceil() as i32)?;
    let mut renderer = Renderer::new(
        overlay.hwnd,
        &atlas,
        size.width.ceil() as u32,
        size.height.ceil() as u32,
    )?;

    tracing::info!(
        width = size.width,
        height = size.height,
        "overlay running; press Ctrl+C in this console to stop it"
    );

    let mut last_draw = Instant::now() - REDRAW_INTERVAL;
    let mut last_topmost = Instant::now();

    loop {
        if pump_messages() {
            break;
        }

        if last_topmost.elapsed() >= TOPMOST_INTERVAL {
            overlay.reassert_topmost();
            last_topmost = Instant::now();
        }

        if last_draw.elapsed() >= REDRAW_INTERVAL {
            last_draw = Instant::now();

            let snapshot = hub.load();
            let (list, size) = hud::build(&atlas, &snapshot, &theme, &opts);

            let (w, h) = (size.width.ceil() as u32, size.height.ceil() as u32);
            if renderer.size() != (w, h) {
                overlay.resize(w as i32, h as i32);
                renderer.resize(w, h)?;
            }
            renderer.render(&list)?;
        }

        // There is nothing to do between redraws, so sleep instead of spinning on the message
        // queue.
        std::thread::sleep(Duration::from_millis(5));
    }

    Ok(())
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

/// Fabricated metrics for checking the overlay's appearance while telemetry does not exist yet.
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
