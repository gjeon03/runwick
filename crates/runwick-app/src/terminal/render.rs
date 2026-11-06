//! Painting a terminal grid: backgrounds, text, decorations, selection, search matches,
//! cursor and IME pre-edit text.
//!
//! Runs of same-style ASCII share one galley (positions match because JetBrains Mono advances
//! exactly one cell per character); other characters are placed one by one, wide ones centred
//! over their two cells. Work per frame is proportional to the visible grid, not the history.

use super::boxdraw;
use super::model::Events;
use crate::theme::{c, mono, mono_bold_family};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::term::search::Match;
use alacritty_terminal::term::{Term, TermMode};
use alacritty_terminal::vte::ansi::{Color, CursorShape, NamedColor};
use egui::text::{LayoutJob, TextFormat};
use egui::{Color32, FontId, Painter, Pos2, Rect, Stroke, Vec2};
use runwick_core::api::sessions::palette;

/// Size factor for characters that take two cells.
const WIDE_SCALE: f32 = 1.1;

/// Inner padding of the terminal area (design: 12px top, 16px sides).
pub const PAD: Vec2 = Vec2::new(14.0, 10.0);

#[derive(Clone)]
pub struct Metrics {
    pub cell: Vec2,
    pub font: FontId,
    pub bold: FontId,
    pub ppp: f32,
}

impl Metrics {
    /// At the terminal text size from the settings (일반 > 터미널 글자 크기).
    pub fn new(ctx: &egui::Context) -> Self {
        Self::with_size(ctx, crate::views::settings::general::terminal_font_size())
    }

    fn with_size(ctx: &egui::Context, size: f32) -> Self {
        let font = mono(size);
        let bold = FontId::new(size, mono_bold_family());
        let ppp = ctx.pixels_per_point();
        let w = ctx.fonts_mut(|f| f.glyph_width(&font, 'M'));
        // Line height from the design (19 at 12.5), on whole physical pixels.
        let h = ((size * 1.52) * ppp).round() / ppp;
        Self { cell: Vec2::new(w, h), font, bold, ppp }
    }

    /// Columns and rows that fit in `size` (including padding).
    pub fn grid_for(&self, size: Vec2) -> (u16, u16) {
        let cols = ((size.x - PAD.x * 2.0) / self.cell.x).floor().clamp(10.0, 1000.0);
        let rows = ((size.y - PAD.y * 2.0) / self.cell.y).floor().clamp(3.0, 500.0);
        (cols as u16, rows as u16)
    }

    fn snap(&self, v: f32) -> f32 {
        (v * self.ppp).round() / self.ppp
    }
}

/// Everything besides the terminal itself that affects how it is drawn.
pub struct Overlay<'a> {
    pub focused: bool,
    /// Cursor drawn this frame (false during the "off" half of a blink).
    pub cursor_on: bool,
    /// No cursor at all (the run has ended).
    pub cursor_hidden: bool,
    pub matches: &'a [Match],
    pub current_match: Option<usize>,
    pub preedit: &'a str,
}

pub struct Layout {
    /// Top-left of cell (0, 0).
    pub origin: Pos2,
    pub m: Metrics,
}

impl Layout {
    pub fn x(&self, col: usize) -> f32 {
        self.m.snap(self.origin.x + col as f32 * self.m.cell.x)
    }
    pub fn y(&self, row: usize) -> f32 {
        self.origin.y + row as f32 * self.m.cell.y
    }
    pub fn cell_rect(&self, row: usize, col: usize, width: usize) -> Rect {
        Rect::from_min_max(Pos2::new(self.x(col), self.y(row)), Pos2::new(self.x(col + width), self.y(row + 1)))
    }
    /// Viewport cell under a position (clamped to the grid).
    pub fn cell_at(&self, pos: Pos2, cols: usize, rows: usize) -> (usize, usize, bool) {
        let fx = (pos.x - self.origin.x) / self.m.cell.x;
        let fy = (pos.y - self.origin.y) / self.m.cell.y;
        let col = (fx.floor().max(0.0) as usize).min(cols.saturating_sub(1));
        let row = (fy.floor().max(0.0) as usize).min(rows.saturating_sub(1));
        let right_half = fx - fx.floor() >= 0.5;
        (row, col, right_half)
    }
}

