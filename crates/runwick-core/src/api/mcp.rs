//! MCP servers as each provider CLI really uses them. Runwick only reads the providers' own
//! configuration (Claude Code: `~/.claude.json` user and local scopes, `<project>/.mcp.json`;
//! Codex: `~/.codex/config.toml`) and asks the CLI itself for connection status on demand.
//! Changes are made in the provider's files; Runwick shows which runs started with an older
//! configuration so they can be refreshed.

use crate::Ts;
use crate::api::sessions::Tool;
use crate::method;
use serde::{Deserialize, Serialize};

/// Topic for MCP configuration and status checks.
pub const TOPIC: &str = "mcp";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum McpScope {
    /// Every project of this user.
    User,
    /// Only this project, private to this user (Claude `projects[<path>]`).
    Local,
    /// Shared with the project (`.mcp.json` in the folder).
    Project,
    /// Reported by the CLI but not defined in a file Runwick reads (plugins, connectors).
    Other,
}

impl McpScope {
    pub fn label(self) -> &'static str {
        match self {
            McpScope::User => "사용자",
            McpScope::Local => "로컬",
            McpScope::Project => "프로젝트",
            McpScope::Other => "CLI 제공",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum McpTransport {
    Stdio,
    Http,
    Sse,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct McpServer {
    pub provider: Tool,
    pub name: String,
    pub scope: McpScope,
    /// Config file that defines it (display path), `None` when only the CLI reported it.
    #[serde(default)]
    pub location: Option<String>,
    pub transport: McpTransport,
    /// Command line or URL for display. Environment values and headers are never included.
    pub target: String,
    /// Names of environment variables or headers given to the server (values hidden).
    #[serde(default)]
    pub secret_keys: Vec<String>,
    pub enabled: bool,
}

/// One configuration file Runwick read.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct McpSource {
    pub provider: Tool,
    pub scope: McpScope,
    /// Absolute path (for "파일 열기").
    pub path: String,
    /// Path for display (`~/…`).
    pub display: String,
    pub exists: bool,
    /// The file exists but could not be read or understood.
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct McpQuery {
    /// Project folder for the local and project scopes; only user scope without it.
    #[serde(default)]
    pub cwd: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct McpConfig {
    pub servers: Vec<McpServer>,
    pub sources: Vec<McpSource>,
    pub read_at: Ts,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum McpHealth {
    Connected,
    Failed,
    NeedsAuth,
    /// A project server the person has not approved yet (Claude `.mcp.json`).
    PendingApproval,
    Disabled,
    /// The CLI did not say (for example Codex does not test connections).
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct McpStatus {
    pub name: String,
    pub health: McpHealth,
    /// The CLI's own words after the status (error reason), when it gave any.
    #[serde(default)]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CheckQuery {
    pub provider: Tool,
    #[serde(default)]
    pub cwd: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct McpCheckResult {
    pub provider: Tool,
    #[serde(default)]
    pub cwd: Option<String>,
    pub checked_at: Ts,
    pub statuses: Vec<McpStatus>,
    /// Why the check could not run (CLI missing, timed out). Statuses are empty then.
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum McpChangeKind {
    Added,
    Changed,
    Removed,
}

/// A server definition that changed since a run started.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpChange {
    pub provider: Tool,
    pub scope: McpScope,
    pub name: String,
    pub kind: McpChangeKind,
}

method!(McpList, "mcp.list", McpQuery => McpConfig, [TOPIC]);
// Ask the provider CLI for connection status (`claude mcp list`, `codex mcp list --json`).
// Slow (seconds); the result is kept and returned by `mcp.last_check`.
method!(McpCheck, "mcp.check", CheckQuery => McpCheckResult);
method!(McpLastCheck, "mcp.last_check", CheckQuery => Option<McpCheckResult>, [TOPIC]);
