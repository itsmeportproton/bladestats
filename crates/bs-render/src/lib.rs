//! Overlay rendering: font rasterisation into a glyph atlas, and turning a snapshot into a
//! list of textured quads.
//!
//! The renderer receives finished vertices and only uploads them to D3D11. Nothing here knows
//! about DirectWrite or any text engine — the atlas is rasterised by hand, which is what keeps
//! the layout code free of the graphics API entirely.

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
