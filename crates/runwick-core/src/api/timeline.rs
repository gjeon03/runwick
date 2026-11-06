//! Timeline: what was observed per project and day, without AI — sessions started/ended,
//! conversation activity, pins, git commits, and events other features record (wiki changes,
//! collaboration results, file changes). AI day summaries are stored next to the observed records
//! and always marked as such; a day with records is never shown empty because a summary is
//! missing.
//!
//! Host-side entry points for other features (in `runwick_host::timeline`):
//! `record_event(store, &NewEvent)` and `store_summary(store, &Summary)`; emit
//! `topic::TIMELINE` afterwards.

use crate::Ts;
use crate::api::projects::{ById, ProjectId, WorkspaceId};
use crate::api::records::{MessageId, Pin, PinId};
use crate::api::sessions::{Session, SessionId};
use crate::method;
use crate::rpc::topic;
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    /// A session started or ended.
    Session,
    /// Conversation activity of one session on one day ("메시지 N개").
    Messages,
    Pin,
    Commit,
    /// A file changed (recorded by the feature that changed it).
    File,
    Wiki,
    Collab,
}

impl EventKind {
    pub const ALL: [EventKind; 7] = [
        EventKind::Session,
        EventKind::Messages,
        EventKind::Pin,
        EventKind::Commit,
        EventKind::File,
        EventKind::Wiki,
        EventKind::Collab,
    ];

    pub fn label(self) -> &'static str {
        match self {
            EventKind::Session => "세션",
            EventKind::Messages => "대화",
            EventKind::Pin => "핀",
            EventKind::Commit => "커밋",
            EventKind::File => "파일 변경",
            EventKind::Wiki => "위키",
            EventKind::Collab => "협업",
        }
    }
}

/// Where a timeline row leads.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EventLink {
    Session {
        id: SessionId,
    },
    Message {
        id: MessageId,
    },
    Pin {
        id: PinId,
    },
    Commit {
        sha: String,
    },
    File {
        #[serde(default)]
        workspace_id: Option<WorkspaceId>,
        path: String,
    },
    Wiki {
        page: String,
    },
    Collab {
        request: Uuid,
        #[serde(default)]
        session_id: Option<SessionId>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Event {
    /// Stable key for this row (for UI identity).
    pub key: String,
    pub kind: EventKind,
    pub title: String,
    pub at: Ts,
    pub project_id: ProjectId,
    #[serde(default)]
    pub session_id: Option<SessionId>,
    #[serde(default)]
    pub link: Option<EventLink>,
    /// Records this row stands for (a conversation row counts its messages).
    pub count: u32,
}

/// An event observed by another feature (wiki, collaboration, documents).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NewEvent {
    pub project_id: ProjectId,
    pub kind: EventKind,
    /// Short Korean line, e.g. `위키 "로그인과 세션" 제안 적용`.
    pub title: String,
    pub at: Ts,
    #[serde(default)]
    pub session_id: Option<SessionId>,
    #[serde(default)]
    pub link: Option<EventLink>,
    /// Recording the same key again replaces the earlier row instead of adding one.
    #[serde(default)]
    pub dedupe_key: Option<String>,
}

/// AI summary of one project day. Shown with its generation time and basis.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Summary {
    pub project_id: ProjectId,
    /// Local calendar day (`YYYY-MM-DD`).
    pub day: NaiveDate,
    pub text: String,
    pub generated_at: Ts,
    /// Number of records the summary was made from ("기록 N건 기준").
    pub basis_count: u32,
    /// CLI that wrote it ("Claude Code"), when known.
    #[serde(default)]
    pub provider: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct RangeQuery {
    /// First local day (inclusive).
    pub from: NaiveDate,
    /// Last local day (inclusive). At most 62 days after `from`.
    pub to: NaiveDate,
    /// One project, or every project with records in the range.
    #[serde(default)]
    pub project_id: Option<ProjectId>,
    /// The viewer's offset from UTC in minutes; days are cut in the viewer's time zone.
    pub utc_offset_min: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProjectDay {
    pub project_id: ProjectId,
    pub day: NaiveDate,
    /// Observed records of the day (messages count one each).
    pub count: u32,
    #[serde(default)]
    pub summary: Option<Summary>,
    /// Newest first.
    pub events: Vec<Event>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProjectRow {
    pub project_id: ProjectId,
    pub name: String,
    pub root_exists: bool,
    /// Why commits are not shown for this project ("폴더 없음", git error), when they are not.
    #[serde(default)]
    pub commits_note: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Range {
    pub from: NaiveDate,
    pub to: NaiveDate,
    pub projects: Vec<ProjectRow>,
    /// Only days with records.
    pub days: Vec<ProjectDay>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct NoteItem {
    pub text: String,
    pub done: bool,
}

/// The person's own memo and remaining items for a project.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct Note {
    pub memo: String,
    pub items: Vec<NoteItem>,
    #[serde(default)]
    pub updated_at: Option<Ts>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LastWork {
    pub session: Session,
    pub workspace_name: String,
    pub workspace_exists: bool,
    /// The run is alive now ("살아 있는 실행 열기").
    pub running: bool,
    pub last_at: Ts,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Resume {
    pub project_id: ProjectId,
    pub project_name: String,
    pub root_exists: bool,
    pub last: Option<LastWork>,
    pub note: Note,
    /// Newest AI day summary of the project.
    pub summary: Option<Summary>,
    /// Recent pins of the project, newest first (to choose handover material).
    pub pins: Vec<Pin>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SaveNote {
    pub project_id: ProjectId,
    pub memo: String,
    pub items: Vec<NoteItem>,
}

method!(TimelineRange, "timeline.range", RangeQuery => Range,
    [topic::TIMELINE, topic::MESSAGES, topic::PINS, topic::SESSIONS, topic::PROJECTS]);
method!(TimelineResume, "timeline.resume", ById => Resume,
    [topic::TIMELINE, topic::SESSIONS, topic::PINS, topic::PROJECTS, topic::WORKSPACES]);
method!(TimelineSaveNote, "timeline.save_note", SaveNote => Note);
