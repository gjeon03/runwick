//! Conversation records: messages collected from the CLIs' own transcript files (no AI), the
//! keyword index over them, pins, and reading a message with its surrounding context.
//!
//! - Claude Code: `~/.claude/projects/<cwd>/<session uuid>.jsonl`; Codex: the rollout file of the
//!   discovered thread. The host tails a session's file while its run is alive and catches up
//!   at host start and when the run ends. Each session's collection state is visible
//!   ([`CaptureStatus`]); collection never blocks the CLI.
//! - Search uses a trigram index, so Korean and code substrings match; queries shorter than
//!   three characters scan instead.
//! - A pin keeps a snapshot of the pinned text. Removing a pin never removes the message, and a
//!   pin stays readable when its source is gone ("원문 연결 불가").

use crate::Ts;
use crate::api::projects::{ById, ProjectId, WorkspaceId};
use crate::api::sessions::{SessionId, Tool};
use crate::method;
use crate::rpc::topic;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub type MessageId = Uuid;
pub type PinId = Uuid;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// What the person typed (or a slash command they ran).
    User,
    /// The agent's answer text.
    Assistant,
    /// A tool call or a short summary of its result ("도구 사용: Bash").
    Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Message {
    pub id: MessageId,
    pub session_id: SessionId,
    pub project_id: ProjectId,
    pub workspace_id: WorkspaceId,
    pub role: Role,
    pub text: String,
    /// When the CLI wrote the message.
    pub at: Ts,
    /// When the host collected it.
    pub collected_at: Ts,
    /// Order inside the session (transcript order).
    pub seq: i64,
    /// Written by a sub-agent inside the CLI (Claude sidechain).
    #[serde(default)]
    pub sidechain: bool,
    /// Tool name for [`Role::Tool`] rows.
    #[serde(default)]
    pub tool_name: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CaptureState {
    /// The run is alive and its transcript is being followed.
    Collecting,
    /// Everything written so far has been collected.
    UpToDate,
    /// The CLI has not written (or no longer has) a transcript file.
    NoFile,
    /// Waiting for the conversation id (Codex reports it once the first message is sent).
    WaitingForId,
    /// Shells have no conversation transcript.
    NotApplicable,
    /// Reading or storing failed; see `last_error`.
    Error,
}

impl CaptureState {
    pub fn label(self) -> &'static str {
        match self {
            CaptureState::Collecting => "수집 중",
            CaptureState::UpToDate => "최신",
            CaptureState::NoFile => "기록 파일 없음",
            CaptureState::WaitingForId => "대화 ID 확인 전",
            CaptureState::NotApplicable => "대화 기록 없음",
            CaptureState::Error => "오류",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CaptureStatus {
    pub session_id: SessionId,
    pub state: CaptureState,
    #[serde(default)]
    pub last_error: Option<String>,
    /// Last time new messages were stored.
    #[serde(default)]
    pub last_collected_at: Option<Ts>,
    /// Last time the transcript was read without error.
    #[serde(default)]
    pub last_checked_at: Option<Ts>,
    pub message_count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StatusQuery {
    #[serde(default)]
    pub project_id: Option<ProjectId>,
    #[serde(default)]
    pub session_id: Option<SessionId>,
}

/// Where to search. Always shown to the person as text.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, Default)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Scope {
    Session {
        id: SessionId,
    },
    Project {
        id: ProjectId,
    },
    #[default]
    All,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct SearchQuery {
    /// Words are matched as substrings (all must appear). `"quoted text"` is one phrase.
    pub text: String,
    #[serde(default)]
    pub scope: Scope,
    /// Messages at or after this time.
    #[serde(default)]
    pub from: Option<Ts>,
    /// Messages before this time.
    #[serde(default)]
    pub to: Option<Ts>,
    #[serde(default)]
    pub tool: Option<Tool>,
    #[serde(default)]
    pub pinned_only: bool,
    /// Results to return, newest first (0 means 50; at most 500). "더 보기" asks for more.
    #[serde(default)]
    pub limit: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Hit {
    pub message: Message,
    pub session_title: String,
    pub project_name: String,
    pub tool: Tool,
    /// One line around the first match ("…" where cut).
    pub snippet: String,
    /// Byte ranges of the matches inside `snippet`.
    pub highlights: Vec<(u32, u32)>,
    pub pinned: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct SearchResult {
    pub hits: Vec<Hit>,
    /// All matches in scope (hits holds at most `limit`).
    pub total: u64,
    /// Sessions in scope whose run is alive: their newest messages may not be searchable yet.
    pub collecting: u32,
    /// Sessions in scope whose collection failed or whose transcript is missing.
    pub incomplete: u32,
    /// The query was shorter than three characters and was matched by scanning.
    pub scanned: bool,
    /// The keyword index could not be used; results come from a slower scan.
    #[serde(default)]
    pub index_error: Option<String>,
}

/// What to read around.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Anchor {
    Message {
        id: MessageId,
    },
    /// The newest messages of a session ("이 세션 기록").
    SessionEnd {
        id: SessionId,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct ContextQuery {
    pub anchor: Anchor,
    /// Messages before the anchor to include.
    pub before: u32,
    /// Messages after the anchor to include.
    pub after: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Context {
    pub session_id: SessionId,
    pub session_title: String,
    pub tool: Tool,
    pub project_id: ProjectId,
    pub workspace_id: WorkspaceId,
    /// The anchor message (None for an empty session).
    pub anchor: Option<MessageId>,
    pub messages: Vec<Message>,
    pub has_before: bool,
    pub has_after: bool,
    /// Pins on the shown messages: (message, pin).
    pub pins: Vec<(MessageId, PinId)>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PinKind {
    /// A whole message.
    Message,
    /// A selected part of a message.
    Excerpt,
    /// Text selected in a terminal; not linked to a message ("터미널 발췌").
    Terminal,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Pin {
    pub id: PinId,
    pub kind: PinKind,
    pub title: String,
    pub note: String,
    /// Snapshot of the pinned text, kept even when the source is gone.
    pub text: String,
    pub message_id: Option<MessageId>,
    pub session_id: Option<SessionId>,
    pub project_id: Option<ProjectId>,
    /// Session title when pinned.
    pub session_title: String,
    pub tool: Option<Tool>,
    /// Time of the pinned message (or of the pin for terminal excerpts).
    pub source_at: Ts,
    pub created_at: Ts,
    pub updated_at: Ts,
    /// The pinned message is still in the records. Always false for terminal excerpts.
    pub source_available: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PinSource {
    Message {
        message_id: MessageId,
    },
    Excerpt {
        message_id: MessageId,
        text: String,
    },
    Terminal {
        session_id: SessionId,
        text: String,
    },
    /// The newest assistant answer of a session (the session bar's 핀 button).
    LatestAnswer {
        session_id: SessionId,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NewPin {
    pub source: PinSource,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct PinQuery {
    #[serde(default)]
    pub project_id: Option<ProjectId>,
    #[serde(default)]
    pub session_id: Option<SessionId>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PinUpdate {
    pub id: PinId,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
}

/// The end of the newest completed turn of a session.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TurnInfo {
    pub session_id: SessionId,
    /// Newest assistant answer collected so far.
    pub last_answer: Option<Message>,
    /// When the CLI reported the newest finished turn (Claude turn end, Codex task complete).
    pub turn_ended_at: Option<Ts>,
}

method!(RecordsStatus, "records.status", StatusQuery => Vec<CaptureStatus>, [topic::MESSAGES, topic::SESSIONS]);
method!(RecordsSearch, "records.search", SearchQuery => SearchResult, [topic::MESSAGES, topic::PINS, topic::SESSIONS]);
method!(RecordsContext, "records.context", ContextQuery => Context, [topic::MESSAGES, topic::PINS, topic::SESSIONS]);
method!(RecordsTurn, "records.turn", ById => TurnInfo, [topic::MESSAGES]);
// Read the transcript of a session now (also for ended sessions, e.g. after "기록 파일 없음").
method!(RecordsCollect, "records.collect", ById => CaptureStatus);
method!(PinsList, "pins.list", PinQuery => Vec<Pin>, [topic::PINS, topic::MESSAGES]);
method!(PinsCreate, "pins.create", NewPin => Pin);
method!(PinsUpdate, "pins.update", PinUpdate => Pin);
// Unpin: removes the pin only; the message stays in the records.
method!(PinsRemove, "pins.remove", ById => ());
