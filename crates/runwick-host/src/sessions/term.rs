//! The host's terminal model of one session: an alacritty `Term` fed with every byte the
//! process writes.
//!
//! - It is the only terminal that answers queries from the program (device attributes, cursor
//!   position, colors); clients render the same bytes but never reply.
//! - [`HostTerm::snapshot`] serializes it (see `snapshot.rs`) into a byte stream that starts
//!   with a full reset (RIS), so a client can always replace whatever it showed before.
//! - Output passes `runwick_core::term_guard` first (overlong OSC strings and hyperlinks, too
//!   many combining marks on a cell). Clients receive exactly what passed
//!   ([`Effects::forward`]), so their terminals and a snapshot always describe the same thing.

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::grid::{Dimensions, Grid};
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::vte;
use alacritty_terminal::vte::ansi::{NamedColor, Processor, Rgb, Timeout};
use runwick_core::api::sessions::palette;
use runwick_core::term_guard::{OutputGuard, Sink};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Lines of scrollback kept per session.
pub const SCROLLBACK: usize = 10_000;

/// Disables synchronized-update buffering: the host model must reflect every byte already
/// forwarded to clients, otherwise a snapshot could miss output that clients never see again.
#[derive(Default)]
pub struct NoSync;

impl Timeout for NoSync {
    fn set_timeout(&mut self, _: Duration) {}
    fn clear_timeout(&mut self) {}
    fn pending_timeout(&self) -> bool {
        false
    }
}

#[derive(Clone, Default)]
pub struct Listener(Arc<Mutex<Vec<Event>>>);

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        if let Ok(mut v) = self.0.lock() {
            v.push(event);
        }
    }
}

/// Terminal size in cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Size {
    pub cols: u16,
    pub rows: u16,
}

impl Size {
    pub fn clamped(cols: u16, rows: u16) -> Self {
        Self { cols: cols.clamp(2, 1000), rows: rows.clamp(1, 500) }
    }
}

impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.rows as usize
    }
    fn screen_lines(&self) -> usize {
        self.rows as usize
    }
    fn columns(&self) -> usize {
        self.cols as usize
    }
}

/// What processing a chunk of output asked the host to do.
#[derive(Debug, Default, PartialEq)]
pub struct Effects {
    /// Replies to terminal queries, to write back to the program.
    pub reply: Vec<u8>,
    /// `Some(None)` resets to the default title.
    pub title: Option<Option<String>>,
    pub bell: bool,
    /// What attached clients receive: the output that passed the guard, in whole characters.
    /// Bytes held for the next chunk (an unfinished character or sequence) come later.
    pub forward: Vec<u8>,
}

pub struct HostTerm {
    term: Term<Listener>,
    parser: Processor<NoSync>,
    region: RegionTracker,
    events: Listener,
    /// Trailing bytes of an incomplete UTF-8 character, held until the rest arrives.
    carry: Utf8Carry,
    guard: OutputGuard,
}

impl HostTerm {
    pub fn new(size: Size) -> Self {
        let events = Listener::default();
        let config = Config { scrolling_history: SCROLLBACK, ..Config::default() };
        Self {
            term: Term::new(config, &size, events.clone()),
            parser: Processor::new(),
            region: RegionTracker::new(size.rows),
            events,
            carry: Utf8Carry::default(),
            guard: OutputGuard::default(),
        }
    }

    pub fn size(&self) -> Size {
        Size { cols: self.term.columns() as u16, rows: self.term.screen_lines() as u16 }
    }

    pub fn resize(&mut self, size: Size) {
        if size != self.size() {
            self.term.resize(size);
            self.region.resize(size.rows);
        }
        self.drain_events();
    }

    pub fn mode(&self) -> TermMode {
        *self.term.mode()
    }

    /// Feed program output.
    pub fn advance(&mut self, bytes: &[u8]) -> Effects {
        let bytes = self.carry.complete(bytes);
        let mut fed =
            Fed { term: &mut self.term, parser: &mut self.parser, region: &mut self.region, forward: Vec::new() };
        self.guard.feed(&bytes, &mut fed);
        let mut fx = Effects { forward: fed.forward, ..Effects::default() };
        for ev in self.drain_events() {
            match ev {
                Event::PtyWrite(s) => fx.reply.extend_from_slice(s.as_bytes()),
                Event::ColorRequest(index, format) => fx.reply.extend_from_slice(format(self.color(index)).as_bytes()),
                Event::TextAreaSizeRequest(format) => {
                    // Pixel sizes are unknown to the host; report zero like terminals that
                    // cannot tell (the cell counts are always answered).
                    let size = self.size();
                    let ws = WindowSize { num_lines: size.rows, num_cols: size.cols, cell_width: 0, cell_height: 0 };
                    fx.reply.extend_from_slice(format(ws).as_bytes())
                }
                Event::Title(t) => fx.title = Some(Some(t)),
                Event::ResetTitle => fx.title = Some(None),
                Event::Bell => fx.bell = true,
                _ => {}
            }
        }
        fx
    }

