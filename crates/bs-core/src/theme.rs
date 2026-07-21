//! Overlay colours.
//!
//! The overlay is drawn on top of arbitrary imagery, so alpha here is a working parameter
//! rather than decoration: both text and backing are user-configurable.

use serde::{Deserialize, Serialize};

/// A colour with straight (non-premultiplied) alpha.
///
/// Premultiplication happens at the boundary with the graphics API — a D3D11 composition
/// swapchain expects premultiplied values — but straight alpha is easier to store and to
/// configure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const TRANSPARENT: Color = Color::rgba(0, 0, 0, 0);
    pub const WHITE: Color = Color::rgb(0xFF, 0xFF, 0xFF);

    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self::rgba(r, g, b, 0xFF)
    }

    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    /// Components with premultiplied alpha — the form the shader receives.
    pub fn to_premultiplied_f32(self) -> [f32; 4] {
        let a = self.a as f32 / 255.0;
        [
            (self.r as f32 / 255.0) * a,
            (self.g as f32 / 255.0) * a,
            (self.b as f32 / 255.0) * a,
            a,
        ]
    }

    /// Parses `#RGB`, `#RRGGBB` or `#RRGGBBAA`; the hash is optional.
    pub fn parse(s: &str) -> Option<Self> {
        let h = s.trim().trim_start_matches('#');
        let nyb = |i: usize| u8::from_str_radix(&h[i..i + 1], 16).ok();
        let byte = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok();
        match h.len() {
            3 => Some(Self::rgb(nyb(0)? * 17, nyb(1)? * 17, nyb(2)? * 17)),
            6 => Some(Self::rgb(byte(0)?, byte(2)?, byte(4)?)),
            8 => Some(Self::rgba(byte(0)?, byte(2)?, byte(4)?, byte(6)?)),
            _ => None,
        }
    }
}

impl From<String> for Color {
    /// An unrecognised colour becomes white rather than a config error: the overlay should not
    /// refuse to start over a typo in one theme field.
    fn from(s: String) -> Self {
        Color::parse(&s).unwrap_or(Color::WHITE)
    }
}

impl From<Color> for String {
    fn from(c: Color) -> Self {
        if c.a == 0xFF {
            format!("#{:02X}{:02X}{:02X}", c.r, c.g, c.b)
        } else {
            format!("#{:02X}{:02X}{:02X}{:02X}", c.r, c.g, c.b, c.a)
        }
    }
}

/// The overlay palette. Vendor colours are applied on top of it when enabled.
///
/// Unknown keys are ignored rather than rejected, for the same reason as the rest of the
/// config: a theme written by a newer build must still open in an older one.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Theme {
    /// Primary colour for values.
    pub text: Color,
    /// Metric labels — dimmer than the values.
    pub label: Color,
    /// Block headings and other structural text. Dimmer again than a label: these words name
    /// a section rather than a reading, and at full weight they would compete with the
    /// numbers they introduce.
    pub faint: Color,
    /// The backing panel. Fully transparent means "no background".
    pub background: Color,
    /// Good / warning / bad scale, used for load and temperature.
    pub good: Color,
    pub warn: Color,
    pub bad: Color,
    /// Whether CPU and GPU names and metrics take their vendor's brand colour.
    pub vendor_colors: bool,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            text: Color::rgb(0xF0, 0xF0, 0xF0),
            label: Color::rgb(0xA0, 0xA0, 0xA0),
            faint: Color::rgb(0x6A, 0x6F, 0x78),
            // Not pure black: a panel that is very slightly blue reads as a surface laid over
            // the game rather than as a hole cut in it. The alpha stays the user's to choose.
            background: Color::rgba(0x08, 0x09, 0x0B, 0x99),
            good: Color::rgb(0x6F, 0xCF, 0x50),
            warn: Color::rgb(0xE8, 0xB3, 0x39),
            bad: Color::rgb(0xE0, 0x50, 0x40),
            vendor_colors: true,
        }
    }
}

impl Theme {
    /// Colour for a value on the load scale: calm below 60%, warning below 85%, alarming above.
    pub fn load_color(&self, pct: f32) -> Color {
        match pct {
            p if p < 60.0 => self.good,
            p if p < 85.0 => self.warn,
            _ => self.bad,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::Vendor;

    #[test]
    fn parses_all_three_hex_forms() {
        assert_eq!(Color::parse("#F00"), Some(Color::rgb(0xFF, 0, 0)));
        assert_eq!(Color::parse("76B900"), Some(Color::rgb(0x76, 0xB9, 0x00)));
        assert_eq!(
            Color::parse("#0071C580"),
            Some(Color::rgba(0x00, 0x71, 0xC5, 0x80))
        );
    }

    #[test]
    fn rejects_garbage() {
        assert!(Color::parse("#12345").is_none());
        assert!(Color::parse("nvidia green").is_none());
    }

    #[test]
    fn round_trips_through_string() {
        for c in [Color::rgb(0xED, 0x1C, 0x24), Color::rgba(0, 0, 0, 0x99)] {
            let s: String = c.into();
            assert_eq!(Color::parse(&s), Some(c));
        }
    }

    #[test]
    fn a_typo_in_the_theme_does_not_stop_the_overlay_from_starting() {
        assert_eq!(Color::from("not a colour".to_string()), Color::WHITE);
    }

    #[test]
    fn premultiplication_scales_colour_by_alpha() {
        let half = Color::rgba(0xFF, 0xFF, 0xFF, 0x80).to_premultiplied_f32();
        assert!(
            (half[0] - half[3]).abs() < 1e-6,
            "premultiplied white equals its own alpha"
        );

        let clear = Color::TRANSPARENT.to_premultiplied_f32();
        assert_eq!(clear, [0.0, 0.0, 0.0, 0.0]);
    }

    #[test]
    fn vendor_colours_match_the_brands() {
        assert_eq!(Vendor::Nvidia.color(), Color::parse("#76B900"));
        assert_eq!(Vendor::Amd.color(), Color::parse("#ED1C24"));
        assert_eq!(Vendor::Intel.color(), Color::parse("#0071C5"));
    }

    #[test]
    fn load_colour_escalates_with_load() {
        let t = Theme::default();
        assert_eq!(t.load_color(10.0), t.good);
        assert_eq!(t.load_color(70.0), t.warn);
        assert_eq!(t.load_color(99.0), t.bad);
    }
}
