//! Delegation between sessions. A session (usually its CLI, through the `runwick` command)
//! hands work to a worker session; the host delivers it, follows the worker's turn and keeps
//! what came back. Each hand-over is a [`Request`] (one attempt); a retry is a new request that
//! points at the previous one.
//!
//! "응답 완료" ([`ReqStatus::Done`]) means the worker answered. It never means the work was
//! verified; verification is what the worker reported in [`WorkResult::checks`].

use crate::Ts;
use crate::api::projects::{ById, WorkspaceId};
use crate::api::sessions::{SessionId, SessionInfo, Tool};
use crate::method;
use crate::rpc::topic;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub type RequestId = Uuid;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ReqStatus {
    /// Not delivered yet; `reason` says what it waits for.
    Queued,
    /// Written to the worker (or given at launch); no response observed yet.
    Delivered,
    /// The worker is responding.
    Running,
    /// The worker shows a prompt only a person can answer (approval, trust, login).
    NeedsInput,
    /// The worker finished its turn (answered). Not "verified".
    Done,
    /// The worker's run ended or could not start before it answered.
    Failed,
    /// Cancelled before delivery by a person.
    Cancelled,
    /// Written, but no response was observed in time. Never re-sent automatically.
    Uncertain,
}

impl ReqStatus {
    /// Holds an assignment slot (counts against the automatic assignment limit).
    pub fn holds_slot(self) -> bool {
        matches!(self, Self::Delivered | Self::Running | Self::NeedsInput | Self::Uncertain)
    }

    /// Still going or waiting (not finished, failed or cancelled).
    pub fn is_open(self) -> bool {
        !matches!(self, Self::Done | Self::Failed | Self::Cancelled)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Queued => "대기",
            Self::Delivered => "전달됨 · 응답 대기",
            Self::Running => "진행 중",
            Self::NeedsInput => "답변 필요",
            Self::Done => "응답 완료",
            Self::Failed => "실패",
            Self::Cancelled => "취소됨",
            Self::Uncertain => "전달 확인 필요",
        }
    }
}

/// Who asked for the work.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// A CLI through the `runwick` command.
    #[default]
    Agent,
    /// A person in the app.
    Person,
}

/// How the instruction reached the worker.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Via {
    /// First prompt on the worker's command line.
    Launch,
    /// Typed into the worker's terminal (bracketed paste + Enter).
    Typed,
}

/// Where a result summary came from.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SummaryFrom {
    /// The worker ran `runwick report`.
    Report,
    /// The worker's newest answer in its conversation record.
    Transcript,
    /// Last lines of the worker's screen (no record available).
    Screen,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChangedFile {
    pub path: String,
    /// Git status letter: A, M, D, R, ? (untracked).
    pub status: String,
    #[serde(default)]
    pub added: Option<u32>,
    #[serde(default)]
    pub removed: Option<u32>,
}

/// A verification the worker says it ran.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Check {
    pub command: String,
    pub passed: bool,
    /// HEAD of the worker's folder when it reported.
    #[serde(default)]
    pub revision: Option<String>,
    /// Working tree fingerprint when it reported; differs once files change afterwards.
    #[serde(default)]
    pub fingerprint: Option<String>,
    /// Last lines of the log the worker pointed at.
    #[serde(default)]
    pub log: Option<String>,
    pub at: Ts,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct WorkResult {
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub summary_from: Option<SummaryFrom>,
    /// `git diff --name-status` from the start revision plus untracked files.
    #[serde(default)]
    pub files: Vec<ChangedFile>,
    /// Files the worker named in its report.
    #[serde(default)]
    pub reported_files: Vec<String>,
    #[serde(default)]
    pub base_rev: Option<String>,
    #[serde(default)]
    pub head_rev: Option<String>,
    #[serde(default)]
    pub checks: Vec<Check>,
    #[serde(default)]
    pub not_done: Vec<String>,
    /// Working tree fingerprint when `files` were read.
    #[serde(default)]
    pub fingerprint: Option<String>,
    #[serde(default)]
    pub collected_at: Option<Ts>,
    /// Another run was seen working in the same folder while this attempt was open: `files` may
    /// include its changes (they are read from the folder, not from the worker).
    #[serde(default)]
    pub shared_folder: bool,
}

