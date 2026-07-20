//! Windows telemetry backends.
//!
//! Every source here is a documented user-mode API: no kernel driver, no MSR reads, no
//! undocumented syscalls beyond `NtQuerySystemInformation`, which Task Manager itself uses.
//! That constraint is what keeps bladestats uninteresting to anti-cheats.

mod cpu;
mod gpu;
mod memory;
pub(crate) mod pdh;
pub(crate) mod registry;

#[cfg(feature = "nvidia")]
mod nvml;

use crate::Sampler;

/// Backends in priority order: whichever runs first owns a field, and later ones only fill
/// what is still empty.
pub fn samplers() -> Vec<Box<dyn Sampler>> {
    // `mut` is only used under the `nvidia` feature, which is off by default.
    #[allow(unused_mut)]
    let mut samplers: Vec<Box<dyn Sampler>> = vec![
        Box::new(cpu::CpuSampler::new()),
        Box::new(memory::MemorySampler::new()),
        Box::new(gpu::GpuSampler::new()),
    ];

    // NVML runs after the generic GPU backend and overwrites what it can do better, then adds
    // temperature, power and clocks that the generic path cannot reach at all.
    #[cfg(feature = "nvidia")]
    if let Some(sampler) = nvml::NvmlSampler::new() {
        samplers.push(Box::new(sampler));
    }

    samplers
}
