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

/// Capacity in megabytes. `0x7FFF` means "see the extended field"; the top bit means kilobytes.
const OFFSET_SIZE: usize = 0x0C;
const OFFSET_EXTENDED_SIZE: usize = 0x1C;
/// The generation: DDR4, DDR5 and so on. **One byte, not two** — the word that follows it is
/// Type Detail, and reading the pair together yields neither.
const OFFSET_MEMORY_TYPE: usize = 0x12;

pub struct MemorySampler {
    /// Read once at startup: memory speed does not change while the machine is running, and
    /// walking the SMBIOS table every tick would be pure waste.
    firmware: MemoryFirmware,
}

impl MemorySampler {
    pub fn new() -> Self {
        let firmware = smbios_memory();
        if firmware.speed_mhz.is_none() {
            tracing::warn!("memory speed unavailable; SMBIOS table missing or unparseable");
        }
        tracing::debug!(?firmware, "memory");
        Self { firmware }
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
        // The speed the modules are actually running at, which is what a heading should name.
        // The rated figure goes beside it in the spec line: on a machine with a profile
        // enabled the rated number is the *lower* one — the JEDEC fallback the kit is
        // guaranteed at — and the gap between them is the profile doing its job.
        into.memory.speed_mhz = self.firmware.speed_mhz;
        into.memory.rated_mhz = self.firmware.rated_mhz;
        into.memory.kind = self.firmware.kind;
        into.memory.modules.clone_from(&self.firmware.modules);

        // No power field. Consumer platforms expose no power sensor for memory, and a
        // fabricated number would be worse than an absent one.
    }
}

/// Reads the raw SMBIOS table and returns the highest configured memory speed found.
///
/// The highest rather than the first: empty slots report zero, and a machine with mismatched
/// modules runs them all at the slowest common speed anyway, so the populated entries agree.
fn smbios_memory() -> MemoryFirmware {
    let Some(table) = read_firmware_table() else {
        return MemoryFirmware::default();
    };

    // The buffer starts with a RawSMBIOSData header; the structures follow it.
    const HEADER_LEN: usize = 8;
    if table.len() <= HEADER_LEN {
        return MemoryFirmware::default();
    }
    parse_memory(&table[HEADER_LEN..])
}

/// What firmware says about the memory installed.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct MemoryFirmware {
    /// What the modules are actually running at.
    pub speed_mhz: Option<u32>,
    /// What they are rated for, which on a machine with a profile enabled is the *lower*
    /// number — the JEDEC fallback the kit is guaranteed at. Worth showing beside the real one
    /// precisely because the difference is the profile doing its job.
    pub rated_mhz: Option<u32>,
    /// `DDR5`, `DDR4`, and so on.
    pub kind: Option<&'static str>,
    /// Capacity of each populated module, in megabytes.
    pub modules: Vec<u32>,
}

/// Walks SMBIOS structures looking for type 17 entries.
///
/// Split out from the firmware call so it can be tested against a synthetic table.
fn parse_memory(data: &[u8]) -> MemoryFirmware {
    let mut out = MemoryFirmware::default();
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
            let read_u32 = |offset: usize| -> Option<u32> {
                if offset + 4 > length {
                    return None;
                }
                let v = u32::from_le_bytes([
                    data[pos + offset],
                    data[pos + offset + 1],
                    data[pos + offset + 2],
                    data[pos + offset + 3],
                ]);
                (v != 0).then_some(v)
            };

            // Prefer the configured speed; fall back to the rated one on older firmware.
            if let Some(speed) =
                read_u16(OFFSET_CONFIGURED_SPEED).or_else(|| read_u16(OFFSET_SPEED))
            {
                out.speed_mhz = Some(out.speed_mhz.map_or(speed, |b: u32| b.max(speed)));
            }
            if let Some(rated) = read_u16(OFFSET_SPEED) {
                out.rated_mhz = Some(out.rated_mhz.map_or(rated, |b: u32| b.max(rated)));
            }
            if OFFSET_MEMORY_TYPE < length
                && let Some(kind) = memory_kind(data[pos + OFFSET_MEMORY_TYPE])
            {
                out.kind = Some(kind);
            }

            // A capacity of 0x7FFF means "too large for this field, see the extended one".
            // Everything current is well under 32GB per module, but the escape exists and
            // ignoring it would report a 64GB stick as 32767MB.
            if let Some(size) = read_u16(OFFSET_SIZE) {
                let mb = if size == 0x7FFF {
                    read_u32(OFFSET_EXTENDED_SIZE).unwrap_or(0)
                } else if size & 0x8000 != 0 {
                    // The top bit means the value is in kilobytes.
                    u32::from(size & 0x7FFF) / 1024
                } else {
                    u32::from(size)
                };
                if mb > 0 {
                    out.modules.push(mb);
                }
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

    out
}

/// The generation, from the SMBIOS memory-type enumeration.
///
/// Only the ones a machine running this could plausibly have. An unrecognised code returns
/// nothing rather than a guess: the heading simply loses its name.
fn memory_kind(code: u8) -> Option<&'static str> {
    Some(match code {
        0x18 => "DDR3",
        0x1A => "DDR4",
        0x1E => "LPDDR3",
        0x1F => "LPDDR4",
        0x22 => "DDR5",
        0x23 => "LPDDR5",
        _ => return None,
    })
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
        s[OFFSET_MEMORY_TYPE] = 0x22; // DDR5
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
        assert_eq!(parse_memory(&table).speed_mhz, Some(4800));
    }

    #[test]
    fn falls_back_to_the_rated_speed_on_older_firmware() {
        // Configured speed reported as zero, as pre-2.7 firmware does.
        let mut table = memory_device(3200, 0);
        table.extend(end_of_table());
        assert_eq!(parse_memory(&table).speed_mhz, Some(3200));
    }

    #[test]
    fn empty_slots_are_ignored() {
        let mut table = memory_device(6000, 6000);
        table.extend(memory_device(0, 0)); // unpopulated slot
        table.extend(end_of_table());
        assert_eq!(parse_memory(&table).speed_mhz, Some(6000));
    }

    #[test]
    fn a_table_without_memory_devices_yields_nothing() {
        assert_eq!(parse_memory(&end_of_table()).speed_mhz, None);
        assert_eq!(parse_memory(&[]).speed_mhz, None);
    }

    #[test]
    fn a_corrupt_table_terminates_instead_of_looping_forever() {
        // Length byte smaller than the header: unparseable, must not hang.
        assert_eq!(parse_memory(&[17, 1, 0, 0]).speed_mhz, None);
        // Length running past the end of the buffer.
        assert_eq!(parse_memory(&[17, 200, 0, 0]).speed_mhz, None);
    }

    #[test]
    fn reads_this_machines_actual_memory() {
        // Cross-check against CPU-Z or Task Manager if this ever looks wrong.
        let firmware = smbios_memory();
        let Some(speed) = firmware.speed_mhz else {
            eprintln!("skipped: SMBIOS memory unavailable on this machine");
            return;
        };
        assert!(
            (400..=20_000).contains(&speed),
            "implausible memory speed: {speed}"
        );
        // The modules have to add up to something, or the spec line would describe a machine
        // with no memory in it.
        assert!(
            firmware.modules.iter().sum::<u32>() >= 1024,
            "no populated modules found: {:?}",
            firmware.modules
        );
        eprintln!(
            "{:?} running {speed} MT/s, rated {:?}, modules {:?}",
            firmware.kind, firmware.rated_mhz, firmware.modules
        );
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
