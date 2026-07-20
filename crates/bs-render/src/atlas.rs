//! Glyph atlas: the font is rasterised once at startup into a single coverage texture.
//!
//! This is what lets Windows and Linux draw text identically — the platform receives finished
//! vertices and knows nothing about DirectWrite or any text engine at all. The price is no
//! hinting and no subpixel antialiasing; for an overlay of monospaced digits that is a fine
//! trade, whereas the two platforms looking different would not be.
//!
//! The overlay draws at five sizes — a small letterspaced block key, units, device names, the
//! readouts and the large frame rate — and all five live in **one** texture, packed side by
//! side. That is not tidiness: one texture means one shader resource, one sampler and one draw
//! call for the entire overlay, which is the property the whole renderer is built around.
//!
//! Signed-distance-field text would have bought arbitrary sizes from a single raster, and was
//! rejected: it needs a filtering step in the pixel shader and visibly softens ten-pixel text.
//! Rasterising each size exactly and sampling it with a point filter is *why* the overlay looks
//! sharp, and that is worth more here than the flexibility.

use std::collections::HashMap;

use ab_glyph::{Font, FontRef, PxScale, ScaleFont, point};

/// Gap between packed items, so filtering cannot bleed neighbours into each other.
const PADDING: u32 = 1;

/// Atlas width. Height is chosen to fit the content.
const ATLAS_WIDTH: u32 = 1024;

/// An opaque square in the top-left corner of the atlas.
///
/// It lets solid rectangles (the backing panel, load bars) go through the same shader and the
/// same draw call as text — otherwise a second pipeline would be needed purely to fill with
/// colour.
const WHITE_TEXEL_SIZE: u32 = 2;

/// The size the design is written at. Every other size is stated relative to this one, and
/// `font_size` in the settings scales the lot.
pub const DESIGN_BASE_PX: f32 = 12.5;

/// Corner radius of the backing panel at the design size.
const DESIGN_RADIUS_PX: f32 = 8.0;

/// The sizes the overlay draws at.
///
/// Not arbitrary: these come from `design/configurator.html`, where the block key is 10px, the
/// units and small print 11px, the device name 11.5px, the readouts 12.5px and the frame rate
/// 21px.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TextStyle {
    /// Block headings: `FRAMES`, `CPU`. Drawn uppercase and letterspaced.
    Key,
    /// Units, percentile lows, the memory spec line.
    Small,
    /// Device names, right-aligned in the block heading.
    Name,
    /// The readouts themselves.
    Readout,
    /// The frame rate, and nothing else.
    Big,
}

impl TextStyle {
    pub const ALL: [TextStyle; 5] = [
        TextStyle::Key,
        TextStyle::Small,
        TextStyle::Name,
        TextStyle::Readout,
        TextStyle::Big,
    ];

    /// Size at the design scale, before `font_size` is applied.
    pub fn design_px(self) -> f32 {
        match self {
            TextStyle::Key => 10.0,
            TextStyle::Small => 11.0,
            TextStyle::Name => 11.5,
            TextStyle::Readout => DESIGN_BASE_PX,
            TextStyle::Big => 21.0,
        }
    }

    fn index(self) -> usize {
        match self {
            TextStyle::Key => 0,
            TextStyle::Small => 1,
            TextStyle::Name => 2,
            TextStyle::Readout => 3,
            TextStyle::Big => 4,
        }
    }

    /// Which characters this size needs.
    ///
    /// Restricting the two extreme sizes is what keeps a five-size atlas roughly the area of
    /// the old single-size one. The 21px face draws a frame rate and nothing else; the 10px
    /// face draws four fixed words.
    fn charset(self) -> Box<dyn Iterator<Item = char>> {
        match self {
            TextStyle::Big => Box::new("0123456789.—".chars()),
            TextStyle::Key => Box::new(('A'..='Z').chain('0'..='9').chain([' '])),
            _ => Box::new(default_charset()),
        }
    }
}

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

/// One rasterised size.
#[derive(Debug)]
pub struct Face {
    pub px: f32,
    pub line_height: f32,
    pub ascent: f32,
    /// Character cell width. The font is monospaced, so one value covers every glyph.
    pub advance: f32,
    /// Printable ASCII, indexed directly. The HUD looks up a few hundred glyphs per frame and
    /// a hash for each of them is a cost with nothing to show for it.
    ascii: Box<[Option<Glyph>; ASCII_SLOTS]>,
    /// The handful of characters outside ASCII: the degree sign, the em dash, the middle dot.
    extra: HashMap<char, Glyph>,
}

