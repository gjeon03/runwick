//! Serializes a terminal into bytes that rebuild the same state in a fresh terminal: history
//! and screen (with wrap flags, wide characters and styles), both screens, saved cursors,
//! modes, colors, scroll region and the cursor with its pen. Used for `sessions.attach`.
//!
//! The output is plain terminal traffic, so any client that renders the live stream can
//! render a snapshot. A few states that only insert/delete-character can produce (a wide
//! character cut in half, a wrap flag moved off the last column) are reproduced as blanks.
//!
//! A snapshot never exceeds [`BUDGET`]: past it, hyperlinks are left out, then styles and
//! combining marks, then the oldest history lines (a program could otherwise make attach and
//! the saved screen file arbitrarily large, for example with a long link on every other cell).

use super::term::Listener;
use alacritty_terminal::grid::{Cursor, Dimensions, Grid};
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::term::cell::{Cell, Flags, Hyperlink};
use alacritty_terminal::term::{Term, TermMode};
use alacritty_terminal::vte::ansi::{CharsetIndex, Color, CursorShape, CursorStyle, StandardCharset};
use runwick_core::term_guard::{MAX_URI, MAX_ZERO_WIDTH};
use std::fmt::Write as _;

/// Most bytes of one snapshot.
pub const BUDGET: usize = 8 * 1024 * 1024;

/// How much of each cell a snapshot reproduces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Detail {
    Full,
    /// Without hyperlinks.
    NoLinks,
    /// Characters only: no styles, links or combining marks.
    Plain,
}

/// Bytes that rebuild `term` in a fresh terminal of the same size. `region` is the scroll
/// region (tracked outside alacritty) and `history` the most lines of scrollback to include.
/// Within [`BUDGET`], with less detail or history when the full state does not fit.
pub(super) fn write(term: &mut Term<Listener>, region: (i32, i32), history: usize) -> Vec<u8> {
    // (The active grid may be the alternate screen, which has no history of its own.)
    let mut history = history.min(super::term::SCROLLBACK);
    let mut detail = Detail::Full;
    loop {
        // The last resort (the plain screen alone) is written whatever its size.
        let last = detail == Detail::Plain && history == 0;
        if let Some(out) = write_within(term, region, history, detail, if last { usize::MAX } else { BUDGET }) {
            return out;
        }
        match detail {
            Detail::Full => detail = Detail::NoLinks,
            Detail::NoLinks => detail = Detail::Plain,
            Detail::Plain => history /= 2,
        }
    }
}

fn write_within(
    term: &mut Term<Listener>,
    region: (i32, i32),
    history: usize,
    detail: Detail,
    budget: usize,
) -> Option<Vec<u8>> {
    let mut out = String::with_capacity(64 * 1024);
    let limit = Limit { detail, budget };
    out.push_str("\x1bc");
    if term.mode().contains(TermMode::ALT_SCREEN) {
        // The primary screen is only reachable as the active grid. Swapping back and forth
        // clears the alternate grid, so keep a copy and put it back afterwards.
        let alt_grid = term.grid().clone();
        term.swap_alt();
        // The primary DECSC state needs no work: entering the alternate screen below sets
        // it to the primary cursor, exactly as it happened originally.
        let fits = write_primary(&mut out, term.grid(), history, limit);
        if fits {
            let primary = term.grid();
            write_cursor(&mut out, primary, 0, limit);
            write_charsets(&mut out, &primary.cursor);
            write_sgr(&mut out, &primary.cursor.template);
        }
        term.swap_alt();
        *term.grid_mut() = alt_grid;
        if !fits {
            return None;
        }
        // Entering copied the primary pen and character sets and cleared the screen with
        // that pen's background: reset both and clear again.
        out.push_str("\x1b[?1049h\x1b[0m\x1b(B\x1b)B\x1b*B\x1b+B\x1b[2J");
        write_saved_cursor(&mut out, &term.grid().saved_cursor);
        if !write_alternate(&mut out, term.grid(), limit) {
            return None;
        }
    } else {
        // DECSC state of the alternate screen survives leaving it; restore it too.
        if let Some(alt_saved) = inactive_saved_cursor(term) {
            out.push_str("\x1b[?1049h");
            write_saved_cursor(&mut out, &alt_saved);
            out.push_str("\x1b[?1049l");
        }
        write_saved_cursor(&mut out, &term.grid().saved_cursor);
        if !write_primary(&mut out, term.grid(), history, limit) {
            return None;
        }
    }
    write_state(&mut out, term, region, limit);
    Some(out.into_bytes())
}

/// What a snapshot pass may write.
#[derive(Debug, Clone, Copy)]
struct Limit {
    detail: Detail,
    budget: usize,
}

impl Limit {
    /// A cell's hyperlink as reproduced (none past the detail level or for overlong URIs).
    fn link(self, cell: &Cell) -> Option<Hyperlink> {
        (self.detail == Detail::Full).then(|| cell.hyperlink()).flatten().filter(|l| l.uri().len() <= MAX_URI)
    }