pub fn rgb(hex: u32) -> Color32 {
    let (r, g, b) = palette::rgb(hex);
    Color32::from_rgb(r, g, b)
}

/// Resolved color of a cell attribute.
pub fn resolve(color: Color, colors: &Colors) -> Color32 {
    let from_table = |i: usize| colors[i].map(|c| Color32::from_rgb(c.r, c.g, c.b));
    match color {
        Color::Spec(c) => Color32::from_rgb(c.r, c.g, c.b),
        Color::Indexed(i) => from_table(i as usize).unwrap_or_else(|| rgb(palette::indexed(i))),
        Color::Named(n) => {
            let i = n as usize;
            if let Some(c) = from_table(i) {
                return c;
            }
            match n {
                NamedColor::Foreground | NamedColor::BrightForeground => rgb(palette::FOREGROUND),
                NamedColor::Background => rgb(palette::BACKGROUND),
                NamedColor::Cursor => rgb(palette::CURSOR),
                NamedColor::DimForeground => dim(rgb(palette::FOREGROUND), rgb(palette::BACKGROUND)),
                _ if i < 16 => rgb(palette::ANSI[i]),
                _ => {
                    // DimBlack..DimWhite follow Cursor.
                    let base = i.saturating_sub(NamedColor::DimBlack as usize).min(7);
                    dim(rgb(palette::ANSI[base]), rgb(palette::BACKGROUND))
                }
            }
        }
    }
}

fn dim(fg: Color32, bg: Color32) -> Color32 {
    let mix = |a: u8, b: u8| (a as f32 * 0.66 + b as f32 * 0.34) as u8;
    Color32::from_rgb(mix(fg.r(), bg.r()), mix(fg.g(), bg.g()), mix(fg.b(), bg.b()))
}

/// Foreground and background of a cell after inverse, dim and hidden.
fn cell_colors(cell: &Cell, colors: &Colors, default_bg: Color32) -> (Color32, Color32) {
    let mut fg = resolve(cell.fg, colors);
    let mut bg = resolve(cell.bg, colors);
    if cell.flags.contains(Flags::INVERSE) {
        std::mem::swap(&mut fg, &mut bg);
    }
    if cell.flags.contains(Flags::DIM) {
        fg = dim(fg, if cell.flags.contains(Flags::INVERSE) { default_bg } else { bg });
    }
    if cell.flags.contains(Flags::HIDDEN) {
        fg = bg;
    }
    (fg, bg)
}

/// Text style key for batching.
#[derive(PartialEq, Clone, Copy)]
struct Style {
    fg: Color32,
    bold: bool,
    italic: bool,
}

