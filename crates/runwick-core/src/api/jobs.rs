//! Automatic and requested AI work (wiki proposals, wiki drafts, timeline day summaries).
//!
//! Automatic work starts only for projects that opted in, while on AC power and after the
//! project has been quiet for a while. Work the person asks for ("지금 정리") always runs,
//! also on battery. AI calls run the person's own CLI non-interactively, so they use the
//! CLI's subscription; Runwick never converts that to money.

use crate::Ts;
use crate::api::projects::ProjectId;
use crate::api::sessions::SessionId;
use crate::method;
use crate::rpc::topic;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub type JobId = Uuid;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    /// Read new conversation records and leave wiki change proposals.
    WikiProposals,
    /// Write a new draft page from chosen sessions (always requested by the person).
    WikiDraft,
    /// Summarize one finished day of the project's timeline.
    TimelineSummary,
}

impl JobKind {
    pub fn label(self) -> &'static str {
        match self {
            JobKind::WikiProposals => "위키 변경분 정리",
            JobKind::WikiDraft => "세션으로 위키 초안",
            JobKind::TimelineSummary => "타임라인 요약",
        }
    }
}

/// Why the job started.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Trigger {
    /// Started by the project's automation settings.
    Auto,
    /// The person asked for it (runs on battery too).
    Requested,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Queued,
    Running,
    Done,
    Failed,
    /// Waiting for a condition; `held_reason` says which.
    Held,
    /// Stopped by the person or by a host restart.
    Stopped,
}

impl JobStatus {
    pub fn is_active(self) -> bool {
        matches!(self, Self::Queued | Self::Running | Self::Held)
    }
}

/// What the job works on.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct JobTarget {
    /// Local calendar day (`YYYY-MM-DD`) for timeline summaries.
    #[serde(default)]
    pub day: Option<String>,
    /// Sessions to draft a wiki page from.
    #[serde(default)]
    pub sessions: Vec<SessionId>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Job {
    pub id: JobId,
    pub kind: JobKind,
    pub project_id: ProjectId,
    /// Project name at the time of listing, for display.
    #[serde(default)]
    pub project_name: Option<String>,
    pub trigger: Trigger,
    /// Why it started, in Korean ("변경 기록 24건이 쌓였고 작업이 한가해짐").
    pub reason: String,
    pub target: JobTarget,
    pub status: JobStatus,
    pub held_reason: Option<String>,
    pub attempts: u32,
    /// When a failed attempt will be retried automatically.
    pub retry_at: Option<Ts>,
    pub created_at: Ts,
    pub started_at: Option<Ts>,
    pub finished_at: Option<Ts>,
    /// Short Korean result ("제안 2건", "요약 저장").
    pub result: Option<String>,
    pub error: Option<String>,
    /// CLI used for the AI call ("Claude Code", "Codex").
    pub provider: Option<String>,
    /// Model reported by the CLI, when it says.
    pub model: Option<String>,
    pub duration_ms: Option<u64>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PowerSource {
    Ac,
    Battery,
    /// Could not be read here; treated as AC for automation.
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Overview {
    pub power: PowerSource,
    pub power_checked_at: Option<Ts>,
    pub running: u32,
    pub queued: u32,
    pub held: u32,
    /// Most jobs that run at once / of those, automatic ones.
    pub max_running: u32,
    pub max_auto: u32,
    /// CLI that AI work would use now, if any is installed.
    pub provider: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProjectAutomation {
    pub project_id: ProjectId,
    pub project_name: String,
    pub wiki: bool,
    pub timeline: bool,
    /// Last finished automatic or requested job per kind.
    #[serde(default)]
    pub last_wiki_at: Option<Ts>,
    #[serde(default)]
    pub last_timeline_at: Option<Ts>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SetAutomation {
    pub project_id: ProjectId,
    #[serde(default)]
    pub wiki: Option<bool>,
    #[serde(default)]
    pub timeline: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct JobQuery {
    #[serde(default)]
    pub project_id: Option<ProjectId>,
    /// Only queued, running and held jobs.
    #[serde(default)]
    pub active_only: bool,
    /// Newest first; 0 means the default (50).
    #[serde(default)]
    pub limit: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestJob {
    pub kind: JobKind,
    pub project_id: ProjectId,
    #[serde(default)]
    pub target: JobTarget,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct JobRef {
    pub id: JobId,
}

method!(JobsList, "jobs.list", JobQuery => Vec<Job>, [topic::JOBS]);
method!(JobsOverview, "jobs.overview", () => Overview, [topic::JOBS]);
// "지금 정리": run now on the person's request (allowed on battery).
method!(JobsRequest, "jobs.request", RequestJob => Job);
// Stop a queued, held or running job (kills the CLI process).
method!(JobsStop, "jobs.stop", JobRef => Job);
// Run a failed or stopped job again as a request.
method!(JobsRetry, "jobs.retry", JobRef => Job);
method!(AutomationList, "automation.list", () => Vec<ProjectAutomation>, [topic::JOBS, topic::PROJECTS]);
method!(AutomationSet, "automation.set", SetAutomation => ProjectAutomation);
