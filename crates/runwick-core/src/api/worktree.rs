//! "분리해서 작업": a separate git worktree as its own workspace, and bringing its work back
//! (local merge only; commit, push, PR and deploy are separate actions).
//!
//! Nothing here stashes, copies `.env` files, installs dependencies or starts servers. Carrying
//! uncommitted changes means applying a patch of them in the new folder; the source folder is
//! left as it was.

use crate::api::collab::ChangedFile;
use crate::api::projects::{ById, Workspace, WorkspaceId};
use crate::api::sessions::{SessionId, SessionInfo, Tool};
use crate::method;
use crate::rpc::topic;
use serde::{Deserialize, Serialize};

/// What a new worktree would start from.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Plan {
    pub workspace_id: WorkspaceId,
    pub is_git: bool,
    /// Why splitting is not possible here (non-git folder, missing folder).
    pub reason: Option<String>,
    pub repo_root: Option<String>,
    pub base_branch: Option<String>,
    /// Short HEAD revision.
    pub base_rev: Option<String>,
    /// `git status --porcelain` lines of tracked changes ("M src/a.rs").
    pub dirty: Vec<String>,
    pub untracked: u32,
    pub suggested_name: String,
    pub suggested_branch: String,
    /// `<repo>.worktrees/<name>` next to the repository.
    pub suggested_path: String,
    /// Local branches (for "기존 branch 쓰기").
    pub branches: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Create {
    /// The workspace to split from.
    pub workspace_id: WorkspaceId,
    pub name: String,
    pub branch: String,
    /// Start point (branch or revision); defaults to the workspace's HEAD.
    #[serde(default)]
    pub base: Option<String>,
    /// Check out an existing branch instead of creating `branch`.
    #[serde(default)]
    pub use_existing_branch: bool,
    /// Folder to create; defaults to the planned path.
    #[serde(default)]
    pub path: Option<String>,
    /// Apply the source's uncommitted tracked changes as a patch in the new folder.
    #[serde(default)]
    pub carry_changes: bool,
    /// CLI to start in the new workspace.
    #[serde(default)]
    pub tool: Option<Tool>,
    #[serde(default)]
    pub cols: u16,
    #[serde(default)]
    pub rows: u16,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StepKind {
    CheckGit,
    CreateWorktree,
    CarryChanges,
    Link,
    StartCli,
}

impl StepKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::CheckGit => "Git 저장소 확인",
            Self::CreateWorktree => "worktree 생성",
            Self::CarryChanges => "커밋하지 않은 변경 적용",
            Self::Link => "공간 연결",
            Self::StartCli => "CLI 시작",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StepState {
    Done,
    Failed,
    /// Not run because an earlier step failed or it was not asked for.
    Skipped,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Step {
    pub kind: StepKind,
    pub state: StepState,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Created {
    pub steps: Vec<Step>,
    /// Present once the worktree is linked, even if starting the CLI failed afterwards.
    pub workspace: Option<Workspace>,
    pub session: Option<SessionInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RunningSession {
    pub id: SessionId,
    pub title: String,
    pub tool: Tool,
}

/// What removing a worktree folder would affect.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RemoveCheck {
    pub workspace_id: WorkspaceId,
    pub path: String,
    pub path_exists: bool,
    pub branch: Option<String>,
    /// Only worktree workspaces can be removed here.
    pub removable: bool,
    pub reason: Option<String>,
    pub running: Vec<RunningSession>,
    pub dirty: Vec<String>,
    pub untracked: u32,
    /// Commits not on the upstream (or, without one, not on the base branch). None: unknown.
    pub unpushed: Option<u32>,
    pub upstream: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Remove {
    pub workspace_id: WorkspaceId,
    /// Remove even with uncommitted or untracked files (`git worktree remove --force`).
    #[serde(default)]
    pub force: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Commit {
    pub rev: String,
    pub subject: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MergePreview {
    pub workspace_id: WorkspaceId,
    pub source_branch: Option<String>,
    pub source_rev: Option<String>,
    /// Branch of the project's default workspace that receives the merge.
    pub target_branch: Option<String>,
    pub target_rev: Option<String>,
    pub target_path: String,
    pub merge_base: Option<String>,
    pub commits: Vec<Commit>,
    pub files: Vec<ChangedFile>,
    /// Uncommitted files in the worktree (not part of a merge).
    pub source_dirty: u32,
    /// Uncommitted tracked changes in the target folder (a merge is refused then).
    pub target_dirty: Vec<String>,
    /// A merge is in progress in the target folder.
    pub in_progress: bool,
    pub conflicts: Vec<String>,
    /// Why merging is not possible here.
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MergeOutcome {
    pub merged: bool,
    pub conflicts: Vec<String>,
    pub message: String,
    pub head: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConflictQuery {
    pub workspace_id: WorkspaceId,
    pub path: String,
}

method!(WorktreePlan, "worktree.plan", ById => Plan, [topic::WORKSPACES]);
method!(WorktreeCreate, "worktree.create", Create => Created);
// Uncommitted changes of a workspace as a patch (for "변경 보기").
method!(WorktreeChanges, "worktree.changes", ById => String, [topic::WORKSPACES]);
method!(WorktreeRemoveCheck, "worktree.remove_check", ById => RemoveCheck, [topic::WORKSPACES, topic::SESSIONS]);
method!(WorktreeRemove, "worktree.remove", Remove => ());
method!(WorktreeMergePreview, "worktree.merge_preview", ById => MergePreview, [topic::WORKSPACES]);
method!(WorktreeMerge, "worktree.merge", ById => MergeOutcome);
method!(WorktreeMergeAbort, "worktree.merge_abort", ById => ());
// Text of a conflicted file in the target folder (with conflict markers).
method!(WorktreeConflict, "worktree.conflict", ConflictQuery => String, [topic::WORKSPACES]);
