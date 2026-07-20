//! A small safe wrapper over PDH, the Windows performance counter API.
//!
//! Both the CPU and GPU backends need wildcard counters (`\Processor Information(*)\...`,
//! `\GPU Engine(*)\...`), and the raw API for those is unpleasant enough to be worth isolating
//! once: a caller-sized buffer, a retry on `PDH_MORE_DATA`, and instance name strings packed
//! into the tail of the same allocation.
//!
//! Counters are added with `PdhAddEnglishCounterW` rather than `PdhAddCounterW`. Counter paths
//! are localised on Windows, so the localised variant would break the moment the program ran
//! on a non-English install.

use anyhow::{Result, bail};
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Performance::{
    PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_DOUBLE, PDH_HCOUNTER, PDH_HQUERY, PdhAddEnglishCounterW,
    PdhCloseQuery, PdhCollectQueryData, PdhGetFormattedCounterArrayW,
};
use windows::core::HSTRING;

/// PDH's "your buffer was too small" status. Not an error, just a size negotiation.
const PDH_MORE_DATA: u32 = 0x8000_07D2;

/// A PDH query plus the counters attached to it.
pub struct PdhQuery {
    handle: PDH_HQUERY,
}

// PDH handles are process-wide and have no thread affinity; the query lives on the telemetry
// thread and is only ever touched from there.
unsafe impl Send for PdhQuery {}

impl PdhQuery {
    pub fn new() -> Result<Self> {
        let mut handle = PDH_HQUERY::default();
        let status =
            unsafe { windows::Win32::System::Performance::PdhOpenQueryW(None, 0, &mut handle) };
        check(status, "PdhOpenQueryW")?;
        Ok(Self { handle })
    }

    /// Attaches a counter path, e.g. `\Processor Information(*)\% Processor Performance`.
    pub fn add(&self, path: &str) -> Result<PdhCounter> {
        let mut handle = PDH_HCOUNTER::default();
        let wide = HSTRING::from(path);
        let status = unsafe { PdhAddEnglishCounterW(self.handle, &wide, 0, &mut handle) };
        check(status, path)?;
        Ok(PdhCounter { handle })
    }

    /// Samples every attached counter.
    ///
    /// Rate counters need two collections separated in time before they produce a value, so
    /// the first call after startup is expected to yield nothing useful.
    pub fn collect(&self) -> Result<()> {
        let status = unsafe { PdhCollectQueryData(self.handle) };
        check(status, "PdhCollectQueryData")
    }
}

impl Drop for PdhQuery {
    fn drop(&mut self) {
        unsafe {
            let _ = PdhCloseQuery(self.handle);
        }
    }
}

pub struct PdhCounter {
    handle: PDH_HCOUNTER,
}

/// One instance of a wildcard counter.
#[derive(Debug, Clone)]
pub struct CounterValue {
    /// The instance name, e.g. `0,3` for a CPU core or a long LUID string for a GPU engine.
    pub instance: String,
    pub value: f64,
}

impl PdhCounter {
    /// Reads every instance of this counter.
    ///
    /// Returns an empty vector rather than an error when PDH has no data yet — that is the
    /// normal state between the first and second collection, not a failure.
    pub fn values(&self) -> Result<Vec<CounterValue>> {
        // First call with a zero-sized buffer just to learn the required size.
        let mut size = 0u32;
        let mut count = 0u32;
        let status = unsafe {
            PdhGetFormattedCounterArrayW(self.handle, PDH_FMT_DOUBLE, &mut size, &mut count, None)
        };
        if status == ERROR_SUCCESS.0 || count == 0 {
            return Ok(Vec::new());
        }
        if status != PDH_MORE_DATA {
            check(status, "PdhGetFormattedCounterArrayW(sizing)")?;
        }

        // PDH packs the item array at the front of this buffer and the instance name strings
        // into its tail, so it has to be one allocation aligned for the item type.
        let items = size as usize / size_of::<PDH_FMT_COUNTERVALUE_ITEM_W>() + 1;
        let mut buffer: Vec<PDH_FMT_COUNTERVALUE_ITEM_W> = Vec::with_capacity(items);
        let status = unsafe {
            PdhGetFormattedCounterArrayW(
                self.handle,
                PDH_FMT_DOUBLE,
                &mut size,
                &mut count,
                Some(buffer.as_mut_ptr()),
            )
        };
        check(status, "PdhGetFormattedCounterArrayW")?;

        let mut out = Vec::with_capacity(count as usize);
        for i in 0..count as usize {
            let item = unsafe { &*buffer.as_ptr().add(i) };
            let instance = unsafe { item.szName.to_string() }.unwrap_or_default();
            // The double lives in a union; PDH_FMT_DOUBLE is what was asked for, so this is
            // the active member.
            let value = unsafe { item.FmtValue.Anonymous.doubleValue };
            out.push(CounterValue { instance, value });
        }
        Ok(out)
    }
}

fn check(status: u32, what: &str) -> Result<()> {
    if status == ERROR_SUCCESS.0 {
        return Ok(());
    }
    bail!("{what} failed: PDH status {:#010x}", status)
}

/// Sums the values of every instance matching `predicate`.
///
/// Used for GPU engines, where total utilisation is spread across one counter instance per
/// process per engine and has to be added back up.
pub fn sum_matching(values: &[CounterValue], predicate: impl Fn(&str) -> bool) -> Option<f64> {
    let sum: f64 = values
        .iter()
        .filter(|v| predicate(&v.instance))
        .map(|v| v.value)
        .sum();
    (!values.is_empty()).then_some(sum)
}

/// Turns a `\Processor Information(0,3)\...` instance name into a core index.
///
/// The name is `group,core`; processor groups only appear above 64 logical processors, and
/// the numbering restarts within each group, so the group has to be taken into account rather
/// than parsing the part after the comma alone.
pub fn parse_core_instance(instance: &str) -> Option<(u32, u32)> {
    let (group, core) = instance.split_once(',')?;
    Some((group.trim().parse().ok()?, core.trim().parse().ok()?))
}

/// Rejects the aggregate instances PDH mixes in with the per-core ones.
pub fn is_total_instance(instance: &str) -> bool {
    instance.eq_ignore_ascii_case("_Total") || instance.contains("_Total")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_processor_instance_names() {
        assert_eq!(parse_core_instance("0,0"), Some((0, 0)));
        assert_eq!(parse_core_instance("0,15"), Some((0, 15)));
        // Above 64 logical processors Windows splits them into groups.
        assert_eq!(parse_core_instance("1,7"), Some((1, 7)));
    }

    #[test]
    fn rejects_aggregate_instance_names() {
        assert!(is_total_instance("_Total"));
        assert!(is_total_instance("0,_Total"));
        assert!(!is_total_instance("0,3"));
        assert_eq!(parse_core_instance("_Total"), None);
    }

    #[test]
    fn sums_only_matching_instances() {
        let values = vec![
            CounterValue {
                instance: "eng_0_engtype_3D".into(),
                value: 30.0,
            },
            CounterValue {
                instance: "eng_1_engtype_Copy".into(),
                value: 5.0,
            },
            CounterValue {
                instance: "eng_2_engtype_3D".into(),
                value: 12.0,
            },
        ];
        let total = sum_matching(&values, |i| i.ends_with("engtype_3D"));
        assert_eq!(total, Some(42.0));
    }

    #[test]
    fn summing_an_empty_set_yields_nothing_rather_than_zero() {
        // No counter instances at all means "could not read", which must stay distinct from a
        // genuine zero percent.
        assert_eq!(sum_matching(&[], |_| true), None);
    }
}
