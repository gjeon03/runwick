//! Sidebar (264, resizable 220–420): project search and filter, projects → workspaces, the
//! selected workspace's sessions or files, and the project tools at the bottom. Below 860 px
//! window width it collapses to an icon rail with a flyout (see [`rail`]).

mod projects;
pub mod rail;
mod sessions;

use super::{actions, widgets};
use crate::App;
use crate::icons;
use crate::state::{Modal, ProjectFilter, RenameTarget, RightPanel, SidebarMode, TabKind, WsKey};
use crate::theme::{Seg, c, font, icon_font, size};
use egui::{Align2, CornerRadius, Pos2, Rect, Sense, Stroke, UiBuilder, Vec2};
use runwick_core::api::projects::ProjectId;
use runwick_core::api::sessions::SessionId;
use std::collections::BTreeSet;

#[derive(Default)]
pub struct State {
    pub organization: projects::OrganizationState,
    /// Parents whose delegated workers are listed under them.
    pub expanded_parents: BTreeSet<SessionId>,
    /// Narrow window: the full sidebar shown over the content.
    pub flyout: bool,
    archive: Option<sessions::ArchiveDialog>,
}

/// Something the person asked for in the sidebar, applied after drawing.
pub enum Act {
    OpenWorker(crate::client::HostKey, uuid::Uuid),
    ToggleExpand(ProjectId),
    SelectProject(ProjectId),
    SelectWs(ProjectId, WsKey),
    Favorite(ProjectId, bool),
    Rename(RenameTarget),
    ArchiveProject(ProjectId, String),
    ArchiveWs(WsKey, String),
    /// Worktree workspace: open its merge tab (S11 통합).
    Merge(ProjectId, WsKey),
    /// Worktree workspace: the folder-removal check (S10).
    RemoveFolder(ProjectId, WsKey),
    Reveal(String),
    CopyPath(String),
    OpenSession {
        project: ProjectId,
        ws: WsKey,
        session: SessionId,
        hidden: bool,
    },
    StopSession(crate::client::HostKey, SessionId),
    Restart(crate::client::HostKey, SessionId, bool),
    ArchiveSession(crate::client::HostKey, SessionId),
    StopAndArchive(crate::client::HostKey, SessionId),
    ToggleEnded(WsKey),
    ArchiveEnded(WsKey, Vec<SessionId>),
    ShowArchive(WsKey),
    ReadProject(ProjectId),
    ToggleWorkers(SessionId),
    SplitWork(WsKey),
    /// `이 공간` panel of a device's workspace.
    SpaceInfo(WsKey),
    NewSession,
    OpenFolder,
}

/// Docked sidebar with a drag handle on its right edge.
pub fn show(app: &mut App, ui: &mut egui::Ui) {
    let width = app.ui.sidebar_width.clamp(size::SIDEBAR_MIN, size::SIDEBAR_MAX);
    let resp = egui::Panel::left("sidebar")
        .exact_size(width)
        .resizable(false)
        .show_separator_line(true)
        .frame(egui::Frame::new().fill(c::NAV))
        .show(ui, |ui| contents(app, ui));
    let rect = resp.response.rect;
    let handle = Rect::from_x_y_ranges(rect.right() - 4.0..=rect.right() + 2.0, rect.y_range());
    if let Some(dx) = super::resize_handle(ui, handle, "sidebar-resize") {
        app.ui.sidebar_width = (app.ui.sidebar_width + dx).clamp(size::SIDEBAR_MIN, size::SIDEBAR_MAX);
    }
}

