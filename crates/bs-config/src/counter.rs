//! Starting and stopping the counter process from the settings window.
//!
//! The counter is another copy of this executable, run with a flag. Keeping it in its own
//! process is what lets the settings window be as heavy as a UI toolkit makes it while the
//! thing left running during a game stays small.

use std::process::{Child, Command};

pub struct Counter {
    child: Option<Child>,
    /// Set when it could not be started, with the reason, so the window can say so rather
    /// than showing a counter that is not there.
    error: Option<String>,
}

impl Counter {
    /// Starts a counter, unless one is already running.
    pub fn start(flag: &str) -> Self {
        if is_already_running() {
            tracing::info!("a counter is already running; leaving it alone");
            return Self {
                child: None,
                error: None,
            };
        }

        let exe = match std::env::current_exe() {
            Ok(exe) => exe,
            Err(e) => {
                return Self {
                    child: None,
                    error: Some(e.to_string()),
                };
            }
        };

        match Command::new(exe).arg(flag).spawn() {
            Ok(child) => {
                tracing::info!(pid = child.id(), "counter started");
                Self {
                    child: Some(child),
                    error: None,
                }
            }
            Err(e) => {
                tracing::error!(error = %e, "could not start the counter");
                Self {
                    child: None,
                    error: Some(e.to_string()),
                }
            }
        }
    }

    /// Whether the counter is up. Checked rather than remembered, so a counter that has died
    /// is reported honestly instead of assumed to be fine.
    pub fn running(&mut self) -> bool {
        match &mut self.child {
            Some(child) => matches!(child.try_wait(), Ok(None)),
            // No child of ours, but one may have been running before this window opened.
            None => self.error.is_none() && is_already_running(),
        }
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Stops the counter and waits for it to go.
    pub fn stop(&mut self) {
        if let Some(child) = &mut self.child {
            let _ = child.kill();
            let _ = child.wait();
            tracing::info!("counter stopped");
        }
        self.child = None;
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
