//! CPU telemetry on Windows: per-core load, per-core clock, model name and an estimated
//! package power.

use anyhow::{Result, bail};
use bs_core::{CoreMetrics, MetricsSnapshot, Power};
use windows::Wdk::System::SystemInformation::{
    NtQuerySystemInformation, SystemProcessorPerformanceInformation,
};
use windows::Win32::System::Power::{
    CallNtPowerInformation, PROCESSOR_POWER_INFORMATION, ProcessorInformation,
};
use windows::Win32::System::SystemInformation::{GetSystemInfo, SYSTEM_INFO};

use super::pdh::{PdhCounter, PdhQuery, is_total_instance, parse_core_instance};
use crate::Sampler;

/// Actual clock as a percentage of the nominal maximum. Above 100 means the core is boosting.
///
/// This is the same derivation Task Manager uses for its "Speed" figure. Reading the clock
/// from an MSR would be more direct but needs a kernel-mode driver, which the project
/// deliberately avoids.
const CLOCK_COUNTER: &str = r"\Processor Information(*)\% Processor Performance";

/// Raw per-processor timings, straight out of `NtQuerySystemInformation`.
///
/// The kernel reports cumulative counters, so load is the ratio of deltas between two samples,
/// which is why the previous reading has to be kept.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct ProcessorPerformance {
    idle_time: i64,
    kernel_time: i64,
    user_time: i64,
    dpc_time: i64,
    interrupt_time: i64,
    interrupt_count: u32,
}

pub struct CpuSampler {
    name: Option<String>,
    logical_cores: usize,
    /// Nominal maximum clock per core, read once. Multiplied by the performance percentage to
    /// get the actual clock.
    max_mhz: Vec<u32>,
    /// Package TDP guess used for the power estimate, in watts.
    tdp_watts: Option<f32>,

    previous: Vec<ProcessorPerformance>,
    clock: Option<(PdhQuery, PdhCounter)>,
}

impl CpuSampler {
    pub fn new() -> Self {
        let logical_cores = logical_core_count();
        let max_mhz = max_clocks(logical_cores);
        let name = model_name();
        let tdp_watts = name.as_deref().and_then(estimate_tdp);

        // PDH is optional: on a locked-down machine the counters may be unavailable, and that
        // costs clock readings but nothing else.
        let clock = match open_clock_counter() {
            Ok(pair) => Some(pair),
            Err(e) => {
                tracing::warn!(error = %e, "per-core clocks unavailable; PDH counter refused");
                None
            }
        };

        Self {
            name,
            logical_cores,
            max_mhz,
            tdp_watts,
            previous: Vec::new(),
            clock,
        }
    }

    /// Per-core load from the delta between two kernel readings.
    fn load(&mut self) -> Option<Vec<f32>> {
        let current = query_processor_performance(self.logical_cores).ok()?;

        // The very first sample has no predecessor to difference against.
        if self.previous.len() != current.len() {
            self.previous = current;
            return None;
        }

        let loads = current
            .iter()
            .zip(&self.previous)
            .map(|(now, before)| {
                // KernelTime includes idle, so total elapsed is kernel + user and the busy
                // fraction is whatever was not idle.
                let idle = now.idle_time.saturating_sub(before.idle_time) as f64;
                let kernel = now.kernel_time.saturating_sub(before.kernel_time) as f64;
                let user = now.user_time.saturating_sub(before.user_time) as f64;
                let total = kernel + user;
                if total <= 0.0 {
                    return 0.0;
                }
                (((total - idle) / total) * 100.0).clamp(0.0, 100.0) as f32
            })
            .collect();

        self.previous = current;
        Some(loads)
    }

