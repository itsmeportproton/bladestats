//! What the counter costs, measured rather than assumed.
//!
//! bladestats promises a budget — a fraction of a core and a modest amount of memory — and a
//! promise nobody measures is a wish. This reports the process's own cost so the overlay can
//! check itself against the budget in the settings file and say something when it goes over.
//!
//! Two deliberate choices. Memory is reported as **private commit**, not working set: the
//! working set moves with system pressure and with whether anything else wanted the pages,
//! so a budget written against it would fire on machines that are merely busy. And processor
//! use is reported as a mean over ten seconds rather than as the latest reading, because
//! `GetProcessTimes` advances in units of about 15 ms — over a two-second gap that alone is
//! nearly a percent of a core, which is most of the budget being measured.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::FILETIME;
use windows::Win32::System::ProcessStatus::{
    GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
};
use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};

/// How often the counters are actually read. They are cheap, but not so cheap as to be worth
/// reading every frame for a figure that only matters over seconds.
pub const SAMPLE_INTERVAL: Duration = Duration::from_secs(2);

/// The span the processor figure is averaged over.
pub const WINDOW: Duration = Duration::from_secs(10);

/// Below this the timer's own resolution is a large part of the answer, so no figure is given
/// at all rather than a confident wrong one.
const MIN_SPAN: Duration = Duration::from_secs(4);

/// One reading of what this process costs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Usage {
    /// Percent of the whole machine, the way Task Manager counts it: one saturated core on a
    /// sixteen-thread part is 6.25%, not 100%. The budget in the settings file is written in
    /// the same units, because that is where the user will go to check.
    ///
    /// `None` until enough time has passed for the figure to mean anything.
    pub cpu_pct: Option<f32>,
    /// Private commit — memory this process alone is charged for.
    pub private_bytes: u64,
    /// Reported for the log only. Never compared against the budget.
    pub working_set_bytes: u64,
}

pub struct SelfStat {
    logical_cores: f64,
    /// Wall clock against cumulative processor time, oldest first. Only spans the window.
    history: VecDeque<(Instant, Duration)>,
    next_sample: Instant,
}

impl SelfStat {
    pub fn new() -> Self {
        Self {
            logical_cores: std::thread::available_parallelism().map_or(1.0, |n| n.get() as f64),
            history: VecDeque::new(),
            // The first call reads immediately: a first figure late is a first figure missing
            // for the whole of a short session.
            next_sample: Instant::now(),
        }
    }

    /// Takes a reading if one is due.
    ///
    /// Safe to call every frame: it rate-limits itself and returns `None` on the calls in
    /// between, so the caller needs no timer of its own.
    pub fn sample(&mut self) -> Option<Usage> {
        let now = Instant::now();
        if now < self.next_sample {
            return None;
        }
        self.next_sample = now + SAMPLE_INTERVAL;

        let cpu = process_cpu_time()?;
        let memory = process_memory()?;

        self.history.push_back((now, cpu));
        // Keep one sample older than the window, so the mean spans the full window rather
        // than whatever is left after trimming.
        while self.history.len() > 2 && now.duration_since(self.history[1].0) >= WINDOW {
            self.history.pop_front();
        }

        let (then, cpu_then) = *self.history.front()?;
        let span = now.duration_since(then);
        let cpu_pct = (span >= MIN_SPAN)
            .then(|| percent(cpu.saturating_sub(cpu_then), span, self.logical_cores));

        Some(Usage {
            cpu_pct,
            private_bytes: memory.0,
            working_set_bytes: memory.1,
        })
    }
}

impl Default for SelfStat {
    fn default() -> Self {
        Self::new()
    }
}

/// Appends readings to the file named by `--profile=<path>`, if that argument was given.
///
/// The release build has no console — `windows_subsystem = "windows"` is what stops a black
/// rectangle appearing behind every game — so the log a developer would read is not there
/// precisely when the figures are worth reading. A file sidesteps that, and costs nothing at
/// all when the argument is absent, which is always outside of a measuring session.
///
/// An argument rather than an environment variable because the counter runs elevated: a
/// process raised through UAC is started by a system service and does not inherit the
/// environment of whoever asked for it, so a variable set in a shell would never arrive.
pub struct ProfileLog {
    file: Option<std::fs::File>,
    started: Instant,
}

/// The argument that turns profiling on, as `--profile=C:\path\to\file.csv`.
pub const PROFILE_ARG: &str = "--profile=";

/// What the overlay was doing when a reading was taken.
#[derive(Debug, Clone, Copy, Default)]
pub struct Context {
    /// The frame rate being reported, if there is a frame source and a game presenting.
    pub fps: Option<f32>,
    /// The process being reported on. Zero means nothing was being tracked.
    pub target: Option<u32>,
    /// Whether the overlay was drawing at all. False in exclusive fullscreen.
    pub visible: bool,
}

impl ProfileLog {
    pub fn from_args() -> Self {
        let file = profile_path().and_then(|path| match std::fs::File::create(&path) {
            Ok(mut f) => {
                use std::io::Write;
                let _ = writeln!(
                    f,
                    "seconds,cpu_percent,private_mb,working_set_mb,fps,target,visible"
                );
                tracing::info!(path = %path, "profiling");
                Some(f)
            }
            Err(e) => {
                tracing::warn!(error = %e, path = %path, "could not open the profile log");
                None
            }
        });
        Self {
            file,
            started: Instant::now(),
        }
    }

