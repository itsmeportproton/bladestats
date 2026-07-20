//! bladestats on Windows: the overlay window, its renderer, frame timing and target tracking.
//!
//! The overlay lives in its own window on top of the game and **injects nothing**: it loads no
//! code into the game, hooks no `Present`, reads no foreign memory. That is also where its one
//! limitation comes from — it only works in borderless ("fullscreen windowed") mode.
//!
//! This is a library rather than one large binary so that the unit tests can run. The
//! executable's manifest demands administrator rights, and cargo cannot launch a test harness
//! that does; keeping the testable code here sidesteps that entirely.

pub mod etw;
pub mod renderer;
pub mod target;
pub mod window;
