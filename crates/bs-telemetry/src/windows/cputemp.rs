//! Processor temperature, from a hardware monitor somebody else is running.
//!
//! This is the one reading with no path that avoids a kernel driver. Package temperature lives
//! behind the system management unit, reached through PCI configuration space, and reaching it
//! means ring zero. bladestats ships no driver and will not: one would need an extended
//! validation certificate and Microsoft's attestation to load at all, and the drivers that do
//! this — the WinRing0 family — are on Microsoft's vulnerable-driver blocklist and are the
//! first thing anti-cheat software looks for.
//!
//! So the reading is borrowed rather than taken. If a hardware monitor is already running, it
//! has already paid that price, and asking it costs nothing further. That is a decision for
//! the user to make knowingly, which is why this is off unless switched on.
//!
//! The source is behind a trait for a specific reason: a signed driver, should this project
//! ever have one, is another file implementing it and no change anywhere else.

use std::time::{Duration, Instant};

use bs_core::MetricsSnapshot;

use crate::Sampler;

mod lhm;

pub use lhm::LibreHardwareMonitor;

/// Somewhere a processor temperature can be obtained from.
pub trait CpuTempSource: Send {
    /// For logs and for saying on screen which one answered.
    fn name(&self) -> &'static str;

    /// The current package temperature, or `None` when this source has nothing to say.
    fn read(&mut self) -> Option<f32>;
}

/// How often a dead source is tried again.
///
/// A monitor that is not running is the ordinary case, not an error, and retrying it every
/// half second for a whole gaming session would be pointless work.
const RETRY_INTERVAL: Duration = Duration::from_secs(5);

/// Reads the processor temperature from whichever source can supply it.
pub struct CpuTempSampler {
    sources: Vec<Box<dyn CpuTempSource>>,
    /// Index of the one that last worked, so a working source is not re-elected every tick.
    preferred: Option<usize>,
    next_retry: Instant,
}

impl CpuTempSampler {
    pub fn new(sources: Vec<Box<dyn CpuTempSource>>) -> Option<Self> {
        (!sources.is_empty()).then(|| Self {
            sources,
            preferred: None,
            next_retry: Instant::now(),
        })
    }
}

impl Sampler for CpuTempSampler {
    fn name(&self) -> &'static str {
        "cpu-temperature"
    }

    fn sample(&mut self, into: &mut MetricsSnapshot) {
        // The one that worked last time, first. Sources are cheap to read and expensive to
        // find, so the search is what gets rate-limited, not the reading.
        if let Some(i) = self.preferred {
            if let Some(c) = self.sources[i].read().filter(|c| plausible(*c)) {
                into.cpu.temp_c = Some(c);
                return;
            }
            tracing::info!(source = self.sources[i].name(), "stopped answering");
            self.preferred = None;
            self.next_retry = Instant::now() + RETRY_INTERVAL;
        }

        if Instant::now() < self.next_retry {
            return;
        }
        self.next_retry = Instant::now() + RETRY_INTERVAL;

        for (i, source) in self.sources.iter_mut().enumerate() {
            if let Some(c) = source.read().filter(|c| plausible(*c)) {
                tracing::info!(source = source.name(), "processor temperature");
                self.preferred = Some(i);
                into.cpu.temp_c = Some(c);
                return;
            }
        }
    }
}

/// Rejects a reading no processor in a working computer produces.
///
/// The value crosses a process boundary in a format written by another program, and a label
/// match that goes wrong yields a fan speed or a voltage rather than an error.
fn plausible(celsius: f32) -> bool {
    (1.0..=125.0).contains(&celsius)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixed(&'static str, Option<f32>);

    impl CpuTempSource for Fixed {
        fn name(&self) -> &'static str {
            self.0
        }
        fn read(&mut self) -> Option<f32> {
            self.1
        }
    }

    fn sample(sources: Vec<Box<dyn CpuTempSource>>) -> Option<f32> {
        let mut s = CpuTempSampler::new(sources)?;
        let mut snapshot = MetricsSnapshot::default();
        s.sample(&mut snapshot);
        snapshot.cpu.temp_c
    }

    #[test]
    fn the_first_source_with_an_answer_is_used() {
        let temp = sample(vec![
            Box::new(Fixed("silent", None)),
            Box::new(Fixed("answering", Some(64.0))),
        ]);
        assert_eq!(temp, Some(64.0));
    }

    #[test]
    fn nothing_running_leaves_a_dash_rather_than_a_zero() {
        assert_eq!(sample(vec![Box::new(Fixed("silent", None))]), None);
        assert!(CpuTempSampler::new(Vec::new()).is_none());
    }

    #[test]
    fn an_absurd_reading_is_refused_rather_than_drawn() {
        // A label match that goes wrong across a process boundary yields a fan speed, not an
        // error, and 2400 degrees on screen would be worse than a dash.
        assert_eq!(sample(vec![Box::new(Fixed("confused", Some(2400.0)))]), None);
        assert_eq!(sample(vec![Box::new(Fixed("confused", Some(0.0)))]), None);
        assert!(plausible(64.0));
    }

    #[test]
    fn a_source_that_stops_answering_does_not_freeze_its_last_reading() {
        // A monitor that is closed mid-session must take its reading with it. A temperature
        // frozen at whatever it was when the program quit is worse than no temperature: it
        // looks live.
        struct Fading(u32);
        impl CpuTempSource for Fading {
            fn name(&self) -> &'static str {
                "fading"
            }
            fn read(&mut self) -> Option<f32> {
                self.0 += 1;
                (self.0 <= 1).then_some(64.0)
            }
        }

        let mut s = CpuTempSampler::new(vec![Box::new(Fading(0))]).unwrap();
        let mut snapshot = MetricsSnapshot::default();
        s.sample(&mut snapshot);
        assert_eq!(snapshot.cpu.temp_c, Some(64.0));

        let mut next = MetricsSnapshot::default();
        s.sample(&mut next);
        assert_eq!(next.cpu.temp_c, None, "the reading must go with its source");
    }
}
