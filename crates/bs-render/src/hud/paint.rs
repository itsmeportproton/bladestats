//! The model becomes geometry.
//!
//! Two passes over the same description: one to find out how wide the panel has to be, one to
//! fill it. They are separate because the second needs the answer from the first — the frame
//! time and the memory rate are right-aligned, and a bar spans the full content width, so
//! nothing can be placed until the width is settled.

use bs_core::{Color, Theme};

use crate::atlas::{Atlas, TextStyle};
use crate::draw::DrawList;

use super::model::{Block, Cell, HudModel, Row};

/// The resulting overlay size — the platform sizes its window from this.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HudSize {
    pub width: f32,
    pub height: f32,
}

/// Spacings, in design pixels. Scaled by the atlas, so the whole panel grows with `font_size`
/// rather than the text outgrowing its padding.
///
/// These are not settings and are not meant to become settings. What the user chooses is which
/// readings appear, the colours and the size; these are the numbers that only decide whether
/// the panel looks like the design.
#[derive(Debug, Clone)]
pub struct HudStyle {
    pub pad_x: f32,
    pub pad_y: f32,
    pub block_pad_y: f32,
    pub head_gap: f32,
    pub head_min_gap: f32,
    pub row_gap: f32,
    pub cell_gap: f32,
    pub key_spacing: f32,
    pub bar_height: f32,
    pub bar_gap: f32,
    pub cores_height: f32,
    pub cores_gap: f32,
    pub core_spacing: f32,
    pub spec_gap: f32,
    pub min_width: f32,
}

impl Default for HudStyle {
    fn default() -> Self {
        Self {
            pad_x: 16.0,
            pad_y: 6.0,
            block_pad_y: 9.0,
            head_gap: 6.0,
            head_min_gap: 10.0,
            row_gap: 4.0,
            cell_gap: 14.0,
            // 0.14em at the 10px key size, which is what the design asks for.
            key_spacing: 1.4,
            bar_height: 4.0,
            bar_gap: 7.0,
            cores_height: 16.0,
            cores_gap: 8.0,
            core_spacing: 2.0,
            spec_gap: 3.0,
            min_width: 240.0,
        }
    }
}

/// The panel's hairline border. Structural rather than thematic — it exists so the panel has
/// an edge against a bright game frame, at every colour scheme.
const BORDER: Color = Color::rgba(0xFF, 0xFF, 0xFF, 0x17);

/// The track a bar sits in, and the same tone behind an idle core.
const TRACK: Color = Color::rgba(0xFF, 0xFF, 0xFF, 0x1A);

/// The rule between blocks.
const DIVIDER: Color = Color::rgba(0xFF, 0xFF, 0xFF, 0x12);

/// A scaled copy of the style, so the arithmetic below reads in real pixels.
struct Metrics<'a> {
    atlas: &'a Atlas,
    s: HudStyle,
}

impl<'a> Metrics<'a> {
    fn new(atlas: &'a Atlas, style: &HudStyle) -> Self {
        let k = atlas.scale;
        let s = HudStyle {
            pad_x: style.pad_x * k,
            pad_y: style.pad_y * k,
            block_pad_y: style.block_pad_y * k,
            head_gap: style.head_gap * k,
            head_min_gap: style.head_min_gap * k,
            row_gap: style.row_gap * k,
            cell_gap: style.cell_gap * k,
            key_spacing: style.key_spacing * k,
            bar_height: (style.bar_height * k).round().max(2.0),
            bar_gap: style.bar_gap * k,
            cores_height: style.cores_height * k,
            cores_gap: style.cores_gap * k,
            core_spacing: (style.core_spacing * k).round().max(1.0),
            spec_gap: style.spec_gap * k,
            min_width: style.min_width * k,
        };
        Self { atlas, s }
    }

    /// Width of a cell at its reserved size, not at the size this frame's value happens to
    /// need. Measuring the actual value here is what used to make the panel breathe.
    fn cell_width(&self, cell: &Cell) -> f32 {
        self.atlas.face(cell.style).advance * cell.slots() as f32
            + self.atlas.face(TextStyle::Small).measure(&cell.unit)
    }

    /// How tall a row is, and how much air goes above it.
    fn row_metrics(&self, row: &Row, first: bool) -> (f32, f32) {
        let lead = if first { self.s.head_gap } else { 0.0 };
        match row {
            Row::Readout { cells, .. } => {
                let h = cells
                    .iter()
                    .map(|c| self.atlas.face(c.style).line_height)
                    .fold(0.0f32, f32::max);
                (lead + if first { 0.0 } else { self.s.row_gap }, h)
            }
            Row::Cores(_) => (lead + self.s.cores_gap, self.s.cores_height),
            Row::Bar(_) => (lead + self.s.bar_gap, self.s.bar_height),
            Row::Spec(_) => (
                lead + self.s.spec_gap,
                self.atlas.face(TextStyle::Small).line_height,
            ),
        }
    }

    fn head_height(&self) -> f32 {
        self.atlas
            .face(TextStyle::Key)
            .line_height
            .max(self.atlas.face(TextStyle::Name).line_height)
    }

