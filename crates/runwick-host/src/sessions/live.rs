//! Background upkeep of runtime state: `session.live` events (coalesced, at most ~10 per second
//! per session), busy expiry, prompt recognition, shell titles from the program, and the
//! session's last-activity time. Sleeps until something is due; no periodic wake-ups while idle.
//!
//! Title precedence: a name the person gave > the first user message (agents; set by records
//! while the title is still the default) > the program's title (shells only) > the default.

use super::rt::{BUSY_WINDOW, Rt};
use super::term::Size;
use super::{attention, record, screens};
use crate::host::Host;
use runwick_core::api::sessions::{Attention, EVENT_LIVE, SessionLive, Tool};
use runwick_core::rpc::topic;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::time::sleep_until;

const MIN_EMIT_GAP: Duration = Duration::from_millis(100);
const SCAN_GAP: Duration = Duration::from_millis(250);
const TITLE_SETTLE: Duration = Duration::from_millis(800);
const TOUCH_GAP: Duration = Duration::from_secs(30);
/// Save the screen this long after output stops.
const SAVE_QUIET: Duration = Duration::from_secs(1);

pub async fn run(host: Arc<Host>) {
    loop {
        let next = tick(&host, Instant::now());
        match next {
            Some(at) => {
                tokio::select! {
                    _ = sleep_until(at.into()) => {}
                    _ = host.sessions.woken() => {}
                }
            }
            None => host.sessions.woken().await,
        }
    }
}

/// Process every session; returns when the next timed step is due.
fn tick(host: &Arc<Host>, now: Instant) -> Option<Instant> {
    let mut next: Option<Instant> = None;
    let mut due = |at: Instant| next = Some(next.map_or(at, |n| n.min(at)));
    let mut records_changed = false;
    for rt in host.sessions.all() {
        let work = step(host, &rt, now, &mut due);
        if let Some(title) = work.title {
            records_changed |= record::set_auto_title(&host.store, rt.id, &title).unwrap_or(false);
        }
        if let Some(at) = work.touch {
            let _ = record::touch(&host.store, rt.id, &at);
        }
        if let Some((version, size, bytes)) = work.screen
            && let Err(e) = screens::save(rt.id, version, size, &bytes)
        {
            tracing::warn!(session = %rt.id, "cannot save the screen: {e}");
        }
        if let Some(live) = work.emit {
            host.bus.event(EVENT_LIVE, runwick_core::rpc::to_value(&live));
        }
    }
    if records_changed {
        host.bus.changed(&[topic::SESSIONS]);
    }
    next
}

#[derive(Default)]
struct Work {
    screen: Option<(u64, Size, Vec<u8>)>,
    title: Option<String>,
    touch: Option<runwick_core::Ts>,
    emit: Option<SessionLive>,
}

fn step(host: &Host, rt: &Rt, now: Instant, due: &mut impl FnMut(Instant)) -> Work {
    let mut work = Work::default();
    let mut st = rt.lock();

    if let Some((title, at)) = &st.pending_title {
        if now.duration_since(*at) >= TITLE_SETTLE {
            work.title = Some(shell_title(title.as_deref()));
            st.pending_title = None;
        } else {
            due(*at + TITLE_SETTLE);
        }
    }

    if st.screen_dirty {
        let ready = st.last_scan.is_none_or(|t| now.duration_since(t) >= SCAN_GAP);
        if ready {
            st.screen_dirty = false;
            st.last_scan = Some(now);
            let (found, prompt) = if st.run.is_some() {
                let (lines, cursor) = (st.term.screen_lines(), st.term.cursor_row());
                // The same check delivery uses, so the label and delivery never disagree.
                (attention::detect(rt.tool, &lines, cursor), attention::ready_for_input(rt.tool, &lines, cursor))
            } else {
                (None, false)
            };
            st.input_prompt = prompt;
            let next = found.map(|(kind, text)| match &st.attention {
                Some(a) if a.kind == kind && a.text == text => a.clone(),
                _ => Attention { kind, text, since: runwick_core::now() },
            });
            st.attention = next;
        } else if let Some(t) = st.last_scan {
            due(t + SCAN_GAP);
        }
    }

    let active_at = st.last_output.max(st.last_input);
    if let Some(active) = active_at
        && st.last_touch.is_none_or(|t| active > t && now.duration_since(t) >= TOUCH_GAP)
    {
        st.last_touch = Some(now);
        work.touch = Some(runwick_core::now());
    } else if let (Some(active), Some(t)) = (active_at, st.last_touch)
        && active > t
    {
        due(t + TOUCH_GAP);
    }

    if st.busy(now)
        && let Some(t) = st.last_output
    {
        due(t + BUSY_WINDOW);
    }

    // Keep the screen file current: once output pauses, or every so often while it streams.
    if st.run.is_some() && st.content_seq != st.saved_seq {
        let quiet_at = st.last_output.map_or(now, |t| t + SAVE_QUIET);
        let gap_at = st.last_save.map_or(now, |t| t + screens::SAVE_GAP);
        if now >= quiet_at || now >= gap_at {
            st.saved_seq = st.content_seq;
            st.last_save = Some(now);
            work.screen = Some((st.content_seq, st.term.size(), st.term.snapshot_with_history(screens::HISTORY)));
        } else {
            due(quiet_at.min(gap_at));
        }
    }

    let live = st.live(rt.id, &host.bus, now);
    let changed = st.emitted.as_ref().map_or(st.has_runtime(), |e| differs(e, &live));
    if changed {
        match st.last_emit {
            Some(t) if now.duration_since(t) < MIN_EMIT_GAP => due(t + MIN_EMIT_GAP),
            _ => {
                st.last_emit = Some(now);
                st.emitted = Some(live.clone());
                work.emit = Some(live);
            }
        }
    }
    work
}

