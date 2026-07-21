//! Working out which process the overlay should be reporting on, and whether it can be drawn
//! over at all.

use windows::Win32::Foundation::{CloseHandle, HMODULE, HWND, RECT};
use windows::Win32::System::ProcessStatus::{
    EnumProcessModulesEx, GetModuleBaseNameW, LIST_MODULES_ALL,
};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_VM_READ,
};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, HMONITOR, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
};
use windows::Win32::UI::Shell::{QUNS_RUNNING_D3D_FULL_SCREEN, SHQueryUserNotificationState};
use windows::Win32::UI::WindowsAndMessaging::{
    GWL_STYLE, GetForegroundWindow, GetWindowLongPtrW, GetWindowRect, GetWindowThreadProcessId,
    WS_CAPTION, WS_THICKFRAME,
};

/// How the focused window is presenting, which decides whether an overlay is possible.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayMode {
    /// An ordinary window with decorations. The overlay works but is rarely wanted.
    Windowed,
    /// Borderless fullscreen. The mode bladestats is built for.
    Borderless,
    /// Exclusive fullscreen. Nothing can be composited on top without a hook, so the overlay
    /// hides rather than pretending otherwise.
    ExclusiveFullscreen,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Target {
    pub pid: u32,
    pub hwnd: HWND,
    pub mode: DisplayMode,
}

impl Target {
    /// Whether the overlay should be visible for this target.
    pub fn overlay_possible(&self) -> bool {
        self.mode != DisplayMode::ExclusiveFullscreen
    }
}

/// Identifies the process currently in the foreground.
///
/// Returns `None` for our own window and when there is no foreground window at all, so the
/// caller can leave the previous target in place rather than flapping.
pub fn current(own_pid: u32) -> Option<Target> {
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.is_invalid() {
            return None;
        }

        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == 0 || pid == own_pid {
            return None;
        }

        Some(Target {
            pid,
            hwnd,
            mode: display_mode(hwnd),
        })
    }
}

/// Which graphics API a process renders with, read from the libraries it has loaded.
///
/// The frame-timing events say nothing about this — the graphics kernel sees every present the
/// same way, which is exactly why it was chosen as the source. Subscribing to the API-specific
/// providers instead would mean a second trace session and a great deal more event traffic for
/// one word on screen. What a process has loaded is far cheaper and nearly as reliable.
///
/// **Order is the whole of the logic here**, because a process usually has several of these
/// open at once:
///
/// - Vulkan first: a Vulkan game still loads `dxgi.dll` for presentation, so testing DXGI
///   first would label every Vulkan title as Direct3D.
/// - D3D12 before D3D11: titles using the 11-on-12 compatibility layer load both, and the one
///   worth reporting is the one they actually render with.
/// - `dxgi.dll` on its own means Direct3D of *some* version, which is worth saying when
///   nothing more precise can be.
///
/// Returns `None` rather than guessing when nothing matches, or when the process cannot be
/// opened at all — a protected process is not an error, it is a process that keeps its own
/// counsel.
pub fn graphics_api(pid: u32) -> Option<&'static str> {
    rendering(pid).api
}

/// What a process is rendering with, as far as its loaded libraries reveal.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Rendering {
    pub api: Option<&'static str>,
    /// The upscaler in use, when it arrives as a library.
    pub upscaler: Option<&'static str>,
    /// Frame generation, likewise.
    pub frame_gen: Option<&'static str>,
}

/// Inspects a process once and answers all three questions.
///
/// One enumeration rather than three: opening a process and walking its module list is the
/// expensive half, and the matching afterwards is three passes over a few hundred strings.
pub fn rendering(pid: u32) -> Rendering {
    let Some(modules) = loaded_modules(pid) else {
        return Rendering::default();
    };
    Rendering {
        api: api_from_modules(&modules),
        upscaler: upscaler_from_modules(&modules),
        frame_gen: frame_gen_from_modules(&modules),
    }
}

