//! Shared overlay rendering: font rasterisation into a glyph atlas, and turning a snapshot
//! into a list of textured quads.
//!
//! The platform receives finished vertices and only uploads them to its own graphics API —
//! D3D11 on Windows, Vulkan on Linux. That is what makes the overlay pixel-identical on both,
//! which would not happen with DirectWrite on one side and Vulkan text on the other.

pub mod atlas;
pub mod draw;
pub mod hud;

pub use atlas::{Atlas, AtlasError, Face, Glyph, GlyphAtlas, TextStyle, default_charset};
pub use draw::{DrawList, Vertex};
pub use hud::{HudModel, HudOptions, HudSize, HudStyle};

/// The overlay font, embedded in the binary.
///
/// The file is not stored in the repository (see `assets/fonts/README.md`), so a build without
/// it fails at `include_bytes!` rather than at runtime.
#[cfg(feature = "embedded-font")]
pub const EMBEDDED_FONT: &[u8] = include_bytes!("../../../assets/fonts/JetBrainsMono-Regular.ttf");
