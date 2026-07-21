//! The notification-area icon.
//!
//! The settings window hides rather than minimises, so there has to be something to bring it
//! back. This is that something, and it is also where the program can be quit from once its
//! window is out of sight.
//!
//! It runs on its own thread with its own message-only window. The alternative — hanging a
//! notification icon off the window eframe already owns — means getting a message into a
//! window procedure that winit controls, and the ways to do that are all reaching under
//! somebody else's floorboards. A second thread costs one hidden window and a few kilobytes,
//! and neither side has to know the other exists.
//!
//! Nothing is drawn here and nothing is decided here. The thread reports what was clicked and
//! the application decides what that means.

use std::sync::mpsc::{Receiver, Sender, channel};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

/// What the user asked for by clicking the icon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayEvent {
    /// Bring the settings window back.
    Show,
    /// Quit the whole program, counter included.
    Quit,
}

/// The message the icon sends us. Anything from `WM_APP` up is ours to define.
const WM_TRAY: u32 = WM_APP + 1;

/// Menu command identifiers.
const ID_SHOW: usize = 1;
const ID_QUIT: usize = 2;

const CLASS_NAME: PCWSTR = w!("BladestatsTray");

pub struct Tray {
    events: Receiver<TrayEvent>,
    /// The hidden window, so it can be told to close and take the icon down with it.
    window: HWND,
}

// The handle is only ever used to post a message, which is the one thing that is documented as
// safe to do to a window from another thread.
unsafe impl Send for Tray {}

impl Tray {
    /// Puts an icon in the notification area.
    ///
    /// `repaint` is the settings window's context. The thread nudges it when something is
    /// clicked, because an idle window is not repainting and would otherwise not notice for
    /// however long it took something else to wake it.
    pub fn new(repaint: egui::Context) -> Option<Self> {
        let (events_tx, events) = channel();
        let (ready_tx, ready) = channel::<Option<isize>>();

        std::thread::Builder::new()
            .name("bs-tray".into())
            .spawn(move || run(events_tx, ready_tx, repaint))
            .ok()?;

        // Wait for the thread to create its window, so `window` is never a handle to nothing.
        // Carried as an integer because a window handle is a raw pointer and Rust will not send
        // one across a thread boundary — which is the right instinct in general and beside the
        // point here, since the only thing done with it is posting a message.
        let window = ready
            .recv_timeout(std::time::Duration::from_secs(2))
            .ok()??;
        Some(Self {
            events,
            window: HWND(window as *mut _),
        })
    }

    /// Whatever has been clicked since last time.
    pub fn poll(&self) -> Option<TrayEvent> {
        self.events.try_recv().ok()
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        // Without this the icon stays in the tray after the program is gone, until something
        // makes Windows notice — usually the user waving the mouse over it.
        unsafe {
            let _ = PostMessageW(Some(self.window), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
    }
}

/// The tray thread: one hidden window, one icon, one message loop.
fn run(events: Sender<TrayEvent>, ready: Sender<Option<isize>>, repaint: egui::Context) {
    unsafe {
        let Ok(instance) = GetModuleHandleW(None) else {
            let _ = ready.send(None);
            return;
        };

        let class = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wnd_proc),
            hInstance: instance.into(),
            lpszClassName: CLASS_NAME,
            ..Default::default()
        };
        RegisterClassExW(&class);

        // Message-only: it has no position, no size and never appears anywhere. It exists
        // solely to be the address the notification icon sends its clicks to.
        let Ok(window) = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            CLASS_NAME,
            w!("bladestats"),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            Some(instance.into()),
            None,
        ) else {
            let _ = ready.send(None);
            return;
        };

        // The channel and the context are handed to the window procedure through the window
        // itself, which is the ordinary way to give a callback some state of its own.
        let state = Box::into_raw(Box::new(State { events, repaint }));
        SetWindowLongPtrW(window, GWLP_USERDATA, state as isize);

        let mut icon = NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: window,
            uID: 1,
            uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
            uCallbackMessage: WM_TRAY,
            hIcon: LoadIconW(None, IDI_APPLICATION).unwrap_or_default(),
            ..Default::default()
        };
        let tip: Vec<u16> = "bladestats\0".encode_utf16().collect();
        icon.szTip[..tip.len()].copy_from_slice(&tip);

        if !Shell_NotifyIconW(NIM_ADD, &icon).as_bool() {
            let _ = DestroyWindow(window);
            let _ = ready.send(None);
            return;
        }
        let _ = ready.send(Some(window.0 as isize));

        let mut message = MSG::default();
        while GetMessageW(&mut message, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }

        let _ = Shell_NotifyIconW(NIM_DELETE, &icon);
        drop(Box::from_raw(state));
    }
}

struct State {
    events: Sender<TrayEvent>,
    repaint: egui::Context,
}

extern "system" fn wnd_proc(window: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        let state = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut State;

        let send = |event: TrayEvent| {
            if let Some(state) = state.as_ref() {
                let _ = state.events.send(event);
                // The window is idle and not repainting. Without this it would not notice
                // until something else happened to wake it.
                state.repaint.request_repaint();
            }
        };

        match msg {
            WM_TRAY => {
                match lparam.0 as u32 {
                    // A plain click is the common case and does the common thing.
                    WM_LBUTTONUP | WM_LBUTTONDBLCLK => send(TrayEvent::Show),
                    WM_RBUTTONUP | WM_CONTEXTMENU => {
                        if let Some(choice) = show_menu(window) {
                            send(choice);
                        }
                    }
                    _ => {}
                }
                LRESULT(0)
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(window, msg, wparam, lparam),
        }
    }
}

/// The right-click menu, and what was chosen from it.
fn show_menu(window: HWND) -> Option<TrayEvent> {
    unsafe {
        let menu = CreatePopupMenu().ok()?;
        let _ = AppendMenuW(menu, MF_STRING, ID_SHOW, w!("Show bladestats"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(menu, MF_STRING, ID_QUIT, w!("Quit"));

        let mut cursor = POINT::default();
        let _ = GetCursorPos(&mut cursor);

        // Documented and load-bearing: without being brought to the foreground first, the menu
        // does not go away when the user clicks elsewhere and sits on screen until dismissed.
        let _ = SetForegroundWindow(window);

        let chosen = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON,
            cursor.x,
            cursor.y,
            None,
            window,
            None,
        );
        let _ = DestroyMenu(menu);

        match chosen.0 as usize {
            ID_SHOW => Some(TrayEvent::Show),
            ID_QUIT => Some(TrayEvent::Quit),
            _ => None,
        }
    }
}
