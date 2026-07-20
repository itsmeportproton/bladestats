//! The bladestats overlay on Windows: the window over the game, its renderer, frame timing
//! and target tracking.
//!
//! The overlay **injects nothing**: it loads no code into the game, hooks no `Present`, reads
//! no foreign memory. That is also where its one limitation comes from &mdash; it only works
//! in borderless ("fullscreen windowed") mode.
//!
//! A library rather than a program of its own. bladestats ships as a single executable that
//! runs [`overlay::run`] in a second copy of itself, so the counter and the window that
//! configures it arrive together while the counter keeps its own small footprint.

pub mod etw;
pub mod overlay;
pub mod renderer;
pub mod selfstat;
pub mod target;
pub mod window;
