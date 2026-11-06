//! Window shell: title bar, sidebar (or icon rail), tabs, center, session bar, right panel,
//! and overlays (palette, modals, toasts). Layout follows docs/design (Graphite Wick).

pub mod actions;
mod center;
pub mod data;
mod keys;
pub mod layout;
pub mod modal;
mod palette;
mod right;
mod session_bar;
mod settings_page;
mod sidebar;
mod start;
mod tabs;
mod titlebar;
mod toasts;
pub mod widgets;

pub use modal::{dialog, dialog_title, show_new_session_modal, show_rename_modal};
pub use start::show_start_tab;

use crate::App;
use crate::client::HostKey;
use crate::state::Page;
use crate::theme::{c, size};
use egui::{Rect, Sense, Stroke};

pub struct State {
    pub layout: layout::State,
    pub keys: keys::State,
    pub palette: palette::State,
    pub modal: modal::State,
    pub sidebar: sidebar::State,
    pub center: center::State,
    pub right: right::State,
    drop_hover: bool,
    /// This computer's host answered at least once (a later loss is "연결 끊김").
    pub ever_connected: bool,
    /// Verification aid: `RUNWICK_UI_OVERLAY=palette|new-session|rename|flyout|workers|stop` (and the
    /// collaboration ones in `views::collab::verify_overlay`) opens that overlay once the data it
    /// needs is loaded (overlays are not part of `RUNWICK_UI_STATE`).
    overlay_request: Option<String>,
}

impl Default for State {
    fn default() -> Self {
        // A verification run that passes its own UI state must not be overridden by the saved one.
        let skip_restore = hook_env!("RUNWICK_UI_STATE").is_some();
        Self {
            layout: layout::State::new(skip_restore),
            keys: keys::State::default(),
            palette: palette::State::default(),
            modal: modal::State::default(),
            sidebar: sidebar::State::default(),
            center: center::State::default(),
            right: right::State::default(),
            drop_hover: false,
            ever_connected: false,
            overlay_request: hook_env!("RUNWICK_UI_OVERLAY"),
        }
    }
}

pub fn on_connected(app: &mut App, host: HostKey) {
    if host == HostKey::Local {
        app.shell.ever_connected = true;
        layout::on_connected(app);
    }
}

pub fn on_exit(app: &mut App) {
    layout::flush(app);
    crate::views::docs::save_drafts(app);
    crate::views::wiki::save_drafts(app);
    crate::presence::on_window_closed(app);
}

/// Welcome when there is no project at all; settings when asked; otherwise the workroom.
fn effective_page(app: &mut App) -> Page {
    if app.ui.page == Page::Settings {
        return Page::Settings;
    }
    let page = match data::projects(app).data {
        Some(list) if data::visible_projects(&list).next().is_none() => Page::Welcome,
        Some(_) => Page::Work,
        // Unknown yet: keep what we had (the workroom shows its own loading states).
        None => app.ui.page,
    };
    app.ui.page = page;
    page
}

pub fn show(app: &mut App, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
    let ctx = ui.ctx().clone();
    frame.set_traffic_lights_position(&ctx, egui::Rangef::new(0.0, size::TITLEBAR), 14.0);
    keys::track_ime(app, &ctx);
    keys::handle(app, &ctx);
    crate::views::lifecycle::tick(app, &ctx, app.shell.keys.composing);
    dropped_folders(app, &ctx);
    layout::validate(app);
    crate::presence::apply_open_request(app);
    open_requested_overlay(app, &ctx);
    let page = effective_page(app);
    titlebar::show(app, ui, page);
    // A window the host refused shows why and what to do, instead of failing lists.
    if crate::views::window_refusal::show(app, ui) {
        toasts::show(app, &ctx);
        return;
    }
    match page {
        Page::Welcome => start::show_welcome(app, ui),
        Page::Settings => settings_page::show(app, ui),
        Page::Work => work(app, ui),
    }
    crate::views::collab::show_overlays(app, &ctx);
    modal::show_container(app, &ctx);
    crate::views::refresh::show_overlay(app, &ctx);
    palette::show(app, &ctx);
    toasts::show(app, &ctx);
    drop_overlay(app, &ctx);
    right::track_focus(app, &ctx);
    layout::tick(app, &ctx);
}