    /// Records one reading together with what the overlay was doing at the time.
    ///
    /// The cost alone is not evidence. An idle counter on a desktop and a counter whose game
    /// went exclusive-fullscreen — so it hid itself and stopped drawing — produce the same
    /// numbers, and a measurement that cannot tell those apart proves nothing about the
    /// budget. The frame rate and the target say which of the two was actually measured.
    pub fn record(&mut self, usage: &Usage, ctx: Context) {
        let Some(file) = &mut self.file else { return };
        use std::io::Write;
        const MB: f64 = 1024.0 * 1024.0;
        let _ = writeln!(
            file,
            "{:.1},{},{:.1},{:.1},{},{},{}",
            self.started.elapsed().as_secs_f32(),
            usage.cpu_pct.map_or(String::new(), |p| format!("{p:.3}")),
            usage.private_bytes as f64 / MB,
            usage.working_set_bytes as f64 / MB,
            ctx.fps.map_or(String::new(), |f| format!("{f:.0}")),
            ctx.target.unwrap_or(0),
            u8::from(ctx.visible),
        );
    }

    pub fn is_on(&self) -> bool {
        self.file.is_some()
    }
}

fn profile_path() -> Option<String> {
    std::env::args().find_map(|a| a.strip_prefix(PROFILE_ARG).map(str::to_owned))
}

/// Processor time as a share of the whole machine over `span`.
///
/// Split out from the reading so the arithmetic can be tested without a process to measure.
fn percent(cpu: Duration, span: Duration, logical_cores: f64) -> f32 {
    if span.is_zero() || logical_cores <= 0.0 {
        return 0.0;
    }
    ((cpu.as_secs_f64() / span.as_secs_f64() / logical_cores) * 100.0) as f32
}

/// Kernel plus user time this process has used since it started.
fn process_cpu_time() -> Option<Duration> {
    let mut creation = FILETIME::default();
    let mut exit = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();

    unsafe {
        GetProcessTimes(
            GetCurrentProcess(),
            &mut creation,
            &mut exit,
            &mut kernel,
            &mut user,
        )
    }
    .ok()?;

    Some(interval(kernel) + interval(user))
}

/// A `FILETIME` used as a duration rather than as a date: hundred-nanosecond units.
fn interval(t: FILETIME) -> Duration {
    let ticks = ((t.dwHighDateTime as u64) << 32) | t.dwLowDateTime as u64;
    Duration::from_nanos(ticks.saturating_mul(100))
}

/// Private commit and working set, in bytes.
fn process_memory() -> Option<(u64, u64)> {
    let mut counters = PROCESS_MEMORY_COUNTERS_EX::default();
    let size = size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;

    // The extended form is passed through the base pointer, which is how this API has always
    // been told which structure it was given: by its size.
    unsafe {
        GetProcessMemoryInfo(
            GetCurrentProcess(),
            &mut counters as *mut _ as *mut PROCESS_MEMORY_COUNTERS,
            size,
        )
    }
    .ok()?;

    Some((counters.PrivateUsage as u64, counters.WorkingSetSize as u64))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_saturated_core_is_reported_as_a_share_of_the_machine() {
        // A whole second of processor time in a second of wall clock, on sixteen threads.
        let pct = percent(Duration::from_secs(1), Duration::from_secs(1), 16.0);
        assert!((pct - 6.25).abs() < 1e-3, "{pct}");
    }

    #[test]
    fn an_idle_process_reads_as_zero_and_a_stopped_clock_does_not_divide_by_it() {
        assert_eq!(percent(Duration::ZERO, Duration::from_secs(5), 8.0), 0.0);
        assert_eq!(percent(Duration::from_secs(1), Duration::ZERO, 8.0), 0.0);
        assert_eq!(percent(Duration::from_secs(1), Duration::from_secs(1), 0.0), 0.0);
    }

    #[test]
    fn a_filetime_interval_is_hundreds_of_nanoseconds() {
        let one_second = FILETIME {
            dwLowDateTime: 10_000_000,
            dwHighDateTime: 0,
        };
        assert_eq!(interval(one_second), Duration::from_secs(1));

        // The high word matters: a long session overflows the low one in about seven minutes
        // of processor time, and dropping it would make the figure collapse to nearly zero.
        let past_the_low_word = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 1,
        };
        assert_eq!(
            interval(past_the_low_word),
            Duration::from_nanos(u64::from(u32::MAX) as u64 * 100 + 100)
        );
    }

    #[test]
    fn the_first_reading_withholds_a_processor_figure_rather_than_guessing() {
        let mut stat = SelfStat::new();
        let usage = stat.sample().expect("this process can measure itself");
        assert!(
            usage.cpu_pct.is_none(),
            "one reading cannot describe a rate"
        );
        assert!(
            usage.private_bytes > 0,
            "a running process has committed memory"
        );
    }

    #[test]
    fn readings_are_rate_limited_so_the_caller_needs_no_timer() {
        let mut stat = SelfStat::new();
        assert!(stat.sample().is_some());
        assert!(
            stat.sample().is_none(),
            "a second reading in the same instant is not a new one"
        );
    }
}
