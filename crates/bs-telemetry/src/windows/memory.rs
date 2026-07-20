//! Memory telemetry on Windows: usage from the kernel, configured speed from SMBIOS.

use bs_core::MetricsSnapshot;
use windows::Win32::System::SystemInformation::{
    FIRMWARE_TABLE_PROVIDER, GetSystemFirmwareTable, GlobalMemoryStatusEx, MEMORYSTATUSEX,
};

use crate::Sampler;

/// `'RSMB'` as a big-endian FourCC: the raw SMBIOS firmware table provider.
const PROVIDER_RSMB: FIRMWARE_TABLE_PROVIDER =
    FIRMWARE_TABLE_PROVIDER(u32::from_be_bytes(*b"RSMB"));

/// SMBIOS structure type 17, "Memory Device" — one per physical DIMM slot.
const SMBIOS_TYPE_MEMORY_DEVICE: u8 = 17;

/// Offset of `Speed` within a type 17 structure. Present since SMBIOS 2.3.
const OFFSET_SPEED: usize = 0x15;

/// Offset of `Configured Memory Speed`. Present since SMBIOS 2.7 and preferred: it reflects
/// what the modules actually run at, whereas `Speed` is what they are rated for. On a machine
/// where XMP was never enabled the two differ, and the configured value is the honest one.
const OFFSET_CONFIGURED_SPEED: usize = 0x20;

pub struct MemorySampler {
    /// Read once at startup: memory speed does not change while the machine is running, and
    /// walking the SMBIOS table every tick would be pure waste.
    speed_mhz: Option<u32>,
}

impl MemorySampler {
    pub fn new() -> Self {
        let speed_mhz = smbios_memory_speed();
        if speed_mhz.is_none() {
            tracing::warn!("memory speed unavailable; SMBIOS table missing or unparseable");
        }
        Self { speed_mhz }
    }
}

impl Sampler for MemorySampler {
    fn name(&self) -> &'static str {
        "memory"
    }

    fn sample(&mut self, into: &mut MetricsSnapshot) {
        let mut status = MEMORYSTATUSEX {
            dwLength: size_of::<MEMORYSTATUSEX>() as u32,
            ..Default::default()
        };
        if unsafe { GlobalMemoryStatusEx(&mut status) }.is_ok() {
            into.memory.total_bytes = Some(status.ullTotalPhys);
            into.memory.used_bytes = Some(status.ullTotalPhys.saturating_sub(status.ullAvailPhys));
        }
        into.memory.speed_mhz = self.speed_mhz;

        // No power field. Consumer platforms expose no power sensor for memory, and a
        // fabricated number would be worse than an absent one.
    }
}

/// Reads the raw SMBIOS table and returns the highest configured memory speed found.
///
/// The highest rather than the first: empty slots report zero, and a machine with mismatched
/// modules runs them all at the slowest common speed anyway, so the populated entries agree.
fn smbios_memory_speed() -> Option<u32> {
    let table = read_firmware_table()?;

    // The buffer starts with a RawSMBIOSData header; the structures follow it.
    const HEADER_LEN: usize = 8;
    if table.len() <= HEADER_LEN {
        return None;
    }
    let data = &table[HEADER_LEN..];

    parse_memory_speed(data)
}

/// Walks SMBIOS structures looking for type 17 entries.
///
/// Split out from the firmware call so it can be tested against a synthetic table.
fn parse_memory_speed(data: &[u8]) -> Option<u32> {
    let mut best: Option<u32> = None;
    let mut pos = 0usize;

    while pos + 4 <= data.len() {
        let struct_type = data[pos];
        let length = data[pos + 1] as usize;

        // A structure shorter than its own header means the table is corrupt; stop rather
        // than loop forever.
        if length < 4 || pos + length > data.len() {
            break;
        }

        if struct_type == SMBIOS_TYPE_MEMORY_DEVICE {
            let read_u16 = |offset: usize| -> Option<u32> {
                if offset + 2 > length {
                    return None;
                }
                let v = u16::from_le_bytes([data[pos + offset], data[pos + offset + 1]]);
                (v != 0).then_some(v as u32)
            };
            // Prefer the configured speed; fall back to the rated one on older firmware.
            if let Some(speed) =
                read_u16(OFFSET_CONFIGURED_SPEED).or_else(|| read_u16(OFFSET_SPEED))
            {
                best = Some(best.map_or(speed, |b: u32| b.max(speed)));
            }
        }

        // The formatted area is followed by a string table terminated by a double NUL.
        pos += length;
        while pos + 1 < data.len() && !(data[pos] == 0 && data[pos + 1] == 0) {
            pos += 1;
        }
        pos += 2;

        // Type 127 marks the end of the table.
        if struct_type == 127 {
            break;
        }
    }

    best
}

