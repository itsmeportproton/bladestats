//! What changed, in the user's terms.
//!
//! Written from the reader's side of the screen rather than from the commit log: "frame timing
//! without injection" rather than the provider and keyword that made it work.

/// `(lead-in, rest, is a fix)`. The lead-in is set brighter, so a list of changes can be
/// skimmed by what each one is about.
pub const ENTRIES: &[(&str, &str, bool)] = &[
    (
        "Frame timing without injection.",
        "FPS, frame time and low percentiles read from the graphics kernel's own events. \
         Nothing is loaded into the game.",
        false,
    ),
    (
        "Direct3D and Vulkan alike.",
        "Presents are counted at the kernel, so Vulkan titles report the same as Direct3D ones.",
        false,
    ),
    (
        "Per-core load and clocks,",
        "with exact processor and graphics card names as Device Manager spells them.",
        false,
    ),
    (
        "Memory speed",
        "read from firmware rather than through WMI.",
        false,
    ),
    (
        "Settings applied live.",
        "The overlay picks up changes about a second after they are made, without restarting.",
        false,
    ),
    (
        "",
        "Overlay composited on the GPU, so transparency costs nothing on the processor.",
        false,
    ),
    (
        "Fixed:",
        "VRAM showed 0.0 GB. It was reporting bladestats' own video memory instead of the \
         whole system's.",
        true,
    ),
    (
        "Fixed:",
        "every graphics reading sat at zero on some machines, from a case-sensitive adapter \
         match.",
        true,
    ),
    (
        "Fixed:",
        "frame rate stayed blank when launched without administrator rights, with no \
         explanation on screen.",
        true,
    ),
];

/// Things the program cannot do, stated rather than left to be discovered.
pub const LIMITS: &[&str] = &[
    "Borderless only. Exclusive fullscreen cannot be drawn over, so the overlay hides itself \
     there.",
    "Graphics temperature and power need a vendor library; AMD and Intel are not read yet.",
    "Processor power is an estimate and processor temperature needs a kernel driver, so \
     neither is a sensor reading.",
    "Memory power is never shown. Consumer boards have no sensor for it.",
];
