//! Снимок всех метрик в один момент времени.
//!
//! Ключевое соглашение: **отсутствующий сенсор — это `None`, а не ноль**. Ноль ватт и
//! «ватты неизвестны» — разные вещи, и UI обязан их различать, иначе пользователь увидит
//! правдоподобное враньё.

use crate::frames::FrameMetrics;
use crate::theme::Color;

/// Всё, что bladestats знает о системе прямо сейчас.
#[derive(Debug, Clone, Default)]
pub struct MetricsSnapshot {
    pub cpu: CpuMetrics,
    pub gpu: GpuMetrics,
    pub memory: MemoryMetrics,
    /// `None`, пока нет источника кадров: например, на Windows без прав администратора
    /// или когда в фокусе не игра.
    pub frames: Option<FrameMetrics>,
}

/// Значение мощности вместе с тем, откуда оно взялось.
///
/// На Windows ватты CPU нельзя прочитать из MSR без ring0-драйвера, а такой драйвер
/// противоречит цели проекта не привлекать внимание анти-читов. Поэтому там значение
/// вычисляется из загрузки, частот и TDP — и UI обязан показать его иначе, чем показание
/// настоящего сенсора.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Power {
    /// Прочитано с сенсора: NVML, RAPL, hwmon.
    Measured(f32),
    /// Вычислено по модели. Рисуется с тильдой: `~65 W`.
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
    /// Точное имя из диспетчера устройств / `/proc/cpuinfo`.
    pub name: Option<String>,
    /// По одной записи на логическое ядро, в порядке нумерации ОС.
    pub cores: Vec<CoreMetrics>,
    /// Суммарная загрузка, 0.0..=100.0.
    pub load_pct: Option<f32>,
    pub temp_c: Option<f32>,
    pub power: Option<Power>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct CoreMetrics {
    /// 0.0..=100.0
    pub load_pct: f32,
    /// Фактическая частота, а не базовая.
    pub freq_mhz: Option<f32>,
}

#[derive(Debug, Clone, Default)]
pub struct GpuMetrics {
    /// Точное имя из диспетчера устройств.
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
    /// Настроенная частота (не максимальная из SPD). Читается один раз на старте.
    pub speed_mhz: Option<u32>,
    // Ватт здесь нет и не будет: у памяти на потребительских платформах нет сенсора
    // мощности — ни в SPD, ни в SMBIOS, ни в hwmon.
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
    /// PCI vendor ID → вендор. Работает и для GPU, и для хост-моста CPU.
    pub fn from_pci_id(id: u16) -> Self {
        match id {
            0x1002 | 0x1022 => Vendor::Amd,
            0x8086 => Vendor::Intel,
            0x10de => Vendor::Nvidia,
            _ => Vendor::Unknown,
        }
    }

    /// Грубый разбор имени устройства — костыль для источников, отдающих только строку
    /// (DXGI description, `/proc/cpuinfo`).
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

    /// Фирменный цвет вендора. Используется, когда в конфиге включён `vendor_colors`.
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
        // Одинаковое число из разных источников — не одно и то же значение.
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