/// Changes worth an event. A moving last-output time alone is sent at most every few seconds.
fn differs(old: &SessionLive, new: &SessionLive) -> bool {
    let mut a = old.clone();
    a.last_output_at = new.last_output_at;
    if a != *new {
        return true;
    }
    match (old.last_output_at, new.last_output_at) {
        (Some(x), Some(y)) => (y - x).num_seconds() >= 5,
        (x, y) => x != y,
    }
}

/// The session title a shell's program title stands for: the title itself when it names
/// something (a running command), otherwise the default. The prompt's `user@host:path` or a bare
/// folder path only repeats what the workspace already shows.
fn shell_title(raw: Option<&str>) -> String {
    raw.and_then(clean_title).filter(|t| !is_prompt_noise(t)).unwrap_or_else(|| Tool::Shell.label().to_string())
}

/// `user@host`, optionally followed by `:`/space and a path, or a bare `~`/`/` path.
fn is_prompt_noise(title: &str) -> bool {
    if title.starts_with('~') || title.starts_with('/') {
        return true;
    }
    let Some((user, rest)) = title.split_once('@') else { return false };
    let name = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_alphanumeric() || matches!(c, '.' | '_' | '-'));
    let host_end = rest.find([':', ' ']).unwrap_or(rest.len());
    let (host, path) = rest.split_at(host_end);
    let path = path.trim_start_matches(':').trim_start();
    name(user) && name(host) && (path.is_empty() || path.starts_with('~') || path.starts_with('/'))
}

/// A program title made fit for a tab: without spinner/status glyphs in front, single line,
/// at most 80 characters, nothing invisible. `None` when nothing readable is left.
pub fn clean_title(raw: &str) -> Option<String> {
    let t = visible(raw.lines().next().unwrap_or(""));
    let t =
        t.trim().trim_start_matches(|c: char| !c.is_alphanumeric() && !matches!(c, '~' | '/' | '.' | '[' | '(' | '"'));
    let t: String = t.trim().chars().take(80).collect();
    (!t.is_empty()).then_some(t)
}

/// `text` without characters that are invisible yet change how the text around them reads:
/// controls, bidi embeddings, overrides, isolates and marks (which can make a title read
/// backwards), zero-width spaces and joiners, and byte order marks.
pub fn visible(text: &str) -> String {
    text.chars().filter(|c| !is_invisible(*c)).collect()
}

fn is_invisible(c: char) -> bool {
    c.is_control()
        || matches!(c, '\u{061c}' | '\u{180e}' | '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{2069}' | '\u{feff}')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_drop_spinner_glyphs() {
        assert_eq!(clean_title("✳ Fix login bug").as_deref(), Some("Fix login bug"));
        assert_eq!(clean_title("⠂ Fix login bug").as_deref(), Some("Fix login bug"));
        assert_eq!(clean_title("~/work/app").as_deref(), Some("~/work/app"));
        assert_eq!(clean_title("로그인 오류 수정").as_deref(), Some("로그인 오류 수정"));
        assert_eq!(clean_title("  ✶  ").as_deref(), None);
    }

    #[test]
    fn titles_lose_bidi_controls_and_zero_width_characters() {
        // U+202E would show "fdp.exe" as "exe.pdf"; zero-width joins hide characters.
        assert_eq!(clean_title("invoice\u{202e}fdp.exe").as_deref(), Some("invoicefdp.exe"));
        assert_eq!(clean_title("\u{2067}배포\u{2069} 로\u{200b}그").as_deref(), Some("배포 로그"));
        assert_eq!(clean_title("\u{200b}\u{feff}\u{200d}"), None);
        assert_eq!(visible("a\u{200e}b\u{061c}c\td"), "abcd");
    }

    #[test]
    fn shell_titles_skip_prompt_noise() {
        for noise in [
            "jy@Jeongs-MacBook-Pro:~/work/app",
            "jy@host: ~/work",
            "jy@host:/tmp",
            "jy@host",
            "user.name@host.local ~",
            "~/work/app",
            "/Users/jy",
        ] {
            assert_eq!(shell_title(Some(noise)), "셸", "{noise}");
        }
        assert_eq!(shell_title(Some("npm run dev")), "npm run dev");
        assert_eq!(shell_title(Some("vim notes.md")), "vim notes.md");
        assert_eq!(shell_title(Some("ssh deploy@prod")), "ssh deploy@prod");
        assert_eq!(shell_title(Some("git push origin main")), "git push origin main");
        assert_eq!(shell_title(Some("⠂ ")), "셸");
        assert_eq!(shell_title(None), "셸", "a title reset goes back to the default");
    }
}
