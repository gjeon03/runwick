//! Layout persistence: the UI state is saved to the host (`kv` key `ui.layout`) a second after
//! it changes and restored on the first connection. Restored selections are checked against
//! the host's records so deleted or archived items never stay selected.

use super::data;
use crate::App;
use crate::client::HostKey;
use crate::state::{TabKind, UiState, WsKey};
use runwick_core::api::host::{KvGet, KvKey, KvPut, KvSet};
use std::time::{Duration, Instant};

pub const KEY: &str = "ui.layout";
const DEBOUNCE: Duration = Duration::from_secs(1);

#[derive(Default)]
pub struct State {
    /// The saved layout has been applied (or there was none, or restoring is disabled).
    restored: bool,
    /// A verification run with its own UI state neither restores nor saves the layout.
    fixed: bool,
    requested: bool,
    last_saved: Option<String>,
    dirty_since: Option<Instant>,
}

impl State {
    pub fn new(fixed: bool) -> Self {
        Self { restored: fixed, fixed, ..Default::default() }
    }
}

/// The saved layout has been applied (or there was none): later UI changes are not replaced.
pub fn is_restored(app: &App) -> bool {
    app.shell.layout.restored
}

/// First connection to this computer's host: fetch the saved layout once.
pub fn on_connected(app: &mut App) {
    if app.shell.layout.restored || app.shell.layout.requested {
        return;
    }
    app.shell.layout.requested = true;
    app.hub.call::<KvGet>(HostKey::Local, KvKey { key: KEY.into() }, |app, res| {
        app.shell.layout.restored = true;
        match res {
            Ok(Some(v)) => match serde_json::from_value::<UiState>(v) {
                Ok(saved) => apply(app, saved),
                Err(e) => tracing::warn!("saved layout ignored: {e}"),
            },
            Ok(None) => {}
            Err(e) => tracing::warn!("layout restore failed: {}", e.message),
        }
        app.shell.layout.last_saved = serde_json::to_string(&app.ui).ok();
    });
}

fn apply(app: &mut App, saved: UiState) {
    let modal = app.ui.modal.take();
    let search = std::mem::take(&mut app.ui.project_search);
    app.ui = saved;
    for set in app.ui.tabsets.values_mut() {
        set.repair();
    }
    app.ui.modal = modal;
    app.ui.project_search = search;
    // Always come back to the workroom (or the welcome page), not into settings.
    if app.ui.page == crate::state::Page::Settings {
        app.ui.page = crate::state::Page::Work;
    }
    app.ui.sidebar_width = app.ui.sidebar_width.clamp(crate::theme::size::SIDEBAR_MIN, crate::theme::size::SIDEBAR_MAX);
    app.ui.right_width =
        app.ui.right_width.clamp(crate::theme::size::RIGHT_PANEL_MIN, crate::theme::size::RIGHT_PANEL_MAX);
}

/// Save the layout a moment after it last changed.
pub fn tick(app: &mut App, ctx: &egui::Context) {
    if !app.shell.layout.restored || app.shell.layout.fixed {
        return;
    }
    let Ok(now_json) = serde_json::to_string(&app.ui) else { return };
    if app.shell.layout.last_saved.as_deref() == Some(now_json.as_str()) {
        app.shell.layout.dirty_since = None;
        return;
    }
    let since = *app.shell.layout.dirty_since.get_or_insert_with(Instant::now);
    let waited = since.elapsed();
    if waited >= DEBOUNCE {
        save_now(app, now_json);
    } else {
        ctx.request_repaint_after(DEBOUNCE - waited);
    }
}

/// Write the layout immediately (window closing).
pub fn flush(app: &mut App) {
    if !app.shell.layout.restored || app.shell.layout.fixed {
        return;
    }
    if let Ok(json) = serde_json::to_string(&app.ui)
        && app.shell.layout.last_saved.as_deref() != Some(json.as_str())
    {
        save_now(app, json);
    }
}

