//! Starting and stopping the counter process from the settings window.
//!
//! The counter is another copy of this executable, run with a flag. Keeping it in its own
//! process is what lets the settings window be as heavy as a UI toolkit makes it while the
//! thing left running during a game stays small.

use std::process::{Child, Command};
use std::time::{Duration, Instant};

/// Tells a copy of this program to be the counter rather than the window.
///
/// Lives here rather than beside `main` so the settings window can start one back up after
/// stopping it, without the two spellings drifting apart.
pub const COUNTER_FLAG: &str = "--counter";

/// How long an answer about somebody else's counter is trusted for.
///
/// Asking the system costs a whole process, and the window asks on every frame it draws. At
/// sixty frames a second that is sixty `tasklist` invocations, which is enough to make the
/// window it is drawn in visibly stutter.
const RUNNING_CACHE: Duration = Duration::from_millis(1000);

pub struct Counter {
    child: Option<Child>,
    /// Set when it could not be started, with the reason, so the window can say so rather
    /// than showing a counter that is not there.
    error: Option<String>,
    /// The last answer about a counter this window does not own, and when it was given.
    seen: Option<(Instant, bool)>,
}

impl Counter {
    /// Starts a counter, unless one is already running.
    pub fn start(flag: &str) -> Self {
        if is_already_running() {
            tracing::info!("a counter is already running; leaving it alone");
            return Self {
                child: None,
                error: None,
                seen: None,
            };
        }

        let exe = match std::env::current_exe() {
            Ok(exe) => exe,
            Err(e) => {
                return Self {
                    child: None,
                    error: Some(e.to_string()),
                    seen: None,
                };
            }
        };

        match Command::new(exe).arg(flag).spawn() {
            Ok(child) => {
                tracing::info!(pid = child.id(), "counter started");
                Self {
                    child: Some(child),
                    error: None,
                    seen: None,
                }
            }
            Err(e) => {
                tracing::error!(error = %e, "could not start the counter");
                Self {
                    child: None,
                    error: Some(e.to_string()),
                    seen: None,
                }
            }
        }
    }

    /// Whether the counter is up. Checked rather than remembered, so a counter that has died
    /// is reported honestly instead of assumed to be fine.
    pub fn running(&mut self) -> bool {
        if let Some(child) = &mut self.child {
            // Ours: asking costs nothing, so it is asked every time.
            return matches!(child.try_wait(), Ok(None));
        }
        if self.error.is_some() {
            return false;
        }

        // Not ours, so the only way to know is to ask the system — and that means starting a
        // process. The window draws this on every frame, so the answer is kept for a moment
        // rather than bought sixty times a second. A second's lag on a status light nobody is
        // watching for is not a cost; the stutter it replaces was.
        if let Some((at, was)) = self.seen
            && at.elapsed() < RUNNING_CACHE
        {
            return was;
        }
        let now = is_already_running();
        self.seen = Some((Instant::now(), now));
        now
    }

    /// Forgets the cached answer, for when this window has just changed it.
    fn invalidate(&mut self) {
        self.seen = None;
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Stops the counter and waits for it to go.
    ///
    /// Also stops one this window did not start. A counter left over from an earlier session
    /// is still a counter drawing over the user's games, and "I did not start it" is no reason
    /// to leave them hunting for it in Task Manager.
    pub fn stop(&mut self) {
        if let Some(child) = &mut self.child {
            let _ = child.kill();
            let _ = child.wait();
            tracing::info!("counter stopped");
        }
        self.child = None;
        self.error = None;
        stop_strays();
        self.invalidate();
    }
}

/// Whether some other copy of this program is already being the counter.
///
/// Prevents a second counter when the settings window is opened again while one is running:
/// two overlays stacked on the same corner look like one broken one.
#[cfg(windows)]
fn is_already_running() -> bool {
    use std::os::windows::process::CommandExt;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    // Asking the system rather than tracking it ourselves, since the counter may have been
    // started by a settings window that has since been closed.
    let Ok(output) = Command::new("tasklist")
        .args(["/FI", "IMAGENAME eq bladestats.exe", "/FO", "CSV", "/NH"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
    else {
        return false;
    };

    // Our own process is in that list too, so one match means only us.
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| line.contains("bladestats.exe"))
        .count()
        > 1
}

#[cfg(not(windows))]
fn is_already_running() -> bool {
    false
}

/// Ends any other copy of this program that is being the counter.
///
/// By process name and not by remembering a handle, because the one worth killing is usually
/// the one this window never met. Our own process is excluded by identifier rather than by
/// name — the counter and the settings window are the same executable, and killing by name
/// alone would take this window with it.
#[cfg(windows)]
fn stop_strays() {
    use std::os::windows::process::CommandExt;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    let own = std::process::id();
    let Ok(output) = Command::new("tasklist")
        .args(["/FI", "IMAGENAME eq bladestats.exe", "/FO", "CSV", "/NH"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
    else {
        return;
    };

    // `"bladestats.exe","1234","Console","1","12,345 K"` — the identifier is the second field.
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let Some(pid) = line
            .split("\",\"")
            .nth(1)
            .and_then(|field| field.trim_matches('"').parse::<u32>().ok())
        else {
            continue;
        };
        if pid == own {
            continue;
        }
        let killed = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/F"])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .is_ok_and(|o| o.status.success());
        tracing::info!(pid, killed, "stopped a counter this window did not start");
    }
}

#[cfg(not(windows))]
fn stop_strays() {}

#[cfg(all(test, windows))]
mod tests {
    /// The field the stray-stopping walk depends on, in the shape `tasklist` prints it.
    ///
    /// Worth pinning because the consequence of reading the wrong column is not a failure to
    /// stop the counter — it is passing something that is not an identifier to `taskkill`.
    #[test]
    fn the_process_identifier_is_the_second_csv_field() {
        let line = "\"bladestats.exe\",\"17164\",\"Console\",\"1\",\"12,345 K\"";
        let pid = line
            .split("\",\"")
            .nth(1)
            .and_then(|f| f.trim_matches('"').parse::<u32>().ok());
        assert_eq!(pid, Some(17164));

        // Memory carries a thousands separator, which is why the split is on the quoted comma
        // and not on the comma.
        let memory = line.split("\",\"").nth(4);
        assert_eq!(memory, Some("12,345 K\""));
    }
}