pub fn paint(p: &Painter, lay: &Layout, term: &Term<Events>, ov: &Overlay<'_>) {
    let grid = term.grid();
    let cols = grid.columns();
    let rows = grid.screen_lines();
    let offset = grid.display_offset() as i32;
    let vp = Viewport { cols, rows, offset };
    let colors = term.colors();
    let default_bg = resolve(Color::Named(NamedColor::Background), colors);
    let selection = term.selection.as_ref().and_then(|s| s.to_range(term));

    // 1. Backgrounds (runs of the same non-default colour).
    for row in 0..rows {
        let line = Line(row as i32 - offset);
        let r = &grid[line];
        let mut run: Option<(usize, Color32)> = None;
        for col in 0..=cols {
            let bg =
                (col < cols).then(|| cell_colors(&r[Column(col)], colors, default_bg).1).filter(|b| *b != default_bg);
            match (run, bg) {
                (Some((_, a)), Some(b)) if a == b => {}
                (prev, next) => {
                    if let Some((start, color)) = prev {
                        p.rect_filled(lay.cell_rect(row, start, col - start), 0.0, color);
                    }
                    run = next.map(|b| (col, b));
                }
            }
        }
    }

    // 2. Search matches and selection, under the text.
    for (i, mtch) in ov.matches.iter().enumerate() {
        let color = if Some(i) == ov.current_match {
            Color32::from_rgba_unmultiplied(255, 139, 62, 120)
        } else {
            Color32::from_rgba_unmultiplied(242, 198, 109, 60)
        };
        highlight(p, lay, vp, *mtch.start(), *mtch.end(), color);
    }
    if let Some(sel) = selection {
        let color = Color32::from_rgba_unmultiplied(116, 183, 255, 80);
        if sel.is_block {
            for line in sel.start.line.0..=sel.end.line.0 {
                let a = Point::new(Line(line), sel.start.column);
                let b = Point::new(Line(line), sel.end.column);
                highlight(p, lay, vp, a, b, color);
            }
        } else {
            highlight(p, lay, vp, sel.start, sel.end, color);
        }
    }

    // 3. Text and decorations.
    let mut run = String::with_capacity(cols);
    for row in 0..rows {
        let line = Line(row as i32 - offset);
        let r = &grid[line];
        let mut run_start = 0;
        let mut run_style: Option<Style> = None;
        let mut col = 0;
        while col < cols {
            let cell = &r[Column(col)];
            let flags = cell.flags;
            if flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                flush(p, lay, row, run_start, &mut run, run_style);
                run_style = None;
                col += 1;
                continue;
            }
            let (fg, _) = cell_colors(cell, colors, default_bg);
            let style = Style { fg, bold: flags.contains(Flags::BOLD), italic: flags.contains(Flags::ITALIC) };
            let width = if flags.contains(Flags::WIDE_CHAR) { 2 } else { 1 };
            decorate(p, lay, lay.cell_rect(row, col, width), cell, fg, colors);
            let ch = cell.c;
            let simple = ch.is_ascii_graphic() || ch == ' ' || ch == '\t';
            if simple && cell.zerowidth().is_none() {
                if run_style != Some(style) {
                    flush(p, lay, row, run_start, &mut run, run_style);
                    run_style = Some(style);
                    run_start = col;
                }
                run.push(if ch == '\t' { ' ' } else { ch });
            } else {
                flush(p, lay, row, run_start, &mut run, run_style);
                run_style = None;
                if !flags.contains(Flags::HIDDEN) {
                    glyph(p, lay, row, col, width, cell, style);
                }
            }
            col += width;
        }
        flush(p, lay, row, run_start, &mut run, run_style);
    }

    // 4. Cursor and pre-edit.
    paint_cursor(p, lay, term, ov, colors, default_bg);
}

/// Visible part of the grid: columns, rows and how far the view is scrolled back.
#[derive(Clone, Copy)]
struct Viewport {
    cols: usize,
    rows: usize,
    offset: i32,
}

fn highlight(p: &Painter, lay: &Layout, vp: Viewport, start: Point, end: Point, color: Color32) {
    let Viewport { cols, rows, offset } = vp;
    for line in start.line.0..=end.line.0 {
        let row = line + offset;
        if row < 0 || row >= rows as i32 {
            continue;
        }
        let a = if line == start.line.0 { start.column.0 } else { 0 };
        let b = if line == end.line.0 { end.column.0 } else { cols - 1 };
        if b >= a {
            p.rect_filled(lay.cell_rect(row as usize, a, b - a + 1), 0.0, color);
        }
    }
}

fn text_format(lay: &Layout, style: Style) -> TextFormat {
    TextFormat {
        font_id: if style.bold { lay.m.bold.clone() } else { lay.m.font.clone() },
        color: style.fg,
        italics: style.italic,
        line_height: Some(lay.m.cell.y),
        ..Default::default()
    }
}

fn flush(p: &Painter, lay: &Layout, row: usize, start: usize, run: &mut String, style: Option<Style>) {
    if let Some(style) = style {
        let text = run.trim_end();
        if !text.is_empty() {
            let mut job = LayoutJob::single_section(text.to_string(), text_format(lay, style));
            job.wrap.max_width = f32::INFINITY;
            let galley = p.layout_job(job);
            p.galley(Pos2::new(lay.x(start), lay.y(row)), galley, style.fg);
        }
    }
    run.clear();
}