    fn row_width(&self, row: &Row) -> f32 {
        match row {
            Row::Readout { cells, .. } => {
                let text: f32 = cells.iter().map(|c| self.cell_width(c)).sum();
                text + self.s.cell_gap * cells.len().saturating_sub(1) as f32
            }
            Row::Spec(text) => self.atlas.face(TextStyle::Small).measure(text),
            // A bar takes whatever it is given and a core strip shrinks its bars, so neither
            // asks the panel to be any particular width.
            Row::Cores(_) | Row::Bar(_) => 0.0,
        }
    }

    fn block_head_width(&self, block: &Block) -> f32 {
        let key = self
            .atlas
            .face(TextStyle::Key)
            .measure_spaced(block.key, self.s.key_spacing);
        match &block.name {
            Some(name) => {
                key + self.s.head_min_gap + self.atlas.face(TextStyle::Name).measure(name)
            }
            None => key,
        }
    }

    fn block_height(&self, block: &Block) -> f32 {
        let mut h = self.s.block_pad_y * 2.0 + self.head_height();
        for (i, row) in block.rows.iter().enumerate() {
            let (lead, height) = self.row_metrics(row, i == 0);
            h += lead + height;
        }
        h
    }
}

/// How large the panel has to be for this model.
pub fn measure(model: &HudModel, atlas: &Atlas, style: &HudStyle) -> HudSize {
    let m = Metrics::new(atlas, style);

    let mut content = m.s.min_width - m.s.pad_x * 2.0;
    let mut height = m.s.pad_y * 2.0;

    for (i, block) in model.blocks.iter().enumerate() {
        content = content.max(m.block_head_width(block));
        for row in &block.rows {
            content = content.max(m.row_width(row));
        }
        height += m.block_height(block);
        if i > 0 {
            height += 1.0; // the divider
        }
    }

    if let Some(notice) = &model.notice {
        content = content.max(atlas.face(TextStyle::Small).measure(notice));
        height += atlas.face(TextStyle::Small).line_height + m.s.block_pad_y;
        if !model.blocks.is_empty() {
            height += 1.0;
        }
    }

    HudSize {
        width: (content + m.s.pad_x * 2.0).ceil(),
        height: height.ceil(),
    }
}

/// Fills `list` with the panel at the given size.
///
/// The size is passed in rather than recomputed so the caller can hold it steady — an animated
/// resize wants to draw the settled layout inside a box that is still moving.
pub fn paint(
    list: &mut DrawList,
    model: &HudModel,
    atlas: &Atlas,
    theme: &Theme,
    style: &HudStyle,
    size: HudSize,
) {
    let m = Metrics::new(atlas, style);
    let (w, h) = (size.width, size.height);

    // Border first, then the fill inset by a pixel over the top of it: what remains visible is
    // a one-pixel ring, and it costs one extra rounded rectangle rather than a stroke path.
    list.rounded_rect(atlas, 0.0, 0.0, w, h, BORDER);
    list.rounded_rect(atlas, 1.0, 1.0, w - 2.0, h - 2.0, theme.background);

    let left = m.s.pad_x;
    let right = w - m.s.pad_x;
    let content_w = right - left;
    let mut y = m.s.pad_y;

    for (i, block) in model.blocks.iter().enumerate() {
        if i > 0 {
            list.hairline(atlas, left, y, content_w, DIVIDER);
            y += 1.0;
        }
        y += m.s.block_pad_y;
        y = paint_block(list, block, atlas, theme, &m, left, right, y);
        y += m.s.block_pad_y;
    }

    if let Some(notice) = &model.notice {
        if !model.blocks.is_empty() {
            list.hairline(atlas, left, y, content_w, DIVIDER);
            y += 1.0;
        }
        y += m.s.block_pad_y;
        let face = atlas.face(TextStyle::Small);
        list.text_styled(
            atlas,
            TextStyle::Small,
            left,
            y + face.ascent,
            notice,
            theme.warn,
            0.0,
        );
    }
}

fn paint_block(
    list: &mut DrawList,
    block: &Block,
    atlas: &Atlas,
    theme: &Theme,
    m: &Metrics<'_>,
    left: f32,
    right: f32,
    mut y: f32,
) -> f32 {
    // The heading: the section's name on the left, the device it describes on the right.
    let head_h = m.head_height();
    let key_face = atlas.face(TextStyle::Key);
    list.text_styled(
        atlas,
        TextStyle::Key,
        left,
        y + key_face.ascent,
        block.key,
        theme.faint,
        m.s.key_spacing,
    );
    if let Some(name) = &block.name {
        let face = atlas.face(TextStyle::Name);
        list.text_styled(
            atlas,
            TextStyle::Name,
            right - face.measure(name),
            y + face.ascent,
            name,
            block.tint,
            0.0,
        );
    }
    y += head_h;

    for (i, row) in block.rows.iter().enumerate() {
        let (lead, height) = m.row_metrics(row, i == 0);
        y += lead;
        match row {
            Row::Readout { cells, right_from } => {
                paint_readout(list, cells, *right_from, atlas, theme, m, left, right, y);
            }
            Row::Cores(loads) => {
                paint_cores(list, loads, atlas, m, left, right, y, height, block.tint);
            }
            Row::Bar(fraction) => {
                list.rect(atlas, left, y, right - left, height, TRACK);
                list.rect(
                    atlas,
                    left,
                    y,
                    (right - left) * fraction.clamp(0.0, 1.0),
                    height,
                    block.tint,
                );
            }
            Row::Spec(text) => {
                let face = atlas.face(TextStyle::Small);
                list.text_styled(
                    atlas,
                    TextStyle::Small,
                    left,
                    y + face.ascent,
                    text,
                    theme.label,
                    0.0,
                );
            }
        }
        y += height;
    }
    y
}

