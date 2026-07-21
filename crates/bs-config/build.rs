//! Embeds the Windows application manifest into the executable.
//!
//! Frame timing needs a real-time ETW session, and creating one is privileged. Asking for
//! elevation up front means Windows prompts once, instead of the program starting, silently
//! failing to open the session and showing a blank frame rate that reads as a bug.
//!
//! The elevation request goes through `/MANIFESTUAC` rather than through the manifest file.
//! rustc embeds its own default manifest declaring `level="asInvoker"`, and mt.exe refuses to
//! merge two snippets that disagree; the linker flag overrides the default instead of
//! conflicting with it.
//!
//! Both flags use `rustc-link-arg-bins`, which excludes the library target and therefore the
//! unit tests. A binary demanding elevation cannot be launched by cargo, so without that
//! distinction `cargo test` fails with "the requested operation requires elevation" before a
//! single test runs. The bin's own test harness is disabled in Cargo.toml for the same reason.

use std::path::Path;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=bladestats.manifest");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    // The GNU toolchain takes a manifest as a linked resource rather than a link argument,
    // which is a different mechanism; only MSVC is wired up here.
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("msvc") {
        println!("cargo:warning=manifest not embedded: only the MSVC toolchain is handled");
        return;
    }

    let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("bladestats.manifest");
    println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
    println!(
        "cargo:rustc-link-arg-bins=/MANIFESTUAC:level='requireAdministrator' uiAccess='false'"
    );
    println!(
        "cargo:rustc-link-arg-bins=/MANIFESTINPUT:{}",
        manifest.display()
    );

    embed_icon();
}

/// Compiles the application icon into the executable.
///
/// The icon has to be a real resource rather than a file loaded at run time: Explorer, the
/// taskbar and Alt-Tab all read it out of the executable without asking the program anything.
/// The notification area then loads the same one by identifier, so there is a single icon
/// rather than one for the shell and another for the tray.
///
/// A missing resource compiler is a warning and not a failure. It is part of the Windows SDK
/// and therefore present wherever this is normally built, but a build that produces a working
/// program with a plain icon is better than one that produces nothing.
fn embed_icon() {
    println!("cargo:rerun-if-changed=bladestats.rc");
    println!("cargo:rerun-if-changed=../../assets/icon/bladestats.ico");

    let Some(rc) = resource_compiler() else {
        println!("cargo:warning=icon not embedded: no resource compiler found");
        return;
    };

    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("bladestats.rc");
    let out = Path::new(&std::env::var("OUT_DIR").unwrap()).join("bladestats.res");

    let status = std::process::Command::new(&rc)
        .arg("/nologo")
        .arg("/fo")
        .arg(&out)
        .arg(&source)
        .status();

    match status {
        Ok(status) if status.success() => {
            // The linker takes a compiled resource file as an ordinary input.
            println!("cargo:rustc-link-arg-bins={}", out.display());
        }
        Ok(status) => println!("cargo:warning=icon not embedded: rc.exe exited with {status}"),
        Err(e) => println!("cargo:warning=icon not embedded: could not run rc.exe: {e}"),
    }
}

/// Finds `rc.exe` in the Windows SDK.
///
/// Searched for rather than assumed, because its path carries the SDK version and that differs
/// between machines. The newest one wins, which is what a version-sorted maximum gives here
/// since the directories are named by version.
fn resource_compiler() -> Option<std::path::PathBuf> {
    // Already on PATH in a developer command prompt.
    if std::process::Command::new("rc.exe")
        .arg("/?")
        .output()
        .is_ok()
    {
        return Some("rc.exe".into());
    }

    let roots = [
        std::env::var("ProgramFiles(x86)").unwrap_or_default(),
        std::env::var("ProgramFiles").unwrap_or_default(),
    ];
    let arch = if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        "x64"
    };

    roots
        .iter()
        .filter(|root| !root.is_empty())
        .map(|root| Path::new(root).join("Windows Kits/10/bin"))
        .filter_map(|bin| std::fs::read_dir(bin).ok())
        .flatten()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.join(arch).join("rc.exe").is_file())
        .max()
        .map(|path| path.join(arch).join("rc.exe"))
}
