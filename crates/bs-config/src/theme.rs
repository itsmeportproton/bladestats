//! The configurator's palette and the rule that colours it.
//!
//! There is no house colour. Each device brings its own, so a machine whose processor and
//! graphics card come from the same vendor gives a single-colour window, and a mismatched one
//! colours each section separately.

use bs_core::Vendor;
use egui::Color32;

/// Window body. Near-black with a slight cool bias, so it reads as chosen rather than as the
/// absence of a choice.
pub const GROUND: Color32 = Color32::from_rgb(0x08, 0x09, 0x0B);
pub const PANEL: Color32 = Color32::from_rgb(0x13, 0x15, 0x19);
pub const PANEL_HOVER: Color32 = Color32::from_rgb(0x19, 0x1C, 0x21);
pub const LINE: Color32 = Color32::from_rgb(0x24, 0x27, 0x2E);
pub const EDGE: Color32 = Color32::from_rgb(0x2A, 0x2E, 0x35);

/// Values, labels and the dimmest tier of supporting text. The first two are the overlay's
/// own, so the two programs agree on what a value looks like.
pub const TEXT: Color32 = Color32::from_rgb(0xF0, 0xF0, 0xF0);
pub const MUTED: Color32 = Color32::from_rgb(0xA0, 0xA0, 0xA0);
pub const FAINT: Color32 = Color32::from_rgb(0x6A, 0x6F, 0x78);

/// Semantic, straight from the overlay's load scale.
pub const GOOD: Color32 = Color32::from_rgb(0x6F, 0xCF, 0x50);
pub const WARN: Color32 = Color32::from_rgb(0xE8, 0xB3, 0x39);
pub const BAD: Color32 = Color32::from_rgb(0xE0, 0x50, 0x40);

/// macOS window controls.
pub const CLOSE: Color32 = Color32::from_rgb(0xFF, 0x5F, 0x57);
pub const MINIMISE: Color32 = Color32::from_rgb(0xFE, 0xBC, 0x2E);

/// A vendor's two colours.
///
/// `fill` is the brand colour, used under a dark glyph where it reads fine. `ink` is the same
/// hue lifted until it passes as text on a near-black ground: Intel's #0071C5 sits at roughly
/// 3:1 unmodified, which is not legible enough for a device name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VendorColor {
    pub fill: Color32,
    pub ink: Color32,
}

impl VendorColor {
    /// The colour for a vendor we could not identify: the neutral text tier, so an unknown
    /// device still reads as a device rather than as an error.
    pub const UNKNOWN: Self = Self {
        fill: Color32::from_rgb(0x55, 0x5B, 0x64),
        ink: MUTED,
    };

    pub fn of(vendor: Vendor) -> Self {
        match vendor {
            Vendor::Amd => Self {
                fill: Color32::from_rgb(0xED, 0x1C, 0x24),
                ink: Color32::from_rgb(0xFF, 0x5B, 0x5F),
            },
            Vendor::Intel => Self {
                fill: Color32::from_rgb(0x00, 0x71, 0xC5),
                ink: Color32::from_rgb(0x4A, 0xA8, 0xEA),
            },
            Vendor::Nvidia => Self {
                fill: Color32::from_rgb(0x76, 0xB9, 0x00),
                ink: Color32::from_rgb(0x91, 0xD8, 0x1C),
            },
            Vendor::Unknown => Self::UNKNOWN,
        }
    }
}

/// The colours in play for the detected machine.
#[derive(Debug, Clone, Copy)]
pub struct Accents {
    pub cpu: VendorColor,
    pub gpu: VendorColor,
    /// Window furniture: the version chip, focus rings, section markers that belong to no
    /// single device.
    pub chrome: VendorColor,
}

impl Accents {
    /// Chrome follows the graphics card. bladestats is a graphics tool and the card is its
    /// headline part; when both vendors agree this resolves to the one colour anyway, which is
    /// what makes a matched machine look like a single-colour window.
    pub fn detect(cpu: Vendor, gpu: Vendor) -> Self {
        let gpu = VendorColor::of(gpu);
        Self {
            cpu: VendorColor::of(cpu),
            gpu,
            chrome: gpu,
        }
    }
}

impl Default for Accents {
    fn default() -> Self {
        Self::detect(Vendor::Unknown, Vendor::Unknown)
    }
}

/// Blends `colour` towards the window body, for tinted fills that must not shout.
pub fn dim(colour: Color32, amount: f32) -> Color32 {
    GROUND.lerp_to_gamma(colour, amount.clamp(0.0, 1.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matching_vendors_give_one_colour_everywhere() {
        let amd = Accents::detect(Vendor::Amd, Vendor::Amd);
        assert_eq!(amd.cpu, amd.gpu);
        assert_eq!(
            amd.chrome, amd.gpu,
            "a matched machine reads as a single colour"
        );
    }

    #[test]
    fn mismatched_vendors_keep_their_own_colours() {
        let mixed = Accents::detect(Vendor::Amd, Vendor::Nvidia);
        assert_ne!(mixed.cpu, mixed.gpu);
        assert_eq!(mixed.cpu, VendorColor::of(Vendor::Amd));
        assert_eq!(mixed.gpu, VendorColor::of(Vendor::Nvidia));
        assert_eq!(mixed.chrome, mixed.gpu, "chrome follows the graphics card");
    }

    #[test]
    fn every_vendor_ink_is_legible_on_the_window_body() {
        // The whole reason a vendor carries two colours. Intel blue unmodified lands around
        // 3:1 on this ground, which is not enough for a device name.
        for vendor in [Vendor::Amd, Vendor::Intel, Vendor::Nvidia, Vendor::Unknown] {
            let ratio = contrast(VendorColor::of(vendor).ink, GROUND);
            assert!(
                ratio >= 4.5,
                "{vendor:?} ink is only {ratio:.1}:1 against the window body"
            );
        }
    }

    #[test]
    fn an_unknown_vendor_still_gets_a_readable_colour() {
        let unknown = Accents::detect(Vendor::Unknown, Vendor::Unknown);
        assert_eq!(unknown.cpu, VendorColor::UNKNOWN);
        assert!(contrast(unknown.cpu.ink, GROUND) >= 4.5);
    }

    #[test]
    fn dimming_moves_towards_the_window_body() {
        let accent = VendorColor::of(Vendor::Nvidia).fill;
        assert_eq!(dim(accent, 0.0), GROUND);
        assert_eq!(dim(accent, 1.0), accent);
    }

    /// WCAG relative luminance contrast ratio.
    fn contrast(a: Color32, b: Color32) -> f32 {
        let l = |c: Color32| {
            let f = |v: u8| {
                let v = v as f32 / 255.0;
                if v <= 0.03928 {
                    v / 12.92
                } else {
                    ((v + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * f(c.r()) + 0.7152 * f(c.g()) + 0.0722 * f(c.b())
        };
        let (x, y) = (l(a), l(b));
        (x.max(y) + 0.05) / (x.min(y) + 0.05)
    }
}
