//! GPU telemetry on Windows, vendor-agnostic.
//!
//! Load comes from PDH's `GPU Engine` counters and VRAM from DXGI. Neither needs a vendor SDK,
//! neither needs administrator rights, and both work on AMD, Intel and NVIDIA alike.
//!
//! What this backend cannot supply is temperature, power and clocks — those live behind
//! vendor SDKs. NVML fills them in on NVIDIA; on AMD and Intel they stay as dashes until
//! ADLX and IGCL bindings exist.

use anyhow::{Result, anyhow};
use bs_core::{MetricsSnapshot, Vendor};
use windows::Win32::Foundation::LUID;
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_ADAPTER_FLAG, DXGI_ADAPTER_FLAG_SOFTWARE, IDXGIFactory1,
};

use super::pdh::{PdhCounter, PdhQuery, sum_matching};
use crate::Sampler;

/// Per-process, per-engine GPU utilisation. Summing the 3D engine instances gives the figure
/// Task Manager shows.
const ENGINE_COUNTER: &str = r"\GPU Engine(*)\Utilization Percentage";

/// System-wide dedicated VRAM in use, per adapter.
///
/// `IDXGIAdapter3::QueryVideoMemoryInfo` looks like the obvious choice here and is wrong: it
/// reports the *calling process's* video memory budget and usage, so bladestats would report
/// its own handful of megabytes and call it the card's VRAM usage. This counter is what Task
/// Manager reads.
const ADAPTER_MEMORY_COUNTER: &str = r"\GPU Adapter Memory(*)\Dedicated Usage";

/// The engine type that corresponds to actual rendering work.
///
/// Instance names look like `pid_1234_luid_0x…_phys_0_eng_0_engtype_3D`. Copy, video decode
/// and encode engines have their own instances, and counting them as "GPU load" would inflate
/// the number whenever a browser plays a video.
const ENGINE_3D_SUFFIX: &str = "engtype_3D";

pub struct GpuSampler {
    name: Option<String>,
    vendor: Vendor,
    vram_total: Option<u64>,
    /// Identifies our adapter's counter instances, e.g. `luid_0x00000000_0x0000C51E`.
    luid_prefix: Option<String>,
    counters: Option<GpuCounters>,
}

struct GpuCounters {
    query: PdhQuery,
    engine: PdhCounter,
    memory: PdhCounter,
}

impl GpuSampler {
    pub fn new() -> Self {
        let (name, vendor, vram_total, luid_prefix) = match primary_adapter() {
            Ok(desc) => (
                Some(desc.name),
                desc.vendor,
                Some(desc.vram_bytes),
                Some(desc.luid_prefix),
            ),
            Err(e) => {
                tracing::warn!(error = %e, "no DXGI adapter; GPU name and VRAM unavailable");
                (None, Vendor::Unknown, None, None)
            }
        };

        let counters = match open_counters() {
            Ok(c) => Some(c),
            Err(e) => {
                tracing::warn!(error = %e, "GPU load and VRAM usage unavailable; PDH refused");
                None
            }
        };

        Self {
            name,
            vendor,
            vram_total,
            luid_prefix,
            counters,
        }
    }

    /// Keeps only the counter instances belonging to our adapter.
    ///
    /// Without this, a laptop would add its integrated GPU's engines to its discrete GPU's and
    /// report the sum as one number.
    fn is_ours(&self, instance: &str) -> bool {
        match &self.luid_prefix {
            // Compared case-insensitively on purpose. Windows spells the hex digits in upper
            // case today, but the instance name format is undocumented, and a casing change
            // would silently reduce every GPU reading to zero rather than fail loudly.
            Some(prefix) => instance.to_ascii_lowercase().contains(prefix.as_str()),
            // LUID unknown: counting every adapter is a better guess than counting none.
            None => true,
        }
    }

    fn load(&self, counters: &GpuCounters) -> Option<f32> {
        let values = counters.engine.values().ok()?;
        let total = sum_matching(&values, |i| {
            i.ends_with(ENGINE_3D_SUFFIX) && self.is_ours(i)
        })?;
        // Engines run in parallel, so the naive sum can exceed 100 on a busy machine.
        Some((total as f32).clamp(0.0, 100.0))
    }