    fn drain_events(&self) -> Vec<Event> {
        self.events.0.lock().map(|mut v| std::mem::take(&mut *v)).unwrap_or_default()
    }

    /// Color the renderer uses for a palette index (answers OSC 4/10/11/12 queries).
    fn color(&self, index: usize) -> Rgb {
        if let Some(c) = self.term.colors()[index.min(COLOR_COUNT - 1)] {
            return c;
        }
        let hex = match index {
            0..=255 => palette::indexed(index as u8),
            i if i == NamedColor::Background as usize => palette::BACKGROUND,
            i if i == NamedColor::Cursor as usize => palette::CURSOR,
            _ => palette::FOREGROUND,
        };
        let (r, g, b) = palette::rgb(hex);
        Rgb { r, g, b }
    }

    /// Screen row of the cursor (0 is the top of the visible screen).
    pub fn cursor_row(&self) -> usize {
        self.term.grid().cursor.point.line.0.max(0) as usize
    }

    /// Column of the cursor.
    pub fn cursor_col(&self) -> usize {
        self.term.grid().cursor.point.column.0
    }

    /// Visible rows as plain text (trailing spaces trimmed).
    pub fn screen_lines(&self) -> Vec<String> {
        let grid = self.term.grid();
        (0..grid.screen_lines() as i32).map(|l| line_text(grid, Line(l))).collect()
    }

    /// Plain text of up to `scrollback` history lines followed by the screen, at most
    /// [`TEXT_BUDGET`] bytes: the oldest lines are left out first.
    pub fn text(&self, scrollback: usize) -> String {
        let grid = self.term.grid();
        let history = grid.history_size().min(scrollback) as i32;
        let mut lines = Vec::new();
        let mut size = 0;
        for l in (-history..grid.screen_lines() as i32).rev() {
            let text = line_text(grid, Line(l));
            size += text.len() + 1;
            if size > TEXT_BUDGET {
                break;
            }
            lines.push(text);
        }
        lines.reverse();
        while lines.last().is_some_and(|l| l.is_empty()) {
            lines.pop();
        }
        lines.join("\n")
    }

    /// Bytes that rebuild this terminal's state in a fresh terminal of the same size.
    pub fn snapshot(&mut self) -> Vec<u8> {
        self.snapshot_with_history(usize::MAX)
    }

    /// Like [`Self::snapshot`] with at most `history` lines of scrollback.
    pub fn snapshot_with_history(&mut self, history: usize) -> Vec<u8> {
        // Bytes still held (an unfinished character or sequence) reach clients later as
        // live output, so the snapshot does not include them.
        let out = super::snapshot::write(&mut self.term, self.region.region(), history);
        self.drain_events();
        out
    }

    #[cfg(test)]
    pub(crate) fn region(&self) -> (i32, i32) {
        self.region.region()
    }

    #[cfg(test)]
    pub(crate) fn term(&self) -> &Term<Listener> {
        &self.term
    }

    #[cfg(test)]
    pub(crate) fn term_mut(&mut self) -> &mut Term<Listener> {
        &mut self.term
    }
}

const COLOR_COUNT: usize = alacritty_terminal::term::color::COUNT;

/// Most bytes of text a screen read returns (`sessions.screen_text`, pins, delegation).
pub const TEXT_BUDGET: usize = 4 * 1024 * 1024;

/// The terminal behind the output guard.
struct Fed<'a> {
    term: &'a mut Term<Listener>,
    parser: &'a mut Processor<NoSync>,
    region: &'a mut RegionTracker,
    forward: Vec<u8>,
}

impl Sink for Fed<'_> {
    fn feed(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        self.parser.advance(self.term, bytes);
        self.region.advance(bytes);
        self.forward.extend_from_slice(bytes);
    }

    fn zero_width_here(&self) -> usize {
        zero_width_at_cursor(self.term)
    }
}

/// Zero-width characters on the cell the next one joins: the one before the cursor (or under
/// it with a pending wrap), the wide character for its spacer. Mirrors `Term::input`.
pub fn zero_width_at_cursor<T>(term: &Term<T>) -> usize {
    let grid = term.grid();
    let cursor = &grid.cursor;
    let mut col = cursor.point.column;
    if !cursor.input_needs_wrap {
        col.0 = col.0.saturating_sub(1);
    }
    let row = &grid[cursor.point.line];
    if row[col].flags.contains(Flags::WIDE_CHAR_SPACER) {
        col.0 = col.0.saturating_sub(1);
    }
    row[col].zerowidth().map_or(0, |z| z.len())
}

