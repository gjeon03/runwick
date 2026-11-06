//! Terminal sessions in the app: a client-side terminal per shown session, fed by the host's
//! output frames, drawn cell by cell, with keyboard/IME/mouse input going back to the host.
//!
//! - `model`: the client terminal state (never answers queries; the host does)
//! - `view`: attach and re-attach, size, input and drawing of one session
//! - `render` / `boxdraw`: painting; `input`: key, paste and mouse encoding
//! - `find`: "화면에서 찾기" (Cmd+F in a focused terminal)
//! - `ask`: confirmations for a program's clipboard write and for multi-line pastes
//! - `banner`: session states (starting, ended, record only, start error, reconnecting,
//!   input held elsewhere)
//!
//! A session is attached the first time it is shown and again after a reconnect; it is
//! detached after it has not been shown for a while (the last screen stays until it is shown
//! again and replaced by a fresh snapshot).

mod ask;
mod banner;
mod boxdraw;
mod find;
mod input;
mod model;
mod render;
#[cfg(any(debug_assertions, feature = "verify-hooks"))]
pub mod verify;
/// Release builds: no scripted input (see `hook_env!`); the observation points do nothing.
#[cfg(not(any(debug_assertions, feature = "verify-hooks")))]
pub mod verify {
    pub fn record(_kind: &str, _value: &str) -> bool {
        false
    }
    pub fn target(_label: &str, _rect: egui::Rect) {}
    pub fn keep_alive() {}
    pub fn requested() -> bool {
        false
    }
}
mod view;

use crate::App;
use crate::client::{EVENT_CONN_LOST, EVENT_CONN_READY, HostKey};
use runwick_core::api::projects::ById;
use runwick_core::api::sessions::{
    EVENT_LIVE, SessionId, SessionLive, SessionsDetach, SessionsView, View as ViewParams,
};
use serde_json::Value;
use std::collections::HashMap;
use std::time::Duration;
use view::{Mode, View};

/// Detach sessions that have not been shown for this long (the host keeps running them).
const DETACH_AFTER: Duration = Duration::from_secs(60);
/// Drop the local copy of a session not shown for this long.
const FORGET_AFTER: Duration = Duration::from_secs(15 * 60);

#[derive(Default)]
pub struct Terminals {
    views: HashMap<(HostKey, SessionId), View>,
    /// Newest `session.live` per session (from events and attach answers).
    live: HashMap<(HostKey, SessionId), SessionLive>,
    /// Per session: whether its program may write the clipboard (asked once, kept while the
    /// app runs).
    clipboard: HashMap<(HostKey, SessionId), bool>,
    ctx: Option<egui::Context>,
}

pub fn on_event(app: &mut App, host: HostKey, topic: &str, data: &Value) {
    if topic == EVENT_LIVE {
        if let Ok(live) = serde_json::from_value::<SessionLive>(data.clone()) {
            let key = (host, live.id);
            let me = app.hub.state(host).info().map(|i| i.client_id);
            if let Some(view) = app.terms.views.get(&key)
                && view.attached.is_some()
                && live.input_owner_client != me
            {
                // Someone else sizes the run: show it at its size.
                view.shared.lock().resize(model::GridSize { cols: live.cols, rows: live.rows });
            }
            app.terms.live.insert(key, live);
        }
    } else if topic == EVENT_CONN_LOST || topic == EVENT_CONN_READY {
        app.terms.connection_changed(host);
    }
}

impl Terminals {
    /// The connection to `host` dropped or was made again: runtime state seen before may be
    /// stale (a restarted host has no runs), so forget it until fresh events or attach answers
    /// arrive. Views keep their last screen and attach again.
    fn connection_changed(&mut self, host: HostKey) {
        self.live.retain(|(h, _), _| *h != host);
        for ((h, _), view) in self.views.iter_mut() {
            if *h == host {
                view.lost();
            }
        }
    }
}

/// Body of a terminal tab.
pub fn show_tab(app: &mut App, ui: &mut egui::Ui, host: HostKey, session: SessionId, _tab: crate::state::TabId) {
    view::show(app, ui, host, session, Mode::Interactive, true);
}

/// Body of a device's terminal tab under a banner that already says how the connection is:
/// the terminal leaves out its own connection chip so the cue is not shown twice.
pub fn show_tab_below_banner(app: &mut App, ui: &mut egui::Ui, host: HostKey, session: SessionId) {
    view::show(app, ui, host, session, Mode::Interactive, false);
}

/// Read-only preview (S05 worker peek): no input or resizing; the caller labels it.
pub fn show_readonly(app: &mut App, ui: &mut egui::Ui, host: HostKey, session: SessionId) {
    view::show(app, ui, host, session, Mode::ReadOnly, true);
}