/// Which upscaler a game has loaded.
///
/// **This says what is loaded, not what is switched on, and it cannot say by how much.** The
/// ratio of render resolution to output resolution lives inside the engine; reading it would
/// mean being inside the process, which this program is built not to be. So the reading is
/// named rather than measured, and two families are invisible to it entirely: FSR compiled
/// into an engine rather than shipped beside it, and driver-side upscaling — Radeon Super
/// Resolution, NVIDIA's own scaling — which never enters the game's address space at all.
fn upscaler_from_modules(modules: &[String]) -> Option<&'static str> {
    const UPSCALERS: [(&str, &str); 7] = [
        ("nvngx_dlss.dll", "DLSS"),
        ("libxess.dll", "XeSS"),
        ("libxess_dx11.dll", "XeSS"),
        ("ffx_fsr3upscaler_x64.dll", "FSR3"),
        ("ffx_fsr2_api_x64.dll", "FSR2"),
        ("amd_fidelityfx_dx12.dll", "FSR"),
        ("amd_fidelityfx_vk.dll", "FSR"),
    ];
    matching(modules, &UPSCALERS)
}

/// Whether frame generation is loaded, and whose.
///
/// Same caveat as above, and one more: the count of generated frames is not obtainable from
/// outside either. AMD's driver-level frame generation leaves no trace in the game's modules
/// at all, so silence here does not mean none.
fn frame_gen_from_modules(modules: &[String]) -> Option<&'static str> {
    const GENERATORS: [(&str, &str); 4] = [
        ("nvngx_dlssg.dll", "DLSS-G"),
        ("ffx_frameinterpolation_x64.dll", "FSR3 FG"),
        ("ffx_opticalflow_x64.dll", "FSR3 FG"),
        // A widely used replacement that presents itself as NVIDIA's and is not.
        ("dlssg_to_fsr3_amd_is_better.dll", "FSR3 FG"),
    ];
    matching(modules, &GENERATORS)
}

