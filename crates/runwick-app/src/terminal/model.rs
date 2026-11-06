//! Client-side terminal state of one session, fed with the host's output frames on the
//! connection's reader thread. It renders only: replies to terminal queries (`PtyWrite`) are
//! dropped because the host's own terminal model already answered them.
//!
//! Output passes `runwick_core::term_guard` here too (the host already applied it; this keeps
//! the app safe from a host on another device that does not).

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::vte::ansi::{Processor, StdSyncHandler};
use runwick_core::term_guard::{OutputGuard, Sink};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

/// Lines of scrollback kept on screen in the app (the host keeps the same amount).
pub const SCROLLBACK: usize = 10_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GridSize {
    pub cols: u16,
    pub rows: u16,
}

impl Dimensions for GridSize {
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

/// Longest text a program may put on the clipboard (OSC 52). Below what the output guard lets
/// through as an OSC string (about 48 KiB of text once decoded), so this is the limit.
pub const MAX_CLIPBOARD: usize = 32 * 1024;

/// A program asked to put text on the clipboard (OSC 52). Never acted on silently.
#[derive(Debug, Clone, PartialEq)]
pub struct CopyRequest {
    pub text: String,
    pub at: Instant,
}

/// Requests from the program. Only the newest clipboard request is kept, so nothing piles up
/// while the session is not shown.
#[derive(Clone, Default)]
pub struct Events(Arc<Mutex<Option<CopyRequest>>>);

impl EventListener for Events {
    fn send_event(&self, event: Event) {
        if let Event::ClipboardStore(_, text) = event
            && text.len() <= MAX_CLIPBOARD
            && let Ok(mut v) = self.0.lock()
        {
            *v = Some(CopyRequest { text, at: Instant::now() });
        }
    }
}

impl Events {
    pub fn take(&self) -> Option<CopyRequest> {
        self.0.lock().ok().and_then(|mut v| v.take())
    }
}

pub struct ClientTerm {
    pub term: Term<Events>,
    parser: Processor<StdSyncHandler>,
    carry: Vec<u8>,
    guard: OutputGuard,
    /// While an attach is in flight: output held until the snapshot's size is known.
    held: Option<Vec<u8>>,
    pub events: Events,
    /// Increments whenever content changes (for search refresh and repaint decisions).
    pub version: u64,
}

impl ClientTerm {
    pub fn new(size: GridSize) -> Self {
        let events = Events::default();
        Self {
            term: Term::new(config(), &size, events.clone()),
            parser: Processor::new(),
            carry: Vec::new(),
            guard: OutputGuard::default(),
            held: None,
            events,
            version: 0,
        }
    }

    pub fn size(&self) -> GridSize {
        GridSize { cols: self.term.columns() as u16, rows: self.term.screen_lines() as u16 }
    }

    /// Output from the host.
    pub fn feed(&mut self, bytes: &[u8]) {
        if let Some(h) = self.held.as_mut() {
            h.extend_from_slice(bytes);
            return;
        }
        // Keep incomplete UTF-8 characters for the next chunk (see the host's `Utf8Carry`).
        let mut joined;
        let data: &[u8] = if self.carry.is_empty() {
            bytes
        } else {
            joined = std::mem::take(&mut self.carry);
            joined.extend_from_slice(bytes);
            &joined
        };
        let keep = incomplete_utf8_tail(data);
        let (now, later) = data.split_at(data.len() - keep);
        self.guard.feed(now, &mut Fed { term: &mut self.term, parser: &mut self.parser });
        let later = later.to_vec();
        self.carry = later;
        self.version += 1;
    }

    /// An attach request was sent: hold output until its answer says the snapshot's size.
    pub fn hold(&mut self) {
        self.held = Some(Vec::new());
    }

    /// The attach answered: start over at the snapshot's size and replay what arrived since
    /// (the snapshot itself begins with a full reset).
    pub fn replace(&mut self, size: GridSize) {
        let held = self.held.take().unwrap_or_default();
        let events = Events::default();
        self.term = Term::new(config(), &size, events.clone());
        self.events = events;
        self.parser = Processor::new();
        self.carry.clear();
        self.guard = OutputGuard::default();
        self.feed(&held);
    }

    /// Give up an attach that failed: keep showing the old content.
    pub fn release(&mut self) {
        self.held = None;
    }

    pub fn resize(&mut self, size: GridSize) {
        if size != self.size() {
            self.term.resize(size);
            self.version += 1;
        }
    }

