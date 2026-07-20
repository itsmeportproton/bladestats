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
}