const ASCII_FIRST: u32 = 0x20;
const ASCII_SLOTS: usize = 0x7F - 0x20;

impl Face {
    pub fn glyph(&self, ch: char) -> Option<&Glyph> {
        let code = ch as u32;
        if (ASCII_FIRST..ASCII_FIRST + ASCII_SLOTS as u32).contains(&code) {
            self.ascii[(code - ASCII_FIRST) as usize].as_ref()
        } else {
            self.extra.get(&ch)
        }
    }

    /// Width of a string in pixels, without drawing it.
    pub fn measure(&self, text: &str) -> f32 {
        text.chars()
            .map(|c| self.glyph(c).map_or(self.advance, |g| g.advance_px))
            .sum()
    }

    /// Width including letterspacing, which the block key uses.
    pub fn measure_spaced(&self, text: &str, letter_spacing: f32) -> f32 {
        let n = text.chars().count();
        self.measure(text) + letter_spacing * n.saturating_sub(1) as f32
    }
}

/// The quarter-disc used to round the panel's corners.
///
/// Baked at exactly the radius it will be drawn at, which is the point: a scaled wedge would
/// need a linear sampler, and the atlas is shared with the text, which a linear sampler would
/// blur. Rasterising it once at the right size keeps the whole atlas on a point filter.
#[derive(Debug, Clone, Copy)]
pub struct Wedge {
    pub uv_min: [f32; 2],
    pub uv_max: [f32; 2],
    /// Side of the square, in pixels — the corner radius.
    pub radius: f32,
}

#[derive(Debug)]
pub struct Atlas {
    pub width: u32,
    pub height: u32,
    /// Coverage, one byte per texel. Colour comes from the vertices, not from the texture.
    pub pixels: Vec<u8>,
    faces: [Face; 5],
    white_uv: [f32; 2],
    wedge: Wedge,
    /// How much the design sizes were scaled by. Spacings and gaps use it too, so the whole
    /// panel grows together rather than the text outgrowing its padding.
    pub scale: f32,

    // Mirrors of the readout face, so the existing callers that reach for these directly keep
    // working while the HUD is ported style by style.
    pub line_height: f32,
    pub ascent: f32,
    pub advance: f32,
}

/// The former name, from when there was only one size.
pub type GlyphAtlas = Atlas;

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
    (' '..='~').chain(['°', '—', '·', '×'])
}

impl Atlas {
    /// Rasterises every size the overlay draws at, scaled so that the readout face is `px`.
    pub fn new(font_bytes: &[u8], px: f32) -> Result<Self, AtlasError> {
        let font = FontRef::try_from_slice(font_bytes).map_err(|_| AtlasError::InvalidFont)?;
        let scale = px / DESIGN_BASE_PX;

        // Radius and sizes are rounded to whole pixels. A face rasterised at 10.3px is not
        // sharper than one at 10px, only muddier, and a wedge at a fractional radius cannot
        // line up with the straight edges it joins.
        let radius = (DESIGN_RADIUS_PX * scale).round().max(2.0) as u32;

        let mut items: Vec<Rasterized> = Vec::new();
        let mut metrics: Vec<(f32, f32, f32, f32)> = Vec::with_capacity(5);

        for style in TextStyle::ALL {
            let size = (style.design_px() * scale).round().max(1.0);
            let scaled = font.as_scaled(PxScale::from(size));
            metrics.push((
                size,
                scaled.ascent() - scaled.descent() + scaled.line_gap(),
                scaled.ascent(),
                scaled.h_advance(font.glyph_id('0')),
            ));

            let mut seen: HashMap<char, ()> = HashMap::new();
            for ch in style.charset() {
                if seen.insert(ch, ()).is_some() {
                    continue;
                }
                items.push(rasterize(&font, size, ch, style.index()));
            }
        }

        if items.is_empty() {
            return Err(AtlasError::EmptyCharset);
        }

        Self::pack(items, metrics, radius, scale)
    }

