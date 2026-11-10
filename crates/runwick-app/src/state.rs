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
    Terminal {
        host: HostKey,
        session: SessionId,
    },
    /// A file in a workspace (edit Markdown/text, view code).
    Doc {
        ws: WsKey,
        path: String,
    },
    /// Project wiki: list and a page.
    Wiki {
        project: ProjectId,
        page: Option<String>,
    },
    /// Wiki relationship graph.
    WikiGraph {
        project: ProjectId,
    },
    /// Timeline for one project or all projects.
    Timeline {
        project: Option<ProjectId>,
    },
    /// Conversation search expanded to the center.
    History,
    /// A specific record, independent of panel navigation.
    Record {
        host: HostKey,
        anchor: runwick_core::api::records::Anchor,
    },
    Pin {
        host: HostKey,
        id: Uuid,
    },
    /// Result of delegated work (S11).
    Result {
        host: HostKey,
        request: Uuid,
    },
    /// Merge/integration of a separated workspace (S11 merge).
    Merge {
        ws: WsKey,
    },
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

/// Fixed tabs of the shared side panel.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum RightPanel {
    #[default]
    Activity,
    History,
    Pins,
    Workers,
}

impl RightPanel {
    pub const ALL: [(Self, &'static str); 4] =
        [(Self::Activity, "활동"), (Self::History, "기록"), (Self::Pins, "핀"), (Self::Workers, "작업자")];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PanelTarget {
    pub host: HostKey,
    pub session: SessionId,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PanelDetail {
    Message(crate::views::history::Opened),
    Pin { host: HostKey, id: Uuid },
    Result { host: HostKey, request: Uuid },
    Worker { host: HostKey, session: SessionId },
    WorkerList { host: HostKey, session: SessionId },
    Space { ws: WsKey },
}

#[derive(Debug, Clone)]
pub struct PanelFrame {
    pub detail: PanelDetail,
    pub target: Option<PanelTarget>,
    return_tab: RightPanel,
    return_target: Option<PanelTarget>,
}

/// Selection and width persist per workspace; navigation is kept only for this window.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PanelState {
    pub selected: RightPanel,
    pub width: f32,
    #[serde(skip)]
    pub target: Option<PanelTarget>,
    #[serde(skip)]
    pub pinned: bool,
    #[serde(skip)]
    followed: Option<PanelTarget>,
    #[serde(skip)]
    pub stacks: BTreeMap<RightPanel, Vec<PanelFrame>>,
}

impl Default for PanelState {
    fn default() -> Self {
        Self {
            selected: RightPanel::Activity,
            width: crate::theme::size::RIGHT_PANEL,
            target: None,
            pinned: false,
            followed: None,
            stacks: BTreeMap::new(),
        }
    }
}

impl PanelState {
    pub fn follow(&mut self, target: Option<PanelTarget>) {
        if self.followed != target {
            self.followed = target;
            if !self.pinned
                && let Some(target) = target
            {
                self.target = Some(target);
                self.stacks.clear();
            }
        }
    }

    pub fn current(&self) -> Option<&PanelFrame> {
        self.stacks.get(&self.selected).and_then(|s| s.last())
    }

    pub fn current_target(&self) -> Option<PanelTarget> {
        match self.current() {
            Some(PanelFrame { detail: PanelDetail::Space { .. }, .. }) | None => self.target,
            Some(frame) => frame.target,
        }
    }

    pub fn pin(&mut self, pinned: bool) {
        self.target = if pinned { self.current_target() } else { self.followed };
        self.pinned = pinned;
        if !pinned {
            self.stacks.clear();
        }
    }

    pub fn push(&mut self, tab: RightPanel, detail: PanelDetail, target: Option<PanelTarget>) {
        if self.selected == tab && self.current().is_some_and(|f| f.detail == detail) {
            return;
        }
        let frame = PanelFrame { detail, target, return_tab: self.selected, return_target: self.target };
        self.stacks.entry(tab).or_default().push(frame);
        self.selected = tab;
    }

    pub fn back(&mut self) {
        if let Some(frame) = self.stacks.entry(self.selected).or_default().pop() {
            self.selected = frame.return_tab;
            self.target = frame.return_target;
        }
    }
}

// Older layouts stored a detail in the panel slot. Restore its fixed tab, leaving the
// transient detail stack empty, without discarding the rest of the saved layout.
fn deserialize_right<'de, D: serde::Deserializer<'de>>(de: D) -> Result<Option<RightPanel>, D::Error> {
    let value = Option::<serde_json::Value>::deserialize(de)?;
    Ok(value.and_then(|v| {
        let name = v.as_str().or_else(|| v.as_object()?.keys().next().map(String::as_str))?;
        match name {
            "Activity" | "Space" => Some(RightPanel::Activity),
            "History" => Some(RightPanel::History),
            "Pins" => Some(RightPanel::Pins),
            "Workers" | "Collab" | "Result" => Some(RightPanel::Workers),
            _ => None,
        }
    }))
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
    #[serde(default, deserialize_with = "deserialize_right")]
    pub right: Option<RightPanel>,
    #[serde(default)]
    pub panels: BTreeMap<String, PanelState>,
    #[serde(skip)]
    panel_workspace: Option<String>,
    #[serde(skip)]
    panel_lock: Option<PanelTarget>,
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
            panels: BTreeMap::new(),
            panel_workspace: None,
            panel_lock: None,
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
    fn panel_key(&self) -> String {
        self.selected_ws.as_ref().map(ws_storage_key).unwrap_or_else(|| "global".into())
    }

    pub fn panel(&self) -> Option<&PanelState> {
        self.panels.get(&self.panel_key())
    }

    pub fn panel_mut(&mut self) -> &mut PanelState {
        self.panels.entry(self.panel_key()).or_default()
    }

    pub fn panel_target(&self) -> Option<PanelTarget> {
        self.panel().and_then(PanelState::current_target)
    }

    pub fn sync_panel(&mut self) {
        let key = self.panel_key();
        if self.panel_workspace.as_ref() != Some(&key) {
            if let Some(old) = &self.panel_workspace
                && let Some(p) = self.panels.get_mut(old)
            {
                if let Some(tab) = self.right {
                    p.selected = tab;
                }
                p.width = self.right_width;
            }
            let legacy_tab = self.right.unwrap_or_default();
            let legacy_width = self.right_width;
            let p = self.panels.entry(key.clone()).or_insert_with(|| PanelState {
                selected: legacy_tab,
                width: legacy_width,
                ..Default::default()
            });
            if let Some(locked) = self.panel_lock
                && p.current_target() != Some(locked)
            {
                p.stacks.clear();
            }
            self.right_width = if p.width.is_finite() {
                p.width.clamp(crate::theme::size::RIGHT_PANEL_MIN, crate::theme::size::RIGHT_PANEL_MAX)
            } else {
                crate::theme::size::RIGHT_PANEL
            };
            if self.right.is_some() {
                self.right = Some(p.selected);
            }
            self.panel_workspace = Some(key);
        }
        let target = self.focused_session().map(|(host, session)| PanelTarget { host, session });
        let tab = self.right;
        let width = self.right_width;
        let locked = self.panel_lock;
        let p = self.panel_mut();
        p.pinned = locked.is_some();
        if let Some(locked) = locked {
            p.target = Some(locked);
        }
        p.follow(target);
        if let Some(tab) = tab {
            p.selected = tab;
        }
        p.width = width;
    }

    pub fn pin_panel(&mut self, pinned: bool) {
        self.panel_lock = if pinned { self.panel_target() } else { None };
        self.panel_mut().pin(pinned);
    }

    pub fn panel_entry_matches(&self, tab: RightPanel, target: Option<PanelTarget>) -> bool {
        self.right == Some(tab)
            && (if tab == RightPanel::Activity {
                self.panel().is_none_or(|p| p.current().is_none())
            } else {
                self.panel_target() == target
            })
    }

    pub fn show_panel(&mut self, tab: RightPanel, target: Option<PanelTarget>) {
        self.sync_panel();
        if self.panel_lock.is_some() && target.is_some() {
            self.panel_lock = target;
        }
        let p = self.panel_mut();
        if let Some(target) = target {
            p.target = Some(target);
            p.stacks.remove(&tab);
        }
        p.selected = tab;
        self.right = Some(tab);
    }

    pub fn show_panel_root(&mut self, tab: RightPanel, target: Option<PanelTarget>) {
        self.show_panel(tab, target);
        self.panel_mut().stacks.remove(&tab);
    }

    pub fn push_panel(&mut self, tab: RightPanel, detail: PanelDetail, target: Option<PanelTarget>) {
        self.sync_panel();
        if self.panel_lock.is_some() && target.is_some() {
            self.panel_lock = target;
        }
        self.panel_mut().push(tab, detail, target);
        self.right = Some(tab);
    }

    pub fn back_panel(&mut self) {
        self.panel_mut().back();
        if self.panel_lock.is_some() {
            self.panel_lock = self.panel_target();
        }
        self.right = Some(self.panel_mut().selected);
    }

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

    fn panel_sessions() -> (UiState, WsKey, PanelTarget, PanelTarget) {
        let ws = WsKey { host: HostKey::Local, id: Uuid::new_v4() };
        let mut ui = UiState { selected_ws: Some(ws), ..Default::default() };
        let left = PanelTarget { host: ws.host, session: Uuid::new_v4() };
        let right = PanelTarget { host: ws.host, session: Uuid::new_v4() };
        ui.open_tab(ws, TabKind::Terminal { host: left.host, session: left.session });
        ui.open_tab_in_pane(ws, TabKind::Terminal { host: right.host, session: right.session }, Pane::Right);
        ui.sync_panel();
        (ui, ws, left, right)
    }

    #[test]
    fn panel_tabs_have_a_fixed_order_and_unresolved_details_do_not_claim_another_session() {
        assert_eq!(RightPanel::ALL.map(|(_, label)| label), ["활동", "기록", "핀", "작업자"]);
        let (mut ui, _, _, _) = panel_sessions();
        ui.push_panel(RightPanel::Workers, PanelDetail::Result { host: HostKey::Local, request: Uuid::new_v4() }, None);
        assert_eq!(ui.panel_target(), None);
    }

    #[test]
    fn panel_follows_the_focused_pane_and_unpin_returns_to_it() {
        let (mut ui, ws, left, right) = panel_sessions();
        ui.show_panel(RightPanel::Pins, Some(right));
        assert_eq!(ui.panel_target(), Some(right));
        ui.pin_panel(true);
        ui.tabset_mut(&ws).focused_pane = Pane::Left;
        ui.sync_panel();
        assert_eq!(ui.panel_target(), Some(right));
        ui.pin_panel(false);
        assert_eq!(ui.panel_target(), Some(left));
        ui.tabset_mut(&ws).focused_pane = Pane::Right;
        ui.sync_panel();
        assert_eq!(ui.panel_target(), Some(right));
        ui.tabset_mut(&ws).set_right_hidden(true);
        ui.sync_panel();
        assert_eq!(ui.panel_target(), Some(left));
    }

    #[test]
    fn panel_entry_matches_both_tab_and_target() {
        let (mut ui, _, left, right) = panel_sessions();
        ui.show_panel(RightPanel::History, Some(right));
        assert!(ui.panel_entry_matches(RightPanel::History, Some(right)));
        assert!(!ui.panel_entry_matches(RightPanel::History, Some(left)));
        assert!(!ui.panel_entry_matches(RightPanel::Pins, Some(right)));
        ui.show_panel(RightPanel::Activity, None);
        assert!(ui.panel_entry_matches(RightPanel::Activity, Some(left)));
    }

    #[test]
    fn an_activity_entry_returns_from_workspace_details_to_the_global_list() {
        let (mut ui, ws, _, _) = panel_sessions();
        ui.show_panel(RightPanel::Activity, None);
        ui.push_panel(RightPanel::Activity, PanelDetail::Space { ws }, None);
        assert!(!ui.panel_entry_matches(RightPanel::Activity, None));
        ui.show_panel_root(RightPanel::Activity, None);
        assert!(ui.panel().unwrap().current().is_none());
        assert!(ui.panel_entry_matches(RightPanel::Activity, None));
    }

    #[test]
    fn tabs_keep_independent_stacks_and_back_returns_to_the_origin() {
        let (mut ui, _, left, right) = panel_sessions();
        ui.show_panel(RightPanel::Activity, None);
        let list = PanelDetail::WorkerList { host: left.host, session: left.session };
        let result = PanelDetail::Result { host: right.host, request: Uuid::new_v4() };
        ui.push_panel(RightPanel::Workers, list.clone(), Some(left));
        ui.push_panel(RightPanel::Workers, result.clone(), Some(right));
        ui.show_panel(RightPanel::History, None);
        let message = PanelDetail::Message(crate::views::history::Opened {
            host: left.host,
            anchor: runwick_core::api::records::Anchor::SessionEnd { id: left.session },
            before: 30,
            after: 0,
            terms: Vec::new(),
        });
        ui.push_panel(RightPanel::History, message.clone(), Some(left));
        ui.show_panel(RightPanel::Workers, None);
        assert_eq!(ui.panel().unwrap().current().unwrap().detail, result);
        ui.back_panel();
        assert_eq!(ui.panel_target(), Some(left));
        assert_eq!(ui.panel().unwrap().current().unwrap().detail, list);
        ui.back_panel();
        assert_eq!(ui.right, Some(RightPanel::Activity));
        ui.show_panel(RightPanel::History, None);
        assert_eq!(ui.panel().unwrap().current().unwrap().detail, message);
    }

    #[test]
    fn pinned_worker_detail_returns_to_the_original_parent_list() {
        let (mut ui, _, parent, worker) = panel_sessions();
        ui.show_panel(RightPanel::Workers, Some(parent));
        ui.pin_panel(true);
        ui.push_panel(
            RightPanel::Workers,
            PanelDetail::Result { host: worker.host, request: Uuid::new_v4() },
            Some(worker),
        );
        ui.sync_panel();
        assert_eq!(ui.panel_target(), Some(worker));
        ui.back_panel();
        ui.sync_panel();
        assert_eq!(ui.panel_target(), Some(parent));
        assert!(ui.panel().unwrap().current().is_none());
    }

    #[test]
    fn expanding_a_record_keeps_the_panel_detail_and_uses_distinct_tabs() {
        let (mut ui, ws, left, _) = panel_sessions();
        let anchor = runwick_core::api::records::Anchor::Message { id: Uuid::new_v4() };
        let detail = PanelDetail::Message(crate::views::history::Opened {
            host: left.host,
            anchor,
            before: 6,
            after: 6,
            terms: Vec::new(),
        });
        ui.push_panel(RightPanel::History, detail.clone(), Some(left));
        let a = ui.open_tab(ws, TabKind::Record { host: left.host, anchor });
        ui.sync_panel();
        assert_eq!(ui.panel().unwrap().current().unwrap().detail, detail);
        let b = ui.open_tab(
            ws,
            TabKind::Record {
                host: left.host,
                anchor: runwick_core::api::records::Anchor::Message { id: Uuid::new_v4() },
            },
        );
        assert_ne!(a, b);
        assert_eq!(ui.open_tab(ws, TabKind::Record { host: left.host, anchor }), a);
    }

    #[test]
    fn workspace_panel_selection_and_width_survive_serialization_without_details() {
        let (mut ui, first, _, right) = panel_sessions();
        ui.show_panel(RightPanel::Pins, Some(right));
        ui.right_width = 410.0;
        ui.sync_panel();
        let second = WsKey { host: HostKey::Device(Uuid::new_v4()), id: Uuid::new_v4() };
        ui.selected_ws = Some(second);
        ui.sync_panel();
        ui.show_panel(RightPanel::Workers, None);
        ui.right_width = 480.0;
        ui.push_panel(
            RightPanel::Workers,
            PanelDetail::Worker { host: right.host, session: right.session },
            Some(right),
        );
        ui.sync_panel();
        let mut restored: UiState = serde_json::from_value(serde_json::to_value(&ui).unwrap()).unwrap();
        restored.sync_panel();
        assert_eq!((restored.right, restored.right_width), (Some(RightPanel::Workers), 480.0));
        assert!(restored.panel().unwrap().current().is_none());
        restored.selected_ws = Some(first);
        restored.sync_panel();
        assert_eq!((restored.right, restored.right_width), (Some(RightPanel::Pins), 410.0));
    }

    #[test]
    fn pin_overrides_an_existing_detail_when_returning_to_another_workspace() {
        let (mut ui, first, left, right) = panel_sessions();
        ui.push_panel(
            RightPanel::Workers,
            PanelDetail::Worker { host: right.host, session: right.session },
            Some(right),
        );
        let second = WsKey { host: HostKey::Local, id: Uuid::new_v4() };
        ui.selected_ws = Some(second);
        ui.open_tab(second, TabKind::Terminal { host: left.host, session: left.session });
        ui.sync_panel();
        ui.pin_panel(true);
        ui.selected_ws = Some(first);
        ui.sync_panel();
        assert_eq!(ui.panel_target(), Some(left));
        assert!(ui.panel().unwrap().current().is_none());
        ui.pin_panel(false);
        assert_eq!(ui.panel_target(), Some(right));
    }

    #[test]
    fn old_detail_layouts_restore_without_losing_workspace_tabs() {
        let (ui, ws, _, right) = panel_sessions();
        for variant in ["Collab", "Result", "Space"] {
            let mut value = serde_json::to_value(&ui).unwrap();
            value.as_object_mut().unwrap().remove("panels");
            value["right"] = serde_json::json!({ variant: { "host": "Local", "session": right.session } });
            let mut restored: UiState = serde_json::from_value(value).unwrap();
            restored.sync_panel();
            assert_eq!(restored.tabset(&ws).unwrap().tabs.len(), 2);
            assert!(RightPanel::ALL.iter().any(|(tab, _)| Some(*tab) == restored.right));
        }
    }

    #[test]
    fn pinned_target_survives_workspace_switch_and_remote_host_is_part_of_identity() {
        let (mut ui, _, _, right) = panel_sessions();
        ui.pin_panel(true);
        let ws = WsKey { host: HostKey::Device(Uuid::new_v4()), id: Uuid::new_v4() };
        ui.selected_ws = Some(ws);
        ui.open_tab(ws, TabKind::Terminal { host: ws.host, session: right.session });
        ui.sync_panel();
        assert_eq!(ui.panel_target(), Some(right));
        ui.pin_panel(false);
        assert_eq!(ui.panel_target(), Some(PanelTarget { host: ws.host, session: right.session }));
    }

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
