//! Draw list: text and rectangles become textured quads.
//!
//! The platform backend receives finished vertices in pixel coordinates (origin at the
//! overlay's top-left) and does exactly two things: converts them to NDC and issues one draw
//! call with the atlas bound. No layout logic exists on the platform side.

use bs_core::Color;

use crate::atlas::{Atlas, GlyphAtlas, TextStyle};

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

    /// A rectangle with rounded corners, in seven quads.
    ///
    /// Four of them sample the corner wedge baked into the atlas, with their texture
    /// coordinates mirrored per corner so that one baked tile serves all four. The other three
    /// fill the straight middle. All of it still goes through the one draw call — a rounded
    /// panel costs six extra quads, not a second pipeline.
    ///
    /// Falls back to a square rectangle when it is too small to round, which is the honest
    /// answer: a radius larger than half the box is not a rounded corner, it is a different
    /// shape.
    pub fn rounded_rect(
        &mut self,
        atlas: &Atlas,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        color: Color,
    ) {
        if color.a == 0 || w <= 0.0 || h <= 0.0 {
            return;
        }
        let wedge = atlas.wedge();
        let r = wedge.radius;
        if w < r * 2.0 || h < r * 2.0 {
            self.rect(atlas, x, y, w, h, color);
            return;
        }

        let c = color.to_premultiplied_f32();
        let [u0, v0] = wedge.uv_min;
        let [u1, v1] = wedge.uv_max;

        // Each corner takes the tile oriented so its solid end faces the middle of the box.
        // The outer end is the transparent one, which is what rounds the corner.
        self.quad(x, y, r, r, [u0, v0], [u1, v1], c);
        self.quad(x + w - r, y, r, r, [u1, v0], [u0, v1], c);
        self.quad(x, y + h - r, r, r, [u0, v1], [u1, v0], c);
        self.quad(x + w - r, y + h - r, r, r, [u1, v1], [u0, v0], c);

        let uv = atlas.white_uv();
        self.quad(x + r, y, w - r * 2.0, r, uv, uv, c);
        self.quad(x, y + r, w, h - r * 2.0, uv, uv, c);
        self.quad(x + r, y + h - r, w - r * 2.0, r, uv, uv, c);
    }

    /// A one-pixel horizontal rule — the divider between blocks.
    ///
    /// The position is snapped to a whole pixel. Geometry here maps one-to-one onto the
    /// swapchain, so a line at a fractional offset would land across two rows of texels and
    /// draw as two half-bright lines instead of one crisp one.
    pub fn hairline(&mut self, atlas: &Atlas, x: f32, y: f32, w: f32, color: Color) {
        // Both edges are snapped and the width derived from them, rather than the width being
        // rounded on its own: that way the line still ends where it was asked to, instead of
        // drifting by up to a pixel from whatever it was meant to sit under.
        let x0 = x.round();
        let x1 = (x + w).round();
        self.rect(atlas, x0, y.round(), x1 - x0, 1.0, color);
    }

    /// Draws text in a chosen size, optionally letterspaced, and returns the final pen
    /// position.
    ///
    /// Letterspacing is applied between characters and not after the last one, so a spaced
    /// heading right-aligns against the same edge as everything below it.
    pub fn text_styled(
        &mut self,
        atlas: &Atlas,
        style: TextStyle,
        x: f32,
        baseline_y: f32,
        text: &str,
        color: Color,
        letter_spacing: f32,
    ) -> f32 {
        let face = atlas.face(style);
        let c = color.to_premultiplied_f32();
        let mut pen = x;
        let mut first = true;
        for ch in text.chars() {
            if !first {
                pen += letter_spacing;
            }
            first = false;

            let Some(g) = face.glyph(ch) else {
                pen += face.advance;
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
    fn a_rounded_rect_is_seven_quads_and_stays_inside_its_box() {
        let atlas = atlas_or_skip!();
        let mut list = DrawList::new();
        list.rounded_rect(&atlas, 10.0, 20.0, 200.0, 100.0, Color::WHITE);

        assert_eq!(list.indices.len() / 6, 7, "four corners and three bands");
        for v in &list.vertices {
            assert!(
                (10.0..=210.0).contains(&v.pos[0]) && (20.0..=120.0).contains(&v.pos[1]),
                "geometry escaped the box: {:?}",
                v.pos
            );
        }
    }

    #[test]
    fn the_four_corners_mirror_one_baked_wedge_rather_than_baking_four() {
        let atlas = atlas_or_skip!();
        let mut list = DrawList::new();
        list.rounded_rect(&atlas, 0.0, 0.0, 200.0, 100.0, Color::WHITE);

        // Every corner quad covers the same texture region; only the winding differs. If a
        // future change bakes separate tiles this test is what will say so.
        let corner_uvs: Vec<[f32; 2]> = list.vertices[..16].iter().map(|v| v.uv).collect();
        let wedge = atlas.wedge();
        for uv in &corner_uvs {
            let on_u = (uv[0] - wedge.uv_min[0]).abs() < 1e-6
                || (uv[0] - wedge.uv_max[0]).abs() < 1e-6;
            let on_v = (uv[1] - wedge.uv_min[1]).abs() < 1e-6
                || (uv[1] - wedge.uv_max[1]).abs() < 1e-6;
            assert!(on_u && on_v, "corner sampled outside the wedge: {uv:?}");
        }

        // Opposite corners must be mirrored, not identical, or three of them would round the
        // wrong way.
        assert_ne!(list.vertices[0].uv, list.vertices[4].uv);
    }

    #[test]
    fn a_box_too_small_to_round_stays_square_instead_of_folding_over_itself() {
        let atlas = atlas_or_skip!();
        let r = atlas.wedge().radius;
        let mut list = DrawList::new();
        list.rounded_rect(&atlas, 0.0, 0.0, r, r, Color::WHITE);
        assert_eq!(list.indices.len() / 6, 1, "one plain quad, not seven");
    }

    #[test]
    fn a_hairline_lands_on_a_whole_pixel() {
        let atlas = atlas_or_skip!();
        let mut list = DrawList::new();
        list.hairline(&atlas, 4.3, 20.6, 100.4, Color::WHITE);

        // Half a pixel off and a one-pixel rule draws as two half-bright ones.
        assert_eq!(list.vertices[0].pos, [4.0, 21.0]);
        // The far edge is snapped too, so the rule still ends where 4.3 + 100.4 asked it to
        // rather than at 4 + 100.
        assert_eq!(list.vertices[2].pos, [105.0, 22.0]);
    }

    #[test]
    fn styled_text_draws_at_the_size_it_was_asked_for() {
        let atlas = atlas_or_skip!();
        let mut small = DrawList::new();
        let small_pen = small.text_styled(
            &atlas,
            TextStyle::Key,
            0.0,
            0.0,
            "142",
            Color::WHITE,
            0.0,
        );
        let mut big = DrawList::new();
        let big_pen =
            big.text_styled(&atlas, TextStyle::Big, 0.0, 0.0, "142", Color::WHITE, 0.0);

        assert!(
            big_pen > small_pen * 1.4,
            "the big face did not draw bigger: {small_pen} then {big_pen}"
        );
    }

    #[test]
    fn letterspacing_sits_between_characters_and_not_after_the_last() {
        let atlas = atlas_or_skip!();
        let mut list = DrawList::new();
        let bare = list.text_styled(&atlas, TextStyle::Key, 0.0, 0.0, "CPU", Color::WHITE, 0.0);
        let spaced = list.text_styled(&atlas, TextStyle::Key, 0.0, 0.0, "CPU", Color::WHITE, 2.0);
        assert!((spaced - bare - 4.0).abs() < 1e-3, "three letters, two gaps");
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
