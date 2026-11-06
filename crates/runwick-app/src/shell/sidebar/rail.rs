//! Collapsed navigation (48): expand button, the selected workspace's sessions as marks with
//! state dots, and the project tools. The expand button opens the full sidebar as a flyout.

use crate::App;
use crate::icons;
use crate::shell::{actions, data, widgets};
use crate::state::{RightPanel, TabKind};
use crate::theme::{c, icon_font, mono, size};
use egui::{Align2, CornerRadius, Pos2, Rect, Sense, Stroke, Vec2};

pub fn show(app: &mut App, ui: &mut egui::Ui, narrow: bool) {
    egui::Panel::left("sidebar-rail")
        .exact_size(size::SIDEBAR_COLLAPSED)
        .resizable(false)
        .show_separator_line(true)
        .frame(egui::Frame::new().fill(c::NAV))
        .show(ui, |ui| {
            let full = ui.max_rect();
            let cx = full.center().x;
            let mut y = full.top() + 8.0;
            let expand = Rect::from_center_size(Pos2::new(cx, y + 17.0), Vec2::splat(34.0));
            let tip = if narrow { "탐색 펼치기" } else { "사이드바 펼치기" };
            let open = app.shell.sidebar.flyout;
            if square(ui, expand, "rail-expand", tip, true, open, |p, r, fg| {
                p.text(r.center(), Align2::CENTER_CENTER, icons::PANEL_LEFT, icon_font(16.0), fg);
            })
            .clicked()
            {
                if narrow {
                    app.shell.sidebar.flyout = !open;
                } else {
                    app.ui.sidebar_collapsed = false;
                }
            }
            y = expand.bottom() + 8.0;
            ui.painter().hline(cx - 11.0..=cx + 11.0, y, Stroke::new(1.0, c::LINE_STRONG));
            y += 9.0;
            // Sessions of the selected workspace.
            let tools_top = full.bottom() - 8.0 - 3.0 * 38.0;
            if let (Some(ws), Some(project)) = (app.ui.selected_ws, app.ui.selected_project) {
                let list = data::sessions_in(app, ws).data;
                let active = match app.ui.active_tab().map(|t| t.kind.clone()) {
                    Some(TabKind::Terminal { session, .. }) => Some(session),
                    _ => None,
                };
                for s in list.iter().flat_map(|l| l.iter()).filter(|s| !s.session.archived) {
                    if y + 34.0 > tools_top - 4.0 {
                        break;
                    }
                    let live = data::live_of(app, ws.host, s);
                    let st = data::status(&s.session, live.as_ref());
                    let r = Rect::from_center_size(Pos2::new(cx, y + 17.0), Vec2::splat(34.0));
                    let tip = format!("{}, {}", s.session.title, st.label);
                    let mark = data::tool_mark(s.session.tool);
                    let on = active == Some(s.session.id);
                    let dot = match st.tone {
                        crate::theme::Tone::Neutral => c::TEXT_2,
                        crate::theme::Tone::Waiting => c::TEXT_OFF,
                        t => t.fg(),
                    };
                    let pulse = st.pulse;
                    let resp = square(ui, r, &format!("rail-{}", s.session.id), &tip, false, on, |p, r, _| {
                        p.text(r.center(), Align2::CENTER_CENTER, mark, mono(10.5), c::TEXT_STRONG);
                    });
                    widgets::dot(ui, Pos2::new(r.right() - 7.5, r.top() + 7.5), dot, pulse, 3.5);
                    if resp.clicked() {
                        actions::open_session(app, project, ws, s.session.id, s.session.hidden);
                    }
                    y += 38.0;
                }
            }
            // Tools.
            let tools = [(icons::HISTORY, "기록"), (icons::BOOK_OPEN, "위키"), (icons::CHART_GANTT, "타임라인")];
            for (i, (glyph, name)) in tools.into_iter().enumerate() {
                let r = Rect::from_center_size(Pos2::new(cx, tools_top + 17.0 + i as f32 * 38.0), Vec2::splat(34.0));
                if square(ui, r, &format!("rail-tool-{i}"), name, false, false, |p, r, fg| {
                    p.text(r.center(), Align2::CENTER_CENTER, glyph, icon_font(15.0), fg);
                })
                .clicked()
                {
                    match i {
                        0 => actions::toggle_right(app, RightPanel::History),
                        1 => {
                            if let Some(project) = app.ui.selected_project {
                                actions::open_center_tab(app, TabKind::Wiki { project, page: None });
                            }
                        }
                        _ => actions::open_center_tab(app, TabKind::Timeline { project: app.ui.selected_project }),
                    }
                }
            }
        });
}

/// 34×34 rail button.
fn square(
    ui: &mut egui::Ui,
    r: Rect,
    id: &str,
    tip: &str,
    filled: bool,
    on: bool,
    paint: impl FnOnce(&egui::Painter, Rect, egui::Color32),
) -> egui::Response {
    let resp = ui.interact(r, ui.scope_id().with(id), Sense::click());
    let t = tip.to_string();
    resp.widget_info(move || egui::WidgetInfo::selected(egui::Role::Button, true, on, &t));
    let bg = if on {
        Some(c::CONTROL_HOVER)
    } else if filled || resp.hovered() {
        Some(c::CONTROL)
    } else {
        None
    };
    if let Some(bg) = bg {
        ui.painter().rect_filled(r, CornerRadius::same(size::RADIUS), bg);
    }
    let fg = if on || resp.hovered() || filled { c::TEXT_STRONG } else { c::TEXT_2 };
    paint(ui.painter(), r, fg);
    crate::theme::focus_ring(ui, &resp, CornerRadius::same(size::RADIUS));
    resp.on_hover_text(tip)
}

/// The full sidebar over the content (narrow windows). Clicking outside closes it.
pub fn flyout(app: &mut App, ctx: &egui::Context) {
    if !app.shell.sidebar.flyout {
        return;
    }
    let screen = ctx.content_rect();
    let width = app.ui.sidebar_width.clamp(size::SIDEBAR_MIN, size::SIDEBAR_MAX).min(screen.width() - 60.0);
    let rect = Rect::from_min_max(
        Pos2::new(screen.left() + size::SIDEBAR_COLLAPSED, screen.top() + size::TITLEBAR),
        Pos2::new(screen.left() + size::SIDEBAR_COLLAPSED + width, screen.bottom()),
    );
    let area = egui::Area::new(egui::Id::unique("sidebar-flyout"))
        .order(egui::Order::Foreground)
        .fixed_pos(rect.min)
        .show(ctx, |ui| {
            ui.set_min_size(rect.size());
            ui.set_max_size(rect.size());
            ui.painter().with_clip_rect(screen).add(
                egui::epaint::Shadow {
                    offset: [16, 0],
                    blur: 40,
                    spread: 0,
                    color: egui::Color32::from_black_alpha(110),
                }
                .as_shape(rect, CornerRadius::ZERO),
            );
            ui.painter().rect_filled(rect, CornerRadius::ZERO, c::NAV);
            ui.painter().vline(rect.right() - 0.5, rect.y_range(), Stroke::new(1.0, c::LINE));
            ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| super::contents(app, ui));
        });
    let clicked_outside = ctx.input(|i| i.pointer.any_pressed())
        && ctx
            .input(|i| i.pointer.interact_pos())
            .is_some_and(|p| !area.response.rect.contains(p) && p.x > size::SIDEBAR_COLLAPSED)
        && !egui::Popup::is_any_open(ctx);
    if clicked_outside {
        app.shell.sidebar.flyout = false;
    }
}
