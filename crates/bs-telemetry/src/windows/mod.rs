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

#[cfg(feature = "amd")]
mod adl;
#[cfg(feature = "amd")]
mod adl_sys;

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

    // The vendor backends run after the generic GPU one and add the temperature, power and
    // clocks it cannot reach at all. Both load their library dynamically, so each simply does
    // not register on a machine without that make of card — a build carries all of them and
    // whichever fits wakes up.
    #[cfg(feature = "nvidia")]
    if let Some(sampler) = nvml::NvmlSampler::new() {
        samplers.push(Box::new(sampler));
    }

    #[cfg(feature = "amd")]
    if let Some(sampler) = adl::AdlSampler::new(gpu::primary_pci(), gpu::integrated_pci()) {
        samplers.push(Box::new(sampler));
    }

    samplers
}
