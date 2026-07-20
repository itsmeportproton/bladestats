//! Общий рендер оверлея: растеризация шрифта в глиф-атлас и превращение снапшота в
//! список текстурированных квадов.
//!
//! Платформа получает готовые вершины и только заливает их в свой графический API — D3D11
//! на Windows, Vulkan на Linux. Благодаря этому обе ОС рисуют оверлей пиксель в пиксель
//! одинаково, чего не вышло бы при использовании DirectWrite на одной стороне и Vulkan-текста
//! на другой.

pub mod atlas;
pub mod draw;
pub mod hud;

pub use atlas::{AtlasError, Glyph, GlyphAtlas, default_charset};
pub use draw::{DrawList, Vertex};
pub use hud::{HudOptions, HudSize};

/// Шрифт оверлея, вшитый в бинарник.
///
/// Файл не хранится в репозитории (см. `assets/fonts/README.md`), поэтому сборка без него
/// падает с внятным сообщением, а не с невнятной ошибкой ввода-вывода из `include_bytes!`.
#[cfg(feature = "embedded-font")]
pub const EMBEDDED_FONT: &[u8] = include_bytes!("../../../assets/fonts/JetBrainsMono-Regular.ttf");
