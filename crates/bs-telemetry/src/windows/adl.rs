//! AMD sensors: temperature, hotspot, power, clocks and fan.
//!
//! The generic Windows backend can read a Radeon's load and its memory use and nothing else —
//! those live behind a vendor library, and this is it. It is the AMD counterpart to `nvml.rs`
//! and registers the same way: after the generic source, filling what that could not reach.
//!
//! Reads through `ADL2_New_QueryPMLogData_Get`, which returns every sensor the card has in one
//! call. The older interfaces — Overdrive 5, 6 and N — each report a temperature and little
//! else, and a card new enough to want an overlay on has the newer one.

use std::ffi::c_int;

use bs_core::{MetricsSnapshot, Power};

use super::adl_sys::{Adl, AdapterInfo, PmLogDataOutput, sensor};
use crate::Sampler;

/// What a PCI device is, as both DXGI and ADL can describe it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PciId {
    pub vendor: u16,
    pub device: u16,
}

pub struct AdlSampler {
    adl: Adl,
    adapter_index: c_int,
    /// The processor's own integrated graphics, when it has some.
    ///
    /// Read for one field and one only: on a desktop Ryzen the integrated part shares a
    /// package with the cores, so what ADL calls its ASIC power is the *processor's* power.
    /// Everything else about it belongs to a graphics adapter nobody is rendering with.
    package_index: Option<c_int>,
    /// Reused between samples. Two kilobytes is not much, but rebuilding it twice a second
    /// for the lifetime of a game is pointless work.
    readout: Box<PmLogDataOutput>,
}

impl AdlSampler {
    /// Opens ADL and settles on which adapters to read, or returns `None` when there is no
    /// Radeon here to read at all.
    pub fn new(target: Option<PciId>, integrated: Option<PciId>) -> Option<Self> {
        let adl = Adl::load()?;
        let adapters = adl.adapters();
        if adapters.is_empty() {
            tracing::debug!("ADL loaded but reports no adapters");
            return None;
        }

        let chosen = choose(&adapters, target)?;
        tracing::info!(
            index = chosen.adapter_index,
            name = %chosen.name(),
            pnp = %chosen.pnp(),
            "AMD sensors"
        );

        // Only when it is genuinely a different adapter. On a machine whose only graphics are
        // integrated, the card being reported on and the package are the same thing, and its
        // ASIC power is then the graphics figure rather than the processor's.
        let package_index = integrated
            .filter(|id| Some(*id) != target)
            .and_then(|id| {
                adapters
                    .iter()
                    .find(|a| a.present != 0 && a.exist != 0 && parse_pci(&a.pnp()) == Some(id))
            })
            .map(|a| {
                tracing::info!(index = a.adapter_index, pnp = %a.pnp(), "processor power");
                a.adapter_index
            });

        Some(Self {
            adl,
            adapter_index: chosen.adapter_index,
            package_index,
            readout: Box::default(),
        })
    }
}

/// Picks the adapter that matches the card the rest of the program is reporting on.
///
/// This machine is the reason it cannot be casual about it: a Ryzen desktop part brings an
/// integrated Radeon along with the discrete one, both answer to vendor 0x1002, and both are
/// called some flavour of "AMD Radeon". Matching on the name would be a coin toss.
///
/// So the device ID decides. DXGI has already chosen a card — the one with the most dedicated
/// memory, which is the one games will run on — and its device ID is carried here to be
/// matched against the `PCI\VEN_xxxx&DEV_xxxx` that ADL reports for each adapter. Failing
/// that, the same rule ADL entries can be sorted by: present, existing, and first.
fn choose(adapters: &[AdapterInfo], target: Option<PciId>) -> Option<&AdapterInfo> {
    let usable = |a: &&AdapterInfo| a.present != 0 && a.exist != 0;

    if let Some(target) = target
        && let Some(matched) = adapters
            .iter()
            .filter(usable)
            .find(|a| parse_pci(&a.pnp()) == Some(target))
    {
        return Some(matched);
    }

    if target.is_some() {
        tracing::warn!(
            ?target,
            "no ADL adapter matched the card DXGI picked; falling back to the first"
        );
    }
    adapters.iter().find(usable).or_else(|| adapters.first())
}

