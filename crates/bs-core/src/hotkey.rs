//! Reading a hotkey out of the settings file.
//!
//! The settings hold something a person can type — `Ctrl+Alt+B` — and the platform needs
//! modifier flags and a virtual key. The translation is here rather than beside the Windows
//! call so that a typo in the settings file can be tested against without a window.

use serde::{Deserialize, Serialize};

/// A parsed accelerator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hotkey {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub win: bool,
    /// Windows virtual-key code.
    pub key: u16,
}

impl Hotkey {
    /// Parses `Ctrl+Alt+B`, `ctrl + shift + f9`, `Win+Home`.
    ///
    /// Returns `None` for anything it does not understand, which the caller treats as "no
    /// hotkey" rather than as a reason to refuse to start. A mistyped shortcut costs the
    /// shortcut, not the overlay.
    pub fn parse(text: &str) -> Option<Self> {
        let mut hotkey = Self {
            ctrl: false,
            alt: false,
            shift: false,
            win: false,
            key: 0,
        };

        for part in text.split('+') {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            match part.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => hotkey.ctrl = true,
                "alt" => hotkey.alt = true,
                "shift" => hotkey.shift = true,
                "win" | "super" | "meta" => hotkey.win = true,
                other => {
                    // Two keys is not a shortcut; it is a mistake worth noticing.
                    if hotkey.key != 0 {
                        return None;
                    }
                    hotkey.key = virtual_key(other)?;
                }
            }
        }

        // A bare letter would fire every time it was typed anywhere, including into a chat
        // box in the game this is drawn over.
        let has_modifier = hotkey.ctrl || hotkey.alt || hotkey.shift || hotkey.win;
        (hotkey.key != 0 && has_modifier).then_some(hotkey)
    }
}

/// Virtual-key code for a key named in the settings file.
fn virtual_key(name: &str) -> Option<u16> {
    let mut chars = name.chars();
    let first = chars.next()?;

    // A single letter or digit is its own code, which is a quirk of the Windows table worth
    // relying on: 'A' is 0x41, '0' is 0x30.
    if chars.next().is_none() && first.is_ascii_alphanumeric() {
        return Some(first.to_ascii_uppercase() as u16);
    }

    if let Some(digits) = name.strip_prefix('f')
        && let Ok(n) = digits.parse::<u16>()
        && (1..=24).contains(&n)
    {
        return Some(0x70 + n - 1);
    }

    Some(match name {
        "home" => 0x24,
        "end" => 0x23,
        "insert" | "ins" => 0x2D,
        "delete" | "del" => 0x2E,
        "pageup" | "pgup" => 0x21,
        "pagedown" | "pgdn" => 0x22,
        "space" => 0x20,
        "tab" => 0x09,
        "escape" | "esc" => 0x1B,
        "pause" => 0x13,
        "scrolllock" => 0x91,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_default_shortcuts() {
        let toggle = Hotkey::parse("Ctrl+Alt+B").unwrap();
        assert!(toggle.ctrl && toggle.alt && !toggle.shift && !toggle.win);
        assert_eq!(toggle.key, b'B' as u16);
    }

    #[test]
    fn is_forgiving_about_how_it_is_written() {
        let a = Hotkey::parse("Ctrl+Alt+B").unwrap();
        for spelling in ["ctrl+alt+b", "CTRL + ALT + B", "Control+Alt+B"] {
            assert_eq!(Hotkey::parse(spelling), Some(a), "{spelling}");
        }
    }

    #[test]
    fn function_keys_and_named_keys_work() {
        assert_eq!(Hotkey::parse("Ctrl+F9").unwrap().key, 0x78);
        assert_eq!(Hotkey::parse("Ctrl+F1").unwrap().key, 0x70);
        assert_eq!(Hotkey::parse("Win+Home").unwrap().key, 0x24);
        assert_eq!(Hotkey::parse("Alt+ScrollLock").unwrap().key, 0x91);
    }

    #[test]
    fn a_shortcut_without_a_modifier_is_refused() {
        // It would otherwise fire while the user was typing the letter into the game.
        assert_eq!(Hotkey::parse("B"), None);
        assert_eq!(Hotkey::parse("F9"), None);
    }

    #[test]
    fn nonsense_is_refused_rather_than_guessed_at() {
        assert_eq!(Hotkey::parse(""), None);
        assert_eq!(Hotkey::parse("Ctrl+"), None);
        assert_eq!(Hotkey::parse("Ctrl+Banana"), None);
        assert_eq!(Hotkey::parse("Ctrl+F25"), None);
        // Two keys and no way to say which was meant.
        assert_eq!(Hotkey::parse("Ctrl+A+B"), None);
    }
}