    fn zerowidth(self, cell: &Cell) -> impl Iterator<Item = &char> {
        let marks = if self.detail == Detail::Plain { None } else { cell.zerowidth() };
        marks.into_iter().flatten().take(MAX_ZERO_WIDTH)
    }
}

/// The alternate screen's saved cursor while the primary screen is active, if not default.
fn inactive_saved_cursor(term: &mut Term<Listener>) -> Option<Cursor<Cell>> {
    let primary_saved = term.grid().saved_cursor.clone();
    term.swap_alt();
    let alt_saved = term.grid().saved_cursor.clone();
    term.swap_alt();
    term.grid_mut().saved_cursor = primary_saved;
    (!is_default_cursor(&alt_saved)).then_some(alt_saved)
}

const STYLE_FLAGS: Flags = Flags::INVERSE
    .union(Flags::BOLD)
    .union(Flags::ITALIC)
    .union(Flags::ALL_UNDERLINES)
    .union(Flags::DIM)
    .union(Flags::HIDDEN)
    .union(Flags::STRIKEOUT);

/// Pen state while writing cells, to emit SGR/OSC 8 only when something changes.
#[derive(Default)]
struct Pen {
    sgr: Option<(Color, Color, Flags, Option<Color>)>,
    link: Option<Hyperlink>,
}

impl Pen {
    fn apply(&mut self, out: &mut String, cell: &Cell, limit: Limit) {
        let key = (cell.fg, cell.bg, cell.flags & STYLE_FLAGS, cell.underline_color());
        if limit.detail != Detail::Plain && self.sgr != Some(key) {
            write_sgr(out, cell);
            self.sgr = Some(key);
        }
        let link = limit.link(cell);
        if self.link != link {
            match &link {
                Some(l) => {
                    let _ = write!(out, "\x1b]8;id={};{}\x1b\\", l.id(), l.uri());
                }
                None => out.push_str("\x1b]8;;\x1b\\"),
            }
            self.link = link;
        }
    }
}

fn is_blank(cell: &Cell) -> bool {
    *cell == Cell::default()
}

/// History and screen of the primary grid, written as printed text so the receiving terminal
/// scrolls the same lines into its own history and keeps line-wrap flags.
/// False when it does not fit in the budget.
fn write_primary(out: &mut String, grid: &Grid<Cell>, history: usize, limit: Limit) -> bool {
    out.push_str("\x1b[H");
    write_lines(out, grid, -(grid.history_size().min(history) as i32), limit)
}

/// The alternate screen has no history; it is printed the same way from its top-left corner.
fn write_alternate(out: &mut String, grid: &Grid<Cell>, limit: Limit) -> bool {
    out.push_str("\x1b[H");
    write_lines(out, grid, 0, limit)
}

fn write_lines(out: &mut String, grid: &Grid<Cell>, top: i32, limit: Limit) -> bool {
    let bottom = grid.screen_lines() as i32;
    let mut pen = Pen::default();
    let mut prev_wrapped = false;
    for l in top..bottom {
        let row = &grid[Line(l)];
        let wrapped = row[grid.last_column()].flags.contains(Flags::WRAPLINE);
        let end = if wrapped {
            grid.columns()
        } else {
            (0..grid.columns()).rev().find(|c| !is_blank(&row[Column(*c)])).map_or(0, |c| c + 1)
        };
        // A line that scrolls in at the bottom is cleared with the pen's background (BCE).
        // After a CRLF the pen is already reset; after a wrap it is the first cell's style.
        let mut scrolled_bg = false;
        if prev_wrapped {
            if end == 0 {
                // Keep the previous line's wrap flag: one blank cell wraps into this line.
                reset_pen(out, &mut pen);
                out.push(' ');
            } else {
                scrolled_bg = row[Column(0)].bg != Cell::default().bg;
            }
        }
        let wraps_into_wide = l + 1 < bottom && leading_spacer_recreatable(grid, Line(l), limit);
        write_cells(out, grid, Line(l), end, wraps_into_wide, &mut pen, limit);
        if out.len() > limit.budget {
            return false;
        }
        if scrolled_bg && end < grid.columns() {
            reset_pen(out, &mut pen);
            out.push_str("\x1b[K");
        }
        prev_wrapped = wrapped;
        if l + 1 < bottom && !wrapped {
            if pen.sgr.is_some_and(|k| k.1 != Cell::default().bg) {
                reset_pen(out, &mut pen);
            }
            out.push_str("\r\n");
        }
    }
    reset_pen(out, &mut pen);
    true
}

fn reset_pen(out: &mut String, pen: &mut Pen) {
    if pen.link.is_some() {
        out.push_str("\x1b]8;;\x1b\\");
        pen.link = None;
    }
    if pen.sgr.is_some() {
        out.push_str("\x1b[0m");
        pen.sgr = None;
    }
}