fn work(app: &mut App, ui: &mut egui::Ui) {
    let ctx = ui.ctx().clone();
    let win_w = ctx.content_rect().width();
    // The design's 860 frame already uses the rail.
    let narrow = win_w <= size::NARROW_WINDOW + 0.5;
    let rail = narrow || app.ui.sidebar_collapsed;
    if !narrow {
        app.shell.sidebar.flyout = false;
    }
    if rail {
        sidebar::rail::show(app, ui, narrow);
    } else {
        sidebar::show(app, ui);
    }
    let side_w = if rail { size::SIDEBAR_COLLAPSED } else { app.ui.sidebar_width };
    let panel = app.ui.right.clone();
    let right_w = app.ui.right_width.clamp(size::RIGHT_PANEL_MIN, size::RIGHT_PANEL_MAX);
    let overlay = panel.is_some() && win_w - side_w - right_w < size::CENTER_MIN;
    if let (Some(p), false) = (&panel, overlay) {
        right::show_docked(app, ui, p);
    }
    center::show(app, ui);
    if let (Some(p), true) = (&panel, overlay) {
        right::show_overlay(app, &ctx, p);
    }
    if panel.is_none() {
        app.shell.right.rect = None;
    }
    if rail && narrow {
        sidebar::rail::flyout(app, &ctx);
    }
}

fn open_requested_overlay(app: &mut App, ctx: &egui::Context) {
    let Some(req) = app.shell.overlay_request.clone() else { return };
    let (Some(ws), Some(project)) = (app.ui.selected_ws, app.ui.selected_project) else { return };
    if data::workspaces(app).data.is_none() || data::projects(app).data.is_none() {
        return;
    }
    match req.as_str() {
        "palette" => palette::open(app, ctx),
        "flyout" => app.shell.sidebar.flyout = true,
        "stop" => match app.ui.active_tab().map(|t| t.kind.clone()) {
            Some(crate::state::TabKind::Terminal { host, session }) => app.shell.modal.ask_stop(host, session),
            _ => return,
        },
        "workers" => {
            let Some(list) = data::sessions_in(app, ws).data else { return };
            app.shell.sidebar.expanded_parents.extend(list.iter().filter_map(|s| s.session.parent_id));
        }
        "new-session" => app.ui.modal = Some(crate::state::Modal::NewSession { ws }),
        "rename" => {
            let target = crate::state::RenameTarget::Project { host: HostKey::Local, id: project };
            app.ui.modal = Some(crate::state::Modal::Rename { target });
        }
        other if crate::views::collab::verify_overlay(app, other) => {}
        other => tracing::warn!("unknown overlay request: {other}"),
    }
    app.shell.overlay_request = None;
}

/// Invisible drag handle along a panel edge. Returns the horizontal drag delta.
pub(crate) fn resize_handle(ui: &mut egui::Ui, rect: Rect, id: &str) -> Option<f32> {
    let resp = ui.interact(rect, ui.scope_id().with(id), Sense::drag());
    if resp.hovered() || resp.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
        let x = rect.center().x;
        ui.painter().vline(
            x,
            rect.y_range(),
            Stroke::new(2.0, if resp.dragged() { c::TEXT_3 } else { c::LINE_STRONG }),
        );
    }
    resp.dragged().then(|| resp.drag_delta().x).filter(|d| *d != 0.0)
}

/// Folders dropped on the window open as projects.
fn dropped_folders(app: &mut App, ctx: &egui::Context) {
    let (hover, dropped) = ctx.input(|i| {
        let hover = i.raw.hovered_files.iter().any(|f| f.path.as_ref().is_some_and(|p| p.is_dir()));
        let dropped: Vec<std::path::PathBuf> = i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).collect();
        (hover, dropped)
    });
    app.shell.drop_hover = hover;
    for path in dropped {
        if path.is_dir() {
            actions::open_folder(app, path.to_string_lossy().to_string());
        } else {
            app.toast_info("폴더만 열 수 있어요. 파일이 있는 폴더를 끌어다 놓으세요");
        }
    }
}

fn drop_overlay(app: &mut App, ctx: &egui::Context) {
    if !app.shell.drop_hover {
        return;
    }
    let screen = ctx.content_rect();
    let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::unique("drop-overlay")));
    painter.rect_filled(screen, egui::CornerRadius::ZERO, egui::Color32::from_black_alpha(140));
    let r = screen.shrink(24.0);
    painter.rect_stroke(r, egui::CornerRadius::same(14), Stroke::new(1.5, c::EMBER), egui::StrokeKind::Inside);
    painter.text(
        r.center(),
        egui::Align2::CENTER_CENTER,
        "놓으면 폴더를 프로젝트로 열어요",
        crate::theme::medium(16.0),
        c::TEXT_STRONG,
    );
}
