//! UI state shared by the shell and feature views. Feature-specific state lives in each
//! feature's own `State` (see [`Features`]); keep this file to cross-cutting structure.

use crate::client::HostKey;
use runwick_core::api::projects::{ProjectId, WorkspaceId};
use runwick_core::api::sessions::SessionId;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

pub type TabId = u64;

/// A workspace as seen by the GUI: which host runs it and its id on that host.
/// For a remote workspace this is the device and the workspace id inside that device's host.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct WsKey {
    pub host: HostKey,
    pub id: WorkspaceId,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TabKind {
    /// A CLI session (Claude, Codex, shell) running on `host`.
    Terminal { host: HostKey, session: SessionId },
    /// A file in a workspace (edit Markdown/text, view code).
    Doc { ws: WsKey, path: String },
    /// Project wiki: list and a page.
    Wiki { project: ProjectId, page: Option<String> },
    /// Wiki relationship graph.
    WikiGraph { project: ProjectId },
    /// Timeline for one project or all projects.
    Timeline { project: Option<ProjectId> },
    /// Conversation search expanded to the center.
    History,
    /// Result of delegated work (S11).
    Result { host: HostKey, request: Uuid },
    /// Merge/integration of a separated workspace (S11 merge).
    Merge { ws: WsKey },
    /// Start screen inside a workspace (pick a CLI or open files).
    Start,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Tab {
    pub id: TabId,
    pub kind: TabKind,
}

/// Tabs of one workspace. Switching workspaces switches the whole set (S01).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TabSet {
    pub tabs: Vec<Tab>,
    pub active: Option<TabId>,
    /// Second pane for side-by-side (terminal + document).
    #[serde(default)]
    pub split: Option<TabId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum SidebarMode {
    #[default]
    Sessions,
    Files,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ProjectFilter {
    #[default]
    All,
    Favorites,
}

/// Right-hand detail panel (S04, S07, S11...). Closed by default.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum RightPanel {
    /// Conversation search, context and pins (S07).
    History,
    /// Delegation detail for a session (S04).
    Collab { host: HostKey, session: SessionId },
    /// Result detail (S11).
    Result { host: HostKey, request: Uuid },
    /// All activity (S06).
    Activity,
    /// A device's workspace: connection, input, logins, environment (RemoteSpace).
    Space { ws: WsKey },
}

/// Top-level page in the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Page {
    /// Sidebar + tabs + center.
    #[default]
    Work,
    /// First start / no project (S00).
    Welcome,
    /// Settings (S13, S14).
    Settings,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum SettingsSection {
    #[default]
    General,
    Tools,
    Env,
    Mcp,
    Collab,
    Roles,
    Automation,
    Devices,
    Backup,
}

/// Modal flows. Each is drawn by the owning feature module.
#[derive(Debug, Clone, PartialEq)]
pub enum Modal {
    /// Pick a CLI for a new session in a workspace (S01).
    NewSession { ws: WsKey },
    /// Separate work into a worktree (S10).
    SplitWork { ws: WsKey },
    /// Session refresh (S12).
    Refresh { host: HostKey, session: SessionId },
    /// Stop everything and quit.
    Quit,
    /// After a reboot: choose sessions to resume.
    Resume,
    /// Add a device (S13 devices).
    AddDevice,
    /// Rename something inline-confirmed.
    Rename { target: RenameTarget },
}

#[derive(Debug, Clone, PartialEq)]
pub enum RenameTarget {
    Project { host: HostKey, id: ProjectId },
    Workspace(WsKey),
    Session { host: HostKey, id: SessionId },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    Info,
    Success,
    Error,
}

#[derive(Debug, Clone)]
pub struct Toast {
    pub kind: ToastKind,
    pub text: String,
    pub created: std::time::Instant,
}

/// Cross-cutting UI state, persisted per computer as the layout.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiState {
    pub page: Page,
    pub selected_project: Option<ProjectId>,
    pub selected_ws: Option<WsKey>,
    pub sidebar_mode: SidebarMode,
    pub project_filter: ProjectFilter,
    #[serde(skip)]
    pub project_search: String,
    pub expanded_projects: BTreeSet<ProjectId>,
    pub tabsets: BTreeMap<String, TabSet>,
    pub right: Option<RightPanel>,
    pub sidebar_width: f32,
    pub right_width: f32,
    pub sidebar_collapsed: bool,
    pub settings_section: SettingsSection,
    /// Filter the sidebar to one device (only shown when devices are registered).
    #[serde(default)]
    pub device_filter: Option<HostKey>,
    /// Interrupted sessions the resume choice was already shown for: it does not open by itself
    /// again for them (전체 활동 keeps offering 재개 선택).
    #[serde(default)]
    pub resume_seen: BTreeSet<SessionId>,
    #[serde(skip)]
    pub palette_open: bool,
    #[serde(skip)]
    pub modal: Option<Modal>,
    #[serde(skip)]
    pub next_tab_id: TabId,
}