fn write_cells(
    out: &mut String,
    grid: &Grid<Cell>,
    line: Line,
    end: usize,
    wraps_into_wide: bool,
    pen: &mut Pen,
    limit: Limit,
) {
    let row = &grid[line];
    let mut col = 0;
    while col < end {
        let cell = &row[Column(col)];
        // Spacers are recreated by the receiving terminal when it prints the wide character.
        // Halves of a wide character split apart by insert/delete-character cannot be printed
        // as such; they are kept as blanks so every other cell stays in its column.
        let split = !is_whole_wide(row, col, grid.columns());
        let leading = cell.flags.contains(Flags::LEADING_WIDE_CHAR_SPACER);
        if !split && (cell.flags.contains(Flags::WIDE_CHAR_SPACER) || (leading && wraps_into_wide)) {
            col += 1;
            continue;
        }
        pen.apply(out, cell, limit);
        if split {
            out.push(' ');
        } else if cell.c == '\t' && col + 1 < grid.columns() {
            // A tab only marks a blank cell and moves to the next tab stop: paint the cell's
            // style with a space, tab from it, then come back. (In the last column the mark is
            // dropped: moving back would cancel the pending wrap.)
            let _ = write!(out, " \x1b[{}G\t\x1b[{}G", col + 1, col + 2);
        } else {
            out.push(printable(cell.c));
        }
        out.extend(limit.zerowidth(cell));
        col += 1;
    }
}

/// Whether printing the next line's first (wide) character recreates this line's leading
/// spacer exactly: the line wraps, the next line starts with a whole wide character, and the
/// spacer has that character's style (the terminal writes it with the same pen).
fn leading_spacer_recreatable(grid: &Grid<Cell>, line: Line, limit: Limit) -> bool {
    let below = Line(line.0 + 1);
    let spacer = &grid[line][grid.last_column()];
    let next = &grid[below][Column(0)];
    spacer.flags.contains(Flags::WRAPLINE)
        && next.flags.contains(Flags::WIDE_CHAR)
        && is_whole_wide(&grid[below], 0, grid.columns())
        // Styles count only where they are written.
        && (limit.detail == Detail::Plain
            || (spacer.fg == next.fg
                && spacer.bg == next.bg
                && spacer.flags & STYLE_FLAGS == next.flags & STYLE_FLAGS
                && spacer.underline_color() == next.underline_color()))
        && limit.link(spacer) == limit.link(next)
}

/// False for a wide character without its spacer, or a spacer without its wide character.
fn is_whole_wide(row: &alacritty_terminal::grid::Row<Cell>, col: usize, cols: usize) -> bool {
    let flags = row[Column(col)].flags;
    if flags.contains(Flags::WIDE_CHAR) {
        col + 1 < cols && row[Column(col + 1)].flags.contains(Flags::WIDE_CHAR_SPACER)
    } else if flags.contains(Flags::WIDE_CHAR_SPACER) {
        col > 0 && row[Column(col - 1)].flags.contains(Flags::WIDE_CHAR)
    } else {
        true
    }
}

/// Characters that would act as controls when printed are shown as spaces.
fn printable(c: char) -> char {
    if (c as u32) < 0x20 || c == '\x7f' || ('\u{80}'..='\u{9f}').contains(&c) { ' ' } else { c }
}

/// Move to the grid's cursor (`top` = first line of the origin-mode region). A pending wrap is
/// recreated by re-printing the last cell, so the next character wraps exactly as before.
/// (Re-printing drops a wrap flag on that cell, which only exists if the cursor was moved
/// onto an already wrapped line without clearing the pending wrap.)
fn write_cursor(out: &mut String, grid: &Grid<Cell>, top: i32, limit: Limit) {
    write_cursor_at(out, grid, &grid.cursor, top, limit);
}

fn write_cursor_at(out: &mut String, grid: &Grid<Cell>, cursor: &Cursor<Cell>, top: i32, limit: Limit) {
    let line = cursor.point.line.0 - top;
    if cursor.input_needs_wrap {
        let row = &grid[cursor.point.line];
        let mut col = cursor.point.column.0;
        let split = !is_whole_wide(row, col, grid.columns());
        if !split && row[Column(col)].flags.contains(Flags::WIDE_CHAR_SPACER) {
            col -= 1;
        }
        let cell = &row[Column(col)];
        let _ = write!(out, "\x1b[{};{}H", line + 1, col + 1);
        if limit.detail != Detail::Plain {
            write_sgr(out, cell);
        }
        let link = limit.link(cell);
        if let Some(l) = &link {
            let _ = write!(out, "\x1b]8;id={};{}\x1b\\", l.id(), l.uri());
        }
        // Same as the content pass: half of a split wide character is a blank.
        out.push(if split { ' ' } else { printable(cell.c) });
        out.extend(limit.zerowidth(cell));
        if link.is_some() {
            out.push_str("\x1b]8;;\x1b\\");
        }
    } else {
        let _ = write!(out, "\x1b[{};{}H", line + 1, cursor.point.column.0 + 1);
    }
}

