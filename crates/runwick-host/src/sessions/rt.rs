//! In-memory runtime state of one session on this host.

use super::run::Run;
use super::term::{ContentScanner, HostTerm, Size};
use crate::host::{Bus, Client, ClientId};
use runwick_core::Ts;
use runwick_core::api::sessions::{Attention, AttentionKind, Phase, SessionId, SessionLive, StartErrorKind, Tool};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// Output within this window counts as busy.
pub const BUSY_WINDOW: Duration = Duration::from_secs(2);

pub struct Rt {
    pub id: SessionId,
    pub tool: Tool,
    st: Mutex<RtState>,
}

impl Rt {
    pub fn new(id: SessionId, tool: Tool, size: Size) -> Self {
        Self { id, tool, st: Mutex::new(RtState::new(size)) }
    }

    /// The state lock. Held briefly; the PTY reader takes it for every chunk of output.
    pub fn lock(&self) -> MutexGuard<'_, RtState> {
        self.st.lock().unwrap_or_else(|e| e.into_inner())
    }
}

pub struct Attachment {
    pub client: Arc<Client>,
    pub interactive: bool,
    /// The client shows this session to the person right now.
    pub viewing: bool,
}

pub struct RtState {
    pub term: HostTerm,
    pub scanner: ContentScanner,
    pub run: Option<Run>,
    /// Increments per run; output and exit of an older run are ignored.
    pub run_seq: u64,
    pub phase: Phase,
    pub ever_ran: bool,
    pub exit_code: Option<i32>,
    pub error: Option<(StartErrorKind, String)>,
    pub attached: Vec<Attachment>,
    pub owner: Option<ClientId>,
    pub unread: bool,
    /// Prompt recognised on the screen (approval, trust/login).
    pub attention: Option<Attention>,
    /// An agent's input prompt is on the screen (`attention::ready_for_input`).
    pub input_prompt: bool,
    /// Processes still alive after the run was stopped (see `stop`).
    pub leftovers: Vec<String>,
    /// Bell rung while nobody was viewing.
    pub bell_at: Option<Ts>,
    /// Printable output (not just cursor or mode traffic).
    pub last_output: Option<Instant>,
    pub last_output_at: Option<Ts>,
    pub last_input: Option<Instant>,
    /// This run received input from a person or a delegated request (not terminal replies).
    pub has_input: bool,
    /// The person typed text that has not been sent with Enter yet (see [`typed_line_after`]).
    pub typed_line: bool,
    pub screen_dirty: bool,
    pub last_scan: Option<Instant>,
    /// Title from the program, applied once stable.
    pub pending_title: Option<(Option<String>, Instant)>,
    pub last_touch: Option<Instant>,
    pub emitted: Option<SessionLive>,
    pub last_emit: Option<Instant>,
    /// Counts output chunks; compared with `saved_seq` to know whether the screen file is old.
    pub content_seq: u64,
    pub saved_seq: u64,
    pub last_save: Option<Instant>,
}

impl RtState {
    fn new(size: Size) -> Self {
        Self {
            term: HostTerm::new(size),
            scanner: ContentScanner::default(),
            run: None,
            run_seq: 0,
            phase: Phase::NotRunning,
            ever_ran: false,
            exit_code: None,
            error: None,
            attached: Vec::new(),
            owner: None,
            unread: false,
            attention: None,
            input_prompt: false,
            leftovers: Vec::new(),
            bell_at: None,
            last_output: None,
            last_output_at: None,
            last_input: None,
            has_input: false,
            typed_line: false,
            screen_dirty: false,
            last_scan: None,
            pending_title: None,
            last_touch: None,
            emitted: None,
            last_emit: None,
            content_seq: 0,
            saved_seq: 0,
            last_save: None,
        }
    }

    pub fn anyone_viewing(&self) -> bool {
        self.attached.iter().any(|a| a.viewing)
    }

    /// Keep startup output quiet until input has actually reached a running session.
    pub fn send_input(&mut self, data: Vec<u8>) -> bool {
        if data.is_empty() || self.run.as_ref().is_none_or(|run| run.input.send(data).is_err()) {
            return false;
        }
        self.has_input = true;
        true
    }