fn read_firmware_table() -> Option<Vec<u8>> {
    let size = unsafe { GetSystemFirmwareTable(PROVIDER_RSMB, 0, None) };
    if size == 0 {
        return None;
    }
    let mut buffer = vec![0u8; size as usize];
    let written = unsafe { GetSystemFirmwareTable(PROVIDER_RSMB, 0, Some(&mut buffer)) };
    if written == 0 || written as usize > buffer.len() {
        return None;
    }
    buffer.truncate(written as usize);
    Some(buffer)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds one type 17 structure with the given rated and configured speeds.
    fn memory_device(rated: u16, configured: u16) -> Vec<u8> {
        let length = 0x22usize;
        let mut s = vec![0u8; length];
        s[0] = SMBIOS_TYPE_MEMORY_DEVICE;
        s[1] = length as u8;
        s[2..4].copy_from_slice(&1u16.to_le_bytes()); // handle
        s[OFFSET_SPEED..OFFSET_SPEED + 2].copy_from_slice(&rated.to_le_bytes());
        s[OFFSET_CONFIGURED_SPEED..OFFSET_CONFIGURED_SPEED + 2]
            .copy_from_slice(&configured.to_le_bytes());
        s.extend_from_slice(&[0, 0]); // empty string table
        s
    }

    fn end_of_table() -> Vec<u8> {
        vec![127, 4, 0, 0, 0, 0]
    }

    #[test]
    fn prefers_the_configured_speed_over_the_rated_one() {
        // A kit rated for 6000 but running at JEDEC 4800 because XMP is off.
        let mut table = memory_device(6000, 4800);
        table.extend(end_of_table());
        assert_eq!(parse_memory_speed(&table), Some(4800));
    }

    #[test]
    fn falls_back_to_the_rated_speed_on_older_firmware() {
        // Configured speed reported as zero, as pre-2.7 firmware does.
        let mut table = memory_device(3200, 0);
        table.extend(end_of_table());
        assert_eq!(parse_memory_speed(&table), Some(3200));
    }

    #[test]
    fn empty_slots_are_ignored() {
        let mut table = memory_device(6000, 6000);
        table.extend(memory_device(0, 0)); // unpopulated slot
        table.extend(end_of_table());
        assert_eq!(parse_memory_speed(&table), Some(6000));
    }

    #[test]
    fn a_table_without_memory_devices_yields_nothing() {
        assert_eq!(parse_memory_speed(&end_of_table()), None);
        assert_eq!(parse_memory_speed(&[]), None);
    }

    #[test]
    fn a_corrupt_table_terminates_instead_of_looping_forever() {
        // Length byte smaller than the header: unparseable, must not hang.
        assert_eq!(parse_memory_speed(&[17, 1, 0, 0]), None);
        // Length running past the end of the buffer.
        assert_eq!(parse_memory_speed(&[17, 200, 0, 0]), None);
    }

    #[test]
    fn reads_this_machines_actual_memory_speed() {
        // Cross-check against CPU-Z or Task Manager if this ever looks wrong.
        match smbios_memory_speed() {
            Some(speed) => assert!(
                (400..=20_000).contains(&speed),
                "implausible memory speed: {speed}"
            ),
            None => eprintln!("skipped: SMBIOS memory speed unavailable on this machine"),
        }
    }

    #[test]
    fn reports_plausible_physical_memory() {
        let mut snapshot = MetricsSnapshot::default();
        MemorySampler::new().sample(&mut snapshot);

        let total = snapshot.memory.total_bytes.expect("total memory");
        let used = snapshot.memory.used_bytes.expect("used memory");
        assert!(total > 1 << 30, "any machine running this has over 1 GiB");
        assert!(used <= total, "used memory cannot exceed the total");
    }
}