    /// End a synchronized update that timed out. Returns when the pending one expires.
    pub fn flush_sync(&mut self) -> Option<Instant> {
        let deadline = self.parser.sync_timeout().sync_timeout()?;
        if Instant::now() >= deadline {
            self.parser.stop_sync(&mut self.term);
            self.version += 1;
            None
        } else {
            Some(deadline)
        }
    }
}

/// The terminal behind the output guard.
struct Fed<'a> {
    term: &'a mut Term<Events>,
    parser: &'a mut Processor<StdSyncHandler>,
}

impl Sink for Fed<'_> {
    fn feed(&mut self, bytes: &[u8]) {
        self.parser.advance(self.term, bytes);
    }

    /// The cell the next zero-width character joins (mirrors `Term::input`). During a
    /// synchronized update the parser holds output back, so this may lag; the host's guard is
    /// exact.
    fn zero_width_here(&self) -> usize {
        let grid = self.term.grid();
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
}

fn config() -> Config {
    Config { scrolling_history: SCROLLBACK, ..Config::default() }
}

/// Bytes at the end of `b` that start a UTF-8 character but do not complete it.
fn incomplete_utf8_tail(b: &[u8]) -> usize {
    for back in 1..=3.min(b.len()) {
        let byte = b[b.len() - back];
        if byte & 0xC0 == 0x80 {
            continue;
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

/// Shared between the UI thread and the connection's reader thread.
#[derive(Clone)]
pub struct Shared(Arc<Mutex<ClientTerm>>);

impl Shared {
    pub fn new(size: GridSize) -> Self {
        Self(Arc::new(Mutex::new(ClientTerm::new(size))))
    }

    pub fn lock(&self) -> MutexGuard<'_, ClientTerm> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Sink for `HostConn::set_output_sink`.
    pub fn sink(&self) -> crate::client::OutputSink {
        let me = self.clone();
        Box::new(move |bytes| me.lock().feed(bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(t: &ClientTerm, line: i32) -> String {
        use alacritty_terminal::index::{Column, Line};
        let g = t.term.grid();
        (0..g.columns()).map(|c| g[Line(line)][Column(c)].c).collect::<String>().trim_end().to_string()
    }

    #[test]
    fn held_output_is_replayed_at_the_snapshot_size() {
        let mut t = ClientTerm::new(GridSize { cols: 10, rows: 3 });
        t.feed(b"old");
        t.hold();
        t.feed(b"\x1bcnew 012345678901234567890");
        assert_eq!(text(&t, 0), "old", "nothing applied while holding");
        t.replace(GridSize { cols: 30, rows: 4 });
        assert_eq!(t.size(), GridSize { cols: 30, rows: 4 });
        assert_eq!(text(&t, 0), "new 012345678901234567890");
    }

    #[test]
    fn split_utf8_is_joined_and_queries_are_not_answered() {
        let mut t = ClientTerm::new(GridSize { cols: 20, rows: 2 });
        let bytes = "é·a한글\x1b[6n".as_bytes();
        for b in bytes.chunks(1) {
            t.feed(b);
        }
        // Wide characters are followed by a spacer cell.
        assert_eq!(text(&t, 0), "é·a한 글");
        assert!(t.events.take().is_none());
    }

    #[test]
    fn output_is_guarded_and_clipboard_requests_do_not_pile_up() {
        use alacritty_terminal::index::{Column, Line};
        let mut t = ClientTerm::new(GridSize { cols: 20, rows: 2 });
        let marks: String = std::iter::repeat_n('\u{301}', 5000).collect();
        t.feed(format!("x{marks}").as_bytes());
        let zw = t.term.grid()[Line(0)][Column(0)].zerowidth().map_or(0, |z| z.len());
        assert_eq!(zw, runwick_core::term_guard::MAX_ZERO_WIDTH);
        // OSC 52 ("c" clipboard, base64 text): only the newest request is kept.
        t.feed(b"\x1b]52;c;Zmlyc3Q=\x07\x1b]52;c;c2Vjb25k\x07");
        assert_eq!(t.events.take().map(|r| r.text).as_deref(), Some("second"));
        assert!(t.events.take().is_none());
        // Too long for the clipboard: never offered ("QUFB" is "AAA").
        let big = "QUFB".repeat(MAX_CLIPBOARD / 3 + 1);
        assert!(big.len() < runwick_core::term_guard::MAX_OSC, "reaches the terminal, refused here");
        t.feed(format!("\x1b]52;c;{big}\x07").as_bytes());
        assert!(t.events.take().is_none());
    }
}