/// Full SGR for a cell's style, starting from a reset.
fn write_sgr(out: &mut String, cell: &Cell) {
    out.push_str("\x1b[0");
    let f = cell.flags;
    for (flag, code) in [
        (Flags::BOLD, "1"),
        (Flags::DIM, "2"),
        (Flags::ITALIC, "3"),
        (Flags::INVERSE, "7"),
        (Flags::HIDDEN, "8"),
        (Flags::STRIKEOUT, "9"),
    ] {
        if f.contains(flag) {
            out.push(';');
            out.push_str(code);
        }
    }
    let underline = if f.contains(Flags::DOUBLE_UNDERLINE) {
        Some("4:2")
    } else if f.contains(Flags::UNDERCURL) {
        Some("4:3")
    } else if f.contains(Flags::DOTTED_UNDERLINE) {
        Some("4:4")
    } else if f.contains(Flags::DASHED_UNDERLINE) {
        Some("4:5")
    } else if f.contains(Flags::UNDERLINE) {
        Some("4")
    } else {
        None
    };
    if let Some(u) = underline {
        out.push(';');
        out.push_str(u);
    }
    write_color(out, cell.fg, 30, 90, 38);
    write_color(out, cell.bg, 40, 100, 48);
    if let Some(ul) = cell.underline_color() {
        write_color(out, ul, 0, 0, 58);
    }
    out.push('m');
}

fn write_color(out: &mut String, color: Color, base: u8, bright_base: u8, extended: u8) {
    match color {
        Color::Named(n) => {
            let i = n as usize;
            if i < 8 && base != 0 {
                let _ = write!(out, ";{}", base as usize + i);
            } else if i < 16 && base != 0 {
                let _ = write!(out, ";{}", bright_base as usize + i - 8);
            } else if i < 16 {
                let _ = write!(out, ";{extended};5;{i}");
            }
            // Foreground/Background are the defaults after the leading reset.
        }
        Color::Indexed(i) => {
            let _ = write!(out, ";{extended};5;{i}");
        }
        Color::Spec(c) => {
            let _ = write!(out, ";{extended};2;{};{};{}", c.r, c.g, c.b);
        }
    }
}

fn is_default_cursor(c: &Cursor<Cell>) -> bool {
    c.point == Point::default()
        && !c.input_needs_wrap
        && c.template == Cell::default()
        && c.charsets == Default::default()
}

/// Set a DECSC state (position, pending wrap, pen, character sets) while the screen is still
/// blank, then reset the pen and character sets. A pending wrap (only possible in the last
/// column) is recreated by printing a blank there; the real content is printed afterwards.
fn write_saved_cursor(out: &mut String, saved: &Cursor<Cell>) {
    if is_default_cursor(saved) {
        return;
    }
    let _ = write!(out, "\x1b[{};{}H", saved.point.line.0 + 1, saved.point.column.0 + 1);
    if saved.input_needs_wrap {
        out.push(' ');
    }
    write_sgr(out, &saved.template);
    write_charsets(out, saved);
    out.push_str("\x1b7\x1b[0m\x1b(B\x1b)B\x1b*B\x1b+B");
}

fn write_charsets(out: &mut String, charsets: &Cursor<Cell>) {
    for (index, designator) in
        [(CharsetIndex::G0, '('), (CharsetIndex::G1, ')'), (CharsetIndex::G2, '*'), (CharsetIndex::G3, '+')]
    {
        if charsets.charsets[index] == StandardCharset::SpecialCharacterAndLineDrawing {
            out.push('\x1b');
            out.push(designator);
            out.push('0');
        }
    }
}

