//! Finding, fetching and starting LibreHardwareMonitor.
//!
//! bladestats needs it for exactly one reading — the processor's temperature — and cannot take
//! that reading itself. It lives behind the system management unit and reaching it means a
//! kernel driver. This program ships none.
//!
//! **What that means for the user is stated plainly rather than buried**, because it is the
//! whole of the decision: LibreHardwareMonitor loads a driver of the WinRing0 family to read
//! those sensors. That family is on Microsoft's vulnerable-driver blocklist and is the first
//! thing anti-cheat software looks for. Installing it is a reasonable thing to do on a machine
//! you own; doing it quietly on somebody's behalf is not.
//!
//! Nothing here runs without being asked. Detection is free and happens at startup; the fetch
//! happens on a click and on nothing else.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Suppresses the console window a shelled-out tool would otherwise flash on screen.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// The official release, followed through GitHub's redirect to whatever is current.
///
/// Pinned to the project's own repository and to https. Following "latest" rather than a
/// version means one fewer thing to go stale, at the cost of not knowing in advance exactly
/// what arrives — which is why what arrives is checked before it is run.
const RELEASE_URL: &str = "https://github.com/LibreHardwareMonitor/LibreHardwareMonitor/releases/latest/download/LibreHardwareMonitor-net472.zip";

/// A release is a few megabytes. Anything far outside that is not one.
const MIN_BYTES: u64 = 512 * 1024;
const MAX_BYTES: u64 = 64 * 1024 * 1024;

/// Where things stand, as far as this program can tell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Not in our folder. It may be installed elsewhere; this only knows about its own copy.
    Missing,
    /// Present but not answering, so either it is not running or its web server is off.
    Installed,
    /// Answering on the expected port.
    Serving,
}

/// The folder bladestats keeps its own copy in.
///
/// Beside the executable rather than in Program Files or AppData, for the same reason the
/// settings live there: bladestats is meant to be unpacked and run, and to leave nothing
/// behind when it is deleted.
pub fn install_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
        .unwrap_or_default()
        .join("lhm")
}

fn executable() -> PathBuf {
    install_dir().join("LibreHardwareMonitor.exe")
}

/// Whether our copy is there, and whether it is answering.
pub fn status(port: u16) -> Status {
    if !executable().is_file() {
        return Status::Missing;
    }
    if serving(port) {
        Status::Serving
    } else {
        Status::Installed
    }
}

/// Whether something is serving a sensor tree on that port.
pub fn serving(port: u16) -> bool {
    std::net::TcpStream::connect_timeout(
        &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
        std::time::Duration::from_millis(150),
    )
    .is_ok()
}

/// Downloads a release and unpacks it into [`install_dir`].
///
/// Uses the tools Windows already ships rather than embedding an HTTP client and a zip
/// decoder. `curl` and `tar` have both been in System32 since Windows 10 1803, and pulling a
/// TLS stack and an archive library into this program for something that happens once — if
/// ever — would cost more than it is worth.
pub fn install() -> Result<(), String> {
    let dir = install_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("could not create {}: {e}", dir.display()))?;

    let archive = dir.join("release.zip");
    let _ = std::fs::remove_file(&archive);

    let curl = run(
        "curl",
        &[
            "--location",
            "--fail",
            "--silent",
            "--show-error",
            // A stalled connection has to end by itself; nobody is watching this.
            "--max-time",
            "120",
            "--output",
            &archive.to_string_lossy(),
            RELEASE_URL,
        ],
    )?;
    if !curl.is_empty() {
        return Err(curl);
    }

    // What arrived is checked before it is unpacked. Following "latest" means not knowing the
    // exact bytes in advance, and a redirect that lands on an error page is a small file, not
    // an archive.
    let size = std::fs::metadata(&archive)
        .map_err(|e| format!("the download did not arrive: {e}"))?
        .len();
    if !(MIN_BYTES..=MAX_BYTES).contains(&size) {
        let _ = std::fs::remove_file(&archive);
        return Err(format!("the download is {size} bytes, which is not a release"));
    }

    run(
        "tar",
        &[
            "-xf",
            &archive.to_string_lossy(),
            "-C",
            &dir.to_string_lossy(),
        ],
    )?;
    let _ = std::fs::remove_file(&archive);

    if !executable().is_file() {
        return Err("the archive did not contain LibreHardwareMonitor.exe".into());
    }
    Ok(())
}

