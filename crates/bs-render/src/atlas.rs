//! Glyph atlas: the font is rasterised once at startup into a single coverage texture.
//!
//! This is what lets Windows and Linux draw text identically — the platform receives finished
//! vertices and knows nothing about DirectWrite or any text engine at all. The price is no
//! hinting and no subpixel antialiasing; for an overlay of monospaced digits that is a fine
//! trade, whereas the two platforms looking different would not be.

use std::collections::HashMap;

use ab_glyph::{Font, FontRef, PxScale, ScaleFont, point};

/// Gap between glyphs in the atlas, so filtering cannot bleed neighbours into each other.
const PADDING: u32 = 1;

/// Atlas width. Height is chosen to fit the content.
const ATLAS_WIDTH: u32 = 512;

/// An opaque square in the top-left corner of the atlas.
///
/// It lets solid rectangles (the backing panel, per-core load bars) go through the same shader
/// and the same draw call as text — otherwise a second pipeline would be needed purely to fill
/// with colour.
const WHITE_TEXEL_SIZE: u32 = 2;

#[derive(Debug, Clone, Copy)]
pub struct Glyph {
    /// Position in the atlas, 0.0..=1.0.
    pub uv_min: [f32; 2],
    pub uv_max: [f32; 2],
    /// Size of the inked area, in pixels.
    pub size_px: [f32; 2],
    /// Offset from the pen position (on the baseline) to the top-left of the inked area.
    pub offset_px: [f32; 2],
    /// How far to advance the pen after this character.
    pub advance_px: f32,
}

#[derive(Debug)]
pub struct GlyphAtlas {
    pub width: u32,
    pub height: u32,
    /// Coverage, one byte per texel. Colour comes from the vertices, not from the texture.
    pub pixels: Vec<u8>,
    glyphs: HashMap<char, Glyph>,
    /// UV of the opaque texel, for solid fills.
    white_uv: [f32; 2],
    pub line_height: f32,
    pub ascent: f32,
    /// Character cell width. The font is monospaced, so one value covers every glyph.
    pub advance: f32,
}

#[derive(Debug)]
pub enum AtlasError {
    /// The font file could not be parsed.
    InvalidFont,
    /// The font contained none of the requested characters.
    EmptyCharset,
}

impl std::fmt::Display for AtlasError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AtlasError::InvalidFont => f.write_str("could not parse the font file"),
            AtlasError::EmptyCharset => {
                f.write_str("the font contains none of the requested characters")
            }
        }
    }
}

impl std::error::Error for AtlasError {}

/// Default character set: printable ASCII plus the few symbols the HUD uses.
///
/// Deliberately small — the UI is English and hardware model strings are Latin. Adding another
/// script later is one `chain` call away.
pub fn default_charset() -> impl Iterator<Item = char> {
    (' '..='~').chain(['°', '—', '·'])
}

impl GlyphAtlas {
    /// Rasterises the font into an atlas at `px` size.
    pub fn new(font_bytes: &[u8], px: f32) -> Result<Self, AtlasError> {
        Self::with_charset(font_bytes, px, default_charset())
    }

    pub fn with_charset(
        font_bytes: &[u8],
        px: f32,
        charset: impl Iterator<Item = char>,
    ) -> Result<Self, AtlasError> {
        let font = FontRef::try_from_slice(font_bytes).map_err(|_| AtlasError::InvalidFont)?;
        let scaled = font.as_scaled(PxScale::from(px));

        let line_height = scaled.ascent() - scaled.descent() + scaled.line_gap();
        let ascent = scaled.ascent();
        // The font is monospaced, so the cell width can be read off any visible character.
        let advance = scaled.h_advance(font.glyph_id('0'));

        // Rasterise everything first, pack afterwards: until the inked sizes are known there
        // is nothing to pack.
        let mut rasterized: Vec<Rasterized> = Vec::new();
        let mut seen = HashMap::new();
        for ch in charset {
            if seen.insert(ch, ()).is_some() {
                continue;
            }
            let id = font.glyph_id(ch);
            let advance_px = scaled.h_advance(id);
            let outlined = font.outline_glyph(id.with_scale_and_position(px, point(0.0, 0.0)));

            let Some(outlined) = outlined else {
                // Space and other invisible characters: no ink, but the pen still moves.
                rasterized.push(Rasterized {
                    ch,
                    coverage: Vec::new(),
                    w: 0,
                    h: 0,
                    offset_px: [0.0, 0.0],
                    advance_px,
                });
                continue;
            };

            let bounds = outlined.px_bounds();
            let w = bounds.width().ceil() as u32;
            let h = bounds.height().ceil() as u32;
            let mut coverage = vec![0u8; (w * h) as usize];
            outlined.draw(|x, y, c| {
                if x < w && y < h {
                    coverage[(y * w + x) as usize] = (c * 255.0 + 0.5) as u8;
                }
            });

            rasterized.push(Rasterized {
                ch,
                coverage,
                w,
                h,
                offset_px: [bounds.min.x, bounds.min.y],
                advance_px,
            });
        }

        if rasterized.is_empty() {
            return Err(AtlasError::EmptyCharset);
        }

        Self::pack(rasterized, line_height, ascent, advance)
    }