impl WorkResult {
    /// Changes in `files` can be called this worker's own: it named files itself, or nobody else
    /// worked in its folder meanwhile.
    pub fn changes_are_its_own(&self) -> bool {
        !self.reported_files.is_empty() || !self.shared_folder
    }
}

/// How a failed attempt is described: what it left behind is only "partial results" when the
/// changes can be attributed to it.
pub fn failure_label(result: Option<&WorkResult>) -> &'static str {
    match result {
        Some(r) if !r.files.is_empty() && r.changes_are_its_own() => "일부 결과 후 실패",
        Some(r) if !r.files.is_empty() => "변경 출처 확인 필요",
        _ => "실패",
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Note {
    pub at: Ts,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Request {
    pub id: RequestId,
    /// The delegating session (None when a person assigned the work in the app).
    pub parent_id: Option<SessionId>,
    pub worker_id: SessionId,
    /// The attempt this one continues or replaces.
    #[serde(default)]
    pub previous_id: Option<RequestId>,
    pub attempt: u32,
    pub title: String,
    pub role: Option<String>,
    pub tool: Tool,
    pub prompt: String,
    pub source: Source,
    pub via: Via,
    /// Asked to interrupt the worker's current answer first (`--now`).
    pub interrupt: bool,
    pub status: ReqStatus,
    /// Why it waits, failed or is uncertain, in Korean (shown as-is).
    pub reason: Option<String>,
    /// HEAD of the worker's folder when the work was handed over.
    pub start_rev: Option<String>,
    pub created_at: Ts,
    pub delivered_at: Option<Ts>,
    pub finished_at: Option<Ts>,
    pub updated_at: Ts,
    pub result: Option<WorkResult>,
    /// The person marked the result as seen. Separate from verification.
    pub acknowledged: bool,
    /// Person actions linked to this attempt ("직접 대화 후 다시 맡김").
    #[serde(default)]
    pub notes: Vec<Note>,
}

impl Request {
    /// Short receipt id shown to people and agents ("r-0414abcd").
    pub fn receipt(&self) -> String {
        format!("r-{}", &self.id.simple().to_string()[24..])
    }
}

fn default_cols() -> u16 {
    120
}

fn default_rows() -> u16 {
    32
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Delegate {
    /// The delegating session (`RUNWICK_SESSION_ID` of the caller).
    #[serde(default)]
    pub parent_id: Option<SessionId>,
    /// Workspace to work in; defaults to the parent's workspace.
    #[serde(default)]
    pub workspace_id: Option<WorkspaceId>,
    pub tool: Tool,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    pub prompt: String,
    /// Work in a new worktree on this new branch ("분리해서 작업").
    #[serde(default)]
    pub split: Option<String>,
    /// Idempotency key: the same key from the same parent returns the existing request.
    #[serde(default)]
    pub key: Option<String>,
    #[serde(default)]
    pub source: Source,
    #[serde(default = "default_cols")]
    pub cols: u16,
    #[serde(default = "default_rows")]
    pub rows: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Delegated {
    pub request: Request,
    pub worker: SessionInfo,
    /// The key matched an earlier request; nothing new was started.
    pub reused: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendTo {
    pub worker_id: SessionId,
    /// Who sends it (the caller's session), when known.
    #[serde(default)]
    pub parent_id: Option<SessionId>,
    pub prompt: String,
    /// Interrupt the worker's current answer first, when allowed.
    #[serde(default)]
    pub now: bool,
    #[serde(default)]
    pub key: Option<String>,
    #[serde(default)]
    pub source: Source,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WaitFor {
    /// A request id, or a worker session id (its newest request).
    pub id: Uuid,
    /// Seconds to wait; 0 means the default (600). At most 3600.
    #[serde(default)]
    pub timeout_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Waited {
    pub request: Request,
    /// Still open when the wait ended. Not a failure: wait again or look at the worker.
    pub timed_out: bool,
    pub answer: Option<String>,
    pub answer_from: Option<SummaryFrom>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckReport {
    pub command: String,
    pub passed: bool,
    /// Not read by the host any more (it never opens a path a worker names); kept so older
    /// commands still parse. The `runwick` command sends [`CheckReport::log`] instead.
    #[serde(default)]
    pub log_path: Option<String>,
    /// The end of the check's log, read by the `runwick` command from a regular file inside the
    /// worker's folder (the host keeps at most 64 KB).
    #[serde(default)]
    pub log: Option<String>,
}

/// `runwick report`: the worker's structured result for its current request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Report {
    pub worker_id: SessionId,
    pub summary: String,
    #[serde(default)]
    pub files: Vec<String>,
    #[serde(default)]
    pub checks: Vec<CheckReport>,
    #[serde(default)]
    pub not_done: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RequestQuery {
    #[serde(default)]
    pub parent_id: Option<SessionId>,
    #[serde(default)]
    pub worker_id: Option<SessionId>,
    /// Only queued and running ones.
    #[serde(default)]
    pub open_only: bool,
    /// Include every open request and unread person-started result, plus bounded recent history.
    #[serde(default)]
    pub activity: bool,
    /// Newest first; 0 means the default (200).
    #[serde(default)]
    pub limit: u32,
}

/// "새 배정 보류": queued work of this parent waits; work already delivered continues.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hold {
    pub parent_id: SessionId,
    pub held: bool,
}

/// A person talks to the worker directly; automatic instructions wait until released.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Direct {
    pub worker_id: SessionId,
    pub on: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ack {
    pub id: RequestId,
    pub acknowledged: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RetryMode {
    /// "이어서 수정": the same worker continues from what is there (new attempt).
    Continue,
    /// "처음부터 다시": a new worker starts the original task (previous attempt kept).
    Fresh,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Retry {
    pub id: RequestId,
    pub mode: RetryMode,
    /// Extra instruction from the person.
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default = "default_cols")]
    pub cols: u16,
    #[serde(default = "default_rows")]
    pub rows: u16,
}

/// A request with what can be said about it now.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RequestView {
    pub request: Request,
    /// The worker's folder exists on this host.
    pub folder_exists: bool,
    pub worker_cwd: String,
    /// Files changed after the reported checks: earlier passes no longer apply.
    pub checks_stale: bool,
    /// The worker's newest answer from its record (or screen) when it has not reported.
    pub answer: Option<String>,
    pub answer_from: Option<SummaryFrom>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CollabSettings {
    /// Tell agent CLIs about `runwick delegate/wait/report` when they start.
    pub guidance: bool,
    /// Most runs busy at once before new automatic assignments wait (agent-assigned work; a
    /// person's own requests are not held back).
    pub auto_limit: u32,
    /// Tools a delegation may start.
    pub allowed_tools: Vec<Tool>,
}

impl Default for CollabSettings {
    fn default() -> Self {
        Self { guidance: true, auto_limit: 3, allowed_tools: vec![Tool::Claude, Tool::Codex] }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Overview {
    pub settings: CollabSettings,
    /// Workers holding an assignment slot right now.
    pub assigned: u32,
    /// Agent CLI runs observed responding right now (direct runs included).
    pub observed_busy: u32,
    /// Runs counted against the automatic assignment limit: slot holders and observed agent
    /// runs together, each once. At or over the limit, new automatic assignments wait.
    #[serde(default)]
    pub in_use: u32,
    pub held_parents: Vec<SessionId>,
    pub direct_workers: Vec<SessionId>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffQuery {
    pub id: RequestId,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FileDiff {
    pub path: String,
    /// Unified diff against the request's start revision (whole file for untracked files).
    pub text: String,
    pub binary: bool,
    pub truncated: bool,
}

/// Prefix of a note a worker left for the session that gave it work (`runwick send <parent>`):
/// shown to the person, never typed into the parent.
pub const WORKER_MESSAGE: &str = "작업자 메시지: ";

/// The `runwick` command proves its session with the run's token
/// ([`crate::paths::ENV_SESSION_TOKEN`]). Never makes the connection a window.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Identify {
    pub token: String,
}

// Returns the session the token belongs to (`None`: not a live run's token).
method!(CollabIdentify, "collab.identify", Identify => Option<SessionId>);
method!(CollabDelegate, "collab.delegate", Delegate => Delegated);
method!(CollabSend, "collab.send", SendTo => Request);
// Long wait (up to `timeout_secs`) until the request is answered, fails or becomes uncertain.
method!(CollabWait, "collab.wait", WaitFor => Waited);
// A request id, or a worker session id (its newest request).
method!(CollabGet, "collab.get", ById => RequestView, [topic::COLLAB, topic::MESSAGES]);
method!(CollabList, "collab.list", RequestQuery => Vec<Request>, [topic::COLLAB]);
method!(CollabReport, "collab.report", Report => Request);
// Cancel a request that has not been delivered.
method!(CollabCancel, "collab.cancel", ById => Request);
method!(CollabHold, "collab.hold", Hold => ());
method!(CollabDirect, "collab.direct", Direct => ());
method!(CollabAck, "collab.ack", Ack => Request);
// Called by the window after displaying a result. Read state is separate from verification.
method!(CollabRead, "collab.read", ById => Request);
method!(CollabRetry, "collab.retry", Retry => Delegated);
// Look at an uncertain request again (never re-sends).
method!(CollabCheck, "collab.check", ById => Request);
method!(CollabOverview, "collab.overview", () => Overview, [topic::COLLAB, topic::SESSIONS, topic::SETTINGS]);
method!(CollabSettingsGet, "collab.settings", () => CollabSettings, [topic::SETTINGS]);
method!(CollabSettingsSet, "collab.set_settings", CollabSettings => CollabSettings);
method!(CollabDiff, "collab.diff", DiffQuery => FileDiff, [topic::COLLAB]);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn receipt_is_short_and_stable() {
        let id: Uuid = "0192f0a1-0000-7000-8000-00000000abcd".parse().unwrap();
        let now = crate::now();
        let r = Request {
            id,
            parent_id: None,
            worker_id: Uuid::nil(),
            previous_id: None,
            attempt: 1,
            title: "t".into(),
            role: None,
            tool: Tool::Codex,
            prompt: "p".into(),
            source: Source::Agent,
            via: Via::Typed,
            interrupt: false,
            status: ReqStatus::Queued,
            reason: None,
            start_rev: None,
            created_at: now,
            delivered_at: None,
            finished_at: None,
            updated_at: now,
            result: None,
            acknowledged: false,
            notes: vec![],
        };
        assert_eq!(r.receipt(), "r-0000abcd");
        let file = ChangedFile { path: "a.rs".into(), status: "M".into(), added: None, removed: None };
        let mut res = WorkResult { files: vec![file], ..Default::default() };
        assert_eq!(failure_label(Some(&res)), "일부 결과 후 실패");
        res.shared_folder = true;
        assert_eq!(failure_label(Some(&res)), "변경 출처 확인 필요", "shared-folder diffs alone are not its results");
        res.reported_files = vec!["a.rs".into()];
        assert_eq!(failure_label(Some(&res)), "일부 결과 후 실패", "files it named itself are");
        assert_eq!(failure_label(Some(&WorkResult::default())), "실패");
        assert_eq!(failure_label(None), "실패");
        assert!(ReqStatus::Uncertain.holds_slot() && !ReqStatus::Queued.holds_slot());
        assert!(!ReqStatus::Done.is_open() && ReqStatus::Uncertain.is_open());
    }
}