/// Pulls the vendor and device out of `PCI\VEN_1002&DEV_747E&SUBSYS_...&REV_C8`.
///
/// The subsystem and revision are deliberately not compared. They are spelled differently by
/// the two sources — DXGI packs the subsystem the other way round — and vendor plus device is
/// already enough to tell a discrete card from an integrated one.
fn parse_pci(pnp: &str) -> Option<PciId> {
    let upper = pnp.to_ascii_uppercase();
    let field = |key: &str| -> Option<u16> {
        let start = upper.find(key)? + key.len();
        let digits: String = upper[start..].chars().take(4).collect();
        u16::from_str_radix(&digits, 16).ok()
    };
    Some(PciId {
        vendor: field("VEN_")?,
        device: field("DEV_")?,
    })
}

impl Sampler for AdlSampler {
    fn name(&self) -> &'static str {
        "amd-adl"
    }

    fn sample(&mut self, into: &mut MetricsSnapshot) {
        // The processor first, because the card's readout overwrites the buffer.
        //
        // This replaces a model with a measurement. The figure that was here before came from
        // a guessed thermal envelope raised to a power of load — the right shape and no
        // relation to what the machine was actually drawing. Verified by loading every core
        // and watching: 33W idle, 125W saturated, while the graphics block sat at its idle
        // clock throughout, which is what says the reading covers the package and not the
        // graphics.
        if let Some(index) = self.package_index
            && self.adl.sensors(index, &mut self.readout)
            && let Some(w) = first_of(&self.readout, &[sensor::ASIC_POWER], |w| {
                (1..=500).contains(&w).then_some(w as f32)
            })
        {
            into.cpu.power = Some(Power::Measured(w));
        }

        if !self.adl.sensors(self.adapter_index, &mut self.readout) {
            return;
        }
        let r = &self.readout;

        // Every one of these is left alone when the card does not have it. A sensor that
        // reports "unsupported" and a sensor that reports zero are different facts.
        //
        // Each is a chain rather than a single index because the two families populate
        // different slots — see the note in `adl_sys::sensor`. Whichever answers first wins,
        // in order of how directly it means the thing being named.
        into.gpu.temp_c = first_of(
            r,
            &[sensor::TEMPERATURE_EDGE, sensor::TEMPERATURE_GFX],
            plausible_temp,
        );
        into.gpu.hotspot_c = first_of(r, &[sensor::TEMPERATURE_HOTSPOT], plausible_temp);
        into.gpu.mem_temp_c = first_of(r, &[sensor::TEMPERATURE_MEM], plausible_temp);

        // Board power first, since that is what a power supply and a case have to deal with.
        // The package and graphics-only figures stand in on parts that do not report a board.
        let watts = first_of(
            r,
            &[
                sensor::BOARD_POWER,
                sensor::ASIC_POWER,
                sensor::GFX_POWER,
            ],
            |w| (1..=1000).contains(&w).then_some(w as f32),
        );
        if let Some(w) = watts {
            // Measured, not estimated: this is a sensor on the board, unlike the processor
            // figure, which is a model and wears a tilde to say so.
            into.gpu.power = Some(Power::Measured(w));
        }

        if let Some(mhz) = r.get(sensor::CLK_GFXCLK)
            && (0..=6000).contains(&mhz)
        {
            into.gpu.core_clock_mhz = Some(mhz as f32);
        }
        if let Some(mhz) = r.get(sensor::CLK_MEMCLK)
            && (0..=20000).contains(&mhz)
        {
            into.gpu.mem_clock_mhz = Some(mhz as f32);
        }

        if let Some(rpm) = r.get(sensor::FAN_RPM)
            && (0..=10000).contains(&rpm)
        {
            into.gpu.fan_rpm = Some(rpm as f32);
        }
        if let Some(pct) = r.get(sensor::FAN_PERCENTAGE)
            && (0..=100).contains(&pct)
        {
            into.gpu.fan_pct = Some(pct as f32);
        }

        // Only when the generic backend could not supply it. Its figure comes from the same
        // counters Task Manager uses and matches what the user sees there, which is worth
        // more than a second opinion that disagrees by a few percent.
        if into.gpu.load_pct.is_none()
            && let Some(pct) = r.get(sensor::INFO_ACTIVITY_GFX)
            && (0..=100).contains(&pct)
        {
            into.gpu.load_pct = Some(pct as f32);
        }
    }
}

