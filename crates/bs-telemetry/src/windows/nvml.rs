//! NVIDIA telemetry through NVML.
//!
//! `nvml.dll` is loaded dynamically, so a build with this feature enabled still runs on
//! machines with no NVIDIA hardware — [`NvmlSampler::new`] simply returns `None` and the
//! generic backend's readings stand.
//!
//! This is the only backend on Windows that reports GPU temperature, power and clocks from
//! real sensors. AMD and Intel need ADLX and IGCL bindings, which do not exist yet.

use bs_core::{MetricsSnapshot, Power, Vendor};
use nvml_wrapper::Nvml;
use nvml_wrapper::enum_wrappers::device::{Clock, TemperatureSensor};

use crate::Sampler;

pub struct NvmlSampler {
    nvml: Nvml,
    index: u32,
}

impl NvmlSampler {
    /// Returns `None` when NVML is absent or reports no devices — neither is an error worth
    /// surfacing, it just means this machine has no NVIDIA GPU.
    pub fn new() -> Option<Self> {
        let nvml = match Nvml::init() {
            Ok(nvml) => nvml,
            Err(e) => {
                tracing::debug!(error = %e, "NVML unavailable; no NVIDIA GPU on this machine");
                return None;
            }
        };
        match nvml.device_count() {
            Ok(0) | Err(_) => None,
            Ok(_) => Some(Self { nvml, index: 0 }),
        }
    }
}

impl Sampler for NvmlSampler {
    fn name(&self) -> &'static str {
        "gpu-nvml"
    }

    fn sample(&mut self, into: &mut MetricsSnapshot) {
        let Ok(device) = self.nvml.device_by_index(self.index) else {
            return;
        };

        into.gpu.vendor = Vendor::Nvidia;
        if let Ok(name) = device.name() {
            into.gpu.name = Some(name);
        }
        if let Ok(util) = device.utilization_rates() {
            into.gpu.load_pct = Some(util.gpu as f32);
        }
        if let Ok(mem) = device.memory_info() {
            into.gpu.vram_used_bytes = Some(mem.used);
            into.gpu.vram_total_bytes = Some(mem.total);
        }
        if let Ok(temp) = device.temperature(TemperatureSensor::Gpu) {
            into.gpu.temp_c = Some(temp as f32);
        }
        if let Ok(clock) = device.clock_info(Clock::Graphics) {
            into.gpu.core_clock_mhz = Some(clock as f32);
        }
        if let Ok(milliwatts) = device.power_usage() {
            // A real sensor reading, unlike the CPU figure, so it carries no tilde in the UI.
            into.gpu.power = Some(Power::Measured(milliwatts as f32 / 1000.0));
        }
    }
}
