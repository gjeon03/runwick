//! Runwick's presence on this computer while the window is closed (MenuBar.dc.html).
//!
//! - An open window holds a shared lock on `RUNWICK_HOME/gui.lock`. The menu bar process
//!   (`runwick menubar`) steps aside while any window holds it: the window is the presence.
//! - Closing the window starts `runwick menubar --after-window` when this computer's host may
//!   still run work. The menu bar process itself checks the host, the runs and the
//!   [`MENUBAR_ENABLED_KEY`] preference, and exits when it has nothing to show.
//! - The menu bar opens a window with `--open activity|quit|session:<id>`; the request is applied
//!   once, after the saved layout was restored (which would otherwise replace it).

use crate::App;
use crate::client::HostKey;
use crate::shell::{actions, data, layout};
use crate::state::{Modal, Page, RightPanel, WsKey};
use runwick_core::api::sessions::Phase;
use runwick_core::paths;
use std::fs::{File, OpenOptions, TryLockError};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::Duration;
use uuid::Uuid;

/// Host key/value preference (`kv.get`/`kv.put`, JSON bool): show the menu bar icon after the
/// window closes while runs continue. Missing means on.
pub const MENUBAR_ENABLED_KEY: &str = "menubar.enabled";

pub fn menubar_enabled(value: Option<&serde_json::Value>) -> bool {
    value.and_then(serde_json::Value::as_bool).unwrap_or(true)
}

fn window_lock_path() -> PathBuf {
    paths::home().join("gui.lock")
}

/// Taken by the window for its whole life. Shared, so a second window does not fail; the menu
/// bar only probes it for a moment, so a short retry covers that probe.
pub fn hold_window_lock() -> Option<File> {
    std::fs::create_dir_all(paths::home()).ok()?;
    let file = OpenOptions::new().create(true).truncate(false).write(true).open(window_lock_path()).ok()?;
    for _ in 0..50 {
        match file.try_lock_shared() {
            Ok(()) => return Some(file),
            Err(TryLockError::WouldBlock) => std::thread::sleep(Duration::from_millis(20)),
            Err(TryLockError::Error(e)) => {
                tracing::warn!("window lock unavailable: {e}");
                return None;
            }
        }
    }
    None
}

/// Whether a Runwick window for this `RUNWICK_HOME` is open right now.
pub fn window_open() -> bool {
    let Ok(file) = OpenOptions::new().write(true).open(window_lock_path()) else {
        return false;
    };
    match file.try_lock() {
        Ok(()) => {
            let _ = file.unlock();
            false
        }
        Err(TryLockError::WouldBlock) => true,
        Err(TryLockError::Error(_)) => false,
    }
}

/// What the window shows first when the menu bar opens it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenRequest {
    /// The 전체 활동 panel.
    Activity,
    /// The 작업까지 종료 dialog.
    Quit,
    /// One session's terminal tab.
    Session(Uuid),
}

impl OpenRequest {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "activity" => Some(Self::Activity),
            "quit" => Some(Self::Quit),
            _ => s.strip_prefix("session:").and_then(|id| id.parse().ok()).map(Self::Session),
        }
    }

    /// The `--open` value that [`OpenRequest::parse`] reads back.
    pub fn to_arg(self) -> String {
        match self {
            Self::Activity => "activity".into(),
            Self::Quit => "quit".into(),
            Self::Session(id) => format!("session:{id}"),
        }
    }
}

static OPEN_REQUEST: Mutex<Option<OpenRequest>> = Mutex::new(None);

/// Remember a request from the command line; [`apply_open_request`] shows it once.
pub fn request_open(req: OpenRequest) {
    if let Ok(mut slot) = OPEN_REQUEST.lock() {
        *slot = Some(req);
    }
}

/// Every frame until done: apply the pending request once the layout is restored.
pub fn apply_open_request(app: &mut App) {
    let Some(req) = OPEN_REQUEST.lock().ok().and_then(|slot| *slot) else {
        return;
    };
    if !layout::is_restored(app) {
        return;
    }
    match req {
        OpenRequest::Activity => {
            app.ui.page = Page::Work;
            app.ui.right = Some(RightPanel::Activity);
        }
        OpenRequest::Quit => app.ui.modal = Some(Modal::Quit),
        OpenRequest::Session(id) => {
            let Some(list) = data::all_sessions(app, HostKey::Local).data else {
                return;
            };
            match list.iter().find(|s| s.session.id == id).map(|s| s.session.clone()) {
                Some(s) => {
                    let ws = WsKey { host: HostKey::Local, id: s.workspace_id };
                    actions::open_session(app, s.project_id, ws, id, s.hidden);
                }
                None => app.toast_info("메뉴 막대에서 고른 세션을 찾지 못했어요"),
            }
        }
    }
    if let Ok(mut slot) = OPEN_REQUEST.lock() {
        *slot = None;
    }
}

/// The window is closing. Keep a presence in the menu bar when this computer's host is still up
/// and a session process is alive (unknown counts as alive); the menu bar process decides the rest.
pub fn on_window_closed(app: &mut App) {
    if !cfg!(target_os = "macos") || verification_run() {
        return;
    }
    // Also false after "작업까지 종료", which stops the host and disconnects.
    if !app.hub.state(HostKey::Local).is_connected() {
        return;
    }
    // Quiet sessions (입력 대기) count: their processes keep running without the window.
    if let Some(list) = data::all_sessions(app, HostKey::Local).data {
        let alive = list.iter().any(|s| {
            data::live_of(app, HostKey::Local, s).is_some_and(|l| matches!(l.phase, Phase::Running | Phase::Starting))
        });
        if !alive {
            return;
        }
    }
    if let Err(e) = spawn_menubar(&app.exe) {
        tracing::warn!("menu bar presence not started: {e}");
    }
}

/// Snapshot and scripted-input runs verify the window; they never leave a menu bar behind.
fn verification_run() -> bool {
    crate::snapshot::requested() || crate::terminal::verify::requested()
}

fn spawn_menubar(exe: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(paths::logs_dir())?;
    let log = OpenOptions::new().create(true).append(true).open(paths::logs_dir().join("menubar.log"))?;
    Command::new(exe)
        .args(["menubar", "--after-window"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(log))
        .process_group(0)
        .spawn()
        .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_requests_round_trip_and_reject_unknown_values() {
        let id = Uuid::new_v4();
        for req in [OpenRequest::Activity, OpenRequest::Quit, OpenRequest::Session(id)] {
            assert_eq!(OpenRequest::parse(&req.to_arg()), Some(req));
        }
        assert_eq!(OpenRequest::parse("session:nope"), None);
        assert_eq!(OpenRequest::parse("settings"), None);
    }

    #[test]
    fn menubar_preference_defaults_to_on() {
        assert!(menubar_enabled(None));
        assert!(menubar_enabled(Some(&serde_json::json!("x"))));
        assert!(!menubar_enabled(Some(&serde_json::json!(false))));
        assert!(menubar_enabled(Some(&serde_json::json!(true))));
    }
}
