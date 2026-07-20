//! Глиф-атлас: шрифт растеризуется один раз на старте в одну текстуру покрытия.
//!
//! Так и Windows, и Linux рисуют текст одинаково — платформа получает готовые вершины и не
//! знает ни про DirectWrite, ни про какой-либо текстовый движок вообще. Плата за это —
//! отсутствие хинтинга и субпиксельного сглаживания; для оверлея с моноширинными цифрами
//! это приемлемо, а вот расхождение вида между ОС было бы не приемлемо.

use std::collections::HashMap;

use ab_glyph::{Font, FontRef, PxScale, ScaleFont, point};

/// Отступ между глифами в атласе, чтобы билинейная фильтрация не тянула соседей.
const PADDING: u32 = 1;

/// Ширина атласа. Высота подбирается под содержимое.
const ATLAS_WIDTH: u32 = 512;

/// Непрозрачный квадрат в левом верхнем углу атласа.
///
/// Нужен, чтобы сплошные прямоугольники (подложка, мини-бары загрузки) рисовались тем же
/// шейдером и тем же вызовом отрисовки, что и текст, — иначе пришлось бы держать вторую
/// пайплайн-ветку ради заливки цветом.
const WHITE_TEXEL_SIZE: u32 = 2;

#[derive(Debug, Clone, Copy)]
pub struct Glyph {
    /// Координаты в атласе, 0.0..=1.0.
    pub uv_min: [f32; 2],
    pub uv_max: [f32; 2],
    /// Размер отрисованного пятна в пикселях.
    pub size_px: [f32; 2],
    /// Смещение от точки пера (на базовой линии) до левого верхнего угла пятна.
    pub offset_px: [f32; 2],
    /// На сколько сдвинуть перо после этого символа.
    pub advance_px: f32,
}

#[derive(Debug)]
pub struct GlyphAtlas {
    pub width: u32,
    pub height: u32,
    /// Покрытие, один байт на тексель. Цвет задаётся в вершинах, а не в текстуре.
    pub pixels: Vec<u8>,
    glyphs: HashMap<char, Glyph>,
    /// UV непрозрачного текселя для сплошных заливок.
    white_uv: [f32; 2],
    pub line_height: f32,
    pub ascent: f32,
    /// Ширина символа. Шрифт моноширинный, так что она одна на всех.
    pub advance: f32,
}

#[derive(Debug)]
pub enum AtlasError {
    /// Файл шрифта не разобрался.
    InvalidFont,
    /// В шрифте не нашлось ни одного глифа из запрошенного набора.
    EmptyCharset,
}

impl std::fmt::Display for AtlasError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AtlasError::InvalidFont => f.write_str("не удалось разобрать файл шрифта"),
            AtlasError::EmptyCharset => {
                f.write_str("в шрифте нет ни одного глифа из запрошенного набора")
            }
        }
    }
}

impl std::error::Error for AtlasError {}

/// Набор символов по умолчанию: печатная латиница, кириллица и знак градуса для температур.
pub fn default_charset() -> impl Iterator<Item = char> {
    (' '..='~')
        .chain('А'..='я')
        .chain(['Ё', 'ё', '°', '—', '·'])
}

impl GlyphAtlas {
    /// Растеризует шрифт в атлас на кегле `px`.
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
        // Шрифт моноширинный, поэтому ширина ячейки берётся с любого видимого символа.
        let advance = scaled.h_advance(font.glyph_id('0'));

        // Сначала растеризуем всё, потом раскладываем: пока не известны размеры пятен,
        // упаковывать нечего.
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
                // Пробел и прочие невидимые символы: глифа нет, но перо двигать надо.
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

    /// Полочная упаковка: глифы сортируются по высоте и укладываются рядами.
    ///
    /// Для нескольких сотен глифов одного кегля этого более чем достаточно — атлас строится
    /// один раз на старте, и выигрывать проценты площади незачем.
    fn pack(
        mut glyphs: Vec<Rasterized>,
        line_height: f32,
        ascent: f32,
        advance: f32,
    ) -> Result<Self, AtlasError> {
        glyphs.sort_unstable_by(|a, b| b.h.cmp(&a.h).then(a.ch.cmp(&b.ch)));

        // Первый ряд занят белым текселем для сплошных заливок.
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

        // Невидимые символы попадают в карту без пятна, но со своим шагом пера.
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

    /// UV непрозрачного текселя — для квадов сплошного цвета.
    pub fn white_uv(&self) -> [f32; 2] {
        self.white_uv
    }

    /// Ширина строки в пикселях без её отрисовки.
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

    /// Шрифт не хранится в репозитории, поэтому тесты, которым он нужен, пропускаются,
    /// если его не скачали. См. assets/fonts/README.md.
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
                Some(bytes) => GlyphAtlas::new(&bytes, $px).expect("атлас должен собраться"),
                None => {
                    eprintln!("пропуск: assets/fonts/JetBrainsMono-Regular.ttf не найден");
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
    fn covers_digits_latin_and_cyrillic() {
        let atlas = atlas_or_skip!(16.0);
        for ch in "0123456789ABCXYZ%°".chars() {
            assert!(atlas.glyph(ch).is_some(), "нет глифа для {ch:?}");
        }
        // Интерфейс и сообщения на русском, поэтому кириллица обязана быть в атласе.
        for ch in "ЦПГрадусов".chars() {
            assert!(atlas.glyph(ch).is_some(), "нет глифа для {ch:?}");
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
            assert!((w - first).abs() < 1e-3, "цифры разной ширины: {widths:?}");
        }
        // Это главная причина выбора моноширинного шрифта: смена 99 на 100 не должна
        // дёргать всю строку.
        assert!((atlas.measure("100") - atlas.measure("999")).abs() < 1e-3);
    }

    #[test]
    fn space_has_advance_but_no_ink() {
        let atlas = atlas_or_skip!(16.0);
        let space = atlas.glyph(' ').expect("пробел должен быть в атласе");
        assert!(space.advance_px > 0.0, "пробел обязан двигать перо");
        assert_eq!(space.size_px, [0.0, 0.0], "у пробела нет пятна");
    }

    #[test]
    fn atlas_has_an_opaque_texel_for_solid_fills() {
        let atlas = atlas_or_skip!(16.0);
        assert_eq!(
            atlas.pixels[0], 0xFF,
            "левый верхний тексель должен быть непрозрачным"
        );

        let [u, v] = atlas.white_uv();
        let x = (u * atlas.width as f32) as usize;
        let y = (v * atlas.height as f32) as usize;
        assert_eq!(
            atlas.pixels[y * atlas.width as usize + x],
            0xFF,
            "white_uv обязан указывать внутрь непрозрачного пятна"
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
                "глиф {ch:?} вылез за атлас"
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
        // Иероглифа в наборе нет; ширина должна деградировать до ячейки, а не до нуля.
        assert!((atlas.measure("漢") - atlas.advance).abs() < 1e-3);
    }
}
