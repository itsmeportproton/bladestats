//! A snapshot of every metric at one instant.
//!
//! The governing rule: **a missing sensor is `None`, never zero**. "Zero watts" and "watts
//! unknown" are different facts, and the UI has to distinguish them, or the user is shown
//! plausible-looking fiction.

use crate::frames::FrameMetrics;
use crate::theme::Color;

/// Everything bladestats currently knows about the machine.
#[derive(Debug, Clone, Default)]
pub struct MetricsSnapshot {
    pub cpu: CpuMetrics,
    pub gpu: GpuMetrics,
    pub memory: MemoryMetrics,
    /// `None` while there is no frame source: on Windows without administrator rights, for
    /// instance, or when the focused window is not a game.
    pub frames: Option<FrameMetrics>,
    /// Which graphics API the target renders with: `D3D12`, `Vulkan`. Inferred rather than
    /// reported, so it stays `None` whenever the guess would be a guess.
    pub graphics_api: Option<&'static str>,
    /// Why something the user expected to see is missing.
    ///
    /// A dash says a value could not be read but not why, and "the frame rate is blank" is
    /// indistinguishable from "the program is broken" unless the reason is on screen. The log
    /// is not good enough: nobody running an overlay is watching a console.
    pub notice: Option<String>,
}

/// A power figure together with where it came from.
///
/// On Windows, CPU package power cannot be read without a kernel-mode driver, and such a
/// driver would carry more anti-cheat risk than the rest of the project combined. The figure
/// there is derived from load, clocks and TDP instead — and the UI must render it differently
/// from a real sensor reading.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Power {
    /// Read from a sensor: NVML, RAPL, hwmon.
    Measured(f32),
    /// Derived from a model. Drawn with a tilde: `~65 W`.
    Estimated(f32),
}

impl Power {
    pub fn watts(self) -> f32 {
        match self {
            Power::Measured(w) | Power::Estimated(w) => w,
        }
    }

    pub fn is_estimated(self) -> bool {
        matches!(self, Power::Estimated(_))
    }
}

#[derive(Debug, Clone, Default)]
pub struct CpuMetrics {
    /// The exact model string, as Device Manager or `/proc/cpuinfo` reports it.
    pub name: Option<String>,
    /// One entry per logical core, in the order the OS numbers them.
    pub cores: Vec<CoreMetrics>,
    /// Total load, 0.0..=100.0.
    pub load_pct: Option<f32>,
    pub temp_c: Option<f32>,
    pub power: Option<Power>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct CoreMetrics {
    /// 0.0..=100.0
    pub load_pct: f32,
    /// The actual clock, not the base clock.
    pub freq_mhz: Option<f32>,
}

#[derive(Debug, Clone, Default)]
pub struct GpuMetrics {
    /// The exact model string, as Device Manager reports it.
    pub name: Option<String>,
    pub vendor: Vendor,
    pub load_pct: Option<f32>,
    pub vram_used_bytes: Option<u64>,
    pub vram_total_bytes: Option<u64>,
    pub temp_c: Option<f32>,
    pub core_clock_mhz: Option<f32>,
    pub power: Option<Power>,
}

#[derive(Debug, Clone, Default)]
pub struct MemoryMetrics {
    pub used_bytes: Option<u64>,
    pub total_bytes: Option<u64>,
    /// The configured speed, not the maximum from SPD. Read once at startup.
    pub speed_mhz: Option<u32>,
    /// Generation, as firmware names it: `DDR5`, `LPDDR5`.
    pub kind: Option<&'static str>,
    /// What the modules are rated for, which is not what they necessarily run at — a kit
    /// rated 6000 sits at 4800 until its profile is switched on, and the difference is
    /// exactly what a user with a new machine wants to see.
    pub rated_mhz: Option<u32>,
    /// Capacity of each installed module in megabytes, in slot order. Empty when firmware
    /// would not say.
    pub modules: Vec<u32>,
    // There is no power field here and there will not be one: consumer platforms expose no
    // power sensor for memory, not in SPD, not in SMBIOS, not in hwmon.
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Vendor {
    #[default]
    Unknown,
    Amd,
    Intel,
    Nvidia,
}

impl Vendor {
    /// PCI vendor ID to vendor. Works for GPUs and for CPU host bridges alike.
    pub fn from_pci_id(id: u16) -> Self {
        match id {
            0x1002 | 0x1022 => Vendor::Amd,
            0x8086 => Vendor::Intel,
            0x10de => Vendor::Nvidia,
            _ => Vendor::Unknown,
        }
    }

    /// Crude name matching, for sources that only hand over a string: DXGI adapter
    /// descriptions, `/proc/cpuinfo` and the like.
    pub fn from_name(name: &str) -> Self {
        let n = name.to_ascii_lowercase();
        if n.contains("nvidia") || n.contains("geforce") || n.contains("quadro") {
            Vendor::Nvidia
        } else if n.contains("amd") || n.contains("radeon") || n.contains("ryzen") {
            Vendor::Amd
        } else if n.contains("intel") || n.contains("arc") {
            Vendor::Intel
        } else {
            Vendor::Unknown
        }
    }

    /// The vendor's brand colour, used when `vendor_colors` is enabled in the config.
    pub fn color(self) -> Option<Color> {
        match self {
            Vendor::Amd => Some(Color::rgb(0xED, 0x1C, 0x24)),
            Vendor::Intel => Some(Color::rgb(0x00, 0x71, 0xC5)),
            Vendor::Nvidia => Some(Color::rgb(0x76, 0xB9, 0x00)),
            Vendor::Unknown => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn power_keeps_its_provenance() {
        assert!(Power::Estimated(65.0).is_estimated());
        assert!(!Power::Measured(65.0).is_estimated());
        // The same number from different sources is not the same value.
        assert_ne!(Power::Estimated(65.0), Power::Measured(65.0));
    }

    #[test]
    fn vendor_from_name_covers_common_marketing_names() {
        assert_eq!(Vendor::from_name("NVIDIA GeForce RTX 4070"), Vendor::Nvidia);
        assert_eq!(Vendor::from_name("AMD Radeon RX 7800 XT"), Vendor::Amd);
        assert_eq!(Vendor::from_name("Intel(R) Arc(TM) A770"), Vendor::Intel);
        assert_eq!(
            Vendor::from_name("Microsoft Basic Render Driver"),
            Vendor::Unknown
        );
    }

    #[test]
    fn unknown_vendor_has_no_colour_so_ui_falls_back_to_theme() {
        assert!(Vendor::Unknown.color().is_none());
        assert!(Vendor::Nvidia.color().is_some());
    }
}
