//! System-wide shortcuts.
//!
//! The overlay takes no input of its own — it is transparent to the mouse and never holds
//! focus — so the only way to reach it while a game is in front is a shortcut the system
//! delivers regardless of who is focused.
//!
//! Registration is allowed to fail quietly. Another program may already own the combination,
//! and losing a shortcut is not a reason to refuse to run: the overlay simply carries on
//! without it, and says so in the log.

use bs_core::Hotkey;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, MOD_WIN, RegisterHotKey,
    UnregisterHotKey,
};

/// What a registered shortcut does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Show the panel when the detection has decided against it, or hide it again.
    Toggle,
    /// Re-read the settings file at once instead of waiting for the poll.
    Reload,
}

impl Action {
    fn id(self) -> i32 {
        match self {
            Action::Toggle => 1,
            Action::Reload => 2,
        }
    }

    fn from_id(id: i32) -> Option<Self> {
        match id {
            1 => Some(Action::Toggle),
            2 => Some(Action::Reload),
            _ => None,
        }
    }
}

/// The shortcuts this process currently holds.
pub struct Hotkeys {
    registered: Vec<Action>,
}

impl Hotkeys {
    /// Registers what the settings ask for, skipping anything unparseable or already taken.
    pub fn register(settings: &bs_core::Hotkeys) -> Self {
        let mut registered = Vec::new();
        for (action, text) in [
            (Action::Toggle, &settings.toggle),
            (Action::Reload, &settings.reload),
        ] {
            match Hotkey::parse(text) {
                None => tracing::warn!(shortcut = %text, ?action, "unreadable shortcut, ignored"),
                Some(key) if claim(action, key) => registered.push(action),
                Some(_) => {
                    tracing::warn!(shortcut = %text, ?action, "shortcut already taken, ignored")
                }
            }
        }
        Self { registered }
    }

    /// Turns a `WM_HOTKEY` identifier back into what it means.
    pub fn action(id: i32) -> Option<Action> {
        Action::from_id(id)
    }
}

impl Drop for Hotkeys {
    fn drop(&mut self) {
        for action in &self.registered {
            unsafe {
                let _ = UnregisterHotKey(None, action.id());
            }
        }
    }
}

fn claim(action: Action, key: Hotkey) -> bool {
    let mut modifiers = HOT_KEY_MODIFIERS(0);
    if key.ctrl {
        modifiers |= MOD_CONTROL;
    }
    if key.alt {
        modifiers |= MOD_ALT;
    }
    if key.shift {
        modifiers |= MOD_SHIFT;
    }
    if key.win {
        modifiers |= MOD_WIN;
    }
    // Without this, holding the combination down repeats it dozens of times a second and the
    // panel strobes.
    modifiers |= MOD_NOREPEAT;

    // Registered against the thread rather than the window: the message then arrives in the
    // loop's own queue, which it is already draining, instead of having to be routed out of a
    // window procedure that has nowhere to put it.
    unsafe { RegisterHotKey(None, action.id(), modifiers, key.key as u32).is_ok() }
}
