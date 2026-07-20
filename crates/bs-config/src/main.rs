//! Entry point for the configurator.
//!
//! Deliberately not elevated. The overlay needs administrator rights for its trace session,
//! but editing a settings file does not, and asking for privileges that are not needed is how
//! programs teach people to click through prompts without reading them.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use bs_config::{ConfigApp, app::WINDOW_SIZE};

fn main() -> eframe::Result {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("BLADESTATS_LOG")
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

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
        Box::new(|cc| Ok(Box::new(ConfigApp::new(cc)))),
    )
}
