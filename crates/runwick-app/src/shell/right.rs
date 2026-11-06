//! Right detail panel (380, resizable): panel head with the open items and a close button,
//! then the feature's body. When the center would drop below 520 it floats over the center.
//! Closing (× or Esc) returns keyboard focus to where it was.

use super::{actions, data, widgets};
use crate::App;
use crate::icons;
use crate::state::{RightPanel, TabKind};
use crate::theme::{c, font, medium, size};
use egui::{CornerRadius, Pos2, Rect, Sense, Stroke, UiBuilder, Vec2};

#[derive(Default)]
pub struct State {
    /// Panel rect of the last frame.
    pub rect: Option<Rect>,
    /// Focus outside the panel before it opened, restored on close.
    return_focus: Option<egui::Id>,
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
    let full = ui.max_rect();
    let head = Rect::from_min_size(full.min, Vec2::new(full.width(), size::PANEL_HEAD));
    ui.scope_builder(UiBuilder::new().max_rect(head), |ui| panel_head(app, ui, panel));
    let rest = Rect::from_min_max(Pos2::new(full.left(), head.bottom()), full.max);
    let mut child = ui.new_child(
        UiBuilder::new().max_rect(rest).id_salt("right-body").layout(egui::Layout::top_down(egui::Align::Min)),
    );
    child.set_clip_rect(rest.intersect(ui.clip_rect()));
    crate::views::show_right_panel(app, &mut child, panel);
}

/// The panels a person can switch between here, in design order.
fn items(app: &mut App, panel: &RightPanel) -> Vec<(RightPanel, &'static str)> {
    let mut v = Vec::new();
    if *panel == RightPanel::Activity {
        v.push((RightPanel::Activity, "전체 활동"));
    }
    v.push((RightPanel::History, "기록·핀"));
    let collab = match panel {
        RightPanel::Collab { host, session } => Some((*host, *session)),
        _ => active_session_with_workers(app),
    };
    if let Some((host, session)) = collab {
        v.push((RightPanel::Collab { host, session }, "협업"));
    }
    if let RightPanel::Result { .. } = panel {
        v.push((panel.clone(), "변경·결과"));
    }
    if let RightPanel::Space { .. } = panel {
        v.push((panel.clone(), "이 공간"));
    }
    v
}

fn active_session_with_workers(app: &mut App) -> Option<(crate::client::HostKey, uuid::Uuid)> {
    let ws = app.ui.selected_ws?;
    let TabKind::Terminal { host, session } = app.ui.active_tab()?.kind.clone() else { return None };
    let list = data::sessions_in(app, ws).data?;
    list.iter().any(|s| s.session.parent_id == Some(session)).then_some((host, session))
}

fn panel_head(app: &mut App, ui: &mut egui::Ui, panel: &RightPanel) {
    let r = ui.max_rect();
    ui.painter().rect_filled(r, CornerRadius::ZERO, c::PANEL);
    ui.painter().hline(r.x_range(), r.bottom() - 0.5, Stroke::new(1.0, c::LINE));
    let mut x = r.left() + 8.0;
    let cy = r.center().y;
    let mut switch = None;
    for (p, title) in items(app, panel) {
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
        actions::toggle_right(app, p);
    }
    if closed {
        close(app, ui.ctx());
    }
}
