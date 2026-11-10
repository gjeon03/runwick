//! Right detail panel (380, resizable): panel head with the open items and a close button,
//! then the feature's body. When the center would drop below 520 it floats over the center.
//! Closing (× or Esc) returns keyboard focus to where it was.

use super::{data, widgets};
use crate::App;
use crate::icons;
use crate::state::{PanelDetail, RightPanel, TabKind};
use crate::theme::{c, font, medium, size};
use egui::{CornerRadius, Pos2, Rect, Sense, Stroke, UiBuilder, Vec2};

#[derive(Default)]
pub struct State {
    /// Panel rect of the last frame.
    pub rect: Option<Rect>,
    /// Focus outside the panel before it opened, restored on close.
    return_focus: Option<egui::Id>,
    focused: bool,
}

/// Remember where keyboard focus was outside the panel (call every frame).
pub fn track_focus(app: &mut App, ctx: &egui::Context) {
    let Some(id) = ctx.memory(|m| m.focused()) else { return };
    let inside = match (app.shell.right.rect, ctx.read_response(id)) {
        (Some(p), Some(r)) => p.contains(r.rect.center()),
        _ => false,
    };
    if !inside {
        app.shell.right.return_focus = Some(id);
    }
}

pub fn close(app: &mut App, ctx: &egui::Context) {
    app.ui.right = None;
    app.shell.right.focused = false;
    app.shell.right.rect = None;
    if let Some(id) = app.shell.right.return_focus.take() {
        ctx.memory_mut(|m| m.request_focus(id));
    }
}

/// Docked panel at the right edge.
pub fn show_docked(app: &mut App, ui: &mut egui::Ui, panel: &RightPanel) {
    let width = app.ui.right_width.clamp(size::RIGHT_PANEL_MIN, size::RIGHT_PANEL_MAX);
    let resp = egui::Panel::right("right-panel")
        .exact_size(width)
        .resizable(false)
        .show_separator_line(true)
        .frame(egui::Frame::new().fill(c::PANEL))
        .show(ui, |ui| body(app, ui, panel));
    let rect = resp.response.rect;
    app.shell.right.rect = Some(rect);
    let handle = Rect::from_x_y_ranges(rect.left() - 2.0..=rect.left() + 4.0, rect.y_range());
    if let Some(dx) = super::resize_handle(ui, handle, "right-resize") {
        app.ui.right_width = (app.ui.right_width - dx).clamp(size::RIGHT_PANEL_MIN, size::RIGHT_PANEL_MAX);
        app.ui.panel_mut().width = app.ui.right_width;
    }
}

/// Panel floating over the center (narrow windows).
pub fn show_overlay(app: &mut App, ctx: &egui::Context, panel: &RightPanel) {
    let screen = ctx.content_rect();
    let width = app.ui.right_width.clamp(size::RIGHT_PANEL_MIN, size::RIGHT_PANEL_MAX).min(screen.width());
    let rect = Rect::from_min_max(Pos2::new(screen.right() - width, screen.top() + size::TITLEBAR), screen.max);
    app.shell.right.rect = Some(rect);
    egui::Area::new(egui::Id::unique("right-overlay")).order(egui::Order::Middle).fixed_pos(rect.min).show(ctx, |ui| {
        ui.set_min_size(rect.size());
        ui.set_max_size(rect.size());
        let shadow =
            egui::epaint::Shadow { offset: [-20, 0], blur: 48, spread: 0, color: egui::Color32::from_black_alpha(115) };
        ui.painter().with_clip_rect(screen).add(shadow.as_shape(rect, CornerRadius::ZERO));
        ui.painter().rect_filled(rect, CornerRadius::ZERO, c::PANEL);
        ui.scope_builder(UiBuilder::new().max_rect(rect), |ui| body(app, ui, panel));
    });
}

fn body(app: &mut App, ui: &mut egui::Ui, panel: &RightPanel) {
    if !app.shell.right.focused {
        let id = ui.scope_id().with("panel-focus");
        ui.interact(egui::Rect::from_min_size(ui.max_rect().min, Vec2::splat(1.0)), id, Sense::click()).request_focus();
        app.shell.right.focused = true;
    }
    resolve_target(app);
    let full = ui.max_rect();
    let head = Rect::from_min_size(full.min, Vec2::new(full.width(), size::PANEL_HEAD));
    ui.scope_builder(UiBuilder::new().max_rect(head), |ui| panel_head(app, ui, panel));
    let rest = Rect::from_min_max(Pos2::new(full.left(), head.bottom()), full.max);
    let mut child = ui.new_child(
        UiBuilder::new().max_rect(rest).id_salt("right-body").layout(egui::Layout::top_down(egui::Align::Min)),
    );
    child.set_clip_rect(rest.intersect(ui.clip_rect()));
    if *panel != RightPanel::Activity {
        target_row(app, &mut child);
    }
    navigation(app, &mut child);
    let selected = app.ui.right.unwrap_or(*panel);
    crate::views::show_right_panel(app, &mut child, &selected);
}