/// The first index in `candidates` that the card both supports and reports sensibly.
///
/// The validator is not decoration. The layouts and indices here are transcribed from headers
/// this repository does not carry, and the failure mode of getting one wrong is not a crash —
/// it is a fan speed printed as a temperature. A range check will not catch every such
/// mistake, but it catches the ones that would put an obvious absurdity on screen, and it also
/// steps politely past a slot a card fills with zero because it has nothing to say.
fn first_of(
    readout: &PmLogDataOutput,
    candidates: &[usize],
    valid: impl Fn(i32) -> Option<f32>,
) -> Option<f32> {
    candidates
        .iter()
        .filter_map(|&i| readout.get(i))
        .find_map(valid)
}

/// Rejects a temperature no silicon in a working computer reports.
///
/// The layouts in `adl_sys` are transcribed from headers this repository does not carry, and
/// the failure mode of getting one wrong is not a crash — it is a fan speed printed as a
/// temperature. This will not catch every such mistake, but it catches the ones that would put
/// an obvious absurdity on screen.
fn plausible_temp(celsius: i32) -> Option<f32> {
    // Above zero rather than from zero: a card reporting exactly nothing is a slot that was
    // never filled in, not silicon at freezing point.
    (1..=150).contains(&celsius).then_some(celsius as f32)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn adapter(index: c_int, pnp: &str, present: bool) -> AdapterInfo {
        let mut info: AdapterInfo = unsafe { std::mem::zeroed() };
        info.size = size_of::<AdapterInfo>() as c_int;
        info.adapter_index = index;
        info.present = i32::from(present);
        info.exist = i32::from(present);
        info.pnp_string[..pnp.len()].copy_from_slice(pnp.as_bytes());
        info
    }

    const DISCRETE: &str = "PCI\\VEN_1002&DEV_747E&SUBSYS_54021DA2&REV_C8";
    const INTEGRATED: &str = "PCI\\VEN_1002&DEV_13C0&SUBSYS_13C01002&REV_00";

    #[test]
    fn reads_the_vendor_and_device_out_of_a_device_path() {
        assert_eq!(
            parse_pci(DISCRETE),
            Some(PciId {
                vendor: 0x1002,
                device: 0x747E
            })
        );
        // Firmware is not consistent about case.
        assert_eq!(parse_pci(&DISCRETE.to_lowercase()), parse_pci(DISCRETE));
        assert_eq!(parse_pci("USB\\VID_046D&PID_C52B"), None);
        assert_eq!(parse_pci(""), None);
    }

    #[test]
    fn the_discrete_card_is_picked_over_an_integrated_one_with_the_same_vendor() {
        // The case this whole function exists for: a Ryzen desktop part brings an integrated
        // Radeon, both answer to 0x1002, and both are named "AMD Radeon" something.
        let adapters = [
            adapter(0, INTEGRATED, true),
            adapter(1, DISCRETE, true),
        ];
        let target = PciId {
            vendor: 0x1002,
            device: 0x747E,
        };
        assert_eq!(choose(&adapters, Some(target)).unwrap().adapter_index, 1);
    }

    #[test]
    fn absent_adapters_are_skipped() {
        let adapters = [
            adapter(0, DISCRETE, false),
            adapter(1, INTEGRATED, true),
        ];
        // The card asked for is not plugged in; the one that is gets reported rather than
        // nothing at all.
        let target = PciId {
            vendor: 0x1002,
            device: 0x747E,
        };
        assert_eq!(choose(&adapters, Some(target)).unwrap().adapter_index, 1);
    }

    #[test]
    fn without_a_card_to_match_the_first_usable_one_is_taken() {
        let adapters = [adapter(0, DISCRETE, true), adapter(1, INTEGRATED, true)];
        assert_eq!(choose(&adapters, None).unwrap().adapter_index, 0);
    }

    #[test]
    fn an_absurd_temperature_is_refused_rather_than_drawn() {
        // What a mistranscribed structure offset looks like from the outside.
        assert_eq!(plausible_temp(68), Some(68.0));
        assert_eq!(plausible_temp(2400), None, "that is a fan, not a temperature");
        assert_eq!(plausible_temp(-40), None);
        // A slot filled with zero is one the card never wrote to, not silicon at freezing
        // point. Reporting it would put a confident 0°C on screen where a dash belongs.
        assert_eq!(plausible_temp(0), None);
    }

    /// Prints every sensor slot the card claims to support, with its index.
    ///
    /// The one tool that settles whether the indices in `adl_sys::sensor` are right. They are
    /// transcribed from headers this repository does not carry, and a wrong one is silent: the
    /// slot simply reads as unsupported, or worse, reports a neighbouring sensor's number.
    /// Ignored by default because its output is for a person to read.
    #[test]
    #[ignore = "diagnostic: run with --ignored --nocapture to see the sensor block"]
    fn dump_every_supported_sensor() {
        let Some(adl) = Adl::load() else {
            eprintln!("no ADL on this machine");
            return;
        };
        let adapters = adl.adapters();
        for a in adapters.iter().filter(|a| a.present != 0 && a.exist != 0) {
            eprintln!(
                "\nadapter {} — {} — {}",
                a.adapter_index,
                a.name(),
                a.pnp()
            );
            let mut out = Box::<PmLogDataOutput>::default();
            if !adl.sensors(a.adapter_index, &mut out) {
                eprintln!("  QueryPMLogData failed");
                continue;
            }
            eprintln!("  reported size: {}", out.size);
            for i in 0..256 {
                if let Some(v) = out.get(i) {
                    eprintln!("  [{i:>3}] = {v}");
                }
            }
        }
    }

    /// Asks whether the integrated Radeon's sensors are really the processor's.
    ///
    /// On a desktop Ryzen the integrated graphics sit on the same die as the cores, so what ADL
    /// calls the ASIC may be the whole package — which would mean processor power and
    /// temperature are readable here, with no kernel driver and no third-party program. That
    /// would be a considerable thing to be right about, and an embarrassing thing to be wrong
    /// about, so it is settled by loading the cores and watching rather than by reasoning.
    #[test]
    #[ignore = "diagnostic: run with --ignored --nocapture, takes ~15s and loads every core"]
    fn does_the_integrated_adapter_report_the_processor() {
        let Some(adl) = Adl::load() else {
            eprintln!("no ADL on this machine");
            return;
        };
        // The integrated part is the one the primary-card matching deliberately avoids.
        let discrete = super::super::gpu::primary_pci();
        let adapters = adl.adapters();
        let Some(integrated) = adapters
            .iter()
            .filter(|a| a.present != 0 && a.exist != 0)
            .find(|a| parse_pci(&a.pnp()) != discrete)
        else {
            eprintln!("no integrated adapter beside the discrete one");
            return;
        };

        let read = |label: &str| {
            let mut out = Box::<PmLogDataOutput>::default();
            if !adl.sensors(integrated.adapter_index, &mut out) {
                eprintln!("{label}: query failed");
                return;
            }
            eprintln!(
                "{label:>6}: asic[23]={:?}W soc[17]={:?}W gfx[30]={:?}W  gfx_t[28]={:?}C soc_t[29]={:?}C  gfxclk[1]={:?}",
                out.get(sensor::ASIC_POWER),
                out.get(17),
                out.get(sensor::GFX_POWER),
                out.get(sensor::TEMPERATURE_GFX),
                out.get(29),
                out.get(sensor::CLK_GFXCLK),
            );
        };

        eprintln!("adapter {} — {}", integrated.adapter_index, integrated.pnp());
        read("idle");
        std::thread::sleep(std::time::Duration::from_secs(3));
        read("idle");

        // Every logical core, busy, for long enough that package power has to respond.
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let threads: Vec<_> = (0..std::thread::available_parallelism().map_or(8, |n| n.get()))
            .map(|_| {
                let stop = stop.clone();
                std::thread::spawn(move || {
                    let mut x = 0u64;
                    while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                        x = x.wrapping_mul(6364136223846793005).wrapping_add(1);
                        std::hint::black_box(x);
                    }
                })
            })
            .collect();

        std::thread::sleep(std::time::Duration::from_secs(4));
        read("LOADED");
        std::thread::sleep(std::time::Duration::from_secs(3));
        read("LOADED");

        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        for t in threads {
            let _ = t.join();
        }
        std::thread::sleep(std::time::Duration::from_secs(4));
        read("after");
    }

    /// Checks that the processor's wattage is a reading and not the old model.
    ///
    /// The estimate it replaced produced a smooth curve from a guessed thermal envelope, which
    /// looked entirely reasonable and bore no relation to the machine. The distinguishing
    /// property of the real thing is that it is marked as measured.
    #[test]
    fn processor_power_arrives_as_a_measurement_where_the_hardware_allows_it() {
        let Some(mut sampler) = AdlSampler::new(
            super::super::gpu::primary_pci(),
            super::super::gpu::integrated_pci(),
        ) else {
            eprintln!("skipped: no AMD adapter on this machine");
            return;
        };
        if sampler.package_index.is_none() {
            eprintln!("skipped: no integrated Radeon to read the package through");
            return;
        }

        let mut snapshot = MetricsSnapshot::default();
        sampler.sample(&mut snapshot);

        let power = snapshot.cpu.power.expect("the package reports its power");
        assert!(
            !power.is_estimated(),
            "this path exists to replace the estimate, not to dress it up"
        );
        assert!(
            (5.0..=400.0).contains(&power.watts()),
            "implausible package power: {}",
            power.watts()
        );
        eprintln!("processor package: {} W", power.watts());
    }

    /// Reads this machine's own card, when there is one. Skips everywhere else, in the same
    /// spirit as the memory backend's test against real firmware.
    #[test]
    fn reads_plausible_sensors_from_the_card_in_this_machine() {
        // Through the same choice the rest of the program makes. Passing `None` here once
        // silently tested the integrated Radeon instead of the card in the slot, which is
        // exactly the confusion `choose` exists to prevent.
        let Some(mut sampler) = AdlSampler::new(super::super::gpu::primary_pci(), super::super::gpu::integrated_pci()) else {
            eprintln!("skipped: no AMD adapter on this machine");
            return;
        };

        let mut snapshot = MetricsSnapshot::default();
        sampler.sample(&mut snapshot);

        // An idle card is warm and drawing something. Zero on both would mean the call
        // succeeded and returned an empty structure, which is worth failing over.
        let temp = snapshot.gpu.temp_c.expect("a Radeon reports its temperature");
        assert!(
            (15.0..=110.0).contains(&temp),
            "implausible temperature: {temp}"
        );
        eprintln!(
            "edge {:?}C hotspot {:?}C power {:?} core {:?}MHz fan {:?}rpm",
            snapshot.gpu.temp_c,
            snapshot.gpu.hotspot_c,
            snapshot.gpu.power,
            snapshot.gpu.core_clock_mhz,
            snapshot.gpu.fan_rpm
        );
    }
}
