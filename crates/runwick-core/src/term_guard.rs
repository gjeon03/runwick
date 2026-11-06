//! Limits on program output before it reaches a terminal model (the host's and the app's), so a
//! program cannot make Runwick hold unbounded memory, snapshots or screen files:
//!
//! - an OSC string longer than [`MAX_OSC`] is dropped whole (for example a title that never
//!   ends);
//! - a hyperlink (OSC 8) whose URI is longer than [`MAX_URI`] becomes "no link";
//! - a cell keeps at most [`MAX_ZERO_WIDTH`] zero-width characters (combining marks, joiners);
//!   further ones for that cell are dropped. The terminal itself is asked, right before each
//!   zero-width character, how many its target cell already has.
//!
//! Everything else passes unchanged and in order. OSC strings start and end exactly where
//! `vte` starts and ends them (ESC ], then BEL, CAN, SUB or ESC), so both parsers agree.

use unicode_width::UnicodeWidthChar;

pub const MAX_OSC: usize = 64 * 1024;
pub const MAX_URI: usize = 2048;
pub const MAX_ZERO_WIDTH: usize = 16;

const ESC: u8 = 0x1b;
const BEL: u8 = 0x07;
const CAN: u8 = 0x18;
const SUB: u8 = 0x1a;

/// The terminal behind the guard.
pub trait Sink {
    /// Program output that passed the guard.
    fn feed(&mut self, bytes: &[u8]);
    /// Zero-width characters already on the cell that the next one would join.
    fn zero_width_here(&self) -> usize;
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum State {
    #[default]
    Ground,
    /// After ESC, which is held until the next byte tells whether an OSC string starts.
    Esc,
    /// Collecting an OSC string (in `osc`).
    Osc,
    /// An OSC string that grew too long: dropped until it ends.
    Dropping,
}

#[derive(Debug, Default)]
pub struct OutputGuard {
    state: State,
    /// The OSC string being collected, from its `ESC ]`.
    osc: Vec<u8>,
}

impl OutputGuard {
    /// Pass `bytes` to `sink`, without what the limits above drop. `bytes` must not end inside
    /// a UTF-8 character (keep incomplete tails for the next call, as the terminals do).
    pub fn feed(&mut self, bytes: &[u8], sink: &mut impl Sink) {
        let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            let b = bytes[i];
            match self.state {
                State::Ground => {
                    if b == ESC {
                        self.state = State::Esc;
                    } else if b >= 0xc0 {
                        let len = utf8_len(b);
                        let c = bytes
                            .get(i..i + len)
                            .and_then(|s| std::str::from_utf8(s).ok())
                            .and_then(|s| s.chars().next());
                        match c {
                            Some(c) if c.width() == Some(0) => {
                                // The terminal must have seen everything before to know the cell.
                                sink.feed(&out);
                                out.clear();
                                if sink.zero_width_here() < MAX_ZERO_WIDTH {
                                    out.extend_from_slice(&bytes[i..i + len]);
                                }
                                i += len;
                                continue;
                            }
                            Some(_) => {
                                out.extend_from_slice(&bytes[i..i + len]);
                                i += len;
                                continue;
                            }
                            None => out.push(b),
                        }
                    } else {
                        out.push(b);
                    }
                }
                State::Esc => match b {
                    b']' => {
                        self.osc.clear();
                        self.osc.extend_from_slice(b"\x1b]");
                        self.state = State::Osc;
                    }
                    // The held ESC was a lone one; the new one is held instead.
                    ESC => out.push(ESC),
                    _ => {
                        self.state = State::Ground;
                        out.extend_from_slice(&[ESC, b]);
                    }
                },
                State::Osc => match b {
                    BEL | CAN | SUB => {
                        self.finish_osc(&mut out);
                        out.push(b);
                        self.state = State::Ground;
                    }
                    ESC => {
                        self.finish_osc(&mut out);
                        self.state = State::Esc;
                    }
                    _ => {
                        self.osc.push(b);
                        if self.osc.len() > MAX_OSC {
                            self.osc = Vec::new();
                            self.state = State::Dropping;
                        }
                    }
                },
                State::Dropping => match b {
                    BEL | CAN | SUB => self.state = State::Ground,
                    // The ESC may start the next sequence (or form a harmless ST).
                    ESC => self.state = State::Esc,
                    _ => {}
                },
            }
            i += 1;
        }
        sink.feed(&out);
    }

    /// Held bytes that start a sequence still waiting for its end (part of a terminal
    /// snapshot, so a client continues it the same way).
    pub fn pending(&self) -> &[u8] {
        match self.state {
            State::Osc => &self.osc,
            State::Esc => b"\x1b",
            State::Ground | State::Dropping => &[],
        }
    }