/// Newest runtime state of a session seen by the terminal views.
/// Some observed session is producing output right now (for the title bar's working pulse).
pub fn any_busy(app: &App) -> bool {
    app.terms.live.values().any(|l| l.busy && l.phase == runwick_core::api::sessions::Phase::Running)
}

pub fn live(app: &App, host: HostKey, session: SessionId) -> Option<SessionLive> {
    app.terms.live.get(&(host, session)).cloned()
}

/// Give keyboard focus to a session's terminal the next time it is drawn.
pub fn focus(app: &mut App, host: HostKey, session: SessionId) {
    if let Some(v) = app.terms.views.get_mut(&(host, session)) {
        v.want_focus = true;
    }
}

/// The terminal that has keyboard focus (shortcuts like Cmd+F belong to it then).
pub fn focused_session(app: &App) -> Option<(HostKey, SessionId)> {
    app.terms.views.iter().find(|(_, v)| v.has_focus).map(|(k, _)| *k)
}

/// Columns and rows of a terminal drawn in an area of `size` points.
pub fn grid_for(ctx: &egui::Context, size: egui::Vec2) -> (u16, u16) {
    render::Metrics::new(ctx).grid_for(size)
}

/// Where an interactive terminal was last drawn (verification runs click there).
pub fn last_area(app: &App) -> Option<egui::Rect> {
    app.terms.views.values().filter(|v| v.area.is_some()).max_by_key(|v| v.last_frame).and_then(|v| v.area)
}

/// Once per frame, before the UI runs (called from `App::logic`): report which sessions the
/// previous frame showed, detach long-hidden sessions and forget very old ones. Also keeps a
/// verification script moving while the window is covered ([`verify::keep_alive`]).
pub fn end_frame(app: &mut App) {
    verify::keep_alive();
    let Some(ctx) = app.terms.ctx.clone() else { return };
    let frame = ctx.cumulative_pass_nr();
    let window_focused = ctx.input(|i| i.focused);
    let mut detach = Vec::new();
    let mut forget = Vec::new();
    let mut viewing = Vec::new();
    for (key, view) in app.terms.views.iter_mut() {
        // Shown in this or the previous frame (a split pane may be drawn after this one).
        let shown = view.last_frame + 1 >= frame;
        let is_viewing = shown && window_focused && view.attached.is_some();
        if view.attached.is_some() && view.viewing_sent != Some(is_viewing) {
            view.viewing_sent = Some(is_viewing);
            viewing.push((*key, is_viewing));
        }
        if !shown {
            view.has_focus = false;
            let hidden = view.last_shown.elapsed();
            if view.attached.is_some() && hidden > DETACH_AFTER {
                view.attached = None;
                view.viewing_sent = None;
                detach.push(*key);
            } else if view.attached.is_none() && hidden > FORGET_AFTER {
                forget.push(*key);
            }
        }
    }
    for ((host, id), v) in viewing {
        app.hub.call::<SessionsView>(host, ViewParams { id, viewing: v }, |_, _| {});
    }
    for (host, id) in detach {
        if let Some(conn) = app.hub.conn(host) {
            conn.remove_output_sink(id);
        }
        app.hub.call::<SessionsDetach>(host, ById { id }, |_, _| {});
    }
    for key in forget {
        app.terms.views.remove(&key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use model::GridSize;

    fn live(id: SessionId) -> SessionLive {
        serde_json::from_value(serde_json::json!({
            "id": id, "phase": "running", "busy": true, "attention": null, "last_output_at": null,
            "cols": 80, "rows": 24, "pid": 42, "input_owner": null, "unread": false
        }))
        .unwrap()
    }

    #[test]
    fn a_connection_change_forgets_that_hosts_runtime_state_only() {
        let (a, b) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let device = HostKey::Device(uuid::Uuid::new_v4());
        let mut t = Terminals::default();
        t.live.insert((HostKey::Local, a), live(a));
        t.live.insert((device, b), live(b));
        let mut v = View::new(GridSize { cols: 80, rows: 24 });
        v.attached = Some(3);
        t.views.insert((HostKey::Local, a), v);

        t.connection_changed(HostKey::Local);

        assert!(!t.live.contains_key(&(HostKey::Local, a)), "stale live state would shadow the session list");
        assert!(t.live.contains_key(&(device, b)), "other hosts keep theirs");
        let view = &t.views[&(HostKey::Local, a)];
        assert_eq!(view.attached, None, "the view attaches again on the new connection");
    }
}