/// Everything inside the sidebar (also used by the narrow-window flyout).
pub fn contents(app: &mut App, ui: &mut egui::Ui) {
    let full = ui.max_rect();
    let mut acts = Vec::new();
    // Top: search + filter.
    let top = Rect::from_min_max(full.min, Pos2::new(full.right(), full.top() + 48.0));
    ui.scope_builder(
        UiBuilder::new().max_rect(top.shrink2(Vec2::new(10.0, 0.0)).translate(Vec2::new(0.0, 10.0))),
        |ui| {
            projects::filter_bar(app, ui);
        },
    );
    let tools = Rect::from_min_max(Pos2::new(full.left(), full.bottom() - 42.0), full.max);
    // Middle: projects, then the selected workspace's list (drawn before the bottom tools so
    // Tab reaches the projects and workspaces first).
    let middle = Rect::from_min_max(Pos2::new(full.left(), top.bottom()), Pos2::new(full.right(), tools.top()));
    ui.scope_builder(UiBuilder::new().max_rect(middle).layout(egui::Layout::top_down(egui::Align::Min)), |ui| {
        ui.spacing_mut().item_spacing = Vec2::new(0.0, 0.0);
        let max_projects = (middle.height() * 0.5).max(120.0);
        egui::ScrollArea::vertical().id_salt("sb-projects").max_height(max_projects).auto_shrink([false, true]).show(
            ui,
            |ui| {
                egui::Frame::new().inner_margin(egui::Margin { left: 6, right: 6, top: 0, bottom: 8 }).show(ui, |ui| {
                    ui.spacing_mut().item_spacing = Vec2::new(0.0, 1.0);
                    projects::list(app, ui, &mut acts);
                });
            },
        );
        sessions::space_header(app, ui);
        egui::ScrollArea::vertical().id_salt("sb-list").auto_shrink([false, false]).show(ui, |ui| {
            egui::Frame::new().inner_margin(egui::Margin { left: 6, right: 6, top: 0, bottom: 8 }).show(ui, |ui| {
                ui.spacing_mut().item_spacing = Vec2::new(0.0, 1.0);
                match (app.ui.sidebar_mode, app.ui.selected_ws) {
                    (SidebarMode::Files, Some(ws)) => crate::views::files::show_sidebar(app, ui, ws),
                    _ => sessions::list(app, ui, &mut acts),
                }
            });
        });
    });
    // Bottom: project tools.
    ui.painter().hline(tools.x_range(), tools.top() + 0.5, Stroke::new(1.0, c::LINE));
    ui.scope_builder(UiBuilder::new().max_rect(tools.shrink(6.0)), |ui| tool_bar(app, ui));
    let ctx = ui.ctx().clone();
    for a in acts {
        apply(app, &ctx, a);
    }
}

fn apply(app: &mut App, ctx: &egui::Context, act: Act) {
    match act {
        Act::ToggleExpand(p) => {
            if !app.ui.expanded_projects.remove(&p) {
                app.ui.expanded_projects.insert(p);
            }
        }
        Act::SelectProject(p) => actions::select_project(app, p),
        Act::SelectWs(p, ws) => actions::select_workspace(app, p, ws),
        Act::Favorite(p, on) => actions::set_favorite(app, p, on),
        Act::Rename(target) => app.ui.modal = Some(Modal::Rename { target }),
        Act::ArchiveProject(p, name) => actions::archive_project(app, p, &name),
        Act::ArchiveWs(ws, name) => actions::archive_workspace(app, ws, &name),
        Act::Merge(p, ws) => crate::views::split::open_merge(app, p, ws, false),
        Act::RemoveFolder(p, ws) => crate::views::split::open_merge(app, p, ws, true),
        Act::Reveal(path) => actions::reveal(app, &path),
        Act::SpaceInfo(ws) => crate::views::devices::open_space_panel(app, ws),
        Act::CopyPath(path) => actions::copy_path(app, ctx, &path),
        Act::OpenWorker(host, session) => crate::views::collab::open_worker(app, host, session),
        Act::OpenSession { project, ws, session, hidden } => actions::open_session(app, project, ws, session, hidden),
        Act::StopSession(host, id) => app.shell.modal.ask_stop(host, id),
        Act::Restart(host, id, resume) => actions::restart_session(app, host, id, resume),
        Act::ArchiveSession(host, id) => sessions::set_archived(app, host, id, true),
        Act::StopAndArchive(host, id) => app.shell.modal.ask_stop_and_archive(host, id),
        Act::ToggleEnded(ws) => {
            if !app.ui.expanded_ended_sessions.remove(&ws) {
                app.ui.expanded_ended_sessions.insert(ws);
            }
        }
        Act::ArchiveEnded(ws, ids) => {
            app.shell.sidebar.archive = Some(sessions::ArchiveDialog::Confirm { ws, ids });
        }
        Act::ShowArchive(ws) => app.shell.sidebar.archive = Some(sessions::ArchiveDialog::List(ws)),
        Act::ReadProject(id) => crate::views::activity::mark_project_read(app, id),
        Act::ToggleWorkers(id) => {
            if !app.shell.sidebar.expanded_parents.remove(&id) {
                app.shell.sidebar.expanded_parents.insert(id);
            }
        }
        Act::SplitWork(ws) => app.ui.modal = Some(Modal::SplitWork { ws }),
        Act::NewSession => actions::new_session(app),
        Act::OpenFolder => actions::open_folder_dialog(app),
    }
}

