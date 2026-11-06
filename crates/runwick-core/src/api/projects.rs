//! Projects (a folder or repository) and their workspaces (the actual working folder that
//! sessions and documents belong to). Opening a folder creates the default workspace.

use crate::Ts;
use crate::method;
use crate::rpc::topic;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub type ProjectId = Uuid;
pub type WorkspaceId = Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Project {
    pub id: ProjectId,
    pub name: String,
    /// Absolute folder on the host that owns this record.
    pub root: String,
    pub is_git: bool,
    pub favorite: bool,
    pub archived: bool,
    pub created_at: Ts,
    pub last_opened_at: Option<Ts>,
    /// Whether `root` exists right now (records stay readable when it does not).
    #[serde(default)]
    pub root_exists: bool,
    /// `origin` remote URL when known; used to match projects across computers.
    #[serde(default)]
    pub remote_url: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceKind {
    /// Created automatically for the project folder.
    Default,
    /// A separate git worktree made by "분리해서 작업".
    Worktree,
}

/// A workspace whose folder and runs live on another registered device.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RemoteLink {
    pub device_id: Uuid,
    /// Workspace id inside the remote host's own records.
    pub remote_workspace_id: WorkspaceId,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Workspace {
    pub id: WorkspaceId,
    pub project_id: ProjectId,
    pub name: String,
    pub path: String,
    pub kind: WorkspaceKind,
    /// Current branch read from git (None for non-git folders or detached HEAD).
    #[serde(default)]
    pub branch: Option<String>,
    /// Branch or revision the worktree started from.
    #[serde(default)]
    pub base_ref: Option<String>,
    pub archived: bool,
    pub created_at: Ts,
    pub last_used_at: Option<Ts>,
    #[serde(default)]
    pub path_exists: bool,
    #[serde(default)]
    pub remote: Option<RemoteLink>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct OpenFolder {
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Opened {
    pub project: Project,
    pub workspace: Workspace,
    /// False when the folder was already known (re-opening moves to the existing project).
    pub created: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProjectUpdate {
    pub id: ProjectId,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub favorite: Option<bool>,
    #[serde(default)]
    pub archived: Option<bool>,
    /// Re-link the project to a folder (for example after moving it or on another computer).
    #[serde(default)]
    pub root: Option<String>,
    #[serde(default)]
    pub touch: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WorkspaceQuery {
    #[serde(default)]
    pub project_id: Option<ProjectId>,
    #[serde(default)]
    pub include_archived: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WorkspaceUpdate {
    pub id: WorkspaceId,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub archived: Option<bool>,
    #[serde(default)]
    pub touch: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ById {
    pub id: Uuid,
}

method!(ProjectsList, "projects.list", () => Vec<Project>, [topic::PROJECTS]);
method!(ProjectsOpen, "projects.open", OpenFolder => Opened);
method!(ProjectsUpdate, "projects.update", ProjectUpdate => Project);
method!(WorkspacesList, "workspaces.list", WorkspaceQuery => Vec<Workspace>, [topic::WORKSPACES, topic::PROJECTS]);
method!(WorkspacesUpdate, "workspaces.update", WorkspaceUpdate => Workspace);