fn line_text(grid: &Grid<Cell>, line: Line) -> String {
    let row = &grid[line];
    let mut s = String::with_capacity(grid.columns());
    for col in 0..grid.columns() {
        let cell = &row[Column(col)];
        if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
            continue;
        }
        s.push(if cell.c == '\t' { ' ' } else { cell.c });
        if let Some(zw) = cell.zerowidth() {
            s.extend(zw.iter());
        }
    }
    s.truncate(s.trim_end().len());
    s
}

/// Holds back an incomplete UTF-8 character at the end of a chunk and prepends it to the
/// next one. vte 0.15 can drop the character that follows a 2-byte character split across
/// two `advance` calls, so the parser is only ever given whole characters.
#[derive(Default)]
pub struct Utf8Carry {
    pending: Vec<u8>,
}

impl Utf8Carry {
    pub fn complete<'a>(&mut self, bytes: &'a [u8]) -> std::borrow::Cow<'a, [u8]> {
        let joined: std::borrow::Cow<'a, [u8]> = if self.pending.is_empty() {
            std::borrow::Cow::Borrowed(bytes)
        } else {
            let mut v = std::mem::take(&mut self.pending);
            v.extend_from_slice(bytes);
            std::borrow::Cow::Owned(v)
        };
        let keep = incomplete_utf8_tail(&joined);
        if keep == 0 {
            return joined;
        }
        let cut = joined.len() - keep;
        self.pending.extend_from_slice(&joined[cut..]);
        match joined {
            std::borrow::Cow::Borrowed(b) => std::borrow::Cow::Borrowed(&b[..cut]),
            std::borrow::Cow::Owned(mut v) => {
                v.truncate(cut);
                std::borrow::Cow::Owned(v)
            }
        }
    }
}

/// Number of bytes at the end of `b` that start a UTF-8 character but do not complete it.
pub fn incomplete_utf8_tail(b: &[u8]) -> usize {
    for back in 1..=3.min(b.len()) {
        let byte = b[b.len() - back];
        if byte & 0xC0 == 0x80 {
            continue; // continuation byte: keep looking for the lead byte
        }
        let need = match byte {
            0xC2..=0xDF => 2,
            0xE0..=0xEF => 3,
            0xF0..=0xF4 => 4,
            _ => return 0,
        };
        return if back < need { back } else { 0 };
    }
    0
}

/// Alacritty keeps the scroll region private, so the host follows the sequences that change
/// it (DECSTBM, RIS, DECCOLM) with a second parser over the same bytes.
#[derive(Default)]
struct RegionTracker {
    parser: vte::Parser,
    perform: RegionPerform,
}

#[derive(Default)]
struct RegionPerform {
    rows: i32,
    /// `(top, bottom_exclusive)`; `None` means the whole screen.
    region: Option<(i32, i32)>,
}

impl RegionTracker {
    fn new(rows: u16) -> Self {
        Self { parser: vte::Parser::new(), perform: RegionPerform { rows: rows as i32, region: None } }
    }

    fn advance(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.perform, bytes);
    }

    fn resize(&mut self, rows: u16) {
        self.perform.rows = rows as i32;
        self.perform.region = None;
    }

    fn region(&self) -> (i32, i32) {
        self.perform.region.unwrap_or((0, self.perform.rows))
    }
}

impl vte::Perform for RegionPerform {
    fn csi_dispatch(&mut self, params: &vte::Params, intermediates: &[u8], ignore: bool, action: char) {
        if ignore {
            return;
        }
        match (action, intermediates) {
            ('r', []) => {
                let mut it = params.iter();
                let top = it.next().map(|p| p[0]).filter(|p| *p != 0).unwrap_or(1) as i32;
                let bottom = it.next().map(|p| p[0] as i32).filter(|p| *p != 0).unwrap_or(self.rows);
                // Same validation as the terminal itself.
                if top < bottom {
                    let start = (top - 1).min(self.rows);
                    let end = bottom.min(self.rows);
                    self.region = if (start, end) == (0, self.rows) { None } else { Some((start, end)) };
                }
            }
            ('h' | 'l', [b'?']) if params.iter().any(|p| p[0] == 3) => self.region = None,
            _ => {}
        }
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], ignore: bool, byte: u8) {
        if !ignore && byte == b'c' && intermediates.is_empty() {
            self.region = None;
        }
    }
}