    /// Per-core clocks in MHz, indexed by logical processor.
    fn clocks(&self) -> Option<Vec<Option<f32>>> {
        let (query, counter) = self.clock.as_ref()?;
        query.collect().ok()?;
        let values = counter.values().ok()?;
        if values.is_empty() {
            return None;
        }

        let mut out = vec![None; self.logical_cores];
        for v in values {
            if is_total_instance(&v.instance) {
                continue;
            }
            let Some((group, core)) = parse_core_instance(&v.instance) else {
                continue;
            };
            // Processor groups hold up to 64 logical processors each.
            let index = (group as usize) * 64 + core as usize;
            let Some(slot) = out.get_mut(index) else {
                continue;
            };
            let base = self.max_mhz.get(index).copied().unwrap_or(0);
            if base > 0 {
                *slot = Some(base as f32 * (v.value as f32) / 100.0);
            }
        }
        Some(out)
    }
}

impl Sampler for CpuSampler {
    fn name(&self) -> &'static str {
        "cpu"
    }

    fn sample(&mut self, into: &mut MetricsSnapshot) {
        into.cpu.name = self.name.clone();

        let Some(loads) = self.load() else {
            // No delta yet: report the core count so the layout is stable, but no values.
            into.cpu.cores = vec![CoreMetrics::default(); self.logical_cores];
            return;
        };
        let clocks = self.clocks();

        into.cpu.cores = loads
            .iter()
            .enumerate()
            .map(|(i, &load_pct)| CoreMetrics {
                load_pct,
                freq_mhz: clocks.as_ref().and_then(|c| c.get(i).copied().flatten()),
            })
            .collect();

        let total = loads.iter().sum::<f32>() / loads.len().max(1) as f32;
        into.cpu.load_pct = Some(total);
        into.cpu.power = self
            .tdp_watts
            .map(|tdp| Power::Estimated(estimate_power(tdp, total)));

        // Package temperature is deliberately absent. Reading it reliably needs an MSR and
        // therefore a kernel-mode driver, which would carry more anti-cheat risk than the
        // whole of the rest of this program.
    }
}

/// A crude package power model: idle draw plus a cubic-ish rise with load.
///
/// This is an estimate and is labelled as one in the UI. The real figure lives in an MSR that
/// cannot be read from user mode. The shape matters more than the constants: power rises far
/// faster than linearly with load because voltage climbs alongside frequency.
fn estimate_power(tdp_watts: f32, load_pct: f32) -> f32 {
    let load = (load_pct / 100.0).clamp(0.0, 1.0);
    let idle = tdp_watts * 0.12;
    idle + (tdp_watts - idle) * load.powf(1.6)
}

/// Guesses a package TDP from the model name.
///
/// Deliberately coarse: without an MSR there is no way to do better, and a wrong TDP only
/// scales an already-approximate number. Returning `None` for an unrecognised part is the
/// honest outcome — the UI then shows a dash instead of a fabricated wattage.
fn estimate_tdp(name: &str) -> Option<f32> {
    let n = name.to_ascii_lowercase();
    let watts = if n.contains("ryzen 9") || n.contains("i9") {
        125.0
    } else if n.contains("ryzen 7") || n.contains("i7") {
        105.0
    } else if n.contains("ryzen 5") || n.contains("i5") || n.contains("ryzen 3") || n.contains("i3")
    {
        65.0
    } else {
        return None;
    };
    // Mobile parts run at a fraction of their desktop namesakes.
    let mobile = n.contains(" hx") || n.contains(" hs") || n.ends_with('u') || n.ends_with('h');
    Some(if mobile { watts * 0.45 } else { watts })
}

fn logical_core_count() -> usize {
    let mut info = SYSTEM_INFO::default();
    unsafe { GetSystemInfo(&mut info) };
    info.dwNumberOfProcessors as usize
}

/// Nominal maximum clock per logical processor.
fn max_clocks(cores: usize) -> Vec<u32> {
    let mut buffer = vec![PROCESSOR_POWER_INFORMATION::default(); cores.max(1)];
    let bytes = std::mem::size_of_val(buffer.as_slice()) as u32;
    let status = unsafe {
        CallNtPowerInformation(
            ProcessorInformation,
            None,
            0,
            Some(buffer.as_mut_ptr() as *mut _),
            bytes,
        )
    };
    if status.is_err() {
        tracing::warn!("CallNtPowerInformation refused; per-core clocks will be unavailable");
        return vec![0; cores];
    }
    buffer.iter().map(|p| p.MaxMhz).collect()
}