    fn vram_used(&self, counters: &GpuCounters) -> Option<u64> {
        let values = counters.memory.values().ok()?;
        let total = sum_matching(&values, |i| self.is_ours(i))?;
        // Genuinely zero dedicated usage does not happen on a running system, so a zero here
        // means the counter had nothing to say — which is a dash, not a zero.
        (total > 0.0).then_some(total as u64)
    }
}

impl Sampler for GpuSampler {
    fn name(&self) -> &'static str {
        "gpu-generic"
    }

    fn sample(&mut self, into: &mut MetricsSnapshot) {
        into.gpu.name = self.name.clone();
        into.gpu.vendor = self.vendor;
        into.gpu.vram_total_bytes = self.vram_total;

        if let Some(counters) = &self.counters {
            // One collection feeds both counters; they share a query for exactly that reason.
            if counters.query.collect().is_ok() {
                into.gpu.load_pct = self.load(counters);
                into.gpu.vram_used_bytes = self.vram_used(counters);
            }
        }

        // Temperature, power and clocks are left untouched: this backend genuinely cannot
        // read them, and a vendor backend may fill them in afterwards.
    }
}

struct AdapterDesc {
    name: String,
    vendor: Vendor,
    vram_bytes: u64,
    luid_prefix: String,
    #[cfg_attr(not(feature = "amd"), allow(dead_code))]
    pci: super::adl::PciId,
}

/// Which card the vendor backends should attach themselves to.
///
/// Exposed so they follow the choice made here rather than making their own. On a machine with
/// an integrated Radeon beside a discrete one, two independent "pick the AMD card" heuristics
/// are two chances to disagree, and the symptom would be an overlay reporting one card's load
/// beside the other card's temperature.
#[cfg(feature = "amd")]
pub(crate) fn primary_pci() -> Option<super::adl::PciId> {
    primary_adapter().ok().map(|a| a.pci)
}

/// Picks the adapter the games will actually run on: the one with the most dedicated VRAM.
///
/// A laptop reports both its integrated and discrete GPUs, and software adapters (the
/// Microsoft Basic Render Driver, WARP) show up too and must be skipped.
fn primary_adapter() -> Result<AdapterDesc> {
    unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1()?;
        let mut best: Option<AdapterDesc> = None;

        for index in 0.. {
            let Ok(adapter) = factory.EnumAdapters1(index) else {
                break;
            };
            let Ok(desc) = adapter.GetDesc1() else {
                continue;
            };
            if DXGI_ADAPTER_FLAG(desc.Flags as i32) == DXGI_ADAPTER_FLAG_SOFTWARE {
                continue;
            }

            let name = String::from_utf16_lossy(&desc.Description)
                .trim_end_matches('\0')
                .trim()
                .to_string();
            let candidate = AdapterDesc {
                vendor: Vendor::from_pci_id(desc.VendorId as u16),
                name,
                vram_bytes: desc.DedicatedVideoMemory as u64,
                luid_prefix: luid_prefix(desc.AdapterLuid),
                pci: super::adl::PciId {
                    vendor: desc.VendorId as u16,
                    device: desc.DeviceId as u16,
                },
            };

            if best
                .as_ref()
                .is_none_or(|b| candidate.vram_bytes > b.vram_bytes)
            {
                best = Some(candidate);
            }
        }

        best.ok_or_else(|| anyhow!("no hardware DXGI adapter found"))
    }
}

/// Formats an adapter LUID the way PDH spells it in counter instance names.
///
/// Instances read `..._luid_0x00000000_0x00015A34_phys_0...`: the high part first, then the
/// low part, each eight hex digits. Windows emits upper case; this returns lower case and
/// [`GpuSampler::is_ours`] folds both sides before comparing.
fn luid_prefix(luid: LUID) -> String {
    format!("luid_0x{:08x}_0x{:08x}", luid.HighPart as u32, luid.LowPart)
}

