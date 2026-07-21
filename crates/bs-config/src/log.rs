//! What changed, in the user's terms.
//!
//! Written from the reader's side of the screen rather than from the commit log: "frame timing
//! without injection" rather than the provider and keyword that made it work.

/// One release, with the entries it brought.
///
/// Grouped rather than kept as one flat list because the window draws a version and a date
/// above each group. Flat, every entry would sit under whatever version happened to be current,
/// and the nine that shipped first would be relabelled with every release after them.
pub struct Release {
    pub version: &'static str,
    pub date: &'static str,
    /// Drawn beside the version. `None` for a plain release.
    pub tag: Option<&'static str>,
    /// `(lead-in, rest, is a fix)`. The lead-in is set brighter, so a list of changes can be
    /// skimmed by what each one is about.
    pub entries: &'static [(&'static str, &'static str, bool)],
}

/// Newest first, which is the order they are drawn in.
pub const RELEASES: &[Release] = &[
    Release {
        version: "0.1.1",
        date: "2026-07-21",
        tag: Some("beta"),
        entries: LATEST,
    },
    Release {
        version: "0.1.0",
        date: "2026-07-20",
        tag: None,
        entries: FIRST,
    },
];

const LATEST: &[(&str, &str, bool)] = &[
    (
        "Radeon sensors.",
        "Temperature, hotspot, board power, clocks and fan, read from the driver's own \
         library. No third-party program and nothing installed.",
        false,
    ),
    (
        "Processor power, measured.",
        "Read from the package rather than modelled from a thermal envelope. On AMD it is now \
         a sensor reading and has lost its tilde.",
        false,
    ),
    (
        "Memory as firmware describes it.",
        "Generation, module sizes, the rated speed beside the configured one, and the live \
         transfer rate — which moves, because the controller clocks down when idle.",
        false,
    ),
    (
        "The panel appears in games and stays out of the way otherwise.",
        "It unrolls when a fullscreen window starts presenting and rolls away when one stops. \
         Ctrl+Alt+B overrides it either way.",
        false,
    ),
    (
        "Redrawn at the display's rate,",
        "so readings ease between samples instead of stepping. It waits on the display rather \
         than polling, and stops drawing entirely when nothing is moving.",
        false,
    ),
    (
        "Fixed:",
        "the frame rate flickered far above the refresh rate on a synchronised game. It was \
         the reciprocal of a single frame's interval, and present timestamps jitter either \
         side of the true one; it is now counted across half a second.",
        true,
    ),
    (
        "Fixed:",
        "the mouse cursor appeared over the panel during mouse-look. Cursor visibility is \
         counted per input queue, so a game hiding its own did not hide it here.",
        true,
    ),
    (
        "Fixed:",
        "the panel jittered sideways as readings changed width. Each now reserves the widest \
         form it can take, so 99 becoming 100 fills a space that was already there.",
        true,
    ),
];

const FIRST: &[(&str, &str, bool)] = &[
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
    "Processor temperature needs a hardware monitor running. It is the one reading with no \
     path that avoids a kernel driver, and bladestats ships none.",
    "Intel graphics temperature and power are not read yet. AMD and NVIDIA are.",
    "The upscaler and frame generation are named, never measured. How much either is doing \
     lives inside the engine, and a dash means not visible rather than off.",
    "Memory power is never shown. Consumer boards have no sensor for it.",
];
