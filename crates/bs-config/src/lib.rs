//! bladestats: the settings window, and the program that ties it to the counter.
//!
//! One executable. Starting it puts the overlay on screen and opens this window; the overlay
//! runs on its own thread and reads the same settings this window edits, so a click reaches
//! the counter at once.

pub mod anim;
pub mod app;
pub mod counter;
pub mod lhm;
pub mod log;
pub mod theme;

pub use app::ConfigApp;