    pub fn attachment(&self, client: ClientId) -> Option<&Attachment> {
        self.attached.iter().find(|a| a.client.id == client)
    }

    pub fn attachment_mut(&mut self, client: ClientId) -> Option<&mut Attachment> {
        self.attached.iter_mut().find(|a| a.client.id == client)
    }

    /// Whether `client` may type into and resize this session: it holds input, or nobody does
    /// and it is attached interactively.
    pub fn may_control(&self, client: ClientId) -> bool {
        match self.owner {
            Some(o) => o == client,
            None => self.attachment(client).is_some_and(|a| a.interactive),
        }
    }

    pub fn busy(&self, now: Instant) -> bool {
        self.run.is_some() && self.last_output.is_some_and(|t| now.duration_since(t) < BUSY_WINDOW)
    }

    /// The runtime state has been used for a run (or a failed start) on this host.
    pub fn has_runtime(&self) -> bool {
        self.ever_ran || self.error.is_some()
    }

    pub fn live(&self, id: SessionId, bus: &Bus, now: Instant) -> SessionLive {
        let size = self.term.size();
        let attention = self.attention.clone().or_else(|| {
            self.bell_at
                .map(|since| Attention { kind: AttentionKind::Notify, text: "알림이 울렸어요".into(), since })
        });
        SessionLive {
            id,
            phase: self.phase,
            busy: self.busy(now),
            attention,
            last_output_at: self.last_output_at,
            cols: size.cols,
            rows: size.rows,
            pid: self.run.as_ref().map(|r| r.pid),
            input_owner: self.owner.map(|o| bus.get(o).map(|c| c.label()).unwrap_or_else(|| format!("client-{o}"))),
            unread: self.unread,
            error: self.error.as_ref().map(|e| e.1.clone()),
            error_kind: self.error.as_ref().map(|e| e.0),
            input_owner_client: self.owner,
            // A job the shell started holds the terminal (it may print nothing for a long time).
            foreground_job: self
                .run
                .as_ref()
                .and_then(|r| super::pty::foreground_group(&r.master).map(|g| g != r.pid))
                .unwrap_or(false),
            input_prompt: self.run.is_some() && self.input_prompt,
            leftovers: self.leftovers.clone(),
        }
    }
}

/// Whether a line typed by the person is still unsent after `data`, given `before`. Enter,
/// Ctrl-C, Ctrl-D and Ctrl-U end it; printable text or a paste starts one; other keys (arrows
/// and other escape sequences) leave it as it was.
pub fn typed_line_after(before: bool, data: &[u8]) -> bool {
    let ends = |b: &u8| matches!(b, b'\r' | b'\n' | 0x03 | 0x04 | 0x15);
    let (mut state, rest) = match data.iter().rposition(ends) {
        Some(i) => (false, &data[i + 1..]),
        None => (before, data),
    };
    // A bracketed paste, or printable text that isn't an escape sequence (arrows etc.), leaves
    // something on the line.
    let pasted = rest.starts_with(b"\x1b[200~");
    let typed = !rest.starts_with(b"\x1b") && rest.iter().any(|b| *b >= 0x20 && *b != 0x7f);
    if pasted || typed {
        state = true;
    }
    state
}

#[cfg(test)]
mod typed_line_tests {
    use super::typed_line_after;

    #[test]
    fn an_unsent_line_is_tracked_until_enter() {
        assert!(typed_line_after(false, b"fix the"));
        assert!(typed_line_after(true, b"\x1b[D"), "arrows keep it");
        assert!(!typed_line_after(true, b"\r"));
        assert!(!typed_line_after(true, b"\x15"), "Ctrl-U clears the line");
        assert!(!typed_line_after(true, b"\x04"), "Ctrl-D ends it");
        assert!(typed_line_after(false, "\x1b[200~붙여넣기\x1b[201~".as_bytes()));
        assert!(!typed_line_after(false, b"1\r"), "an answered prompt");
        assert!(typed_line_after(false, b"ok\rnext"), "text after Enter starts a new line");
        assert!(!typed_line_after(false, b"\x1b"));
    }
}