fn matching(modules: &[String], table: &[(&str, &'static str)]) -> Option<&'static str> {
    table
        .iter()
        .find(|(dll, _)| modules.iter().any(|m| m == dll))
        .map(|(_, name)| *name)
}

/// The ranking itself, separated so the ordering can be tested without a process to inspect.
fn api_from_modules(modules: &[String]) -> Option<&'static str> {
    const APIS: [(&str, &str); 7] = [
        ("vulkan-1.dll", "Vulkan"),
        ("d3d12.dll", "D3D12"),
        ("d3d11.dll", "D3D11"),
        ("d3d10.dll", "D3D10"),
        ("d3d9.dll", "D3D9"),
        ("opengl32.dll", "OpenGL"),
        ("dxgi.dll", "Direct3D"),
    ];
    APIS.iter()
        .find(|(dll, _)| modules.iter().any(|m| m == dll))
        .map(|(_, name)| *name)
}

/// Lowercased base names of every module loaded in a process.
fn loaded_modules(pid: u32) -> Option<Vec<String>> {
    unsafe {
        let process = OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ,
            false,
            pid,
        )
        .ok()?;

        let mut handles = [HMODULE::default(); 512];
        let mut needed = 0u32;
        let ok = EnumProcessModulesEx(
            process,
            handles.as_mut_ptr(),
            size_of_val(&handles) as u32,
            &mut needed,
            LIST_MODULES_ALL,
        );
        let count = if ok.is_ok() {
            (needed as usize / size_of::<HMODULE>()).min(handles.len())
        } else {
            0
        };

        let mut names = Vec::with_capacity(count);
        for handle in &handles[..count] {
            let mut buffer = [0u16; 260];
            let len = GetModuleBaseNameW(process, Some(*handle), &mut buffer);
            if len > 0 {
                names.push(String::from_utf16_lossy(&buffer[..len as usize]).to_lowercase());
            }
        }

        let _ = CloseHandle(process);
        (!names.is_empty()).then_some(names)
    }
}

/// Classifies how a window is presenting.
fn display_mode(hwnd: HWND) -> DisplayMode {
    // Exclusive fullscreen is asked about directly rather than guessed at from window styles.
    // This is the documented way to find out, and it is far more reliable than the usual
    // heuristics, which cannot tell exclusive fullscreen from a borderless window that
    // happens to cover the screen.
    if unsafe { SHQueryUserNotificationState() } == Ok(QUNS_RUNNING_D3D_FULL_SCREEN) {
        return DisplayMode::ExclusiveFullscreen;
    }
    if covers_its_monitor(hwnd) && !has_decorations(hwnd) {
        return DisplayMode::Borderless;
    }
    DisplayMode::Windowed
}

fn has_decorations(hwnd: HWND) -> bool {
    let style = unsafe { GetWindowLongPtrW(hwnd, GWL_STYLE) } as u32;
    style & (WS_CAPTION.0 | WS_THICKFRAME.0) != 0
}

fn covers_its_monitor(hwnd: HWND) -> bool {
    unsafe {
        let mut window = RECT::default();
        if GetWindowRect(hwnd, &mut window).is_err() {
            return false;
        }

        let monitor: HMONITOR = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if !GetMonitorInfoW(monitor, &mut info).as_bool() {
            return false;
        }

        rects_match(window, info.rcMonitor)
    }
}

/// Compares a window rect against its monitor, allowing a pixel of slack.
///
/// Some games size themselves one pixel off the monitor, and a few sit one pixel outside it,
/// so an exact comparison would classify genuinely borderless windows as ordinary ones.
fn rects_match(window: RECT, monitor: RECT) -> bool {
    const SLACK: i32 = 1;
    (window.left - monitor.left).abs() <= SLACK
        && (window.top - monitor.top).abs() <= SLACK
        && (window.right - monitor.right).abs() <= SLACK
        && (window.bottom - monitor.bottom).abs() <= SLACK
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(left: i32, top: i32, right: i32, bottom: i32) -> RECT {
        RECT {
            left,
            top,
            right,
            bottom,
        }
    }

    #[test]
    fn a_window_filling_its_monitor_counts_as_covering_it() {
        let monitor = rect(0, 0, 2560, 1440);
        assert!(rects_match(rect(0, 0, 2560, 1440), monitor));
    }

    #[test]
    fn a_pixel_of_slack_is_tolerated() {
        // Games that size themselves fractionally off the monitor are still borderless.
        let monitor = rect(0, 0, 2560, 1440);
        assert!(rects_match(rect(0, 0, 2559, 1440), monitor));
        assert!(rects_match(rect(-1, 0, 2560, 1441), monitor));
    }

    #[test]
    fn an_ordinary_window_does_not_cover_its_monitor() {
        let monitor = rect(0, 0, 2560, 1440);
        assert!(!rects_match(rect(100, 100, 1200, 800), monitor));
        // Nearly fullscreen, but not close enough to be borderless.
        assert!(!rects_match(rect(0, 0, 2540, 1440), monitor));
    }

    #[test]
    fn works_on_a_secondary_monitor_with_negative_coordinates() {
        // A monitor to the left of the primary one has negative coordinates; the comparison
        // must be positional, not size-based.
        let monitor = rect(-1920, 0, 0, 1080);
        assert!(rects_match(rect(-1920, 0, 0, 1080), monitor));
        assert!(!rects_match(rect(0, 0, 1920, 1080), monitor));
    }

    fn modules(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_vulkan_game_is_not_mistaken_for_direct3d() {
        // Vulkan titles still load dxgi for presentation. Testing DXGI first would label every
        // one of them Direct3D — this ordering is the entire substance of the check.
        let api = api_from_modules(&modules(&["kernel32.dll", "dxgi.dll", "vulkan-1.dll"]));
        assert_eq!(api, Some("Vulkan"));
    }

    #[test]
    fn a_title_on_the_compatibility_layer_reports_what_it_renders_with() {
        // 11-on-12 loads both. The one worth naming is the newer.
        let api = api_from_modules(&modules(&["d3d11.dll", "d3d12.dll", "dxgi.dll"]));
        assert_eq!(api, Some("D3D12"));
    }

    #[test]
    fn each_generation_is_named_on_its_own() {
        assert_eq!(api_from_modules(&modules(&["d3d9.dll"])), Some("D3D9"));
        assert_eq!(
            api_from_modules(&modules(&["d3d10.dll", "dxgi.dll"])),
            Some("D3D10")
        );
        assert_eq!(
            api_from_modules(&modules(&["opengl32.dll"])),
            Some("OpenGL")
        );
    }

    #[test]
    fn dxgi_alone_says_direct3d_without_claiming_a_version() {
        // Better than nothing and better than a guess: something is presenting through DXGI,
        // and which generation cannot be told from here.
        assert_eq!(
            api_from_modules(&modules(&["dxgi.dll"])),
            Some("Direct3D")
        );
    }

    #[test]
    fn a_process_that_renders_nothing_gets_no_label() {
        assert_eq!(
            api_from_modules(&modules(&["kernel32.dll", "user32.dll"])),
            None
        );
    }

    #[test]
    fn an_upscaler_is_named_when_it_ships_as_a_library() {
        assert_eq!(
            upscaler_from_modules(&modules(&["d3d12.dll", "nvngx_dlss.dll"])),
            Some("DLSS")
        );
        assert_eq!(
            upscaler_from_modules(&modules(&["ffx_fsr3upscaler_x64.dll"])),
            Some("FSR3")
        );
        assert_eq!(upscaler_from_modules(&modules(&["libxess.dll"])), Some("XeSS"));
    }

    #[test]
    fn an_engine_that_compiled_its_upscaler_in_reports_nothing() {
        // The honest outcome, and the reason this reading is named rather than measured.
        // FSR built into an engine leaves no library behind, and driver-side upscaling never
        // enters the game's address space at all. A dash here does not mean "off".
        assert_eq!(
            upscaler_from_modules(&modules(&["d3d12.dll", "dxgi.dll"])),
            None
        );
    }

    #[test]
    fn frame_generation_is_named_including_the_replacement_that_poses_as_another() {
        assert_eq!(
            frame_gen_from_modules(&modules(&["nvngx_dlssg.dll"])),
            Some("DLSS-G")
        );
        // Presents itself under NVIDIA's name and is not NVIDIA's.
        assert_eq!(
            frame_gen_from_modules(&modules(&["dlssg_to_fsr3_amd_is_better.dll"])),
            Some("FSR3 FG")
        );
        assert_eq!(frame_gen_from_modules(&modules(&["nvngx_dlss.dll"])), None);
    }

    #[test]
    fn upscaling_and_generation_are_told_apart() {
        // The two ship side by side and their libraries differ by two letters. Confusing them
        // would report frame generation on every game merely using DLSS.
        let both = modules(&["nvngx_dlss.dll", "nvngx_dlssg.dll", "d3d12.dll"]);
        assert_eq!(upscaler_from_modules(&both), Some("DLSS"));
        assert_eq!(frame_gen_from_modules(&both), Some("DLSS-G"));

        let upscaling_only = modules(&["nvngx_dlss.dll", "d3d12.dll"]);
        assert_eq!(frame_gen_from_modules(&upscaling_only), None);
    }

    #[test]
    fn exclusive_fullscreen_suppresses_the_overlay() {
        let target = Target {
            pid: 1234,
            hwnd: HWND::default(),
            mode: DisplayMode::ExclusiveFullscreen,
        };
        assert!(!target.overlay_possible());

        for mode in [DisplayMode::Borderless, DisplayMode::Windowed] {
            let target = Target { mode, ..target };
            assert!(
                target.overlay_possible(),
                "{mode:?} should allow the overlay"
            );
        }
    }
}