fn open_clock_counter() -> Result<(PdhQuery, PdhCounter)> {
    let query = PdhQuery::new()?;
    let counter = query.add(CLOCK_COUNTER)?;
    // Rate counters need a first collection to anchor against.
    query.collect()?;
    Ok((query, counter))
}

/// The exact model string, from the same registry value Device Manager displays.
fn model_name() -> Option<String> {
    super::registry::read_string(
        r"HARDWARE\DESCRIPTION\System\CentralProcessor\0",
        "ProcessorNameString",
    )
    .map(|s| s.trim().to_string())
    .filter(|s| !s.is_empty())
}

fn query_processor_performance(cores: usize) -> Result<Vec<ProcessorPerformance>> {
    let mut buffer = vec![ProcessorPerformance::default(); cores.max(1)];
    let bytes = std::mem::size_of_val(buffer.as_slice()) as u32;
    let mut returned = 0u32;

    let status = unsafe {
        NtQuerySystemInformation(
            SystemProcessorPerformanceInformation,
            buffer.as_mut_ptr() as *mut _,
            bytes,
            &mut returned,
        )
    };
    if status.is_err() {
        bail!("NtQuerySystemInformation failed: {status:?}");
    }

    let count = returned as usize / size_of::<ProcessorPerformance>();
    buffer.truncate(count);
    Ok(buffer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn power_estimate_rises_with_load_and_stays_within_tdp() {
        let tdp = 105.0;
        let idle = estimate_power(tdp, 0.0);
        let half = estimate_power(tdp, 50.0);
        let full = estimate_power(tdp, 100.0);

        assert!(idle > 0.0, "an idle CPU still draws power");
        assert!(idle < half && half < full, "power must rise with load");
        assert!((full - tdp).abs() < 0.01, "full load reaches the TDP");
        assert!(
            half < tdp * 0.5,
            "power rises faster than linearly, so half load is well under half the TDP"
        );
    }

    #[test]
    fn power_estimate_clamps_nonsense_load() {
        let tdp = 65.0;
        assert_eq!(estimate_power(tdp, -10.0), estimate_power(tdp, 0.0));
        assert_eq!(estimate_power(tdp, 150.0), estimate_power(tdp, 100.0));
    }

    #[test]
    fn tdp_is_guessed_for_known_families() {
        assert_eq!(
            estimate_tdp("AMD Ryzen 9 7950X 16-Core Processor"),
            Some(125.0)
        );
        assert_eq!(estimate_tdp("Intel(R) Core(TM) i5-12600K"), Some(65.0));
    }

    #[test]
    fn an_unknown_cpu_yields_no_estimate_rather_than_a_made_up_one() {
        // A dash in the UI is correct here; inventing a wattage would not be.
        assert_eq!(estimate_tdp("Some Exotic Server CPU 9000"), None);
        assert_eq!(estimate_tdp(""), None);
    }

    #[test]
    fn mobile_parts_are_derated() {
        let desktop = estimate_tdp("Intel(R) Core(TM) i7-13700K").unwrap();
        let mobile = estimate_tdp("Intel(R) Core(TM) i7-13700H").unwrap();
        assert!(
            mobile < desktop,
            "a laptop part must not be rated as a desktop one"
        );
    }

    #[test]
    fn the_machine_reports_at_least_one_core() {
        assert!(logical_core_count() >= 1);
    }

    #[test]
    fn processor_performance_matches_the_kernel_struct_layout() {
        // NtQuerySystemInformation writes into this struct directly, so its size has to match
        // SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION exactly: five LARGE_INTEGERs and a ULONG,
        // padded to 8-byte alignment.
        assert_eq!(size_of::<ProcessorPerformance>(), 48);
    }

    #[test]
    fn querying_the_kernel_returns_one_entry_per_logical_processor() {
        let cores = logical_core_count();
        let perf = query_processor_performance(cores).expect("the kernel should answer");
        assert_eq!(perf.len(), cores);
        // Cumulative counters are monotonic and non-zero on a machine that has been up.
        assert!(perf.iter().any(|p| p.idle_time > 0));
    }
}
