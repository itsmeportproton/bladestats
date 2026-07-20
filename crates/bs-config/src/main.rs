//! bladestats.
//!
//! One executable in two roles. Run it and you get the counter plus the window that
//! configures it; run it with `--counter` and you get only the counter, which is how the
//! window starts it.
//!
//! Two processes rather than two threads, and the reason is measured rather than assumed.
//! Holding the settings window in the same process cost 1.3% of a core and 224 MB even
//! minimised, against 0.23% and 36 MB for the counter alone &mdash; the settings window's
//! toolkit stays resident whether or not anybody is looking at it. Splitting them means a
//! games-long session holds only the small half.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use bs_config::{ConfigApp, app::WINDOW_SIZE};
use bs_core::Config;

/// Tells a copy of this program to be the counter rather than the window.
const COUNTER_FLAG: &str = "--counter";

fn main() -> eframe::Result {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("BLADESTATS_LOG")
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let path = bs_core::config::default_path();

    if std::env::args().any(|a| a == COUNTER_FLAG) {
        if let Err(e) = bs_windows::overlay::run(path) {
            tracing::error!(error = %e, "counter stopped");
        }
        return Ok(());
    }

    let (config, outcome) = Config::load(&path);
    tracing::info!(path = %path.display(), ?outcome, "settings");

    // Written out on a first run so there is something to edit by hand as well. A read-only
    // directory means no file, not no counter.
    if outcome == bs_core::LoadOutcome::Missing
        && let Err(e) = config.save(&path)
    {
        tracing::warn!(error = %e, "could not write default settings");
    }

    let counter = bs_config::counter::Counter::start(COUNTER_FLAG);

    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("bladestats")
            .with_inner_size(WINDOW_SIZE)
            .with_resizable(false)
            // The window shape, its corners and its controls are all drawn by us, so the
            // system frame would only get in the way.
            .with_decorations(false)
            .with_transparent(true),
        ..Default::default()
    };

    eframe::run_native(
        "bladestats",
        options,
        Box::new(move |cc| Ok(Box::new(ConfigApp::new(cc, counter, path, config)))),
    )
}
