//! Which CLI tools are installed on a host and what Runwick can do with each.

use crate::Ts;
use crate::api::sessions::Tool;
use crate::method;
use crate::rpc::topic;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AuthState {
    LoggedIn,
    NeedsLogin,
    /// We could not tell without starting the CLI.
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolSupport {
    /// Resume the same conversation by id.
    pub resume: bool,
    /// Runwick reads the provider transcript for search and pins.
    pub transcript: bool,
    /// Can be given work by another session.
    pub delegate: bool,
    /// Session refresh keeps the conversation (restart + resume).
    pub refresh: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolInfo {
    pub tool: Tool,
    pub installed: bool,
    pub path: Option<String>,
    pub version: Option<String>,
    pub auth: AuthState,
    pub support: ToolSupport,
    /// Short Korean note shown in settings, e.g. why something is unsupported.
    pub note: Option<String>,
    pub checked_at: Ts,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ToolsQuery {
    /// Re-run detection instead of returning the cached result.
    #[serde(default)]
    pub refresh: bool,
}

method!(ToolsList, "tools.list", ToolsQuery => Vec<ToolInfo>, [topic::TOOLS]);
