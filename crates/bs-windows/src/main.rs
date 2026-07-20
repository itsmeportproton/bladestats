//! bladestats под Windows.
//!
//! Оверлей живёт в собственном окне поверх игры и **не делает инжект**: он не грузит в игру
//! ничего, не хукает `Present` и не читает чужую память. Отсюда же его ограничение — работа
//! только в режиме «полноэкранный в окне».

// Консольное окно не нужно, но при запуске с `--demo` или при ошибке хочется видеть вывод,
// поэтому подсистема остаётся консольной до появления конфига и логов в файл.

mod renderer;
mod window;

use std::time::{Duration, Instant};

use anyhow::Result;
use bs_core::{
    CoreMetrics, FrameMetrics, GpuMetrics, MetricsSnapshot, Power, SnapshotHub, Theme, Vendor,
};
use bs_render::{GlyphAtlas, HudOptions, hud};
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, MSG, PM_REMOVE, PeekMessageW, TranslateMessage, WM_QUIT,
};

use crate::renderer::Renderer;
use crate::window::OverlayWindow;

/// Кегль шрифта оверлея. Станет настройкой на этапе конфига.
const FONT_PX: f32 = 16.0;

/// Как часто перерисовываем. 10 Гц: цифры на экране всё равно не читаются быстрее,
/// а каждый лишний кадр оверлея — это отнятое у игры время.
const REDRAW_INTERVAL: Duration = Duration::from_millis(100);

/// Как часто возвращаем окно наверх. Игра перебивает порядок окон при активации,
/// и без этого оверлей со временем уезжает под неё.
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
        tracing::info!("режим демонстрации: показываются выдуманные метрики");
    }

    let atlas = GlyphAtlas::new(bs_render::EMBEDDED_FONT, FONT_PX)
        .map_err(|e| anyhow::anyhow!("не собрался глиф-атлас: {e}"))?;
    let theme = Theme::default();
    let opts = HudOptions::default();
    let hub = SnapshotHub::new();
    if demo {
        hub.store(demo_snapshot());
    }

    // Размер окна задаётся раскладкой, а не наоборот: HUD знает, сколько ему нужно.
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
        "оверлей запущен; закрыть — Ctrl+C в этом окне"
    );

    let mut last_draw = Instant::now() - REDRAW_INTERVAL;
    let mut last_topmost = Instant::now();

    loop {
        if pump_messages(overlay.hwnd) {
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

        // Между перерисовками процессу делать нечего: спим до следующего тика вместо
        // холостого опроса очереди сообщений.
        std::thread::sleep(Duration::from_millis(5));
    }

    Ok(())
}

/// Разбирает накопившиеся сообщения. Возвращает `true`, если пора выходить.
fn pump_messages(_hwnd: HWND) -> bool {
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

/// Выдуманные метрики для проверки внешнего вида, пока телеметрии ещё нет.
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