impl Default for UiState {
    fn default() -> Self {
        Self {
            page: Page::Work,
            selected_project: None,
            selected_ws: None,
            sidebar_mode: SidebarMode::Sessions,
            project_filter: ProjectFilter::All,
            project_search: String::new(),
            expanded_projects: BTreeSet::new(),
            tabsets: BTreeMap::new(),
            right: None,
            sidebar_width: crate::theme::size::SIDEBAR,
            right_width: crate::theme::size::RIGHT_PANEL,
            sidebar_collapsed: false,
            settings_section: SettingsSection::General,
            device_filter: None,
            resume_seen: BTreeSet::new(),
            palette_open: false,
            modal: None,
            next_tab_id: 1,
        }
    }
}

pub fn ws_storage_key(ws: &WsKey) -> String {
    match ws.host {
        HostKey::Local => format!("local:{}", ws.id),
        HostKey::Device(d) => format!("{d}:{}", ws.id),
    }
}

impl UiState {
    pub fn tabset(&self, ws: &WsKey) -> Option<&TabSet> {
        self.tabsets.get(&ws_storage_key(ws))
    }

    pub fn tabset_mut(&mut self, ws: &WsKey) -> &mut TabSet {
        self.tabsets.entry(ws_storage_key(ws)).or_default()
    }

    pub fn current_tabset(&self) -> Option<&TabSet> {
        self.selected_ws.as_ref().and_then(|w| self.tabset(w))
    }

    fn alloc_tab_id(&mut self) -> TabId {
        let max_existing = self.tabsets.values().flat_map(|s| s.tabs.iter().map(|t| t.id)).max().unwrap_or(0);
        self.next_tab_id = self.next_tab_id.max(max_existing + 1);
        let id = self.next_tab_id;
        self.next_tab_id += 1;
        id
    }

    /// Open (or focus, if already open) a tab in a workspace and make it active.
    pub fn open_tab(&mut self, ws: WsKey, kind: TabKind) -> TabId {
        if let Some(t) = self.tabset(&ws).and_then(|s| s.tabs.iter().find(|t| t.kind == kind)) {
            let id = t.id;
            self.tabset_mut(&ws).active = Some(id);
            return id;
        }
        let id = self.alloc_tab_id();
        let set = self.tabset_mut(&ws);
        set.tabs.push(Tab { id, kind });
        set.active = Some(id);
        id
    }

    /// Close a tab in the view only. Runs and records continue.
    pub fn close_tab(&mut self, ws: &WsKey, id: TabId) {
        let set = self.tabset_mut(ws);
        let idx = set.tabs.iter().position(|t| t.id == id);
        if let Some(i) = idx {
            set.tabs.remove(i);
            if set.split == Some(id) {
                set.split = None;
            }
            if set.active == Some(id) {
                set.active = set.tabs.get(i.saturating_sub(1)).or(set.tabs.first()).map(|t| t.id);
            }
        }
    }

    pub fn find_tab(&self, id: TabId) -> Option<(WsKey, &Tab)> {
        for (k, set) in &self.tabsets {
            if let Some(t) = set.tabs.iter().find(|t| t.id == id) {
                return parse_ws_storage_key(k).map(|ws| (ws, t));
            }
        }
        None
    }

    pub fn active_tab(&self) -> Option<&Tab> {
        let set = self.current_tabset()?;
        let id = set.active?;
        set.tabs.iter().find(|t| t.id == id)
    }
}

pub fn parse_ws_storage_key(k: &str) -> Option<WsKey> {
    let (h, id) = k.split_once(':')?;
    let id: Uuid = id.parse().ok()?;
    let host = if h == "local" { HostKey::Local } else { HostKey::Device(h.parse().ok()?) };
    Some(WsKey { host, id })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_tab_focuses_existing_and_close_moves_active() {
        let mut s = UiState::default();
        let ws = WsKey { host: HostKey::Local, id: Uuid::new_v4() };
        let a = s.open_tab(ws, TabKind::Start);
        let b = s.open_tab(ws, TabKind::History);
        assert_ne!(a, b);
        assert_eq!(s.open_tab(ws, TabKind::Start), a);
        assert_eq!(s.tabset(&ws).unwrap().active, Some(a));
        s.close_tab(&ws, a);
        assert_eq!(s.tabset(&ws).unwrap().active, Some(b));
        assert_eq!(parse_ws_storage_key(&ws_storage_key(&ws)), Some(ws));
    }
}
