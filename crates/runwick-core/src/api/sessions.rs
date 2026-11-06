//! Sessions: one CLI conversation (Claude, Codex) or a plain shell, run by the host in a PTY.
//!
//! The host keeps the process and its screen state; clients attach to receive output frames.
//! `sessions.attach` first streams a snapshot that begins with a full terminal reset (RIS) and
//! then live output, so a client can always rebuild its screen from a fresh terminal.

use crate::Ts;
use crate::api::projects::{ProjectId, WorkspaceId};
use crate::method;
use crate::rpc::topic;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub type SessionId = Uuid;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Tool {
    Claude,
    Codex,
    Shell,
}

impl Tool {
    pub const ALL: [Tool; 3] = [Tool::Claude, Tool::Codex, Tool::Shell];

    pub fn label(self) -> &'static str {
        match self {
            Tool::Claude => "Claude Code",
            Tool::Codex => "Codex",
            Tool::Shell => "셸",
        }
    }

    /// Whether the tool keeps a conversation that can be resumed by id.
    pub fn is_agent(self) -> bool {
        !matches!(self, Tool::Shell)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Session {
    pub id: SessionId,
    pub project_id: ProjectId,
    pub workspace_id: WorkspaceId,
    pub title: String,
    /// The user renamed it; automatic titles must not overwrite it.
    pub title_user_set: bool,
    pub tool: Tool,
    /// Conversation id known to the provider CLI (Claude session UUID, Codex thread id).
    pub provider_conv_id: Option<String>,
    /// Set when another session delegated work to this one.
    pub parent_id: Option<SessionId>,
    /// Role shown for delegated sessions, e.g. "회귀 검토".
    pub role: Option<String>,
    pub cwd: String,
    pub created_at: Ts,
    pub last_active_at: Option<Ts>,
    pub ended_at: Option<Ts>,
    pub exit_code: Option<i32>,
    /// Tab closed: hidden from tabs but the run and records continue.
    pub hidden: bool,
    /// Removed from the sidebar list (records stay searchable).
    pub archived: bool,
    /// Version of the global environment the current run started with.
    pub env_version: u64,
    /// The run was alive when the host last stopped (for example a reboot). Offered for resume;
    /// never started again automatically.
    #[serde(default)]
    pub interrupted: bool,
    /// Computer this record was synced from (GitHub sync); `None` for this computer's own
    /// sessions. A synced record is read-only here: it is never run or resumed.
    #[serde(default)]
    pub origin_device: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Starting,
    Running,
    /// The process ended while the host was running; output stays readable.
    Exited,
    /// Only the record exists (for example after a reboot). Can be resumed if supported.
    NotRunning,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AttentionKind {
    /// The CLI shows a permission/approval prompt we recognised.
    Approval,
    /// The CLI asks for login or trust.
    Auth,
    /// The CLI rang the bell or sent a notification while not focused.
    Notify,
    /// The CLI shows a numbered menu we do not otherwise know (Codex's "Update available"
    /// choice, …): the person picks an option on the CLI's screen.
    Choice,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Attention {
    pub kind: AttentionKind,
    /// The prompt line we recognised, shown as-is.
    pub text: String,
    pub since: Ts,
}

/// Runtime state that changes often. Sent in `session.live` events.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionLive {
    pub id: SessionId,
    pub phase: Phase,
    /// Output observed within the last few seconds.
    pub busy: bool,
    pub attention: Option<Attention>,
    pub last_output_at: Option<Ts>,
    pub cols: u16,
    pub rows: u16,
    pub pid: Option<u32>,
    /// Label of the client that currently holds keyboard input, if any.
    pub input_owner: Option<String>,
    /// New output arrived since a client last marked it read.
    pub unread: bool,
    /// Last line of a failure to start (missing CLI, bad folder), for display.
    #[serde(default)]
    pub error: Option<String>,
    /// Why the run could not start, so clients can tell "CLI not installed" from app errors.
    #[serde(default)]
    pub error_kind: Option<StartErrorKind>,
    /// Host connection id of the client holding keyboard input (compare with
    /// `HostInfo::client_id` to know whether this client owns it).
    #[serde(default)]
    pub input_owner_client: Option<u64>,
    /// Shells: a program other than the shell runs in the foreground (possibly silently).
    #[serde(default)]
    pub foreground_job: bool,
    /// Agents: their own input prompt is on the screen and no dialog asks for a choice (the
    /// same check that decides when a delegated instruction can be typed in).
    #[serde(default)]
    pub input_prompt: bool,
    /// Processes of the run still alive after it was stopped ("name (pid N)"). Stopping did
    /// not succeed while this is not empty.
    #[serde(default)]
    pub leftovers: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StartErrorKind {
    /// The CLI binary is not on the login PATH.
    NotInstalled,
    /// The workspace folder does not exist or cannot be entered.
    FolderMissing,
    /// The record cannot be resumed (for example no saved conversation id).
    NotResumable,
    /// Starting the process failed for another reason (see `error`).
    SpawnFailed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionInfo {
    pub session: Session,
    /// None when the host has no runtime state (record only).
    pub live: Option<SessionLive>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SessionQuery {
    #[serde(default)]
    pub project_id: Option<ProjectId>,
    #[serde(default)]
    pub workspace_id: Option<WorkspaceId>,
    #[serde(default)]
    pub include_hidden: bool,
    #[serde(default)]
    pub include_archived: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartSession {
    pub workspace_id: WorkspaceId,
    pub tool: Tool,
    #[serde(default)]
    pub title: Option<String>,
    /// First message given to an agent CLI on start.
    #[serde(default)]
    pub initial_prompt: Option<String>,
    #[serde(default)]
    pub parent_id: Option<SessionId>,
    #[serde(default)]
    pub role: Option<String>,
    pub cols: u16,
    pub rows: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Attach {
    pub id: SessionId,
    pub cols: u16,
    pub rows: u16,
    /// Interactive attaches resize the PTY and may send input.
    pub interactive: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Resize {
    pub id: SessionId,
    pub cols: u16,
    pub rows: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Stop {
    pub id: SessionId,
    /// Kill immediately instead of hang-up then kill after a grace period.
    #[serde(default)]
    pub force: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Restart {
    pub id: SessionId,
    /// Resume the same provider conversation (`claude --resume`, `codex resume`).
    /// Shells always start a new process.
    pub resume: bool,
    pub cols: u16,
    pub rows: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SessionUpdate {
    pub id: SessionId,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub hidden: Option<bool>,
    #[serde(default)]
    pub archived: Option<bool>,
    #[serde(default)]
    pub mark_read: bool,
    /// Dismiss the "was running before the restart" offer without resuming.
    #[serde(default)]
    pub clear_interrupted: bool,
}

/// Whether a client is showing this session to the person right now (tab visible and window
/// focused). Unread output and bell notifications are only raised while nobody is viewing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct View {
    pub id: SessionId,
    pub viewing: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScreenQuery {
    pub id: SessionId,
    /// Include up to this many scrollback lines above the screen.
    #[serde(default)]
    pub scrollback: usize,
}

method!(SessionsList, "sessions.list", SessionQuery => Vec<SessionInfo>, [topic::SESSIONS]);
method!(SessionsGet, "sessions.get", crate::api::projects::ById => SessionInfo, [topic::SESSIONS]);
method!(SessionsStart, "sessions.start", StartSession => SessionInfo);
method!(SessionsAttach, "sessions.attach", Attach => SessionLive);
method!(SessionsDetach, "sessions.detach", crate::api::projects::ById => ());
method!(SessionsResize, "sessions.resize", Resize => ());
method!(SessionsStop, "sessions.stop", Stop => ());
method!(SessionsRestart, "sessions.restart", Restart => SessionInfo);
method!(SessionsUpdate, "sessions.update", SessionUpdate => Session);
method!(SessionsTakeInput, "sessions.take_input", crate::api::projects::ById => ());
method!(SessionsView, "sessions.view", View => ());
// Plain text of the current screen (and optional scrollback), for previews and agents.
method!(SessionsScreenText, "sessions.screen_text", ScreenQuery => String);

/// Event topic carrying a [`SessionLive`] whenever runtime state changes.
pub const EVENT_LIVE: &str = "session.live";

/// Default terminal colors. The GUI renders with them and the host answers color queries
/// (OSC 4/10/11/12) with them, so CLIs that adapt to the background see what is drawn.
pub mod palette {
    /// ANSI 0–15, tuned for the Graphite Wick terminal ground.
    pub const ANSI: [u32; 16] = [
        0x2A2B31, // black
        0xFF7B86, // red
        0x5AD6A4, // green
        0xF2C66D, // yellow
        0x74B7FF, // blue
        0xBBA2FF, // magenta
        0x7CCFE6, // cyan
        0xD6D7DC, // white
        0x6F7078, // bright black
        0xFF9C9C, // bright red
        0x7EE0A8, // bright green
        0xFFD98A, // bright yellow
        0x9CCBFF, // bright blue
        0xCDA8F5, // bright magenta
        0xA4E2F2, // bright cyan
        0xEDEDF0, // bright white
    ];
    pub const FOREGROUND: u32 = 0xD6D7DC;
    pub const BACKGROUND: u32 = 0x0B0C0E;
    pub const CURSOR: u32 = 0xEDEDF0;

    /// xterm 256-color palette entry as 0xRRGGBB.
    pub fn indexed(i: u8) -> u32 {
        match i {
            0..=15 => ANSI[i as usize],
            16..=231 => {
                let i = i - 16;
                let level = |v: u8| if v == 0 { 0u32 } else { 55 + 40 * v as u32 };
                (level(i / 36) << 16) | (level((i / 6) % 6) << 8) | level(i % 6)
            }
            _ => {
                let v = 8 + 10 * (i as u32 - 232);
                (v << 16) | (v << 8) | v
            }
        }
    }

    pub fn rgb(c: u32) -> (u8, u8, u8) {
        ((c >> 16) as u8, (c >> 8) as u8, c as u8)
    }

    #[cfg(test)]
    mod tests {
        #[test]
        fn cube_and_grey_ramp_match_xterm() {
            assert_eq!(super::indexed(16), 0x000000);
            assert_eq!(super::indexed(196), 0xFF0000);
            assert_eq!(super::indexed(231), 0xFFFFFF);
            assert_eq!(super::indexed(232), 0x080808);
            assert_eq!(super::indexed(255), 0xEEEEEE);
        }
    }
}