    /// Shelf packing: glyphs are sorted by height and laid out in rows.
    ///
    /// For a couple of hundred glyphs at a single size this is more than enough — the atlas is
    /// built once at startup, and squeezing out a few percent of area would buy nothing.
    fn pack(
        mut glyphs: Vec<Rasterized>,
        line_height: f32,
        ascent: f32,
        advance: f32,
    ) -> Result<Self, AtlasError> {
        glyphs.sort_unstable_by(|a, b| b.h.cmp(&a.h).then(a.ch.cmp(&b.ch)));

        // The first row starts after the opaque texel reserved for solid fills.
        let mut pen_x = WHITE_TEXEL_SIZE + PADDING;
        let mut pen_y = 0u32;
        let mut row_height = WHITE_TEXEL_SIZE;
        let mut placements: Vec<(usize, u32, u32)> = Vec::with_capacity(glyphs.len());

        for (i, g) in glyphs.iter().enumerate() {
            if g.w == 0 || g.h == 0 {
                continue;
            }
            if pen_x + g.w > ATLAS_WIDTH {
                pen_x = 0;
                pen_y += row_height + PADDING;
                row_height = 0;
            }
            placements.push((i, pen_x, pen_y));
            pen_x += g.w + PADDING;
            row_height = row_height.max(g.h);
        }

        let height = (pen_y + row_height).max(WHITE_TEXEL_SIZE);
        let mut pixels = vec![0u8; (ATLAS_WIDTH * height) as usize];

        for y in 0..WHITE_TEXEL_SIZE {
            for x in 0..WHITE_TEXEL_SIZE {
                pixels[(y * ATLAS_WIDTH + x) as usize] = 0xFF;
            }
        }

        let mut map = HashMap::with_capacity(glyphs.len());
        for (i, x, y) in &placements {
            let g = &glyphs[*i];
            for row in 0..g.h {
                let src = (row * g.w) as usize;
                let dst = ((y + row) * ATLAS_WIDTH + x) as usize;
                pixels[dst..dst + g.w as usize]
                    .copy_from_slice(&g.coverage[src..src + g.w as usize]);
            }
            map.insert(
                g.ch,
                Glyph {
                    uv_min: [*x as f32 / ATLAS_WIDTH as f32, *y as f32 / height as f32],
                    uv_max: [
                        (*x + g.w) as f32 / ATLAS_WIDTH as f32,
                        (*y + g.h) as f32 / height as f32,
                    ],
                    size_px: [g.w as f32, g.h as f32],
                    offset_px: g.offset_px,
                    advance_px: g.advance_px,
                },
            );
        }

        // Invisible characters land in the map with no ink but with their own advance.
        for g in &glyphs {
            map.entry(g.ch).or_insert(Glyph {
                uv_min: [0.0, 0.0],
                uv_max: [0.0, 0.0],
                size_px: [0.0, 0.0],
                offset_px: [0.0, 0.0],
                advance_px: g.advance_px,
            });
        }

        let half = 0.5;
        Ok(Self {
            width: ATLAS_WIDTH,
            height,
            pixels,
            glyphs: map,
            white_uv: [half / ATLAS_WIDTH as f32, half / height as f32],
            line_height,
            ascent,
            advance,
        })
    }

    pub fn glyph(&self, ch: char) -> Option<&Glyph> {
        self.glyphs.get(&ch)
    }

    /// UV of the opaque texel — for solid-colour quads.
    pub fn white_uv(&self) -> [f32; 2] {
        self.white_uv
    }

    /// Width of a string in pixels, without drawing it.
    pub fn measure(&self, text: &str) -> f32 {
        text.chars()
            .map(|c| self.glyph(c).map_or(self.advance, |g| g.advance_px))
            .sum()
    }
}

