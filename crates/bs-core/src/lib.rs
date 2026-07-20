//! Платформо-независимое ядро bladestats.
//!
//! Здесь нет ни Win32, ни Vulkan, ни sysfs — только типы метрик, арифметика таймингов кадров,
//! тема и конфиг. Всё, что знает про конкретную ОС, живёт в `bs-telemetry`, `bs-windows` и
//! `bs-linux-layer` и общается с ядром через [`MetricsSnapshot`].

pub mod frames;
pub mod hub;
pub mod snapshot;
pub mod theme;

pub use frames::{FrameMetrics, FrameTimeline};
pub use hub::SnapshotHub;
pub use snapshot::{
    CoreMetrics, CpuMetrics, GpuMetrics, MemoryMetrics, MetricsSnapshot, Power, Vendor,
};
pub use theme::{Color, Theme};
