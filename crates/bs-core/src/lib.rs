//! Platform-independent core of bladestats.
//!
//! No Win32, no Vulkan, no sysfs here — only metric types, frame timing arithmetic, theme and
//! config. Everything that knows about a specific OS lives in `bs-telemetry`, `bs-windows` and
//! `bs-linux-layer`, and talks to the core through [`MetricsSnapshot`].

pub mod config;
pub mod frames;
pub mod hub;
pub mod snapshot;
pub mod theme;

pub use config::{Config, Corner, Experimental, Hotkeys, LoadOutcome, Metrics, Placement};
pub use frames::{FrameMetrics, FrameTimeline};
pub use hub::SnapshotHub;
pub use snapshot::{
    CoreMetrics, CpuMetrics, GpuMetrics, MemoryMetrics, MetricsSnapshot, Power, Vendor,
};
pub use theme::{Color, Theme};
