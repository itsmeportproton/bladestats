//! Windows telemetry backends.
//!
//! Every source here is a documented user-mode API: no kernel driver, no MSR reads, no
//! undocumented syscalls beyond `NtQuerySystemInformation`, which Task Manager itself uses.
//! That constraint is what keeps bladestats uninteresting to anti-cheats.

mod cpu;
pub mod cputemp;
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
pub fn samplers(config: &bs_core::Config) -> Vec<Box<dyn Sampler>> {
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

    // Off unless asked for. Every source of a processor temperature is a program that has
    // loaded a kernel driver to get it, and inheriting that quietly is not this program's
    // decision to make.
    if config.sensors.cpu_temp != bs_core::CpuTempChoice::Off {
        let sources: Vec<Box<dyn cputemp::CpuTempSource>> = vec![Box::new(
            cputemp::LibreHardwareMonitor::new(config.sensors.lhm_port),
        )];
        if let Some(sampler) = cputemp::CpuTempSampler::new(sources) {
            samplers.push(Box::new(sampler));
        }
    }

    samplers
}
