//! The core of bladestats.
//!
//! No Win32 here — only metric types, frame timing arithmetic, theme and config. Everything
//! that touches the system lives in `bs-telemetry` and `bs-windows`, and talks to the core
//! through [`MetricsSnapshot`].

pub mod config;
pub mod frames;
pub mod gamewatch;
pub mod hotkey;
pub mod hub;
pub mod snapshot;
pub mod theme;

pub use config::{
    Behaviour, Config, Corner, CpuTempSource as CpuTempChoice, Experimental, Hotkeys, LoadOutcome,
    Metrics, Placement, Sensors,
};
pub use frames::{FrameMetrics, FrameTimeline};
pub use gamewatch::{GameWatch, Presence};
pub use hotkey::Hotkey;
pub use hub::SnapshotHub;
pub use snapshot::{
    CoreMetrics, CpuMetrics, GpuMetrics, MemoryMetrics, MetricsSnapshot, Power, Vendor,
};
pub use theme::{Color, Theme};