/// Bottom tools: 기록 · 위키 · 타임라인 for the selected project.
fn tool_bar(app: &mut App, ui: &mut egui::Ui) {
    let r = ui.max_rect();
    let w = (r.width() - 4.0) / 3.0;
    let active_kind = app.ui.active_tab().map(|t| t.kind.clone());
    let history_on = app.ui.right == Some(RightPanel::History);
    let wiki_on = matches!(active_kind, Some(TabKind::Wiki { .. }) | Some(TabKind::WikiGraph { .. }));
    let timeline_on = matches!(active_kind, Some(TabKind::Timeline { .. }));
    let items = [
        (icons::HISTORY, "기록", history_on, "대화 기록과 핀 (⌘⇧F)"),
        (icons::BOOK_OPEN, "위키", wiki_on, "프로젝트 위키"),
        (icons::CHART_GANTT, "타임라인", timeline_on, "프로젝트 타임라인"),
    ];
    for (i, (glyph, text, on, tip)) in items.into_iter().enumerate() {
        let rect = Rect::from_min_size(Pos2::new(r.left() + i as f32 * (w + 2.0), r.top()), Vec2::new(w, 30.0));
        let resp = ui.interact(rect, ui.scope_id().with(("sb-tool", i)), Sense::click());
        resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, on, text));
        let bg = if on {
            Some(c::CONTROL)
        } else if resp.hovered() {
            Some(c::ROW)
        } else {
            None
        };
        if let Some(bg) = bg {
            ui.painter().rect_filled(rect, CornerRadius::same(6), bg);
        }
        let fg = if on || resp.hovered() { c::TEXT_STRONG } else { c::TEXT_2 };
        let tw = widgets::text_width(ui, text, font(12.0));
        let x0 = rect.center().x - (tw + 18.0) / 2.0;
        ui.painter().text(Pos2::new(x0 + 6.5, rect.center().y), Align2::CENTER_CENTER, glyph, icon_font(13.0), fg);
        ui.painter().text(Pos2::new(x0 + 18.0, rect.center().y), Align2::LEFT_CENTER, text, font(12.0), fg);
        crate::theme::focus_ring(ui, &resp, CornerRadius::same(6));
        if resp.on_hover_text(tip).clicked() {
            match i {
                0 => actions::toggle_right(app, RightPanel::History),
                1 => match app.ui.selected_project {
                    Some(project) => actions::open_center_tab(app, TabKind::Wiki { project, page: None }),
                    None => app.toast_info("먼저 프로젝트를 고르세요"),
                },
                _ => actions::open_center_tab(app, TabKind::Timeline { project: app.ui.selected_project }),
            }
        }
    }
}

/// Segment index for the project filter.
fn filter_index(f: ProjectFilter) -> usize {
    match f {
        ProjectFilter::All => 0,
        ProjectFilter::Favorites => 1,
    }
}

fn filter_segments() -> [Seg<'static>; 2] {
    [Seg::text("전체"), Seg::icon(icons::STAR, "즐겨찾기")]
}

/// Session management dialogs also remain available when the sidebar is collapsed.
pub fn show_dialogs(app: &mut App, ctx: &egui::Context) {
    sessions::show_archive_dialog(app, ctx);
}