    fn finish_osc(&mut self, out: &mut Vec<u8>) {
        let osc = std::mem::take(&mut self.osc);
        match osc.strip_prefix(b"\x1b]8;").and_then(|rest| rest.iter().position(|b| *b == b';').map(|p| &rest[p + 1..]))
        {
            Some(uri) if uri.len() > MAX_URI => out.extend_from_slice(b"\x1b]8;;"),
            _ => out.extend_from_slice(&osc),
        }
    }
}

fn utf8_len(lead: u8) -> usize {
    match lead {
        0xf0.. => 4,
        0xe0.. => 3,
        _ => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Collects what passes and counts zero-width characters per "cell" the way a terminal
    /// would for text without cursor movement: they join the last printed character.
    #[derive(Default)]
    struct Out {
        bytes: Vec<u8>,
        zw_on_last: usize,
    }

    impl Sink for Out {
        fn feed(&mut self, bytes: &[u8]) {
            self.bytes.extend_from_slice(bytes);
            for c in String::from_utf8_lossy(bytes).chars() {
                if c.width() == Some(0) && !c.is_control() {
                    self.zw_on_last += 1;
                } else if !c.is_control() {
                    self.zw_on_last = 0;
                }
            }
        }
        fn zero_width_here(&self) -> usize {
            self.zw_on_last
        }
    }

    fn run(chunks: &[&[u8]]) -> Vec<u8> {
        let mut g = OutputGuard::default();
        let mut out = Out::default();
        for c in chunks {
            g.feed(c, &mut out);
        }
        out.bytes
    }

    #[test]
    fn ordinary_output_passes_unchanged() {
        let s = "plain 한글 \x1b[1;31mred\x1b[0m \x1b]0;title\x07 \x1b]8;id=1;https://x.example/a\x1b\\link\x1b]8;;\x1b\\ e\u{301}";
        assert_eq!(run(&[s.as_bytes()]), s.as_bytes());
        // Split anywhere, including inside an OSC string and between ESC and ']'.
        let b = s.as_bytes();
        for cut in 1..b.len() {
            if std::str::from_utf8(&b[..cut]).is_ok() {
                assert_eq!(run(&[&b[..cut], &b[cut..]]), b, "cut at {cut}");
            }
        }
    }

    #[test]
    fn an_osc_string_that_never_ends_is_dropped_and_output_continues() {
        let mut long = b"\x1b]2;".to_vec();
        long.extend(std::iter::repeat_n(b'A', MAX_OSC + 10));
        let mut g = OutputGuard::default();
        let mut out = Out::default();
        g.feed(&long, &mut out);
        assert!(g.osc.capacity() <= MAX_OSC + 1024, "nothing more is kept");
        g.feed(&[b'A'; 100_000], &mut out);
        g.feed(b"\x07after", &mut out);
        assert_eq!(out.bytes, b"after");
        // Ended by ESC: the ESC still starts the next sequence.
        let mut out = Out::default();
        g.feed(&long, &mut out);
        g.feed(b"\x1b[1mX", &mut out);
        assert_eq!(out.bytes, b"\x1b[1mX");
    }

    #[test]
    fn long_hyperlink_uris_become_no_link() {
        let uri = format!("https://a.example/{}", "A".repeat(MAX_URI));
        let s = format!("\x1b]8;id=7;{uri}\x1b\\text\x1b]8;;\x1b\\");
        assert_eq!(run(&[s.as_bytes()]), b"\x1b]8;;\x1b\\text\x1b]8;;\x1b\\");
        let ok = format!("\x1b]8;;https://a.example/{}\x07", "A".repeat(100));
        assert_eq!(run(&[ok.as_bytes()]), ok.as_bytes());
    }

    #[test]
    fn a_cell_keeps_at_most_the_limit_of_zero_width_characters() {
        let marks: String = std::iter::repeat_n('\u{200b}', 1000).collect();
        let s = format!("x{marks}y\u{301}");
        let out = String::from_utf8(run(&[s.as_bytes()])).unwrap();
        let zw = out.chars().filter(|c| *c == '\u{200b}').count();
        assert_eq!(zw, MAX_ZERO_WIDTH);
        assert!(out.ends_with("y\u{301}"), "the next cell starts over: {out:?}");
        // Normal combining text is untouched (decomposed Korean: two jamo per syllable).
        let nfd = "\u{1112}\u{1161}\u{11ab}\u{1100}\u{1173}\u{11af}";
        assert_eq!(run(&[nfd.as_bytes()]), nfd.as_bytes());
    }
}
