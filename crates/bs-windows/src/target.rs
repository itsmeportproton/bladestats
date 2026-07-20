//! Working out which process the overlay should be reporting on, and whether it can be drawn
//! over at all.

use windows::Win32::Foundation::{HWND, RECT};
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
