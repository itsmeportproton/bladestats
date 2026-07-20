//! Hardware telemetry collection.
//!
//! Each backend reports only what it can genuinely read and leaves the rest as `None`. The
//! aggregator merges them richest-first: a vendor SDK overrides the generic source, and
//! whatever it cannot supply is filled in from below.
//!
//! Backends never abort startup. A machine without NVIDIA has no NVML; a machine with locked
//! down performance counters has no PDH. Either way the overlay comes up and shows dashes for
//! what it could not read.

use std::time::Duration;

use bs_core::{MetricsSnapshot, SnapshotHub};

#[cfg(windows)]
mod windows;

/// How often hardware is sampled.
///
/// Deliberately much slower than the redraw rate: PDH queries and DXGI calls are the expensive
/// part of this program, and none of these values move meaningfully within half a second.
pub const SAMPLE_INTERVAL: Duration = Duration::from_millis(500);

/// One source of hardware metrics.
///
/// Not `Send` on purpose: several backends hold COM interfaces and PDH handles that have no
/// business crossing threads. Samplers are built inside the telemetry thread and never leave
/// it, so the bound would buy nothing and would force `unsafe impl Send` on wrappers that do
/// not deserve it.
pub trait Sampler {
    /// Human-readable name, for logs.
    fn name(&self) -> &'static str;

    /// Fills in whatever this backend can read, leaving everything else untouched.
    ///
    /// Takes `&mut MetricsSnapshot` rather than returning a value so that backends compose:
    /// they run richest-first and each one only fills gaps left by its predecessors.
    fn sample(&mut self, into: &mut MetricsSnapshot);
}

/// Builds the set of samplers appropriate for this machine.
///
/// Ordering matters: the first backend to fill a field wins, so vendor-specific sources come
/// before generic ones.
pub fn samplers() -> Vec<Box<dyn Sampler>> {
    #[cfg(windows)]
    {
        windows::samplers()
    }
    #[cfg(not(windows))]
    {
        // The Linux backends arrive with the sysfs/hwmon stage.
        Vec::new()
    }
}

/// Takes a single reading and returns it.
///
/// For callers that want to know what hardware is present without running a loop &mdash; the
/// configurator colours itself by the detected vendors and needs the answer once, at startup.
/// Rate-based readings such as load will be absent, since those need two samples to exist.
pub fn sample_once() -> MetricsSnapshot {
    let mut snapshot = MetricsSnapshot::default();
    for sampler in &mut samplers() {
        sampler.sample(&mut snapshot);
    }
    snapshot
}

/// Runs the sampling loop on its own thread, publishing into `hub`.
///
/// The thread owns the samplers; nothing else touches them. It never exits, and it never
/// propagates a sampling failure — a backend that starts failing simply stops filling its
/// fields, and the overlay shows dashes.
pub fn spawn(hub: SnapshotHub) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("bs-telemetry".into())
        .spawn(move || {
            let mut samplers = samplers();
            for s in &samplers {
                tracing::info!(backend = s.name(), "telemetry backend active");
            }
            if samplers.is_empty() {
                tracing::warn!("no telemetry backends available on this platform");
            }

            loop {
                // Built fresh each tick rather than edited in place: a backend that starts
                // failing must make its values disappear, not leave the last good reading on
                // screen forever.
                let mut snapshot = MetricsSnapshot::default();
                for sampler in &mut samplers {
                    sampler.sample(&mut snapshot);
                }

                // Frame metrics come from a different thread and must survive this swap.
                hub.update(move |current| {
                    let frames = current.frames;
                    *current = snapshot;
                    current.frames = frames;
                });

                std::thread::sleep(SAMPLE_INTERVAL);
            }
        })
        .expect("failed to spawn the telemetry thread")
}
