//! Navigation and host mutations shared by the shell's surfaces (sidebar, tabs, palette,
//! shortcuts). Feature views may call these too (for example to open a session's tab).

use crate::App;
use crate::client::HostKey;
use crate::state::{Modal, Page, RightPanel, TabId, TabKind, WsKey};
use runwick_core::api::projects::{
    OpenFolder, ProjectId, ProjectUpdate, ProjectsOpen, ProjectsUpdate, WorkspaceUpdate, WorkspacesUpdate,
};
use runwick_core::api::sessions::{
    Restart, SessionId, SessionUpdate, SessionsRestart, SessionsStart, SessionsStop, SessionsUpdate, StartSession,
    Stop, Tool,
};
use runwick_core::josa;

/// Ask for a folder with the system dialog and open it as a project.
pub fn open_folder_dialog(app: &mut App) {
    if let Some(dir) = rfd::FileDialog::new().set_title("폴더 열기").pick_folder() {
        open_folder(app, dir.to_string_lossy().to_string());
    }
}

/// Open a folder as a project (or move to it when already known) and select its workspace.
pub fn open_folder(app: &mut App, path: String) {
    app.hub.call::<ProjectsOpen>(HostKey::Local, OpenFolder { path }, |app, res| match res {
        Ok(o) => {
            let ws = WsKey { host: HostKey::Local, id: o.workspace.id };
            if app.ui.page == Page::Welcome {
                app.ui.page = Page::Work;
            }
            select_workspace(app, o.project.id, ws);
            if !o.created {
                app.toast_info(&format!("이미 열려 있는 {} 이동했어요", josa::euro_ro(&o.project.name)));
            }
        }
        Err(e) => app.toast_error(&e.message),
    });
}

/// Select a workspace: its tabs and files replace the center (other workspaces keep running).
pub fn select_workspace(app: &mut App, project: ProjectId, ws: WsKey) {
    let prev = app.ui.selected_ws;
    let changed = prev != Some(ws);
    app.ui.selected_project = Some(project);
    app.ui.selected_ws = Some(ws);
    app.ui.expanded_projects.insert(project);
    if app.ui.page == Page::Settings {
        app.ui.page = Page::Work;
    }
    if changed {
        app.shell.center.note_switch(prev, ws);
        app.shell.sidebar.flyout = false;
        if ws.host == HostKey::Local {
            app.hub.call::<WorkspacesUpdate>(
                ws.host,
                WorkspaceUpdate { id: ws.id, touch: true, ..Default::default() },
                |_, _| {},
            );
        }
    }
}

/// Select a project: expand it and open its most recently used workspace.
pub fn select_project(app: &mut App, project: ProjectId) {
    app.ui.expanded_projects.insert(project);
    let Some(list) = super::data::workspaces(app).data else { return };
    if let Some(w) = super::layout::best_workspace(&list, project) {
        let key = super::data::ws_key(w);
        select_workspace(app, project, key);
    }
}

/// Open (or focus) a session's terminal tab in its workspace. Shows the tab again if it was closed.
pub fn open_session(app: &mut App, project: ProjectId, ws: WsKey, session: SessionId, hidden: bool) {
    select_workspace(app, project, ws);
    app.shell.center.note_opened(session);
    app.ui.open_tab(ws, TabKind::Terminal { host: ws.host, session });
    if hidden {
        let upd = SessionUpdate { id: session, hidden: Some(false), ..Default::default() };
        app.hub.send::<SessionsUpdate>(ws.host, upd);
    }
}

/// Open a document-like tab (wiki, timeline, history) in the selected workspace.
pub fn open_center_tab(app: &mut App, kind: TabKind) {
    match app.ui.selected_ws {
        Some(ws) => {
            if app.ui.page != Page::Work {
                app.ui.page = Page::Work;
            }
            app.ui.open_tab(ws, kind);
        }
        None => app.toast_info("먼저 작업 공간을 고르세요"),
    }
}

/// Close a tab in the view only. Terminal runs and records continue.
pub fn close_tab(app: &mut App, ws: WsKey, id: TabId) {
    let kind = app.ui.tabset(&ws).and_then(|s| s.tabs.iter().find(|t| t.id == id)).map(|t| t.kind.clone());
    // A document with unsaved edits asks first (저장 / 편집 유지 / 버리기).
    if let Some(TabKind::Doc { ws: doc_ws, path }) = &kind
        && crate::views::docs::ask_before_close(app, ws, id, *doc_ws, path)
    {
        return;
    }
    if let Some(TabKind::Terminal { host, session }) = kind {
        app.hub.send::<SessionsUpdate>(host, SessionUpdate { id: session, hidden: Some(true), ..Default::default() });
    }
    app.ui.close_tab(&ws, id);
}

