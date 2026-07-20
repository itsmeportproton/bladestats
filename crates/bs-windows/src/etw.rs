//! Frame timing from ETW, without injecting anything into the game.
//!
//! This is the piece that makes bladestats different from a conventional overlay. Rather than
//! hooking `Present` inside the target process, it subscribes to the events the graphics
//! kernel already emits and derives frame times from their timestamps. The game is never
//! touched; from its point of view nothing is observing it at all.
//!
//! `Microsoft-Windows-DxgKrnl` is the provider rather than `Microsoft-Windows-DXGI` because
//! every present goes through the graphics kernel regardless of API. The DXGI provider only
//! sees Direct3D, which would leave Vulkan titles reporting nothing.
//!
//! Requires administrator rights: creating a real-time ETW session is a privileged operation.
//! Without them the overlay runs on and shows a dash for the frame rate.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Result, anyhow};
use bs_core::{FrameMetrics, FrameTimeline};
use ferrisetw::provider::{EventFilter, Provider};
use ferrisetw::trace::UserTrace;
use ferrisetw::{EventRecord, SchemaLocator};

/// `Microsoft-Windows-DxgKrnl`.
const DXGKRNL_GUID: &str = "802ec45a-1e99-4b83-9920-87c98277ba9d";

/// The `Base` keyword, which is where present and flip events live.
///
/// Enabling everything would drown the callback in events the overlay has no use for and
/// spend real CPU doing it, which for this project would be self-defeating.
const KEYWORD_BASE: u64 = 0x1;

/// Informational. Verbose adds a great deal of noise and nothing we read.
const LEVEL_INFORMATION: u8 = 4;

/// Session name. Fixed rather than random so an orphan from a previous crash can be found and
/// stopped instead of accumulating.
const SESSION_NAME: &str = "bladestats-frames";

/// Event IDs on DxgKrnl that mark a frame being handed to the display.
///
/// These are not contractual — the provider's manifest can change between Windows builds — so
/// [`FrameSource::observed_events`] records what actually arrived, and the self-test compares
/// the resulting rate against a known one.
const PRESENT_EVENT_IDS: &[u16] = &[
    184, // Present, Start
];

/// Converts an ETW timestamp to nanoseconds.
///
/// The session is not created with `PROCESS_TRACE_MODE_RAW_TIMESTAMP`, so the field holds
/// FILETIME units — 100 ns since 1601. The precision comes from the session's clock rather
/// than from the coarse system tick, which is what makes frame timing viable at all.
fn timestamp_ns(raw: i64) -> u64 {
    (raw.max(0) as u64).saturating_mul(100)
}

pub struct FrameSource {
    timeline: Arc<Mutex<FrameTimeline>>,
    /// Only events from this process are recorded. Written by the UI thread when the
    /// foreground window changes, read inside the ETW callback.
    target_pid: Arc<AtomicU32>,
    /// Counts every event the callback saw, whether or not it was used. Diagnostic only.
    seen: Arc<AtomicU64>,
    /// Counts events that were actually treated as frames.
    used: Arc<AtomicU64>,
    /// Kept alive for as long as the source is: dropping it stops the session.
    _trace: UserTrace,
}

impl FrameSource {
    /// Opens the ETW session and starts consuming events on a background thread.
    ///
    /// Fails without administrator rights, which is expected and must be handled by the
    /// caller rather than treated as fatal.
    pub fn start() -> Result<Self> {
        if !is_elevated() {
            return Err(anyhow!(
                "frame timing needs administrator rights: creating an ETW session is privileged"
            ));
        }

        let timeline = Arc::new(Mutex::new(FrameTimeline::default()));
        let target_pid = Arc::new(AtomicU32::new(0));
        let seen = Arc::new(AtomicU64::new(0));
        let used = Arc::new(AtomicU64::new(0));

        let callback = {
            let timeline = Arc::clone(&timeline);
            let target_pid = Arc::clone(&target_pid);
            let seen = Arc::clone(&seen);
            let used = Arc::clone(&used);

            move |record: &EventRecord, _: &SchemaLocator| {
                seen.fetch_add(1, Ordering::Relaxed);

                let wanted = target_pid.load(Ordering::Relaxed);
                if wanted == 0 || record.process_id() != wanted {
                    return;
                }
                if !PRESENT_EVENT_IDS.contains(&record.event_id()) {
                    return;
                }

                used.fetch_add(1, Ordering::Relaxed);
                // The callback runs on an ETW thread and must not block: a poisoned or
                // contended lock is worth skipping a frame over, never worth panicking over.
                if let Ok(mut timeline) = timeline.lock() {
                    timeline.push(timestamp_ns(record.raw_timestamp()));
                }
            }
        };

        // An ETW session outlives the process that created it. A crash, a kill, or Ctrl+C all
        // leave ours running, and the next launch would then fail with AlreadyExist forever.
        // Reclaiming the name is therefore part of starting up, not an error path.
        stop_orphan_session();

        let provider = Provider::by_guid(DXGKRNL_GUID)
            .add_callback(callback)
            // Filtering in the kernel rather than in the callback. The Base keyword on this
            // provider carries tens of thousands of events per second and only a few dozen of
            // them are presents; without this the session costs several times what the whole
            // overlay does, which for this project would defeat the point.
            .add_filter(EventFilter::ByEventIds(PRESENT_EVENT_IDS.to_vec()))
            .any(KEYWORD_BASE)
            .level(LEVEL_INFORMATION)
            .build();

        // TraceError does not implement std::error::Error, so anyhow's context cannot be
        // attached directly and the message is composed by hand.
        let trace = UserTrace::new()
            .named(SESSION_NAME.to_string())
            .enable(provider)
            .start_and_process()
            .map_err(|e| anyhow!("could not start the ETW session: {e:?}"))?;

        tracing::info!(session = SESSION_NAME, "frame timing active");

        Ok(Self {
            timeline,
            target_pid,
            seen,
            used,
            _trace: trace,
        })
    }

