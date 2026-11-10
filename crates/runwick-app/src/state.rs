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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TabSet {
    pub tabs: Vec<Tab>,
    pub active: Option<TabId>,
    /// Second pane for side-by-side (terminal + document).
    #[serde(default)]
    pub split: Option<TabId>,
    #[serde(default)]
    pub focused_pane: Pane,
    #[serde(default = "default_split_ratio")]
    pub split_ratio: f32,
    /// Rendering constraint, never part of the saved layout.
    #[serde(skip)]
    pub right_hidden: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Pane {
    #[default]
    Left,
    Right,
}

fn default_split_ratio() -> f32 {
    0.5
}

impl Default for TabSet {
    fn default() -> Self {
        Self {
            tabs: Vec::new(),
            active: None,
            split: None,
            focused_pane: Pane::Left,
            split_ratio: 0.5,
            right_hidden: false,
        }
    }
}

impl TabSet {
    pub fn pane_tab(&self, pane: Pane) -> Option<TabId> {
        match pane {
            Pane::Left => self.active,
            Pane::Right => self.split,
        }
    }

    pub fn focused_tab(&self) -> Option<TabId> {
        if self.right_hidden { self.active } else { self.pane_tab(self.focused_pane).or(self.active) }
    }

    pub fn set_right_hidden(&mut self, hidden: bool) {
        self.right_hidden = hidden;
        if hidden {
            self.focused_pane = Pane::Left;
        }
    }

    /// Selecting an already visible tab focuses it without duplicating it.
    pub fn select_tab(&mut self, id: TabId) {
        let pane = if self.right_hidden { Pane::Left } else { self.focused_pane };
        self.show_in_pane(id, pane);
    }

    pub fn select_number(&mut self, number: usize) {
        let index = if number == 8 { self.tabs.len().checked_sub(1) } else { Some(number) };
        if let Some(id) = index.and_then(|i| self.tabs.get(i)).map(|t| t.id) {
            self.select_tab(id);
        }
    }

    pub fn show_in_pane(&mut self, id: TabId, pane: Pane) {
        if !self.tabs.iter().any(|t| t.id == id) {
            return;
        }
        if self.active == Some(id) {
            self.focused_pane = Pane::Left;
        } else if self.split == Some(id) {
            self.focused_pane = Pane::Right;
        } else if pane == Pane::Right && self.active.is_some() {
            self.split = Some(id);
            self.focused_pane = Pane::Right;
        } else {
            self.active = Some(id);
            self.focused_pane = Pane::Left;
        }
        // An explicit selection must be visible even when only one pane fits.
        if self.right_hidden && self.focused_pane == Pane::Right {
            self.swap_panes();
        }
    }

    pub fn swap_panes(&mut self) {
        if self.split.is_some() {
            std::mem::swap(&mut self.active, &mut self.split);
            self.focused_pane = Pane::Left;
        }
    }

    /// A drop moves the tab to the requested pane, swapping visible tabs if necessary.
    pub fn drop_in_pane(&mut self, id: TabId, pane: Pane) {
        let other = if pane == Pane::Left { Pane::Right } else { Pane::Left };
        if self.split.is_some() && self.pane_tab(other) == Some(id) {
            self.swap_panes();
        }
        self.show_in_pane(id, pane);
    }

    /// Close only the pane; both tabs and their running sessions stay open.
    pub fn close_pane(&mut self, pane: Pane) {
        if pane == Pane::Left && self.split.is_some() {
            self.active = self.split;
        }
        self.split = None;
        self.focused_pane = Pane::Left;
    }

    /// Insert at a gap in the original order (0..=len), preserving pane identities.
    pub fn reorder(&mut self, id: TabId, gap: usize) {
        let Some(from) = self.tabs.iter().position(|t| t.id == id) else { return };
        let gap = gap.min(self.tabs.len());
        let to = gap.saturating_sub(usize::from(from < gap));
        let tab = self.tabs.remove(from);
        self.tabs.insert(to, tab);
    }

    pub fn repair(&mut self) {
        if self.active.is_none_or(|a| !self.tabs.iter().any(|t| t.id == a)) {
            self.active = self.tabs.last().map(|t| t.id);
        }
        if self.split.is_some_and(|s| !self.tabs.iter().any(|t| t.id == s) || Some(s) == self.active) {
            self.split = None;
        }
        if self.split.is_none() || self.right_hidden {
            self.focused_pane = Pane::Left;
        }
        self.split_ratio = if self.split_ratio.is_finite() { self.split_ratio.clamp(0.1, 0.9) } else { 0.5 };
    }
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
        let id = self.ensure_tab(ws, kind);
        self.tabset_mut(&ws).select_tab(id);
        id
    }

    pub fn open_tab_in_pane(&mut self, ws: WsKey, kind: TabKind, pane: Pane) -> TabId {
        let id = self.ensure_tab(ws, kind);
        self.tabset_mut(&ws).show_in_pane(id, pane);
        id
    }

    fn ensure_tab(&mut self, ws: WsKey, kind: TabKind) -> TabId {
        if let Some(t) = self.tabset(&ws).and_then(|s| s.tabs.iter().find(|t| t.kind == kind)) {
            return t.id;
        }
        let id = self.alloc_tab_id();
        let set = self.tabset_mut(&ws);
        set.tabs.push(Tab { id, kind });
        id
    }

    /// Close a tab in the view only. Runs and records continue.
    pub fn close_tab(&mut self, ws: &WsKey, id: TabId) {
        let set = self.tabset_mut(ws);
        let idx = set.tabs.iter().position(|t| t.id == id);
        if let Some(i) = idx {
            set.tabs.remove(i);
            if set.split == Some(id) {
                set.close_pane(Pane::Right);
            } else if set.active == Some(id) {
                if set.split.is_some() {
                    set.close_pane(Pane::Left);
                } else {
                    set.active = set.tabs.get(i.saturating_sub(1)).or(set.tabs.first()).map(|t| t.id);
                }
            }
            set.repair();
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

    /// Session in the focused visible pane, for detail-panel consumers.
    pub fn focused_session(&self) -> Option<(HostKey, SessionId)> {
        let set = self.current_tabset()?;
        let id = set.focused_tab()?;
        match set.tabs.iter().find(|t| t.id == id)?.kind {
            TabKind::Terminal { host, session } => Some((host, session)),
            _ => None,
        }
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

    fn split_tabs() -> (UiState, WsKey, [TabId; 3]) {
        let ws = WsKey { host: HostKey::Local, id: Uuid::new_v4() };
        let mut ui = UiState { selected_ws: Some(ws), ..Default::default() };
        let a = ui.open_tab(ws, TabKind::Start);
        let b = ui.open_tab(ws, TabKind::History);
        let c = ui.open_tab(ws, TabKind::Timeline { project: None });
        let set = ui.tabset_mut(&ws);
        set.select_tab(a);
        set.show_in_pane(b, Pane::Right);
        (ui, ws, [a, b, c])
    }

    #[test]
    fn selecting_tabs_replaces_focused_pane_and_focuses_visible_duplicates() {
        let (mut ui, ws, [a, b, c]) = split_tabs();
        ui.open_tab(ws, TabKind::Start);
        assert_eq!(ui.tabset(&ws).unwrap().focused_pane, Pane::Left);
        ui.open_tab(ws, TabKind::History);
        let set = ui.tabset_mut(&ws);
        assert_eq!((set.active, set.split, set.focused_pane), (Some(a), Some(b), Pane::Right));
        set.select_tab(c);
        assert_eq!((set.active, set.split), (Some(a), Some(c)));
        set.select_tab(a);
        set.select_tab(b);
        assert_eq!((set.active, set.split, set.focused_pane), (Some(b), Some(c), Pane::Left));
    }

    #[test]
    fn explicit_open_does_not_replace_left_before_creating_split() {
        let (mut ui, ws, [a, b, _]) = split_tabs();
        let doc = TabKind::Doc { ws, path: "README.md".into() };
        let id = ui.open_tab_in_pane(ws, doc.clone(), Pane::Right);
        assert_eq!((ui.tabset(&ws).unwrap().active, ui.tabset(&ws).unwrap().split), (Some(a), Some(id)));
        assert!(ui.tabset(&ws).unwrap().tabs.iter().any(|t| t.id == b));
        ui.open_tab_in_pane(ws, doc, Pane::Left);
        assert_eq!(ui.tabset(&ws).unwrap().focused_pane, Pane::Right);
        assert_eq!(ui.tabset(&ws).unwrap().active, Some(a));
    }

    #[test]
    fn number_shortcuts_target_focused_pane_and_nine_selects_last() {
        let (mut ui, ws, [a, b, c]) = split_tabs();
        let set = ui.tabset_mut(&ws);
        set.select_number(8);
        assert_eq!((set.active, set.split, set.focused_tab()), (Some(a), Some(c), Some(c)));
        set.select_number(0);
        assert_eq!(set.focused_pane, Pane::Left);
        set.select_number(1);
        assert_eq!((set.active, set.split, set.focused_tab()), (Some(b), Some(c), Some(b)));
        set.select_number(7);
        assert_eq!(set.focused_tab(), Some(b));
    }

    #[test]
    fn closing_either_pane_keeps_all_tabs() {
        for pane in [Pane::Left, Pane::Right] {
            let (mut ui, ws, [a, b, _]) = split_tabs();
            let set = ui.tabset_mut(&ws);
            let tabs = set.tabs.clone();
            set.close_pane(pane);
            assert_eq!(set.tabs, tabs);
            assert_eq!(set.split, None);
            assert_eq!(set.active, Some(if pane == Pane::Left { b } else { a }));
            assert_eq!(set.focused_pane, Pane::Left);
        }
    }

    #[test]
    fn closing_a_tab_in_split_promotes_survivor_without_duplicates() {
        for pane in [Pane::Left, Pane::Right] {
            let (mut ui, ws, [a, b, _]) = split_tabs();
            ui.close_tab(&ws, if pane == Pane::Left { a } else { b });
            let set = ui.tabset(&ws).unwrap();
            assert_eq!(set.tabs.len(), 2);
            assert_eq!(set.active, Some(if pane == Pane::Left { b } else { a }));
            assert_eq!(set.split, None);
        }
    }

    #[test]
    fn narrow_layout_never_closes_hidden_pane_and_can_swap() {
        let (mut ui, ws, [a, b, _]) = split_tabs();
        let set = ui.tabset_mut(&ws);
        set.set_right_hidden(true);
        assert_eq!(set.focused_tab(), Some(a));
        set.swap_panes();
        assert_eq!((set.active, set.split, set.focused_tab()), (Some(b), Some(a), Some(b)));
        set.select_tab(a);
        assert_eq!((set.active, set.split), (Some(a), Some(b)));
        let target = set.focused_tab().unwrap();
        ui.close_tab(&ws, target);
        let set = ui.tabset(&ws).unwrap();
        assert!(set.tabs.iter().any(|t| t.id == b));
        assert_eq!(set.active, Some(b));
    }

    #[test]
    fn reorder_preserves_panes_and_is_scoped_to_workspace() {
        let (mut ui, ws, [a, b, c]) = split_tabs();
        let other = WsKey { id: Uuid::new_v4(), ..ws };
        ui.open_tab(other, TabKind::Start);
        let other_before = ui.tabset(&other).unwrap().clone();
        let set = ui.tabset_mut(&ws);
        set.reorder(a, 3);
        assert_eq!(set.tabs.iter().map(|t| t.id).collect::<Vec<_>>(), [b, c, a]);
        set.reorder(a, 0);
        assert_eq!(set.tabs.iter().map(|t| t.id).collect::<Vec<_>>(), [a, b, c]);
        set.reorder(b, 2);
        assert_eq!(set.tabs.iter().map(|t| t.id).collect::<Vec<_>>(), [a, b, c]);
        assert_eq!((set.active, set.split, set.focused_pane), (Some(a), Some(b), Pane::Right));
        assert_eq!(ui.tabset(&other).unwrap(), &other_before);
    }

    #[test]
    fn dropping_visible_tab_swaps_instead_of_duplicating() {
        let (mut ui, ws, [a, b, c]) = split_tabs();
        let set = ui.tabset_mut(&ws);
        set.drop_in_pane(a, Pane::Right);
        assert_eq!((set.active, set.split, set.focused_pane), (Some(b), Some(a), Pane::Right));
        set.drop_in_pane(c, Pane::Left);
        assert_eq!((set.active, set.split), (Some(c), Some(a)));
        set.drop_in_pane(a, Pane::Left);
        assert_eq!((set.active, set.split), (Some(a), Some(c)));
    }

    #[test]
    fn split_layout_round_trip_and_legacy_defaults() {
        let (mut ui, ws, [a, _, _]) = split_tabs();
        let set = ui.tabset_mut(&ws);
        set.split_ratio = 0.63;
        set.reorder(a, 3);
        let saved = serde_json::to_value(&ui).unwrap();
        let back: UiState = serde_json::from_value(saved.clone()).unwrap();
        assert_eq!(back.tabset(&ws), ui.tabset(&ws));
        let mut legacy = saved;
        let obj = legacy["tabsets"][ws_storage_key(&ws)].as_object_mut().unwrap();
        obj.remove("split_ratio");
        obj.remove("focused_pane");
        let back: UiState = serde_json::from_value(legacy).unwrap();
        let set = back.tabset(&ws).unwrap();
        assert_eq!(set.split_ratio, 0.5);
        assert_eq!(set.focused_pane, Pane::Left);
        assert_eq!(set.tabs, ui.tabset(&ws).unwrap().tabs);
    }

    #[test]
    fn repair_sanitizes_invalid_layout_values() {
        let (mut ui, ws, [a, _, _]) = split_tabs();
        let set = ui.tabset_mut(&ws);
        set.split = Some(a);
        set.split_ratio = f32::NAN;
        set.repair();
        assert_eq!(set.split, None);
        assert_eq!(set.focused_pane, Pane::Left);
        assert_eq!(set.split_ratio, 0.5);
    }
}