/// Tells whether output changes what is drawn (printable text or line feeds) as opposed to
/// pure control traffic such as cursor blinking or mode switches. Keeps state across chunks.
#[derive(Default)]
pub struct ContentScanner {
    state: ScanState,
}

#[derive(Default, Clone, Copy, PartialEq)]
enum ScanState {
    #[default]
    Ground,
    Esc,
    Csi,
    Str,
    StrEsc,
}

impl ContentScanner {
    /// True when `bytes` contain printable output.
    pub fn scan(&mut self, bytes: &[u8]) -> bool {
        let mut content = false;
        for &b in bytes {
            self.state = match self.state {
                ScanState::Ground => match b {
                    0x1b => ScanState::Esc,
                    b'\n' => {
                        content = true;
                        ScanState::Ground
                    }
                    0x20..=0x7e | 0x80..=0xff => {
                        content = true;
                        ScanState::Ground
                    }
                    _ => ScanState::Ground,
                },
                ScanState::Esc => match b {
                    b'[' => ScanState::Csi,
                    b']' | b'P' | b'X' | b'^' | b'_' => ScanState::Str,
                    0x20..=0x2f => ScanState::Esc,
                    _ => ScanState::Ground,
                },
                ScanState::Csi => match b {
                    0x40..=0x7e => ScanState::Ground,
                    0x1b => ScanState::Esc,
                    _ => ScanState::Csi,
                },
                ScanState::Str => match b {
                    0x07 => ScanState::Ground,
                    0x1b => ScanState::StrEsc,
                    _ => ScanState::Str,
                },
                ScanState::StrEsc => match b {
                    b'\\' => ScanState::Ground,
                    0x1b => ScanState::StrEsc,
                    _ => ScanState::Str,
                },
            };
        }
        content
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn term(cols: u16, rows: u16) -> HostTerm {
        HostTerm::new(Size { cols, rows })
    }

    #[test]
    fn host_answers_queries_with_the_renderer_palette() {
        let mut a = term(10, 3);
        let fx = a.advance(b"ab\x1b[6n\x1b]11;?\x07\x1b[c");
        let reply = String::from_utf8(fx.reply).unwrap();
        assert!(reply.starts_with("\x1b[1;3R"), "{reply:?}");
        assert!(reply.contains("\x1b]11;rgb:0b0b/0c0c/0e0e\x07"), "{reply:?}");
        assert!(reply.ends_with("\x1b[?6c"), "{reply:?}");
        let fx = a.advance(b"\x1b]2;my title\x07\x07");
        assert_eq!(fx.title, Some(Some("my title".into())));
        assert!(fx.bell);
    }

    #[test]
    fn screen_text_trims_and_skips_wide_spacers() {
        let mut a = term(12, 3);
        a.advance("한글 ok   \r\nnext".as_bytes());
        assert_eq!(a.screen_lines(), vec!["한글 ok".to_string(), "next".into(), String::new()]);
        assert_eq!(a.text(100), "한글 ok\nnext");
        assert_eq!(a.cursor_row(), 1);
        a.advance(b"\x1b[3;1H");
        assert_eq!(a.cursor_row(), 2);
    }

    #[test]
    fn utf8_carry_keeps_split_characters_whole() {
        assert_eq!(incomplete_utf8_tail(b"ab"), 0);
        assert_eq!(incomplete_utf8_tail(&[b'a', 0xED]), 1);
        assert_eq!(incomplete_utf8_tail(&[b'a', 0xED, 0x95]), 2);
        assert_eq!(incomplete_utf8_tail(&[0xED, 0x95, 0x9C]), 0);
        assert_eq!(incomplete_utf8_tail(&[0xF0, 0x9F, 0x8E]), 3);
        // Feeding "e\u{301}e\u{301}" split after the first byte of U+0301 keeps both letters.
        let text = "xe\u{301}e\u{301}한y".as_bytes();
        for split in 1..text.len() {
            let mut t = term(20, 2);
            t.advance(&text[..split]);
            t.advance(&text[split..]);
            assert_eq!(t.screen_lines()[0], "xe\u{301}e\u{301}한y", "split at {split}");
        }
    }

    #[test]
    fn content_scanner_ignores_pure_control_traffic() {
        let mut s = ContentScanner::default();
        assert!(!s.scan(b"\x1b[?25l\x1b[?25h\x1b[12;4H\r"));
        assert!(!s.scan(b"\x1b]0;title with text\x07"));
        assert!(!s.scan(b"\x1b[?20"));
        assert!(!s.scan(b"26h"), "CSI split across chunks");
        assert!(s.scan(b"\x1b[1mx"));
        assert!(s.scan("한".as_bytes()));
    }
}
