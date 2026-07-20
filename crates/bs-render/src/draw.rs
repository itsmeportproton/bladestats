//! Draw list: text and rectangles become textured quads.
//!
//! The platform backend receives finished vertices in pixel coordinates (origin at the
//! overlay's top-left) and does exactly two things: converts them to NDC and issues one draw
//! call with the atlas bound. No layout logic exists on the platform side.

use bs_core::Color;

use crate::atlas::GlyphAtlas;

/// A vertex. `#[repr(C)]` is mandatory: the buffer goes to the graphics API as-is.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Vertex {
    /// Pixels, origin at the overlay's top-left.
    pub pos: [f32; 2],
    pub uv: [f32; 2],
    /// Premultiplied alpha — the form both a D3D11 composition swapchain and Vulkan expect.
    pub color: [f32; 4],
}

#[derive(Debug, Default)]
pub struct DrawList {
    pub vertices: Vec<Vertex>,
    pub indices: Vec<u32>,
}

impl DrawList {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn clear(&mut self) {
        self.vertices.clear();
        self.indices.clear();
    }

    pub fn is_empty(&self) -> bool {
        self.indices.is_empty()
    }

    fn quad(&mut self, x: f32, y: f32, w: f32, h: f32, uv0: [f32; 2], uv1: [f32; 2], c: [f32; 4]) {
        let base = self.vertices.len() as u32;
        self.vertices.extend_from_slice(&[
            Vertex {
                pos: [x, y],
                uv: uv0,
                color: c,
            },
            Vertex {
                pos: [x + w, y],
                uv: [uv1[0], uv0[1]],
                color: c,
            },
            Vertex {
                pos: [x + w, y + h],
                uv: uv1,
                color: c,
            },
            Vertex {
                pos: [x, y + h],
                uv: [uv0[0], uv1[1]],
                color: c,
            },
        ]);
        self.indices
            .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }

    /// A solid-colour rectangle. Drawn with the atlas's opaque texel, so it needs neither a
    /// separate shader nor a second draw call.
    pub fn rect(&mut self, atlas: &GlyphAtlas, x: f32, y: f32, w: f32, h: f32, color: Color) {
        if color.a == 0 || w <= 0.0 || h <= 0.0 {
            return;
        }
        let uv = atlas.white_uv();
        self.quad(x, y, w, h, uv, uv, color.to_premultiplied_f32());
    }

    /// Draws text with the pen at `(x, baseline_y)` and returns the final pen position.
    ///
    /// Characters missing from the atlas are skipped but still advance the pen, so the line
    /// does not shift.
    pub fn text(
        &mut self,
        atlas: &GlyphAtlas,
        x: f32,
        baseline_y: f32,
        text: &str,
        color: Color,
    ) -> f32 {
        let c = color.to_premultiplied_f32();
        let mut pen = x;
        for ch in text.chars() {
            let Some(g) = atlas.glyph(ch) else {
                pen += atlas.advance;
                continue;
            };
            if g.size_px[0] > 0.0 && g.size_px[1] > 0.0 && color.a > 0 {
                self.quad(
                    pen + g.offset_px[0],
                    baseline_y + g.offset_px[1],
                    g.size_px[0],
                    g.size_px[1],
                    g.uv_min,
                    g.uv_max,
                    c,
                );
            }
            pen += g.advance_px;
        }
        pen
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn atlas() -> Option<GlyphAtlas> {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../assets/fonts/JetBrainsMono-Regular.ttf"
        );
        let bytes = std::fs::read(path).ok()?;
        GlyphAtlas::new(&bytes, 16.0).ok()
    }

    macro_rules! atlas_or_skip {
        () => {
            match atlas() {
                Some(a) => a,
                None => return,
            }
        };
    }