/// One character placed on its own: box/block drawing, or a glyph centred in its cells.
fn glyph(p: &Painter, lay: &Layout, row: usize, col: usize, width: usize, cell: &Cell, style: Style) {
    let rect = lay.cell_rect(row, col, width);
    if cell.zerowidth().is_none() && boxdraw::draw(p, cell.c, rect, style.fg, lay.m.ppp) {
        return;
    }
    let mut text = String::with_capacity(8);
    text.push(cell.c);
    if let Some(zw) = cell.zerowidth() {
        text.extend(zw.iter().take(runwick_core::term_guard::MAX_ZERO_WIDTH));
    }
    let mut format = text_format(lay, style);
    if width == 2 {
        // Korean/CJK glyphs are about one em wide; two cells are 1.2 em. Slightly larger
        // glyphs close most of the gap without touching the neighbours.
        format.font_id.size *= WIDE_SCALE;
    }
    let galley = p.layout_job(LayoutJob::single_section(text, format));
    let x = rect.center().x - galley.size().x / 2.0;
    p.galley(Pos2::new(lay.m.snap(x), rect.top()), galley, style.fg);
}

fn decorate(p: &Painter, lay: &Layout, rect: Rect, cell: &Cell, fg: Color32, colors: &Colors) {
    let flags = cell.flags;
    if !flags.intersects(Flags::ALL_UNDERLINES | Flags::STRIKEOUT) || flags.contains(Flags::HIDDEN) {
        return;
    }
    let px = 1.0 / lay.m.ppp;
    let t = px.max(lay.m.cell.y / 19.0);
    let color = cell.underline_color().map(|c| resolve(c, colors)).unwrap_or(fg);
    let base = lay.m.snap(rect.bottom() - lay.m.cell.y * 0.2);
    let stroke = Stroke::new(t, color);
    if flags.contains(Flags::DOUBLE_UNDERLINE) {
        p.hline(rect.x_range(), base - t, stroke);
        p.hline(rect.x_range(), base + t, stroke);
    } else if flags.contains(Flags::UNDERCURL) {
        let steps = (rect.width() / 2.0).ceil().max(2.0) as usize;
        let pts: Vec<Pos2> = (0..=steps)
            .map(|i| {
                let x = rect.left() + rect.width() * i as f32 / steps as f32;
                let y = base + if i % 2 == 0 { -t } else { t };
                Pos2::new(x, y)
            })
            .collect();
        p.add(egui::Shape::line(pts, stroke));
    } else if flags.contains(Flags::DOTTED_UNDERLINE) || flags.contains(Flags::DASHED_UNDERLINE) {
        let seg = if flags.contains(Flags::DOTTED_UNDERLINE) { t * 2.0 } else { lay.m.cell.x / 2.0 };
        let mut x = rect.left();
        while x < rect.right() {
            p.hline(x..=(x + seg / 2.0).min(rect.right()), base, stroke);
            x += seg;
        }
    } else if flags.contains(Flags::UNDERLINE) {
        p.hline(rect.x_range(), base, stroke);
    }
    if flags.contains(Flags::STRIKEOUT) {
        p.hline(rect.x_range(), lay.m.snap(rect.center().y), stroke);
    }
}