    /// Shelf packing: items are sorted by height and laid out in rows.
    ///
    /// For a couple of thousand glyphs built once at startup this is more than enough —
    /// squeezing out a few percent of area would buy nothing.
    fn pack(
        mut items: Vec<Rasterized>,
        metrics: Vec<(f32, f32, f32, f32)>,
        radius: u32,
        scale: f32,
    ) -> Result<Self, AtlasError> {
        items.sort_unstable_by(|a, b| b.h.cmp(&a.h).then(a.ch.cmp(&b.ch)));

        // The first shelf starts after the two things with fixed positions: the opaque texel
        // and the corner wedge.
        let wedge_x = WHITE_TEXEL_SIZE + PADDING;
        let reserved_height = WHITE_TEXEL_SIZE.max(radius);

        let mut pen_x = wedge_x + radius + PADDING;
        let mut pen_y = 0u32;
        let mut row_height = reserved_height;
        let mut placements: Vec<(usize, u32, u32)> = Vec::with_capacity(items.len());

        for (i, g) in items.iter().enumerate() {
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

        let height = (pen_y + row_height).max(reserved_height);
        let mut pixels = vec![0u8; (ATLAS_WIDTH * height) as usize];

        for y in 0..WHITE_TEXEL_SIZE {
            for x in 0..WHITE_TEXEL_SIZE {
                pixels[(y * ATLAS_WIDTH + x) as usize] = 0xFF;
            }
        }
        draw_wedge(&mut pixels, wedge_x, radius);

        let mut faces: Vec<FaceBuilder> = (0..5).map(|_| FaceBuilder::default()).collect();

        for (i, x, y) in &placements {
            let g = &items[*i];
            for row in 0..g.h {
                let src = (row * g.w) as usize;
                let dst = ((y + row) * ATLAS_WIDTH + x) as usize;
                pixels[dst..dst + g.w as usize]
                    .copy_from_slice(&g.coverage[src..src + g.w as usize]);
            }
            faces[g.face].insert(
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

        // Invisible characters land in the table with no ink but with their own advance, so a
        // space still moves the pen.
        for g in &items {
            faces[g.face].insert_if_absent(
                g.ch,
                Glyph {
                    uv_min: [0.0, 0.0],
                    uv_max: [0.0, 0.0],
                    size_px: [0.0, 0.0],
                    offset_px: [0.0, 0.0],
                    advance_px: g.advance_px,
                },
            );
        }

        let built: Vec<Face> = faces
            .into_iter()
            .zip(&metrics)
            .map(|(b, &(px, line_height, ascent, advance))| b.finish(px, line_height, ascent, advance))
            .collect();
        let faces: [Face; 5] = built
            .try_into()
            .map_err(|_| AtlasError::EmptyCharset)?;

        let half = 0.5;
        let readout = &faces[TextStyle::Readout.index()];
        let (line_height, ascent, advance) = (readout.line_height, readout.ascent, readout.advance);

        Ok(Self {
            width: ATLAS_WIDTH,
            height,
            wedge: Wedge {
                uv_min: [wedge_x as f32 / ATLAS_WIDTH as f32, 0.0],
                uv_max: [
                    (wedge_x + radius) as f32 / ATLAS_WIDTH as f32,
                    radius as f32 / height as f32,
                ],
                radius: radius as f32,
            },
            pixels,
            faces,
            white_uv: [half / ATLAS_WIDTH as f32, half / height as f32],
            scale,
            line_height,
            ascent,
            advance,
        })
    }

    pub fn face(&self, style: TextStyle) -> &Face {
        &self.faces[style.index()]
    }

    /// UV of the opaque texel — for solid-colour quads.
    pub fn white_uv(&self) -> [f32; 2] {
        self.white_uv
    }

    pub fn wedge(&self) -> Wedge {
        self.wedge
    }

    /// Looks up a glyph in the readout face. Kept for callers not yet ported to a style.
    pub fn glyph(&self, ch: char) -> Option<&Glyph> {
        self.face(TextStyle::Readout).glyph(ch)
    }

    /// Width of a string in the readout face.
    pub fn measure(&self, text: &str) -> f32 {
        self.face(TextStyle::Readout).measure(text)
    }
}

/// Rasterises the quarter-disc used for rounded corners.
///
/// Coverage is the signed distance to a circle centred on the *inner* corner of the tile, so
/// the tile's top-left is fully outside the shape and its bottom-right fully inside. Every
/// other corner of the panel reuses this same tile with its texture coordinates mirrored,
/// which is why only one is baked.
fn draw_wedge(pixels: &mut [u8], x0: u32, radius: u32) {
    let r = radius as f32;
    for y in 0..radius {
        for x in 0..radius {
            // Distance from the pixel's centre to the circle's centre at (r, r).
            let dx = r - (x as f32 + 0.5);
            let dy = r - (y as f32 + 0.5);
            let dist = (dx * dx + dy * dy).sqrt();
            // Half a pixel of falloff either side of the edge: cheap analytic antialiasing,
            // and the only antialiasing in the whole renderer.
            let coverage = (r - dist + 0.5).clamp(0.0, 1.0);
            pixels[(y * ATLAS_WIDTH + x0 + x) as usize] = (coverage * 255.0 + 0.5) as u8;
        }
    }
}

fn rasterize(font: &FontRef<'_>, px: f32, ch: char, face: usize) -> Rasterized {
    let scaled = font.as_scaled(PxScale::from(px));
    let id = font.glyph_id(ch);
    let advance_px = scaled.h_advance(id);

    let Some(outlined) = font.outline_glyph(id.with_scale_and_position(px, point(0.0, 0.0))) else {
        // Space and other invisible characters: no ink, but the pen still moves.
        return Rasterized {
            ch,
            face,
            coverage: Vec::new(),
            w: 0,
            h: 0,
            offset_px: [0.0, 0.0],
            advance_px,
        };
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

    Rasterized {
        ch,
        face,
        coverage,
        w,
        h,
        offset_px: [bounds.min.x, bounds.min.y],
        advance_px,
    }
}

struct Rasterized {
    ch: char,
    face: usize,
    coverage: Vec<u8>,
    w: u32,
    h: u32,
    offset_px: [f32; 2],
    advance_px: f32,
}

#[derive(Default)]
struct FaceBuilder {
    ascii: Vec<(usize, Glyph)>,
    extra: HashMap<char, Glyph>,
}

impl FaceBuilder {
    fn slot(ch: char) -> Option<usize> {
        let code = ch as u32;
        (ASCII_FIRST..ASCII_FIRST + ASCII_SLOTS as u32)
            .contains(&code)
            .then(|| (code - ASCII_FIRST) as usize)
    }

    fn insert(&mut self, ch: char, glyph: Glyph) {
        match Self::slot(ch) {
            Some(i) => self.ascii.push((i, glyph)),
            None => {
                self.extra.insert(ch, glyph);
            }
        }
    }

    fn insert_if_absent(&mut self, ch: char, glyph: Glyph) {
        match Self::slot(ch) {
            Some(i) if self.ascii.iter().all(|(j, _)| *j != i) => self.ascii.push((i, glyph)),
            Some(_) => {}
            None => {
                self.extra.entry(ch).or_insert(glyph);
            }
        }
    }

    fn finish(self, px: f32, line_height: f32, ascent: f32, advance: f32) -> Face {
        let mut ascii: Box<[Option<Glyph>; ASCII_SLOTS]> = Box::new([None; ASCII_SLOTS]);
        for (i, g) in self.ascii {
            ascii[i] = Some(g);
        }
        Face {
            px,
            line_height,
            ascent,
            advance,
            ascii,
            extra: self.extra,
        }
    }
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
                Some(bytes) => Atlas::new(&bytes, $px).expect("the atlas should build"),
                None => {
                    eprintln!("skipped: assets/fonts/JetBrainsMono-Regular.ttf not found");
                    return;
                }
            }
        };
    }

    #[test]
    fn rejects_garbage_instead_of_panicking() {
        let err = Atlas::new(b"not a font at all", 16.0);
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
        // Every face, not just the readout: the frame rate is drawn in the big one, and it is
        // the number that changes most often.
        for style in TextStyle::ALL {
            let face = atlas.face(style);
            let widths: Vec<f32> = "0123456789"
                .chars()
                .map(|c| face.glyph(c).expect("digits are in every face").advance_px)
                .collect();
            let first = widths[0];
            for w in &widths {
                assert!(
                    (w - first).abs() < 1e-3,
                    "digits differ in width in {style:?}: {widths:?}"
                );
            }
            // This is the whole reason for choosing a monospaced font: 99 becoming 100 must
            // not shove the rest of the line sideways.
            assert!((face.measure("100") - face.measure("999")).abs() < 1e-3);
        }
    }

    #[test]
    fn the_faces_are_ordered_by_size_the_way_the_design_states_them() {
        let atlas = atlas_or_skip!(16.0);
        let advance = |s| atlas.face(s).advance;
        assert!(advance(TextStyle::Key) <= advance(TextStyle::Small));
        assert!(advance(TextStyle::Small) <= advance(TextStyle::Name));
        assert!(advance(TextStyle::Name) <= advance(TextStyle::Readout));
        assert!(
            advance(TextStyle::Big) > advance(TextStyle::Readout) * 1.4,
            "the frame rate is the one number meant to be read across a room"
        );
    }

    #[test]
    fn font_size_scales_every_face_together() {
        let small = atlas_or_skip!(12.0);
        let big = match font() {
            Some(b) => Atlas::new(&b, 24.0).unwrap(),
            None => return,
        };
        for style in TextStyle::ALL {
            assert!(
                big.face(style).advance > small.face(style).advance,
                "{style:?} did not grow with the setting"
            );
        }
        // The corner radius is part of the same scale, or the panel would look wrong at any
        // size but the default.
        assert!(big.wedge().radius > small.wedge().radius);
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
    fn the_corner_wedge_runs_from_empty_to_solid_and_no_glyph_sits_on_it() {
        let atlas = atlas_or_skip!(16.0);
        let w = atlas.wedge();
        let r = w.radius as u32;
        let x0 = (w.uv_min[0] * atlas.width as f32).round() as u32;
        let at = |x: u32, y: u32| atlas.pixels[(y * atlas.width + x0 + x) as usize];

        // The outer corner of the tile is outside the circle, the inner corner well inside.
        assert_eq!(at(0, 0), 0, "the outside of the corner must be transparent");
        assert_eq!(at(r - 1, r - 1), 0xFF, "the inside must be solid");
        // And it must be a curve, not a step. Checked by counting partial coverage across the
        // whole tile rather than by sampling the diagonal: along the diagonal the distance to
        // the centre changes by more than a pixel per pixel, so the one-pixel falloff band can
        // fall between two samples and a correct wedge would look like a hard edge.
        let partial = (0..r)
            .flat_map(|y| (0..r).map(move |x| (x, y)))
            .filter(|&(x, y)| (1..0xFF).contains(&at(x, y)))
            .count();
        assert!(
            partial >= (r / 2) as usize,
            "the arc is not antialiased: only {partial} partial texels at radius {r}"
        );
    }

    #[test]
    fn glyphs_fit_inside_the_atlas() {
        let atlas = atlas_or_skip!(24.0);
        assert_eq!(atlas.pixels.len(), (atlas.width * atlas.height) as usize);
        for style in TextStyle::ALL {
            for ch in default_charset() {
                let Some(g) = atlas.face(style).glyph(ch) else {
                    continue;
                };
                assert!(
                    g.uv_max[0] <= 1.0 && g.uv_max[1] <= 1.0,
                    "glyph {ch:?} in {style:?} spills out of the atlas"
                );
                assert!(g.uv_min[0] >= 0.0 && g.uv_min[1] >= 0.0);
            }
        }
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
    fn letterspacing_widens_a_run_but_not_a_single_character() {
        let atlas = atlas_or_skip!(16.0);
        let key = atlas.face(TextStyle::Key);
        // Spacing sits *between* characters, so trailing space is not added — otherwise a
        // right-aligned heading would sit a pixel or two off from everything below it.
        assert!((key.measure_spaced("F", 2.0) - key.measure("F")).abs() < 1e-3);
        assert!((key.measure_spaced("CPU", 2.0) - key.measure("CPU") - 4.0).abs() < 1e-3);
    }

    #[test]
    fn unknown_characters_fall_back_to_one_cell_instead_of_vanishing() {
        let atlas = atlas_or_skip!(16.0);
        // Not in the charset: the width must degrade to one cell, not to zero.
        assert!((atlas.measure("漢") - atlas.advance).abs() < 1e-3);
    }

    #[test]
    fn the_restricted_faces_still_carry_what_they_are_for() {
        let atlas = atlas_or_skip!(16.0);
        // The big face draws a frame rate and a dash when there is none, and nothing else.
        for ch in "0123456789.—".chars() {
            assert!(
                atlas.face(TextStyle::Big).glyph(ch).is_some(),
                "the frame rate needs {ch:?}"
            );
        }
        // The key face draws four fixed uppercase words.
        for ch in "FRAMESCPUGURAM".chars() {
            assert!(atlas.face(TextStyle::Key).glyph(ch).is_some());
        }
    }

    #[test]
    fn five_faces_still_fit_in_one_texture() {
        let atlas = atlas_or_skip!(16.0);
        // One texture is the invariant the single draw call rests on. If this ever needs
        // relaxing, the renderer changes too — so the size is asserted rather than assumed.
        assert_eq!(atlas.width, ATLAS_WIDTH);
        assert!(
            atlas.pixels.len() < 1024 * 1024,
            "the atlas has grown past a megabyte: {} bytes",
            atlas.pixels.len()
        );
    }
}