    /// Points the source at a different process, discarding the previous game's history.
    pub fn set_target(&self, pid: u32) {
        if self.target_pid.swap(pid, Ordering::Relaxed) != pid
            && let Ok(mut timeline) = self.timeline.lock()
        {
            // Frame times from the previous game say nothing about this one.
            timeline.clear();
        }
    }

    pub fn metrics(&self, now_ns: u64) -> Option<FrameMetrics> {
        self.timeline.lock().ok()?.metrics(now_ns)
    }

    /// How many events arrived and how many were treated as frames.
    ///
    /// Exists because the event IDs above are an empirical claim about an undocumented
    /// manifest: a large `seen` with a zero `used` means the IDs need revisiting, and that
    /// should be diagnosable without a debugger.
    pub fn observed_events(&self) -> (u64, u64) {
        (
            self.seen.load(Ordering::Relaxed),
            self.used.load(Ordering::Relaxed),
        )
    }
}

/// Stops a session of ours left behind by a previous run.
///
/// Nothing here is an error: the usual outcome is "no such session", which simply means the
/// last run shut down cleanly.
fn stop_orphan_session() {
    use windows::Win32::System::Diagnostics::Etw::{
        CONTROLTRACE_HANDLE, ControlTraceW, EVENT_TRACE_CONTROL_STOP, EVENT_TRACE_PROPERTIES,
    };
    use windows::core::HSTRING;

    // ControlTraceW writes the session name back into the tail of this buffer, so it has to
    // be larger than the struct itself and the struct has to say where the name begins.
    const NAME_BYTES: usize = 1024;
    let size = size_of::<EVENT_TRACE_PROPERTIES>() + NAME_BYTES;
    let mut buffer = vec![0u8; size];

    unsafe {
        let properties = buffer.as_mut_ptr() as *mut EVENT_TRACE_PROPERTIES;
        (*properties).Wnode.BufferSize = size as u32;
        (*properties).LoggerNameOffset = size_of::<EVENT_TRACE_PROPERTIES>() as u32;

        let name = HSTRING::from(SESSION_NAME);
        let status = ControlTraceW(
            CONTROLTRACE_HANDLE::default(),
            &name,
            properties,
            EVENT_TRACE_CONTROL_STOP,
        );
        if status.is_ok() {
            tracing::info!(
                session = SESSION_NAME,
                "reclaimed an ETW session left over from a previous run"
            );
        }
    }
}

/// The current time on the same scale as the event timestamps.
pub fn now_ns() -> u64 {
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::System::SystemInformation::GetSystemTimePreciseAsFileTime;

    let ft: FILETIME = unsafe { GetSystemTimePreciseAsFileTime() };
    let quad = ((ft.dwHighDateTime as u64) << 32) | ft.dwLowDateTime as u64;
    quad.saturating_mul(100)
}

/// Whether this process is running elevated.
fn is_elevated() -> bool {
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Security::{
        GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation,
    };
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return false;
        }

        let mut elevation = TOKEN_ELEVATION::default();
        let mut size = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            Some(&mut elevation as *mut _ as *mut _),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut size,
        )
        .is_ok();

        let _ = windows::Win32::Foundation::CloseHandle(token);
        ok && elevation.TokenIsElevated != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filetime_units_become_nanoseconds() {
        // One FILETIME tick is 100 ns.
        assert_eq!(timestamp_ns(1), 100);
        assert_eq!(timestamp_ns(10_000_000), 1_000_000_000); // one second
    }

    #[test]
    fn a_negative_timestamp_does_not_wrap_around() {
        // ETW should never produce one, but an i64 to u64 cast of a negative value would
        // become an enormous timestamp and freeze the frame rate at a nonsense value.
        assert_eq!(timestamp_ns(-1), 0);
    }

    #[test]
    fn the_clock_matches_the_event_timescale() {
        // now_ns and the event timestamps have to share an epoch, or every frame would look
        // stale the moment it arrived.
        let a = now_ns();
        std::thread::sleep(std::time::Duration::from_millis(50));
        let b = now_ns();

        let elapsed = b.saturating_sub(a);
        assert!(
            (40_000_000..500_000_000).contains(&elapsed),
            "50 ms of sleep measured as {elapsed} ns — wrong timescale"
        );
    }

    #[test]
    fn elevation_check_answers_without_panicking() {
        // The value depends on how the test was launched; that it returns at all is the point.
        let _ = is_elevated();
    }
}
