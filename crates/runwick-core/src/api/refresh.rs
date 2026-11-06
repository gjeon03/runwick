//! Session refresh (S12): apply changed global environment variables and MCP configuration to
//! an existing conversation. The Runwick session, its tab, pins and records and the provider
//! conversation stay the same; only the run underneath is restarted (`sessions.restart` with
//! resume). Other sessions are never restarted by this flow.

use crate::Ts;
use crate::api::env::EnvChange;
use crate::api::mcp::{self, McpChange};
use crate::api::projects::{ProjectId, WorkspaceId};
use crate::api::sessions::{SessionId, Tool};
use crate::method;
use crate::rpc::topic;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Topic for refresh jobs (scheduled, progress, result).
pub const TOPIC: &str = "refresh";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    Starting,
    /// Producing output right now (a response or a tool run).
    Busy,
    /// Alive and quiet: waiting for input.
    Idle,
    /// Alive and showing a prompt only the person can answer (approval, login).
    NeedsAnswer,
    /// No run is alive.
    Ended,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Method {
    /// Restart the CLI and resume the same conversation by its saved id.
    Resume,
    /// Nothing was said yet (no saved conversation): start the CLI again in the same session.
    StartFresh,
    /// A shell keeps no conversation: a new shell starts; running commands and shell state end.
    RestartShell,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BlockerKind {
    /// No run is alive; the next start already uses the new settings.
    NotRunning,
    /// The provider conversation id is unknown, so the same conversation cannot be resumed.
    NoConversation,
    FolderMissing,
    CliMissing,
    /// The session runs on another device; refresh it there.
    Remote,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Blocker {
    pub kind: BlockerKind,
    /// Korean explanation for display.
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Plan {
    pub session: SessionId,
    pub project_id: ProjectId,
    pub workspace_id: WorkspaceId,
    pub title: String,
    pub tool: Tool,
    #[serde(default)]
    pub project_name: String,
    #[serde(default)]
    pub workspace_name: String,
    pub state: RunState,
    /// How the refresh would run; `None` when blocked.
    #[serde(default)]
    pub method: Option<Method>,
    #[serde(default)]
    pub blockers: Vec<Blocker>,
    /// Environment version of the current run and the current saved version.
    pub env_from: u64,
    pub env_to: u64,
    #[serde(default)]
    pub env_changes: Vec<EnvChange>,
    /// MCP definitions changed since the run started; `None` when the configuration at the start
    /// of the run is not known (it started before Runwick recorded it).
    #[serde(default)]
    pub mcp_changes: Option<Vec<McpChange>>,
    /// The current or last refresh of this session.
    #[serde(default)]
    pub job: Option<Job>,
    /// The person typed into the CLI within the last minute: an unsent draft may be lost by
    /// stopping the run now.
    #[serde(default)]
    pub input_recent: bool,
}

impl Plan {
    pub fn has_changes(&self) -> bool {
        !self.env_changes.is_empty() || self.mcp_changes.as_ref().is_some_and(|c| !c.is_empty())
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum When {
    /// Wait until the current response ends (output stops, no prompt is waiting).
    AfterResponse,
    /// Stop the current response and apply now.
    Now,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Target {
    pub session: SessionId,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartParams {
    pub session: SessionId,
    pub when: When,
    /// Only check settings and connections again (after a partial result), without restarting.
    #[serde(default)]
    pub verify_only: bool,
    pub cols: u16,
    pub rows: u16,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StepKind {
    /// Make sure the conversation is saved by the provider before stopping the run.
    SaveRecord,
    Restart,
    /// The same provider conversation is running again.
    Reconnect,
    /// The restarted CLI stayed up (no saved conversation to reconnect to).
    Alive,
    /// Environment version and MCP connections as far as they can be checked.
    Verify,
}

impl StepKind {
    pub fn label(self) -> &'static str {
        match self {
            StepKind::SaveRecord => "기록 저장",
            StepKind::Restart => "실행 다시 시작",
            StepKind::Reconnect => "같은 대화 다시 연결",
            StepKind::Alive => "CLI 실행 확인",
            StepKind::Verify => "설정 확인",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StepState {
    Pending,
    Running,
    Done,
    Failed,
    Skipped,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Step {
    pub kind: StepKind,
    pub state: StepState,
    #[serde(default)]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CheckState {
    Ok,
    NeedsAuth,
    Failed,
    /// Could not be checked (the CLI does not report it).
    Unknown,
}

/// One verified item after the restart (environment, one MCP server).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CheckItem {
    pub label: String,
    pub state: CheckState,
    #[serde(default)]
    pub detail: Option<String>,
    /// The MCP server this item is about, when it is about one.
    #[serde(default)]
    pub mcp: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum JobPhase {
    /// Waiting for the current response to end.
    Scheduled,
    Running,
    /// Every step and check passed.
    Done,
    /// Reconnected to the same conversation, but some checks did not pass.
    Partial,
    /// A step failed; the session keeps its records and can be retried.
    Failed,
    Cancelled,
}

impl JobPhase {
    pub fn is_active(self) -> bool {
        matches!(self, JobPhase::Scheduled | JobPhase::Running)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Job {
    pub id: Uuid,
    pub session: SessionId,
    pub when: When,
    pub phase: JobPhase,
    pub steps: Vec<Step>,
    #[serde(default)]
    pub checks: Vec<CheckItem>,
    pub created_at: Ts,
    #[serde(default)]
    pub finished_at: Option<Ts>,
    /// One-line Korean summary of the result.
    #[serde(default)]
    pub message: Option<String>,
}

/// A running session whose run started with older settings.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Pending {
    pub session: SessionId,
    pub project_id: ProjectId,
    pub workspace_id: WorkspaceId,
    pub title: String,
    pub tool: Tool,
    #[serde(default)]
    pub project_name: String,
    #[serde(default)]
    pub workspace_name: String,
    pub state: RunState,
    pub env_from: u64,
    #[serde(default)]
    pub env_changes: Vec<EnvChange>,
    #[serde(default)]
    pub mcp_changes: Vec<McpChange>,
    /// A refresh is scheduled or running.
    #[serde(default)]
    pub job: Option<JobPhase>,
}

method!(RefreshPlan, "refresh.plan", Target => Plan, [TOPIC, topic::ENV, topic::SESSIONS, mcp::TOPIC]);
method!(RefreshStart, "refresh.start", StartParams => Job);
method!(RefreshCancel, "refresh.cancel", Target => ());
method!(RefreshStatus, "refresh.status", Target => Option<Job>, [TOPIC]);
// Forget a finished job (the result card was closed).
method!(RefreshDismiss, "refresh.dismiss", Target => ());
method!(RefreshPending, "refresh.pending", () => Vec<Pending>, [TOPIC, topic::ENV, topic::SESSIONS, mcp::TOPIC]);
