//! The bladestats configurator.
//!
//! A separate program from the overlay, launched when wanted and closed again. It edits the
//! settings file; the overlay watches that file and applies changes about a second later.
//! Nothing else passes between them, which is why the overlay stays as small as it is: none of
//! this window's weight is loaded while a game is running.

pub mod anim;
pub mod app;
pub mod log;
pub mod monitor;
pub mod theme;

pub use app::ConfigApp;
