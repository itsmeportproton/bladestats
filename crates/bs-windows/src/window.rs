//! The overlay window: always on top, transparent to the mouse, never taking focus.

use anyhow::{Context, Result};
use bs_core::Corner;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::HBRUSH;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

/// Window class name. Registered once per process.
const CLASS_NAME: PCWSTR = w!("BladestatsOverlay");

pub struct OverlayWindow {
    pub hwnd: HWND,
}

impl OverlayWindow {
    /// Creates the overlay window of the given size at screen position `(x, y)`.
    ///
    /// The set of extended styles is not decoration; every one of them is load-bearing:
    ///
    /// - `WS_EX_NOREDIRECTIONBITMAP` — the window has no redirection surface, and
    ///   DirectComposition supplies the entire image. This is what produces true per-pixel
    ///   alpha. Note that `WS_EX_LAYERED` is not merely unnecessary here but harmful: it
    ///   selects the older `UpdateLayeredWindow` path, which composition does not work with.
    /// - `WS_EX_TRANSPARENT` — the window is transparent to hit-testing, so clicks reach the
    ///   game.
    /// - `WS_EX_NOACTIVATE` — the window never takes focus, so it cannot knock a game out of
    ///   fullscreen or interfere with input.
    /// - `WS_EX_TOOLWINDOW` — no taskbar button, no Alt-Tab entry.
    /// - `WS_EX_TOPMOST` — above other windows; kept there by more than this flag alone, see
    ///   [`OverlayWindow::reassert_topmost`].
    pub fn new(x: i32, y: i32, width: i32, height: i32) -> Result<Self> {
        unsafe {
            // DPI awareness is set from code rather than from a manifest: a manifest will be
            // needed later for the administrator rights that ETW requires, and until then it
            // is better not to introduce one.
            let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);

            let instance = GetModuleHandleW(None).context("GetModuleHandleW")?;

            let class = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(wnd_proc),
                hInstance: instance.into(),
                lpszClassName: CLASS_NAME,
                hbrBackground: HBRUSH::default(),
                // No class cursor on purpose. With one, the default message handler would
                // restore it behind our back every time the pointer crossed the panel.
                hCursor: HCURSOR::default(),
                ..Default::default()
            };
            // Re-registering the same class is harmless and returns 0, so it is not checked.
            RegisterClassExW(&class);

            let hwnd = CreateWindowExW(
                WS_EX_NOREDIRECTIONBITMAP
                    | WS_EX_TRANSPARENT
                    | WS_EX_NOACTIVATE
                    | WS_EX_TOOLWINDOW
                    | WS_EX_TOPMOST,
                CLASS_NAME,
                w!("bladestats"),
                WS_POPUP | WS_VISIBLE,
                x,
                y,
                width,
                height,
                None,
                None,
                Some(instance.into()),
                None,
            )
            .context("CreateWindowExW")?;

            // The cursor's visibility is counted per input queue, not per desktop. A game that
            // hides its cursor for mouse-look hides it on *its* queue; this thread's counter is
            // untouched and still says "visible", so the moment the pointer crosses this
            // window the arrow comes back — in the middle of the game, while the camera is
            // moving. Hiding it here too makes the two queues agree.
            //
            // This is separate from click-through and does not replace it: one decides where a
            // click lands, the other decides what is drawn under the pointer. Fixing only the
            // first is what left the arrow showing.
            //
            // Safe to do unconditionally: there is nothing on this panel to point at. It takes
            // no clicks and holds no focus.
            while ShowCursor(false) >= 0 {}

            Ok(Self { hwnd })
        }
    }

    /// Puts the window back on top.
    ///
    /// `WS_EX_TOPMOST` at creation is not enough on its own: a game going fullscreen, or
    /// merely being activated, reorders the window stack and the overlay slips underneath. So
    /// the Z-order is reasserted — on a timer, and when the foreground window changes.
    ///
    /// `SWP_NOACTIVATE` is mandatory; without it this would steal focus from the game once a
    /// second.
    pub fn reassert_topmost(&self) {
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER,
            );
        }
    }

    /// Needed once target tracking lands: the overlay follows the game's window when it moves
    /// to another monitor.
    #[allow(dead_code)]
    pub fn set_position(&self, x: i32, y: i32) {
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                x,
                y,
                0,
                0,
                SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER,
            );
        }
    }

    /// Moves and resizes in one call.
    ///
    /// One call rather than two so the overlay cannot be seen at the new size in the old
    /// place: anchored to a right or bottom corner, the two happen to disagree by exactly the
    /// amount the window grew.
    pub fn set_bounds(&self, x: i32, y: i32, width: i32, height: i32) {
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                x,
                y,
                width,
                height,
                SWP_NOACTIVATE | SWP_NOOWNERZORDER,
            );
        }
    }

    /// Needed once exclusive-fullscreen detection lands: in that mode the overlay hides
    /// entirely.
    #[allow(dead_code)]
    pub fn show(&self, visible: bool) {
        unsafe {
            let _ = ShowWindow(self.hwnd, if visible { SW_SHOWNA } else { SW_HIDE });
        }
    }
}

/// Screen position for a window of this size in the requested corner.
///
/// Uses the work area rather than the full screen, so a bottom-anchored overlay does not end
/// up underneath the taskbar.
pub fn corner_position(corner: Corner, margin: f32, width: i32, height: i32) -> (i32, i32) {
    let mut work = RECT::default();
    let ok = unsafe {
        SystemParametersInfoW(
            SPI_GETWORKAREA,
            0,
            Some(&mut work as *mut RECT as *mut _),
            Default::default(),
        )
    }
    .is_ok();

    if !ok {
        return (margin as i32, margin as i32);
    }

    let m = margin as i32;
    let (left, top) = (work.left + m, work.top + m);
    let right = work.right - m - width;
    let bottom = work.bottom - m - height;

    match corner {
        Corner::TopLeft => (left, top),
        Corner::TopRight => (right, top),
        Corner::BottomLeft => (left, bottom),
        Corner::BottomRight => (right, bottom),
    }
}

extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            // Backstop for WS_EX_TRANSPARENT: tell the system there is no hit here at all, so
            // the cursor and clicks go to whatever is underneath.
            WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),

            // A pointer is not the same thing as a click, and the two travel by different
            // routes. Hit-testing decides who receives a click; the cursor's *appearance* is
            // decided separately, by asking whichever window the pointer is over. A game that
            // has hidden its cursor for mouse-look hides it on its own window — so as the
            // pointer wanders across this one, the system asks here instead and gets the
            // default arrow back, and an arrow appears in the middle of the game.
            //
            // Answering with no cursor at all, and claiming the message as handled so the
            // default handler does not put the arrow back, is what keeps it gone. There is
            // nothing on this panel to point at in any case: it takes no clicks.
            WM_SETCURSOR => {
                let _ = SetCursor(None);
                LRESULT(1)
            }

            // Belt to the WS_EX_NOACTIVATE braces: never take focus from the game, not even
            // for the instant a click would otherwise do it.
            WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),

            WM_DESTROY => {
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}