/// Writes the settings that make it serve its sensor tree, then starts it.
///
/// The web server is off in a fresh installation and is switched on from this file. Since
/// bladestats owns this copy, it owns the file — which is the whole reason for installing a
/// copy rather than using one already on the machine.
pub fn configure_and_launch(port: u16) -> Result<(), String> {
    let config = install_dir().join("LibreHardwareMonitor.config");
    std::fs::write(&config, settings(port))
        .map_err(|e| format!("could not write {}: {e}", config.display()))?;

    // Elevated, because the sensors it is being started for need it, and started in its own
    // folder so it finds its settings.
    let exe = executable();
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        Command::new("powershell")
            .args([
                "-NoProfile",
                "-WindowStyle",
                "Hidden",
                "-Command",
                &format!(
                    "Start-Process -FilePath '{}' -WorkingDirectory '{}' -Verb RunAs",
                    exe.display(),
                    install_dir().display()
                ),
            ])
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .map_err(|e| format!("could not start it: {e}"))?;
    }
    #[cfg(not(windows))]
    let _ = exe;
    Ok(())
}

/// The settings file, with the web server on and the window out of the way.
///
/// A .NET application settings document. The keys are LibreHardwareMonitor's own; writing one
/// it does not recognise is harmless — it falls back to its defaults — which is why a failure
/// here shows up as "installed but not answering" rather than as an error, and why the window
/// says so and offers the manual route.
fn settings(port: u16) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<configuration>
  <appSettings>
    <add key="listenerPort" value="{port}" />
    <add key="runWebServerMenuItem" value="true" />
    <add key="minTrayMenuItem" value="true" />
    <add key="startMinMenuItem" value="true" />
    <add key="minCloseMenuItem" value="true" />
  </appSettings>
</configuration>
"#
    )
}

/// Runs a system tool and returns whatever it complained about.
fn run(program: &str, args: &[&str]) -> Result<String, String> {
    let mut command = Command::new(program);
    command.args(args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_NO_WINDOW);
    }

    let output = command
        .output()
        .map_err(|e| format!("{program} could not be run: {e}"))?;
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    let why = String::from_utf8_lossy(&output.stderr);
    Err(format!(
        "{program} failed: {}",
        why.trim().lines().last().unwrap_or("no reason given")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_copy_lives_beside_the_program_rather_than_in_the_system() {
        // bladestats is meant to be unpacked and run, and to leave nothing behind when it is
        // deleted. A folder in Program Files would outlive it.
        let dir = install_dir();
        assert!(dir.ends_with("lhm"), "{}", dir.display());
        let exe = std::env::current_exe().unwrap();
        assert!(dir.starts_with(exe.parent().unwrap()));
    }

    #[test]
    fn nothing_installed_is_reported_as_missing_rather_than_as_a_failure() {
        // The ordinary state on a first run, and it must not read as something being wrong.
        if !executable().is_file() {
            assert_eq!(status(1), Status::Missing);
        }
    }

    #[test]
    fn the_settings_turn_the_server_on_at_the_port_it_will_be_asked_on() {
        let written = settings(8085);
        assert!(written.contains(r#"key="listenerPort" value="8085""#));
        assert!(written.contains(r#"key="runWebServerMenuItem" value="true""#));
        // Started for a reading, not to be looked at.
        assert!(written.contains(r#"key="startMinMenuItem" value="true""#));
    }

    #[test]
    fn the_release_is_fetched_from_the_project_over_https() {
        // The one place this program reaches outside the machine. Pinned to the project's own
        // repository, and to a scheme that authenticates it.
        assert!(RELEASE_URL.starts_with("https://github.com/LibreHardwareMonitor/"));
        assert!(!RELEASE_URL.contains("http://"));
    }

    #[test]
    fn an_error_page_is_not_mistaken_for_an_archive() {
        // Following "latest" means not knowing the exact bytes in advance. A redirect landing
        // somewhere unexpected yields a few hundred bytes of HTML, which must not be handed
        // to the unpacker and certainly must not be run.
        assert!(!(MIN_BYTES..=MAX_BYTES).contains(&2_048));
        assert!((MIN_BYTES..=MAX_BYTES).contains(&4_000_000));
    }
}