/// Modes, colors, scroll region and the final cursor with its pen.
fn write_state(out: &mut String, term: &Term<Listener>, region: (i32, i32), limit: Limit) {
    let grid = term.grid();
    let mode = *term.mode();

    for (flag, code) in [
        (TermMode::APP_CURSOR, "?1"),
        (TermMode::MOUSE_REPORT_CLICK, "?1000"),
        (TermMode::MOUSE_DRAG, "?1002"),
        (TermMode::MOUSE_MOTION, "?1003"),
        (TermMode::FOCUS_IN_OUT, "?1004"),
        (TermMode::UTF8_MOUSE, "?1005"),
        (TermMode::SGR_MOUSE, "?1006"),
        (TermMode::BRACKETED_PASTE, "?2004"),
        (TermMode::LINE_FEED_NEW_LINE, "20"),
    ] {
        if mode.contains(flag) {
            let _ = write!(out, "\x1b[{code}h");
        }
    }
    for (flag, code) in [(TermMode::ALTERNATE_SCROLL, "?1007"), (TermMode::URGENCY_HINTS, "?1042")] {
        if !mode.contains(flag) {
            let _ = write!(out, "\x1b[{code}l");
        }
    }
    if mode.contains(TermMode::APP_KEYPAD) {
        out.push_str("\x1b=");
    }
    let style = term.cursor_style();
    if style != CursorStyle::default() {
        let code = match (style.shape, style.blinking) {
            (CursorShape::Underline, true) => 3,
            (CursorShape::Underline, false) => 4,
            (CursorShape::Beam, true) => 5,
            (CursorShape::Beam, false) => 6,
            (_, true) => 1,
            (_, false) => 2,
        };
        let _ = write!(out, "\x1b[{code} q");
    }
    let colors = term.colors();
    for i in 0..259usize {
        if let Some(c) = colors[i] {
            let rgb = format!("rgb:{:02x}/{:02x}/{:02x}", c.r, c.g, c.b);
            match i {
                0..=255 => {
                    let _ = write!(out, "\x1b]4;{i};{rgb}\x1b\\");
                }
                256 => {
                    let _ = write!(out, "\x1b]10;{rgb}\x1b\\");
                }
                257 => {
                    let _ = write!(out, "\x1b]11;{rgb}\x1b\\");
                }
                _ => {
                    let _ = write!(out, "\x1b]12;{rgb}\x1b\\");
                }
            }
        }
    }

    let rows = grid.screen_lines() as i32;
    if region != (0, rows) {
        let _ = write!(out, "\x1b[{};{}r", region.0 + 1, region.1);
    }
    let cursor = &grid.cursor;
    let origin = mode.contains(TermMode::ORIGIN);
    if origin {
        out.push_str("\x1b[?6h");
    }
    // With origin mode on, a cursor left outside the region (for example the primary cursor
    // after an alternate-screen program changed the region) cannot be addressed; it lands on
    // the nearest line of the region.
    write_cursor(out, grid, if origin { region.0 } else { 0 }, limit);
    if mode.contains(TermMode::INSERT) {
        out.push_str("\x1b[4h");
    }
    if !mode.contains(TermMode::LINE_WRAP) {
        out.push_str("\x1b[?7l");
    }
    if !mode.contains(TermMode::SHOW_CURSOR) {
        out.push_str("\x1b[?25l");
    }
    write_charsets(out, cursor);
    write_sgr(out, &cursor.template);
    if let Some(l) = limit.link(&cursor.template) {
        let _ = write!(out, "\x1b]8;id={};{}\x1b\\", l.id(), l.uri());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sessions::term::{HostTerm, Size};

    const FULL: Limit = Limit { detail: Detail::Full, budget: usize::MAX };

    fn term(cols: u16, rows: u16) -> HostTerm {
        HostTerm::new(Size { cols, rows })
    }

    /// A cell as the snapshot is able to reproduce it (see the notes on each adjustment).
    fn reproducible(g: &Grid<Cell>, l: i32, c: usize) -> Cell {
        let mut x = g[Line(l)][Column(c)].clone();
        if x.zerowidth().is_some_and(|z| z.is_empty()) && x.underline_color().is_none() && x.hyperlink().is_none() {
            x.extra = None; // an emptied extra is the same as none
        }
        if x.flags.contains(Flags::WIDE_CHAR_SPACER) || (x.c == '\t' && c + 1 == g.columns()) {
            x.c = ' '; // a tab mark on a spacer or in the last column is not reproduced
        }
        let rows = g.screen_lines() as i32;
        if !is_whole_wide(&g[Line(l)], c, g.columns()) {
            // Half of a wide character split by insert/delete-character: kept as a blank.
            x.c = ' ';
            x.flags.remove(Flags::WIDE_CHAR | Flags::WIDE_CHAR_SPACER);
        }
        if !(l + 1 < rows && leading_spacer_recreatable(g, Line(l), FULL)) {
            // A leading spacer no longer followed by its wrapped wide character (shifted away,
            // unwrapped or restyled by later edits): kept as a styled blank.
            x.flags.remove(Flags::LEADING_WIDE_CHAR_SPACER);
        }
        if c + 1 < g.columns() || l + 1 == rows {
            // A stale wrap flag that insert/delete-character moved away from the last column
            // (meaningless there), or one that line insertion moved onto the bottom row
            // (recreating it would scroll the screen).
            x.flags.remove(Flags::WRAPLINE);
        }
        x
    }

    fn compare_grid(a: &Grid<Cell>, b: &Grid<Cell>, what: &str) {
        assert_eq!(a.history_size(), b.history_size(), "{what}: history size");
        assert_eq!(a.columns(), b.columns());
        for l in -(a.history_size() as i32)..a.screen_lines() as i32 {
            for c in 0..a.columns() {
                assert_eq!(reproducible(a, l, c), reproducible(b, l, c), "{what}: line {l} column {c}");
            }
        }
        assert_eq!(a.cursor.point, b.cursor.point, "{what}: cursor");
        assert_eq!(a.cursor.input_needs_wrap, b.cursor.input_needs_wrap, "{what}: pending wrap");
        assert_eq!(a.cursor.template, b.cursor.template, "{what}: pen");
        assert_eq!(a.cursor.charsets, b.cursor.charsets, "{what}: charsets");
    }

    /// Compare everything a client could observe, including the inactive screen.
    fn assert_same(a: &mut HostTerm, b: &mut HostTerm) {
        assert_eq!(a.mode(), b.mode(), "modes");
        assert_eq!(a.term().cursor_style(), b.term().cursor_style());
        for i in 0..259 {
            assert_eq!(a.term().colors()[i], b.term().colors()[i], "color {i}");
        }
        compare_grid(a.term().grid(), b.term().grid(), "active");
        assert_eq!(a.term().grid().saved_cursor.point, b.term().grid().saved_cursor.point, "saved cursor");
        assert_eq!(a.term().grid().saved_cursor.template, b.term().grid().saved_cursor.template);
        if a.mode().contains(TermMode::ALT_SCREEN) {
            let (alt_a, alt_b) = (a.term().grid().clone(), b.term().grid().clone());
            a.term_mut().swap_alt();
            b.term_mut().swap_alt();
            compare_grid(a.term().grid(), b.term().grid(), "primary");
            a.term_mut().swap_alt();
            b.term_mut().swap_alt();
            *a.term_mut().grid_mut() = alt_a;
            *b.term_mut().grid_mut() = alt_b;
        }
    }

    fn grid_is_lossy(g: &Grid<Cell>) -> bool {
        let rows = g.screen_lines() as i32;
        (-(g.history_size() as i32)..rows).any(|l| {
            let row = &g[Line(l)];
            (0..g.columns()).any(|c| {
                let f = row[Column(c)].flags;
                let last = c + 1 == g.columns();
                !is_whole_wide(row, c, g.columns())
                    || (f.contains(Flags::WRAPLINE) && (!last || l + 1 == rows))
                    || (f.contains(Flags::LEADING_WIDE_CHAR_SPACER)
                        && !(l + 1 < rows
                            && g[Line(l + 1)][Column(0)].flags.contains(Flags::WIDE_CHAR)
                            && is_whole_wide(&g[Line(l + 1)], 0, g.columns())))
            })
        })
    }

    /// A cursor with a pending wrap on a cell that already carries a wrap flag, on either screen.
    fn wrap_flag_under_pending_wrap(t: &mut HostTerm) -> bool {
        let check = |g: &Grid<Cell>| {
            g.cursor.input_needs_wrap && g[g.cursor.point.line][g.cursor.point.column].flags.contains(Flags::WRAPLINE)
        };
        if check(t.term().grid()) {
            return true;
        }
        if !t.mode().contains(TermMode::ALT_SCREEN) {
            return false;
        }
        let alt = t.term().grid().clone();
        t.term_mut().swap_alt();
        let found = check(t.term().grid());
        t.term_mut().swap_alt();
        *t.term_mut().grid_mut() = alt;
        found
    }

    fn has_lossy_state(t: &mut HostTerm) -> bool {
        if grid_is_lossy(t.term().grid()) {
            return true;
        }
        if !t.mode().contains(TermMode::ALT_SCREEN) {
            return false;
        }
        let alt = t.term().grid().clone();
        t.term_mut().swap_alt();
        let lossy = grid_is_lossy(t.term().grid());
        t.term_mut().swap_alt();
        *t.term_mut().grid_mut() = alt;
        lossy
    }

    fn roundtrip(a: &mut HostTerm) -> HostTerm {
        let snap = a.snapshot();
        let mut b = term(a.size().cols, a.size().rows);
        // Feed in small pieces: clients receive the snapshot in chunks.
        for chunk in snap.chunks(7) {
            b.advance(chunk);
        }
        assert_same(a, &mut b);
        b
    }

    const RICH: &str = concat!(
        "plain \x1b[1mbold\x1b[22m \x1b[3mit\x1b[23m \x1b[4munder\x1b[24m \x1b[4:3mcurl\x1b[0m\r\n",
        "\x1b[31mred\x1b[92mbright\x1b[38;5;208m256\x1b[38;2;10;200;30mtrue\x1b[48;2;40;0;60mbg\x1b[0m\r\n",
        "\x1b[2mdim\x1b[0m \x1b[7minv\x1b[0m \x1b[8mhid\x1b[0m \x1b[9mstrike\x1b[0m \x1b[4;58;5;3mulc\x1b[0m\r\n",
        "한글 테스트 가나다라마바사아자차카타파하\r\n",
        "e\u{301}\u{301} combining, emoji 🎉 ok\r\n",
        "tab\there\tand\tmore\r\n",
        "\x1b]8;id=x;https://example.com\x1b\\link\x1b]8;;\x1b\\ after\r\n",
        "\x1b[44m\x1b[K\x1b[0mbg-erased line\r\n",
    );

    #[test]
    fn snapshot_reproduces_rich_primary_screen_and_history() {
        let mut a = term(24, 6);
        for i in 0..40 {
            a.advance(format!("history line {i} with some words\r\n").as_bytes());
        }
        a.advance(RICH.as_bytes());
        // Wide char that does not fit at the end of a line (leading spacer) and a pending wrap.
        a.advance("abcdefghijklmnopqrstuvw가".as_bytes());
        a.advance(b"\r\n123456789012345678901234");
        assert!(a.term().grid().cursor.input_needs_wrap);
        let mut b = roundtrip(&mut a);
        // Continue both with the same output: future behavior must match too.
        let more = "Z next 한글\r\nend\x1b[3;5Hx".as_bytes();
        a.advance(more);
        b.advance(more);
        assert_same(&mut a, &mut b);
    }

    #[test]
    fn snapshot_reproduces_modes_cursor_and_regions() {
        let mut a = term(30, 10);
        a.advance(b"\x1b[?1h\x1b=\x1b[?2004h\x1b[?1002h\x1b[?1006h\x1b[?1004h\x1b[5 q\x1b[?1007l");
        a.advance(b"\x1b]11;rgb:20/30/40\x1b\\\x1b]4;1;rgb:ff/00/00\x1b\\");
        a.advance(b"\x1b(0lqk\x1b(B text \x1b[2;3H\x1b[1;35m\x1b7\x1b[0m");
        a.advance(b"\x1b[3;8r\x1b[8;1Hline a\r\nline b\r\nline c\x1b[5;4H\x1b[33;1m\x1b(0");
        let mut b = roundtrip(&mut a);
        // Scrolling inside the region and restoring the saved cursor behave the same.
        let more = b"\x1b[8;1H\r\nscrolled\r\nagain\x1b8written q";
        a.advance(more);
        b.advance(more);
        assert_same(&mut a, &mut b);
    }

    #[test]
    fn snapshot_reproduces_alternate_screen_and_primary_below_it() {
        let mut a = term(20, 5);
        for i in 0..12 {
            a.advance(format!("shell {i}\r\n").as_bytes());
        }
        a.advance(b"$ vim\x1b[?1049h\x1b[?25l\x1b[H\x1b[2J\x1b[1;1H~\x1b[2;1H~ \x1b[7mstatus\x1b[0m\x1b[4;6Hcursor");
        let mut b = roundtrip(&mut a);
        let more = b"\x1b[?1049l\x1b[?25hback\r\n";
        a.advance(more);
        b.advance(more);
        assert_same(&mut a, &mut b);
    }

    /// Deterministic pseudo-random terminal traffic.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn below(&mut self, n: u64) -> u64 {
            self.next() % n
        }
    }

    /// `edits` adds insert mode and character/line insertion and deletion, which can cut
    /// through wide characters and wrapped lines.
    fn random_traffic(rng: &mut Rng, ops: usize, cols: u64, rows: u64, edits: bool) -> Vec<u8> {
        const WORDS: &[&str] =
            &["alpha", "beta ", "한글", "가나다라마", "🎉", "x", "  ", "e\u{301}", "wide世界", "─│╭╮"];
        let mut out = String::new();
        for _ in 0..ops {
            match rng.below(24) {
                0..=7 => out.push_str(WORDS[rng.below(WORDS.len() as u64) as usize]),
                8 => out.push_str("\r\n"),
                9 => out.push_str(["\n", "\r", "\t", "\x08"][rng.below(4) as usize]),
                10 => {
                    let c = match rng.below(4) {
                        0 => format!("3{}", rng.below(8)),
                        1 => format!("38;5;{}", rng.below(256)),
                        2 => format!("48;2;{};{};{}", rng.below(256), rng.below(256), rng.below(256)),
                        _ => format!("{}", [0, 1, 2, 3, 4, 7, 9, 22, 24, 27][rng.below(10) as usize]),
                    };
                    out.push_str(&format!("\x1b[{c}m"));
                }
                11 => out.push_str(&format!("\x1b[{};{}H", rng.below(rows + 2), rng.below(cols + 2))),
                12 => out.push_str(&format!("\x1b[{}K", rng.below(3))),
                13 => out.push_str(&format!("\x1b[{}J", rng.below(3))),
                14 => out.push_str(&format!("\x1b[{};{}r", rng.below(rows) + 1, rng.below(rows) + 1)),
                15 => out.push_str("\x1b[r"),
                16 => out.push_str(["\x1bM", "\x1bD", "\x1bE"][rng.below(3) as usize]),
                17 if edits => out.push_str(&format!(
                    "\x1b[{}{}",
                    rng.below(4) + 1,
                    ["@", "P", "L", "M", "X", "S", "T"][rng.below(7) as usize]
                )),
                17 => out.push_str(&format!("\x1b[{}{}", rng.below(4) + 1, ["S", "T"][rng.below(2) as usize])),
                18 => out.push_str(["\x1b[?1049h", "\x1b[?1049l"][rng.below(2) as usize]),
                19 => out.push_str(["\x1b7", "\x1b8"][rng.below(2) as usize]),
                20 if edits => out.push_str(["\x1b[4h", "\x1b[4l", "\x1b[?7l", "\x1b[?7h"][rng.below(4) as usize]),
                20 => out.push_str(["\x1b[4l", "\x1b[?7l", "\x1b[?7h"][rng.below(3) as usize]),
                21 => out.push_str(["\x1b(0", "\x1b(B", "\x1b[?6h", "\x1b[?6l"][rng.below(4) as usize]),
                22 => out.push_str(["\x1b[?25l", "\x1b[?25h", "\x1b[?2004h", "\x1b[4 q"][rng.below(4) as usize]),
                _ => out.push_str(&"y".repeat(rng.below(cols * 2) as usize)),
            }
        }
        out.into_bytes()
    }

    #[test]
    fn snapshot_roundtrips_random_traffic() {
        for seed in 1..=2000u64 {
            let mut rng = Rng(seed.wrapping_mul(0x9E3779B97F4A7C15) | 1);
            let (cols, rows) = (rng.below(30) as u16 + 4, rng.below(10) as u16 + 2);
            let mut a = term(cols, rows);
            let traffic = random_traffic(&mut rng, 120, cols as u64, rows as u64, true);
            a.advance(&traffic);
            let (top, bottom) = a.region();
            let line = a.term().grid().cursor.point.line.0;
            if a.mode().contains(TermMode::ORIGIN) && (line < top || line >= bottom) {
                continue; // documented limitation in `write_state`
            }
            if wrap_flag_under_pending_wrap(&mut a) {
                continue; // documented limitation in `write_cursor`
            }
            let mut b = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| roundtrip(&mut a)))
                .unwrap_or_else(|_| panic!("seed {seed}: {:?}", String::from_utf8_lossy(&traffic)));
            let more = random_traffic(&mut rng, 40, cols as u64, rows as u64, false);
            if has_lossy_state(&mut a) {
                // States only reachable by insert/delete-character or line insertion cutting
                // through wide characters or wrapped lines are reproduced approximately, so
                // later edits next to them may legitimately diverge.
                continue;
            }
            a.advance(&more);
            b.advance(&more);
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| assert_same(&mut a, &mut b))).is_err() {
                panic!(
                    "seed {seed} (after continuing): {:?} then {:?}",
                    String::from_utf8_lossy(&traffic),
                    String::from_utf8_lossy(&more)
                );
            }
        }
    }

    /// One link fills each screen and a second one overwrites every other cell: every cell
    /// boundary changes the link, so a naive snapshot repeats a long URI per cell.
    fn alternating_links(uri_len: usize, screens: usize, cols: usize, rows: usize) -> Vec<u8> {
        let a = format!("https://a.example/{}", "A".repeat(uri_len));
        let b = format!("https://b.example/{}", "B".repeat(uri_len));
        let mut o = String::new();
        for _ in 0..screens {
            o.push_str(&format!("\x1b[H\x1b]8;;{a}\x1b\\"));
            for r in 0..rows {
                o.push_str(&format!("\x1b[{};1H{}", r + 1, "x".repeat(cols)));
            }
            o.push_str(&format!("\x1b]8;;{b}\x1b\\"));
            for r in 0..rows {
                o.push_str(&format!("\x1b[{};1H{}", r + 1, "y\x1b[C".repeat(cols / 2)));
            }
            o.push_str(&format!("\x1b]8;;\x1b\\\x1b[{rows};1H{}", "\n".repeat(rows)));
        }
        o.into_bytes()
    }

    #[test]
    fn hostile_hyperlinks_cannot_make_a_snapshot_unbounded() {
        let mut a = term(100, 30);
        a.advance(&alternating_links(2000, 6, 100, 30));
        let snap = a.snapshot();
        assert!(snap.len() <= BUDGET, "{} bytes", snap.len());
        // The text survives; only the links were left out.
        let mut b = term(100, 30);
        b.advance(&snap);
        assert_eq!(b.text(usize::MAX), a.text(usize::MAX));
        assert!(b.term().grid()[Line(0)][Column(0)].hyperlink().is_none());
        // URIs over the guard's limit never reach the terminal at all.
        let mut c = term(20, 2);
        c.advance(format!("\x1b]8;;https://x/{}\x1b\\link", "x".repeat(MAX_URI)).as_bytes());
        assert!(c.term().grid()[Line(0)][Column(0)].hyperlink().is_none());
    }

    #[test]
    fn ordinary_links_and_marks_survive_and_plain_fallback_keeps_text() {
        let mut a = term(30, 3);
        a.advance("\x1b]8;id=k;https://runwick.example/docs\x1b\\docs\x1b]8;;\x1b\\ e\u{301} \x1b[1;31mred".as_bytes());
        let b = roundtrip(&mut a);
        assert_eq!(
            b.term().grid()[Line(0)][Column(0)].hyperlink().map(|l| l.uri().to_string()).as_deref(),
            Some("https://runwick.example/docs")
        );
        // Forced to the plainest level, the characters are still all there.
        let plain = write_within(a.term_mut(), (0, 3), 0, Detail::Plain, usize::MAX).unwrap();
        let mut c = term(30, 3);
        c.advance(&plain);
        assert_eq!(c.screen_lines()[0], "docs e red");
    }

    #[test]
    fn snapshot_of_fresh_terminal_is_just_a_reset() {
        let mut a = term(10, 3);
        let snap = a.snapshot();
        assert!(snap.starts_with(b"\x1bc"));
        roundtrip(&mut a);
    }
}