fn resolve_target(app: &mut App) {
    use runwick_core::api::{
        collab::CollabGet,
        projects::ById,
        records::{ContextQuery, PinQuery, PinsList, RecordsContext},
    };
    let Some(frame) = app.ui.panel().and_then(|p| p.current()).cloned() else { return };
    let target = match &frame.detail {
        PanelDetail::Pin { host, id } => app
            .hub
            .query::<PinsList>(*host, &PinQuery::default())
            .data
            .and_then(|l| l.iter().find(|p| p.id == *id).and_then(|p| p.session_id))
            .map(|session| crate::state::PanelTarget { host: *host, session }),
        PanelDetail::Message(o) => app
            .hub
            .query::<RecordsContext>(o.host, &ContextQuery { anchor: o.anchor, before: 0, after: 0 })
            .data
            .map(|cx| crate::state::PanelTarget { host: o.host, session: cx.session_id }),
        PanelDetail::Result { host, request } => app
            .hub
            .query::<CollabGet>(*host, &ById { id: *request })
            .data
            .map(|v| crate::state::PanelTarget { host: *host, session: v.request.worker_id }),
        _ => frame.target,
    };
    if let Some(tab) = app.ui.right
        && let Some(top) = app.ui.panel_mut().stacks.get_mut(&tab).and_then(|s| s.last_mut())
    {
        top.target = target;
    }
}

fn target_row(app: &mut App, ui: &mut egui::Ui) {
    let target = app.ui.panel_target();
    let label = target
        .and_then(|t| {
            let sessions = data::all_sessions(app, t.host).data?;
            let session = sessions.iter().find(|s| s.session.id == t.session)?;
            let place = crate::views::collab::place(app, t.host, &session.session);
            let path = format!("{} › {} › {}", place.project, place.workspace, session.session.title);
            Some(if t.host == crate::client::HostKey::Local {
                path
            } else {
                format!("{} › {path}", crate::views::history::host_label(app, t.host))
            })
        })
        .unwrap_or_else(|| {
            if target.is_some() {
                "대상을 불러오는 중…".into()
            } else {
                "세션을 고르면 여기에 보여요".into()
            }
        });
    egui::Frame::new().inner_margin(egui::Margin::symmetric(12, 8)).show(ui, |ui| {
        ui.horizontal(|ui| {
            let mut pinned = app.ui.panel().is_some_and(|p| p.pinned);
            let label_width = (ui.available_width() - 58.0).max(40.0);
            ui.allocate_ui(Vec2::new(label_width, 20.0), |ui| {
                crate::theme::truncated(ui, &label, font(11.5), c::TEXT_2).on_hover_text(&label);
            });
            if ui
                .add_enabled(target.is_some(), egui::Button::selectable(pinned, "고정"))
                .on_hover_text("이 대상을 유지해요. 고정을 풀면 선택한 세션을 따라가요")
                .clicked()
            {
                pinned = !pinned;
                app.ui.pin_panel(pinned);
            }
        });
    });
}

fn navigation(app: &mut App, ui: &mut egui::Ui) {
    let detail = app.ui.panel().and_then(|p| p.current()).map(|f| f.detail.clone());
    let Some(detail) = detail else { return };
    egui::Frame::new().inner_margin(egui::Margin::symmetric(12, 6)).show(ui, |ui| {
        ui.horizontal(|ui| {
            if crate::theme::secondary_button(ui, "← 뒤로").clicked() {
                app.ui.back_panel();
            }
            let kind = match &detail {
                PanelDetail::Pin { host, id } => Some(TabKind::Pin { host: *host, id: *id }),
                PanelDetail::Message(o) => Some(TabKind::Record { host: o.host, anchor: o.anchor }),
                PanelDetail::Result { host, request } => Some(TabKind::Result { host: *host, request: *request }),
                _ => None,
            };
            if let Some(kind) = kind
                && crate::theme::secondary_button(ui, "크게 보기").clicked()
            {
                super::actions::open_center_tab(app, kind);
            }
        });
    });
}

fn panel_head(app: &mut App, ui: &mut egui::Ui, panel: &RightPanel) {
    let r = ui.max_rect();
    ui.painter().rect_filled(r, CornerRadius::ZERO, c::PANEL);
    ui.painter().hline(r.x_range(), r.bottom() - 0.5, Stroke::new(1.0, c::LINE));
    let mut x = r.left() + 8.0;
    let cy = r.center().y;
    let mut switch = None;
    for (p, title) in RightPanel::ALL {
        let on = p == *panel;
        let f = if on { medium(12.5) } else { font(12.5) };
        let w = widgets::text_width(ui, title, f.clone()) + 20.0;
        let ir = Rect::from_min_size(Pos2::new(x, cy - 14.0), Vec2::new(w, 28.0));
        let resp = ui.interact(ir, ui.scope_id().with(("panel-item", title)), Sense::click());
        resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::Tab, true, on, title));
        if on {
            ui.painter().rect_filled(ir, CornerRadius::same(6), c::CONTROL);
        } else if resp.hovered() {
            ui.painter().rect_filled(ir, CornerRadius::same(6), c::ROW);
        }
        ui.painter().text(
            ir.center(),
            egui::Align2::CENTER_CENTER,
            title,
            f,
            if on { c::TEXT_STRONG } else { c::TEXT_2 },
        );
        crate::theme::focus_ring(ui, &resp, CornerRadius::same(6));
        if resp.clicked() && !on {
            switch = Some(p);
        }
        x = ir.right() + 2.0;
    }
    let close_r = Rect::from_min_size(Pos2::new(r.right() - 6.0 - 28.0, cy - 14.0), Vec2::splat(28.0));
    let closed = ui
        .scope_builder(UiBuilder::new().max_rect(close_r), |ui| {
            let resp = widgets::icon_btn(ui, icons::X, "패널 닫기 (Esc)", 28.0, 28.0, 13.0);
            resp.widget_info(|| {
                egui::WidgetInfo::labeled(egui::Role::Button, true, "패널 닫기, 실행과 검색 상태는 유지됩니다")
            });
            resp.clicked()
        })
        .inner;
    if let Some(p) = switch {
        app.ui.show_panel(p, None);
    }
    if closed {
        close(app, ui.ctx());
    }
}
