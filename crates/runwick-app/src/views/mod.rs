//! Feature views. Each module owns its `State` (held in `App::feat`) and its screens.
//! The dispatch functions below route tabs, right panels and modals to the owning module.

pub mod activity;
pub mod backup;
pub mod collab;
pub mod devices;
pub mod docs;
pub mod files;
pub mod history;
pub mod lifecycle;
pub mod refresh;
pub mod settings;
pub mod split;
pub mod timeline;
pub mod wiki;
pub mod window_refusal;

use crate::App;
use crate::client::HostKey;
use crate::state::{Modal, PanelDetail, RightPanel, Tab, TabKind, WsKey};
use serde_json::Value;

/// Host events for features (after the shell and terminal have seen them).
pub fn on_event(app: &mut App, host: HostKey, topic: &str, data: &Value) {
    devices::on_event(app, host, topic, data);
    docs::on_event(app, host, topic, data);
    wiki::on_event(app, host, topic, data);
    lifecycle::on_event(app, host, topic, data);
}

/// Draw the body of a tab.
pub fn show_tab(app: &mut App, ui: &mut egui::Ui, ws: WsKey, tab: &Tab) {
    match &tab.kind {
        TabKind::Terminal { host, session } => devices::show_terminal_tab(app, ui, *host, *session, tab.id),
        TabKind::Doc { ws, path } => docs::show_tab(app, ui, *ws, path, tab.id),
        TabKind::Wiki { project, page } => wiki::show_tab(app, ui, *project, page.as_deref(), tab.id),
        TabKind::WikiGraph { project } => wiki::show_graph_tab(app, ui, *project, tab.id),
        TabKind::Timeline { project } => timeline::show_tab(app, ui, *project, tab.id),
        TabKind::History => history::show_tab(app, ui, tab.id),
        TabKind::Record { host, anchor } => history::show_record_tab(app, ui, *host, *anchor, tab.id),
        TabKind::Pin { host, id } => history::show_pin(app, ui, *host, *id),
        TabKind::Result { host, request } => collab::show_result_tab(app, ui, *host, *request, tab.id),
        TabKind::Merge { ws } => split::show_merge_tab(app, ui, *ws, tab.id),
        TabKind::Start => crate::shell::show_start_tab(app, ui, ws, tab.id),
    }
}

/// Draw the right detail panel.
pub fn show_right_panel(app: &mut App, ui: &mut egui::Ui, panel: &RightPanel) {
    let detail = app.ui.panel().and_then(|p| p.current()).map(|f| f.detail.clone());
    if let Some(detail) = detail {
        match detail {
            PanelDetail::Pin { host, id } => history::show_pin(app, ui, host, id),
            PanelDetail::Message(open) => history::show_detail(app, ui, open),
            PanelDetail::Result { host, request } => collab::show_result_panel(app, ui, host, request),
            PanelDetail::WorkerList { host, session } => collab::show_panel(app, ui, host, session),
            PanelDetail::Worker { host, session } => collab::show_worker(app, ui, host, session),
            PanelDetail::Space { ws } => devices::show_space_panel(app, ui, ws),
        }
        return;
    }
    match panel {
        RightPanel::History => history::show_panel(app, ui),
        RightPanel::Pins => history::show_pins(app, ui),
        RightPanel::Workers => match app.ui.panel_target() {
            Some(t) => collab::show_panel(app, ui, t.host, t.session),
            None => crate::shell::widgets::empty_note(ui, "세션을 고르면 맡긴 일을 볼 수 있어요"),
        },
        RightPanel::Activity => activity::show_panel(app, ui),
    }
}

/// Draw the open modal, if any. Returns false when the modal closed itself.
pub fn show_modal(app: &mut App, ctx: &egui::Context, modal: &Modal) -> bool {
    match modal {
        Modal::NewSession { ws } => crate::shell::show_new_session_modal(app, ctx, *ws),
        Modal::SplitWork { ws } => split::show_modal(app, ctx, *ws),
        Modal::Refresh { host, session } => refresh::show_modal(app, ctx, *host, *session),
        Modal::Quit => lifecycle::show_quit_modal(app, ctx),
        Modal::Resume => lifecycle::show_resume_modal(app, ctx),
        Modal::AddDevice => devices::show_add_modal(app, ctx),
        Modal::Rename { target } => crate::shell::show_rename_modal(app, ctx, target),
    }
}

/// Small helper for not-yet-built screens: never show fake data.
pub fn placeholder(ui: &mut egui::Ui, title: &str) {
    use crate::theme::{c, font, semibold, ty};
    ui.add_space(24.0);
    ui.vertical_centered(|ui| {
        ui.label(egui::RichText::new(title).font(semibold(ty::PANEL_TITLE)).color(c::TEXT_STRONG));
        ui.label(egui::RichText::new("준비 중인 화면이에요").font(font(ty::BODY)).color(c::TEXT_3));
    });
}