struct Rasterized {
    ch: char,
    coverage: Vec<u8>,
    w: u32,
    h: u32,
    offset_px: [f32; 2],
    advance_px: f32,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The font is not stored in the repository, so tests that need it skip when it is
    /// missing. See assets/fonts/README.md.
    fn font() -> Option<Vec<u8>> {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../assets/fonts/JetBrainsMono-Regular.ttf"
        );
        std::fs::read(path).ok()
    }

    macro_rules! atlas_or_skip {
        ($px:expr) => {
            match font() {
                Some(bytes) => GlyphAtlas::new(&bytes, $px).expect("the atlas should build"),
                None => {
                    eprintln!("skipped: assets/fonts/JetBrainsMono-Regular.ttf not found");
                    return;
                }
            }
        };
    }

    #[test]
    fn rejects_garbage_instead_of_panicking() {
        let err = GlyphAtlas::new(b"not a font at all", 16.0);
        assert!(matches!(err, Err(AtlasError::InvalidFont)));
    }

    #[test]
    fn covers_everything_the_hud_draws() {
        let atlas = atlas_or_skip!(16.0);
        for ch in "0123456789ABCXYZ%°~/.—".chars() {
            assert!(atlas.glyph(ch).is_some(), "no glyph for {ch:?}");
        }
    }

    #[test]
    fn digits_are_monospaced_so_numbers_do_not_jitter() {
        let atlas = atlas_or_skip!(16.0);
        let widths: Vec<f32> = "0123456789"
            .chars()
            .map(|c| atlas.glyph(c).unwrap().advance_px)
            .collect();
        let first = widths[0];
        for w in &widths {
            assert!(
                (w - first).abs() < 1e-3,
                "digits differ in width: {widths:?}"
            );
        }
        // This is the whole reason for choosing a monospaced font: 99 becoming 100 must not
        // shove the rest of the line sideways.
        assert!((atlas.measure("100") - atlas.measure("999")).abs() < 1e-3);
    }

    #[test]
    fn space_has_advance_but_no_ink() {
        let atlas = atlas_or_skip!(16.0);
        let space = atlas.glyph(' ').expect("space should be in the atlas");
        assert!(space.advance_px > 0.0, "a space must move the pen");
        assert_eq!(space.size_px, [0.0, 0.0], "a space has no ink");
    }

    #[test]
    fn atlas_has_an_opaque_texel_for_solid_fills() {
        let atlas = atlas_or_skip!(16.0);
        assert_eq!(atlas.pixels[0], 0xFF, "the top-left texel must be opaque");

        let [u, v] = atlas.white_uv();
        let x = (u * atlas.width as f32) as usize;
        let y = (v * atlas.height as f32) as usize;
        assert_eq!(
            atlas.pixels[y * atlas.width as usize + x],
            0xFF,
            "white_uv must point inside the opaque patch"
        );
    }

    #[test]
    fn glyphs_fit_inside_the_atlas() {
        let atlas = atlas_or_skip!(24.0);
        assert_eq!(atlas.pixels.len(), (atlas.width * atlas.height) as usize);
        for ch in default_charset() {
            let Some(g) = atlas.glyph(ch) else { continue };
            assert!(
                g.uv_max[0] <= 1.0 && g.uv_max[1] <= 1.0,
                "glyph {ch:?} spills out of the atlas"
            );
            assert!(g.uv_min[0] >= 0.0 && g.uv_min[1] >= 0.0);
        }
    }

    #[test]
    fn larger_size_yields_larger_metrics() {
        let small = atlas_or_skip!(12.0);
        let big = match font() {
            Some(b) => GlyphAtlas::new(&b, 24.0).unwrap(),
            None => return,
        };
        assert!(big.advance > small.advance);
        assert!(big.line_height > small.line_height);
    }

    #[test]
    fn measure_matches_manual_advance_sum() {
        let atlas = atlas_or_skip!(16.0);
        let text = "FPS 144";
        let expected: f32 = text
            .chars()
            .map(|c| atlas.glyph(c).unwrap().advance_px)
            .sum();
        assert!((atlas.measure(text) - expected).abs() < 1e-3);
    }

    #[test]
    fn unknown_characters_fall_back_to_one_cell_instead_of_vanishing() {
        let atlas = atlas_or_skip!(16.0);
        // Not in the charset: the width must degrade to one cell, not to zero.
        assert!((atlas.measure("漢") - atlas.advance).abs() < 1e-3);
    }
}