    #[test]
    fn a_rect_is_two_triangles() {
        let atlas = atlas_or_skip!();
        let mut list = DrawList::new();
        list.rect(&atlas, 10.0, 20.0, 100.0, 30.0, Color::WHITE);

        assert_eq!(list.vertices.len(), 4);
        assert_eq!(list.indices.len(), 6);
        assert_eq!(list.vertices[0].pos, [10.0, 20.0]);
        assert_eq!(list.vertices[2].pos, [110.0, 50.0]);
    }

    #[test]
    fn fully_transparent_geometry_is_skipped_entirely() {
        let atlas = atlas_or_skip!();
        let mut list = DrawList::new();
        list.rect(&atlas, 0.0, 0.0, 10.0, 10.0, Color::TRANSPARENT);
        assert!(
            list.is_empty(),
            "a transparent background must not reach the vertex buffer"
        );

        // ...but the pen still moves, or the layout would drift.
        let pen = list.text(&atlas, 0.0, 0.0, "invisible", Color::rgba(255, 255, 255, 0));
        assert!(list.is_empty());
        assert!(pen > 0.0);
    }

    #[test]
    fn degenerate_rects_are_dropped() {
        let atlas = atlas_or_skip!();
        let mut list = DrawList::new();
        list.rect(&atlas, 0.0, 0.0, 0.0, 10.0, Color::WHITE);
        list.rect(&atlas, 0.0, 0.0, 10.0, -5.0, Color::WHITE);
        assert!(list.is_empty());
    }

    #[test]
    fn text_advances_the_pen_by_its_measured_width() {
        let atlas = atlas_or_skip!();
        let mut list = DrawList::new();
        let pen = list.text(&atlas, 100.0, 50.0, "FPS 144", Color::WHITE);

        assert!((pen - (100.0 + atlas.measure("FPS 144"))).abs() < 1e-3);
        assert!(!list.is_empty());
    }

    #[test]
    fn spaces_produce_no_geometry() {
        let atlas = atlas_or_skip!();
        let mut only_spaces = DrawList::new();
        only_spaces.text(&atlas, 0.0, 0.0, "   ", Color::WHITE);
        assert!(
            only_spaces.is_empty(),
            "spaces only move the pen, they do not draw"
        );
    }

    #[test]
    fn missing_glyphs_keep_the_line_aligned() {
        let atlas = atlas_or_skip!();
        let mut list = DrawList::new();
        // Not in the atlas: no ink, but it still occupies a cell.
        let pen = list.text(&atlas, 0.0, 0.0, "漢", Color::WHITE);
        assert!(list.is_empty());
        assert!((pen - atlas.advance).abs() < 1e-3);
    }

    #[test]
    fn colour_reaches_the_vertices_premultiplied() {
        let atlas = atlas_or_skip!();
        let mut list = DrawList::new();
        let half = Color::rgba(0xFF, 0x00, 0x00, 0x80);
        list.rect(&atlas, 0.0, 0.0, 4.0, 4.0, half);

        let c = list.vertices[0].color;
        assert!((c[3] - 0.5).abs() < 0.01, "alpha");
        assert!(
            (c[0] - c[3]).abs() < 0.01,
            "premultiplied red equals the alpha"
        );
        assert_eq!(c[1], 0.0);
    }

    #[test]
    fn clear_resets_the_list_for_the_next_frame() {
        let atlas = atlas_or_skip!();
        let mut list = DrawList::new();
        list.text(&atlas, 0.0, 0.0, "leftovers", Color::WHITE);
        list.clear();
        assert!(list.is_empty());
        assert!(list.vertices.is_empty());
    }

    #[test]
    fn indices_always_reference_existing_vertices() {
        let atlas = atlas_or_skip!();
        let mut list = DrawList::new();
        list.rect(&atlas, 0.0, 0.0, 10.0, 10.0, Color::WHITE);
        list.text(&atlas, 0.0, 20.0, "GPU 78%", Color::WHITE);

        let n = list.vertices.len() as u32;
        assert!(
            list.indices.iter().all(|&i| i < n),
            "index past the end of the vertex buffer"
        );
        assert_eq!(
            list.indices.len() % 6,
            0,
            "geometry consists of whole quads"
        );
    }
}