fn paint_cursor(
    p: &Painter,
    lay: &Layout,
    term: &Term<Events>,
    ov: &Overlay<'_>,
    colors: &Colors,
    default_bg: Color32,
) {
    let content = term.renderable_content();
    let cursor = content.cursor;
    let offset = content.display_offset as i32;
    let row = cursor.point.line.0 + offset;
    let rows = term.screen_lines() as i32;
    if row < 0 || row >= rows {
        return;
    }
    let row = row as usize;
    let col = cursor.point.column.0;
    let grid = term.grid();
    let cell = &grid[cursor.point];
    let width = if cell.flags.contains(Flags::WIDE_CHAR) { 2 } else { 1 };
    let rect = lay.cell_rect(row, col, width);

    if !ov.preedit.is_empty() {
        preedit(p, lay, rect, ov.preedit);
        return;
    }
    let shape = if content.mode.contains(TermMode::SHOW_CURSOR) { cursor.shape } else { CursorShape::Hidden };
    if shape == CursorShape::Hidden || ov.cursor_hidden {
        return;
    }
    let color = resolve(Color::Named(NamedColor::Cursor), colors);
    let px = 1.0 / lay.m.ppp;
    if !ov.focused {
        p.rect_stroke(
            rect.shrink(px / 2.0),
            0.0,
            Stroke::new(px.max(1.0), color.gamma_multiply(0.7)),
            egui::StrokeKind::Inside,
        );
        return;
    }
    if !ov.cursor_on {
        return;
    }
    match shape {
        CursorShape::Beam => {
            let w = (2.0 * px).max(lay.m.cell.x / 8.0);
            p.rect_filled(Rect::from_min_size(rect.min, Vec2::new(w, rect.height())), 0.0, color);
        }
        CursorShape::Underline => {
            let h = (2.0 * px).max(lay.m.cell.y / 10.0);
            p.rect_filled(Rect::from_min_max(Pos2::new(rect.left(), rect.bottom() - h), rect.max), 0.0, color);
        }
        CursorShape::HollowBlock => {
            p.rect_stroke(rect, 0.0, Stroke::new(px.max(1.0), color), egui::StrokeKind::Inside);
        }
        _ => {
            // Block: fill, then the character in the background colour.
            p.rect_filled(rect, 0.0, color);
            if cell.c != ' ' && !cell.flags.contains(Flags::HIDDEN) {
                let style = Style {
                    fg: default_bg,
                    bold: cell.flags.contains(Flags::BOLD),
                    italic: cell.flags.contains(Flags::ITALIC),
                };
                glyph(p, lay, row, col, width, cell, style);
            }
        }
    }
}

/// IME composition shown at the cursor, underlined, over whatever is there.
fn preedit(p: &Painter, lay: &Layout, at: Rect, text: &str) {
    // Same size as committed wide characters, so the syllable does not jump when it commits.
    let mut font_id = lay.m.font.clone();
    if !text.is_ascii() {
        font_id.size *= WIDE_SCALE;
    }
    let galley = p.layout_job(LayoutJob::single_section(
        text.to_string(),
        TextFormat { font_id, color: c::TEXT_STRONG, line_height: Some(lay.m.cell.y), ..Default::default() },
    ));
    let rect = Rect::from_min_size(at.min, Vec2::new(galley.size().x.max(lay.m.cell.x), at.height()));
    p.rect_filled(rect.expand2(Vec2::new(1.0, 0.0)), 2.0, c::CONTROL);
    p.galley(rect.min, galley, c::TEXT_STRONG);
    let px = 1.0 / lay.m.ppp;
    p.hline(rect.x_range(), lay.m.snap(rect.bottom() - 2.0 * px), Stroke::new(px.max(1.0), c::EMBER));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cells_follow_the_terminal_text_size() {
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        ctx.run_ui(egui::RawInput::default(), |_| {}).textures_delta.clear(); // no renderer here
        let (normal, large) = (Metrics::with_size(&ctx, 12.5), Metrics::with_size(&ctx, 18.0));
        assert_eq!(normal.cell.y, (12.5f32 * 1.52).round(), "19 points at the design size");
        assert!(large.cell.x > normal.cell.x && large.cell.y > normal.cell.y);
        let area = Vec2::new(800.0, 600.0);
        let (c0, r0) = normal.grid_for(area);
        let (c1, r1) = large.grid_for(area);
        assert!(c1 < c0 && r1 < r0, "a larger text size fits fewer cells, so the run is resized");
    }

    #[test]
    fn colors_resolve_through_palette_and_overrides() {
        let colors = Colors::default();
        assert_eq!(resolve(Color::Named(NamedColor::Background), &colors), rgb(palette::BACKGROUND));
        assert_eq!(resolve(Color::Named(NamedColor::Red), &colors), rgb(palette::ANSI[1]));
        assert_eq!(resolve(Color::Indexed(196), &colors), Color32::from_rgb(255, 0, 0));
        let mut over = Colors::default();
        over[1] = Some(alacritty_terminal::vte::ansi::Rgb { r: 1, g: 2, b: 3 });
        assert_eq!(resolve(Color::Named(NamedColor::Red), &over), Color32::from_rgb(1, 2, 3));
        let dimmed = resolve(Color::Named(NamedColor::DimRed), &colors);
        assert_ne!(dimmed, rgb(palette::ANSI[1]));
    }
}