fn open_counters() -> Result<GpuCounters> {
    let query = PdhQuery::new()?;
    let engine = query.add(ENGINE_COUNTER)?;
    let memory = query.add(ADAPTER_MEMORY_COUNTER)?;
    // Rate counters need a first collection to anchor against.
    query.collect()?;
    Ok(GpuCounters {
        query,
        engine,
        memory,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_a_real_adapter_on_this_machine() {
        let desc = primary_adapter().expect("a machine running this has a GPU");
        assert!(!desc.name.is_empty(), "the adapter must report a name");
        assert!(
            !desc.name.contains("Basic Render"),
            "software adapters must be filtered out, got {:?}",
            desc.name
        );
        // Cross-check the name against Device Manager if it ever looks wrong.
        eprintln!("adapter: {:?} vendor {:?}", desc.name, desc.vendor);
    }

    fn sampler_for(luid: LUID) -> GpuSampler {
        GpuSampler {
            name: None,
            vendor: Vendor::Unknown,
            vram_total: None,
            luid_prefix: Some(luid_prefix(luid)),
            counters: None,
        }
    }

    #[test]
    fn formats_a_luid_the_way_pdh_spells_it() {
        let luid = LUID {
            LowPart: 0x0001_5A34,
            HighPart: 0,
        };
        assert_eq!(luid_prefix(luid), "luid_0x00000000_0x00015a34");
    }

    /// Windows writes these hex digits in upper case, which cost an evening the first time.
    /// A case-sensitive match silently zeroed every GPU metric instead of failing.
    #[test]
    fn matches_instance_names_regardless_of_hex_casing() {
        let sampler = sampler_for(LUID {
            LowPart: 0x0001_5A34,
            HighPart: 0,
        });

        assert!(
            sampler.is_ours(
                "\\GPU Adapter Memory(luid_0x00000000_0x00015A34_phys_0)\\Dedicated Usage"
            )
        );
        assert!(sampler.is_ours("pid_10356_luid_0x00000000_0x00015a34_phys_0_eng_0_engtype_3D"));
    }

    #[test]
    fn ignores_other_adapters_on_the_same_machine() {
        let sampler = sampler_for(LUID {
            LowPart: 0x0001_5A34,
            HighPart: 0,
        });
        // A second adapter, as reported alongside ours on a multi-GPU machine.
        assert!(!sampler.is_ours("pid_10356_luid_0x00000000_0x00018923_phys_0_eng_0_engtype_3D"));
    }

    #[test]
    fn reports_system_wide_vram_not_this_processes_own_usage() {
        let mut sampler = GpuSampler::new();
        let mut snapshot = MetricsSnapshot::default();
        // Rate counters need two collections separated in time before they say anything.
        sampler.sample(&mut snapshot);
        std::thread::sleep(std::time::Duration::from_millis(200));
        sampler.sample(&mut snapshot);

        let Some(total) = snapshot.gpu.vram_total_bytes else {
            return;
        };
        match snapshot.gpu.vram_used_bytes {
            Some(used) => {
                assert!(used <= total, "VRAM in use cannot exceed the total");
                // The whole point of using PDH here: a desktop session always has hundreds of
                // megabytes of dedicated VRAM committed. Anything near zero would mean we had
                // gone back to reporting our own process's budget.
                assert!(
                    used > 64 * 1024 * 1024,
                    "implausibly low VRAM usage ({used} bytes) — is this our own process again?"
                );
            }
            None => eprintln!("skipped: GPU adapter memory counter unavailable"),
        }
    }

    #[test]
    fn leaves_sensor_only_fields_alone_for_a_vendor_backend_to_fill() {
        let mut snapshot = MetricsSnapshot::default();
        GpuSampler::new().sample(&mut snapshot);

        assert!(
            snapshot.gpu.temp_c.is_none(),
            "this backend cannot read temperature"
        );
        assert!(
            snapshot.gpu.power.is_none(),
            "this backend cannot read power"
        );
        assert!(
            snapshot.gpu.core_clock_mhz.is_none(),
            "this backend cannot read clocks"
        );
    }
}
