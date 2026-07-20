//! Список отрисовки: текст и прямоугольники превращаются в текстурированные квады.
//!
//! Платформенный бэкенд получает готовые вершины в пиксельных координатах (начало в левом
//! верхнем углу оверлея) и делает ровно две вещи: переводит их в NDC и рисует одним вызовом
//! с атласом на входе. Никакой платформенной логики раскладки не существует.

use bs_core::Color;

use crate::atlas::GlyphAtlas;

/// Вершина. `#[repr(C)]` обязателен: буфер уезжает в графическое API как есть.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Vertex {
    /// Пиксели, начало в левом верхнем углу оверлея.
    pub pos: [f32; 2],
    pub uv: [f32; 2],
    /// Premultiplied alpha — в таком виде ждут и D3D11-swapchain композиции, и Vulkan.
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

    /// Прямоугольник сплошного цвета. Рисуется белым текселем атласа, поэтому не требует
    /// отдельного шейдера или второго вызова отрисовки.
    pub fn rect(&mut self, atlas: &GlyphAtlas, x: f32, y: f32, w: f32, h: f32, color: Color) {
        if color.a == 0 || w <= 0.0 || h <= 0.0 {
            return;
        }
        let uv = atlas.white_uv();
        self.quad(x, y, w, h, uv, uv, color.to_premultiplied_f32());
    }

    /// Рисует текст пером в точке `(x, baseline_y)` и возвращает конечную позицию пера.
    ///
    /// Символы, которых нет в атласе, пропускаются с сохранением шага — строка не съезжает.
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
            "прозрачный фон не должен попадать в буфер вершин"
        );

        // ...но перо всё равно двигается, иначе раскладка поедет.
        let pen = list.text(&atlas, 0.0, 0.0, "невидимо", Color::rgba(255, 255, 255, 0));
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
            "пробелы не рисуются, только двигают перо"
        );
    }

    #[test]
    fn missing_glyphs_keep_the_line_aligned() {
        let atlas = atlas_or_skip!();
        let mut list = DrawList::new();
        // Иероглифа в атласе нет: он не рисуется, но занимает ячейку.
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
        assert!((c[3] - 0.5).abs() < 0.01, "альфа");
        assert!(
            (c[0] - c[3]).abs() < 0.01,
            "красный premultiplied равен альфе"
        );
        assert_eq!(c[1], 0.0);
    }

    #[test]
    fn clear_resets_the_list_for_the_next_frame() {
        let atlas = atlas_or_skip!();
        let mut list = DrawList::new();
        list.text(&atlas, 0.0, 0.0, "мусор", Color::WHITE);
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
            "индекс за пределами буфера вершин"
        );
        assert_eq!(
            list.indices.len() % 6,
            0,
            "геометрия состоит из целых квадов"
        );
    }
}