#[allow(clippy::too_many_arguments)]
fn paint_readout(
    list: &mut DrawList,
    cells: &[Cell],
    right_from: Option<usize>,
    atlas: &Atlas,
    theme: &Theme,
    m: &Metrics<'_>,
    left: f32,
    right: f32,
    top: f32,
) {
    // One baseline for the whole row, taken from its tallest face. That is what puts the small
    // "fps" on the same line as the large number rather than floating above it.
    let baseline = top
        + cells
            .iter()
            .map(|c| atlas.face(c.style).ascent)
            .fold(0.0f32, f32::max);

    let split = right_from.unwrap_or(cells.len());
    let mut pen = left;
    for cell in &cells[..split.min(cells.len())] {
        pen = paint_cell(list, cell, atlas, theme, pen, baseline) + m.s.cell_gap;
    }

    if split < cells.len() {
        let tail = &cells[split..];
        let width: f32 = tail.iter().map(|c| m.cell_width(c)).sum::<f32>()
            + m.s.cell_gap * tail.len().saturating_sub(1) as f32;
        let mut pen = right - width;
        for cell in tail {
            pen = paint_cell(list, cell, atlas, theme, pen, baseline) + m.s.cell_gap;
        }
    }
}

fn paint_cell(
    list: &mut DrawList,
    cell: &Cell,
    atlas: &Atlas,
    theme: &Theme,
    pen: f32,
    baseline: f32,
) -> f32 {
    let face = atlas.face(cell.style);
    let slots = cell.slots();
    let width = face.advance * slots as f32;

    // Right-aligned inside the reservation, so the last digit is the one that stays put. That
    // is what makes a counter readable: the eye holds position on the units column while the
    // digits above it change.
    let used = cell.value.chars().count();
    let start = pen + face.advance * slots.saturating_sub(used) as f32;

    let mut chars = cell.value.chars();
    if cell.lead_alpha < 1.0
        && let Some(lead) = chars.next()
    {
        // A reading that just grew a digit: fade the new leading character in and let it
        // settle down into line, rather than having it appear from nowhere between two frames.
        let mut faded = cell.color;
        faded.a = (cell.color.a as f32 * cell.lead_alpha) as u8;
        let rise = face.line_height * 0.35 * (1.0 - cell.lead_alpha);
        list.text_styled(
            atlas,
            cell.style,
            start,
            baseline - rise,
            &lead.to_string(),
            faded,
            0.0,
        );
        let rest: String = chars.collect();
        list.text_styled(
            atlas,
            cell.style,
            start + face.advance,
            baseline,
            &rest,
            cell.color,
            0.0,
        );
    } else {
        list.text_styled(atlas, cell.style, start, baseline, &cell.value, cell.color, 0.0);
    }

    let pen = pen + width;
    if cell.unit.is_empty() {
        return pen;
    }
    list.text_styled(
        atlas,
        TextStyle::Small,
        pen,
        baseline,
        &cell.unit,
        theme.label,
        0.0,
    )
}

/// Per-core load as a strip of bars.
///
/// As numbers this would take several lines on any modern processor, and what the eye wants
/// from an overlay is the shape of the distribution, not exact per-core percentages.
#[allow(clippy::too_many_arguments)]
fn paint_cores(
    list: &mut DrawList,
    loads: &[f32],
    atlas: &Atlas,
    m: &Metrics<'_>,
    left: f32,
    right: f32,
    top: f32,
    height: f32,
    tint: Color,
) {
    if loads.is_empty() {
        return;
    }
    let available = right - left;
    let gap = m.s.core_spacing;
    let pitch = (available + gap) / loads.len() as f32;
    let width = (pitch - gap).max(1.0);
    let floor = (2.0 * atlas.scale).min(height);

    // No track behind them, unlike the usage bars. A strip of grey columns with red tips reads
    // as sixteen half-full gauges; the bare columns read as a load profile, which is the thing
    // being shown. The floor height is what keeps an idle core visible instead.
    for (i, load) in loads.iter().enumerate() {
        let x = left + i as f32 * pitch;
        let filled = (load.clamp(0.0, 1.0) * height).max(floor);
        list.rect(atlas, x, top + height - filled, width, filled, tint);
    }
}