/// Start a CLI in a workspace and open its tab.
pub fn start_session(app: &mut App, ws: WsKey, project: ProjectId, tool: Tool, title: Option<String>) {
    let (cols, rows) = app.shell.center.terminal_grid();
    app.shell.center.starting = Some(tool);
    let params = StartSession {
        workspace_id: ws.id,
        tool,
        title,
        initial_prompt: None,
        parent_id: None,
        role: None,
        cols,
        rows,
    };
    app.hub.call::<SessionsStart>(ws.host, params, move |app, res| {
        app.shell.center.starting = None;
        match res {
            Ok(info) => {
                app.shell.modal.last_tool = Some(tool);
                open_session(app, project, ws, info.session.id, false);
            }
            Err(e) => app.toast_error(&format!("{} 시작하지 못했어요: {}", josa::eul_reul(tool.label()), e.message)),
        }
    });
}

pub fn stop_session(app: &mut App, host: HostKey, id: SessionId) {
    app.hub.send::<SessionsStop>(host, Stop { id, force: false });
}

/// Run an ended session again: resume the same conversation or start a new process.
pub fn restart_session(app: &mut App, host: HostKey, id: SessionId, resume: bool) {
    let (cols, rows) = app.shell.center.terminal_grid();
    app.hub.call::<SessionsRestart>(host, Restart { id, resume, cols, rows }, move |app, res| {
        if let Err(e) = res {
            let what = if resume { "같은 대화를 재개하지" } else { "새로 실행하지" };
            app.toast_error(&format!("{what} 못했어요: {}", e.message));
        }
    });
}

pub fn set_favorite(app: &mut App, id: ProjectId, favorite: bool) {
    app.hub
        .send::<ProjectsUpdate>(HostKey::Local, ProjectUpdate { id, favorite: Some(favorite), ..Default::default() });
}

/// Hide a project from the list. Runs, records and the folder stay.
pub fn archive_project(app: &mut App, id: ProjectId, name: &str) {
    let name = name.to_string();
    app.hub.call::<ProjectsUpdate>(
        HostKey::Local,
        ProjectUpdate { id, archived: Some(true), ..Default::default() },
        move |app, res| match res {
            Ok(_) => {
                if app.ui.selected_project == Some(id) {
                    app.ui.selected_project = None;
                    app.ui.selected_ws = None;
                }
                app.toast_info(&format!("{} 목록에서 숨겼어요. 폴더를 다시 열면 돌아와요", josa::eul_reul(&name)));
            }
            Err(e) => app.toast_error(&e.message),
        },
    );
}

/// Hide a workspace from the list. Its folder and records stay.
pub fn archive_workspace(app: &mut App, ws: WsKey, name: &str) {
    let name = name.to_string();
    app.hub.call::<WorkspacesUpdate>(
        HostKey::Local,
        WorkspaceUpdate { id: ws.id, archived: Some(true), ..Default::default() },
        move |app, res| match res {
            Ok(_) => {
                if app.ui.selected_ws == Some(ws) {
                    app.ui.selected_ws = None;
                }
                app.toast_info(&format!("{name} 공간을 목록에서 숨겼어요. 폴더와 기록은 그대로예요"));
            }
            Err(e) => app.toast_error(&e.message),
        },
    );
}

/// Show a folder or file in Finder (or the platform file manager).
pub fn reveal(app: &mut App, path: &str) {
    if !std::path::Path::new(path).exists() {
        app.toast_error(&format!("폴더를 찾을 수 없어요: {path}"));
        return;
    }
    #[cfg(target_os = "macos")]
    let r = std::process::Command::new("open").arg(path).spawn();
    #[cfg(not(target_os = "macos"))]
    let r = std::process::Command::new("xdg-open").arg(path).spawn();
    if let Err(e) = r {
        app.toast_error(&format!("파일 관리자를 열지 못했어요: {e}"));
    }
}

pub fn copy_path(app: &mut App, ctx: &egui::Context, path: &str) {
    ctx.copy_text(path.to_string());
    app.toast(crate::state::ToastKind::Success, "경로를 복사했어요");
}

pub fn toggle_right(app: &mut App, panel: RightPanel) {
    if app.ui.right.as_ref() == Some(&panel) {
        app.ui.right = None;
    } else {
        app.ui.right = Some(panel);
    }
}

pub fn new_session(app: &mut App) {
    match app.ui.selected_ws {
        Some(ws) => app.ui.modal = Some(Modal::NewSession { ws }),
        None => app.toast_info("먼저 작업 공간을 고르세요"),
    }
}

pub fn open_settings(app: &mut App, section: Option<crate::state::SettingsSection>) {
    app.ui.page = Page::Settings;
    if let Some(s) = section {
        app.ui.settings_section = s;
    }
}