fn save_now(app: &mut App, json: String) {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&json) else { return };
    if !app.hub.state(HostKey::Local).is_connected() {
        return;
    }
    app.shell.layout.last_saved = Some(json);
    app.shell.layout.dirty_since = None;
    app.hub.call::<KvPut>(HostKey::Local, KvSet { key: KEY.into(), value }, |app, res| {
        if res.is_err() {
            // Try again on the next change.
            app.shell.layout.last_saved = None;
        }
    });
}

/// Drop selections and tabs that point at records that no longer exist or are archived, and
/// pick a workspace when none is selected. Runs only on complete, successful lists.
pub fn validate(app: &mut App) {
    if !app.shell.layout.restored {
        return;
    }
    let (Some(projects), Some(workspaces)) = (data::projects(app).data, data::workspaces(app).data) else { return };
    let live_projects: Vec<_> = data::visible_projects(&projects).map(|p| p.id).collect();
    let ui = &mut app.ui;
    ui.expanded_projects.retain(|p| live_projects.contains(p));
    if ui.selected_project.is_some_and(|p| !live_projects.contains(&p)) {
        ui.selected_project = None;
    }
    let ws_ok = |k: &WsKey| data::find_ws(&workspaces, k).is_some_and(|w| live_projects.contains(&w.project_id));
    if ui.selected_ws.is_some_and(|k| !ws_ok(&k)) {
        ui.selected_ws = None;
    }
    // Tabs of workspaces that are gone, and tabs that point into removed projects.
    ui.tabsets.retain(|k, _| crate::state::parse_ws_storage_key(k).is_some_and(|k| ws_ok(&k)));
    for set in ui.tabsets.values_mut() {
        let before = set.tabs.len();
        set.tabs.retain(|t| match &t.kind {
            TabKind::Wiki { project, .. } | TabKind::WikiGraph { project } => live_projects.contains(project),
            TabKind::Timeline { project: Some(p) } => live_projects.contains(p),
            _ => true,
        });
        if set.tabs.len() != before {
            fix_active(set);
        }
    }
    if ui.selected_ws.is_none() {
        let pick = ui
            .selected_project
            .and_then(|p| best_workspace(&workspaces, p))
            .or_else(|| live_projects.first().and_then(|p| best_workspace(&workspaces, *p)));
        if let Some(w) = pick {
            ui.selected_project = Some(w.project_id);
            ui.selected_ws = Some(data::ws_key(w));
            ui.expanded_projects.insert(w.project_id);
        }
    } else if let Some(w) = ui.selected_ws.and_then(|k| data::find_ws(&workspaces, &k)) {
        ui.selected_project = Some(w.project_id);
    }
}

/// The workspace to open for a project: most recently used, else the default one.
pub fn best_workspace(
    list: &[runwick_core::api::projects::Workspace],
    project: uuid::Uuid,
) -> Option<&runwick_core::api::projects::Workspace> {
    list.iter()
        .filter(|w| w.project_id == project && !w.archived)
        .max_by_key(|w| (w.last_used_at, matches!(w.kind, runwick_core::api::projects::WorkspaceKind::Default)))
}

/// Keep the active/split tab ids pointing at existing tabs.
pub fn fix_active(set: &mut crate::state::TabSet) {
    set.repair();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{Tab, TabSet};

    #[test]
    fn fix_active_falls_back_and_clears_bad_split() {
        let mut set = TabSet {
            tabs: vec![Tab { id: 1, kind: TabKind::Start }, Tab { id: 2, kind: TabKind::History }],
            active: Some(9),
            split: Some(2),
            ..Default::default()
        };
        fix_active(&mut set);
        assert_eq!(set.active, Some(2));
        assert_eq!(set.split, None);
    }

    #[test]
    fn layout_round_trips_through_json() {
        let mut ui = UiState::default();
        let ws = WsKey { host: HostKey::Local, id: uuid::Uuid::new_v4() };
        ui.open_tab(ws, TabKind::History);
        ui.sidebar_width = 300.0;
        let json = serde_json::to_value(&ui).unwrap();
        let back: UiState = serde_json::from_value(json).unwrap();
        assert_eq!(back.sidebar_width, 300.0);
        assert_eq!(back.tabset(&ws).unwrap().tabs.len(), 1);
    }
}
