//! Custom title bar (44): breadcrumb, command search, activity, settings. The empty area drags
//! the window; a double-click zooms it. Room is left for the macOS traffic lights.

use super::{actions, data, widgets};
use crate::App;
use crate::client::{ConnState, HostKey};
use crate::icons;
use crate::state::{Page, RightPanel};
use crate::theme::{c, font, icon_font, medium, mono, size};
use egui::{Align2, CornerRadius, Pos2, Rect, Sense, Stroke, Vec2};

/// Left edge of the breadcrumb: 14 (inset) + traffic lights (52) + 12.
const LIGHTS_END: f32 = 78.0;

pub fn show(app: &mut App, ui: &mut egui::Ui, page: Page) {
    let welcome = page == Page::Welcome;
    let fill = if welcome { c::WINDOW } else { c::NAV };
    egui::Panel::top("titlebar")
        .exact_size(size::TITLEBAR)
        .resizable(false)
        .show_separator_line(false)
        .frame(egui::Frame::new().fill(fill))
        .show(ui, |ui| {
            let bar = ui.max_rect();
            drag_area(ui, bar);
            if !welcome {
                ui.painter().hline(bar.x_range(), bar.bottom() - 0.5, Stroke::new(1.0, c::LINE));
            }
            let right_x = right_side(app, ui, bar, welcome);
            if welcome {
                return;
            }
            let search = search_box(app, ui, bar, right_x);
            breadcrumb(
                app,
                ui,
                Rect::from_min_max(
                    Pos2::new(bar.left() + LIGHTS_END, bar.top()),
                    Pos2::new(search.left() - 12.0, bar.bottom()),
                ),
            );
        });
}

fn drag_area(ui: &mut egui::Ui, bar: Rect) {
    let resp = ui.interact(bar, ui.scope_id().with("titlebar-drag"), Sense::click_and_drag());
    if resp.double_clicked() {
        let maximized = ui.input(|i| i.viewport().maximized.unwrap_or(false));
        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
    } else if resp.drag_started_by(egui::PointerButton::Primary) {
        ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
    }
}

fn breadcrumb(app: &mut App, ui: &mut egui::Ui, area: Rect) {
    if area.width() < 40.0 {
        return;
    }
    let Some(ws) = app.ui.selected_ws else { return };
    let (Some(projects), Some(workspaces)) = (data::projects(app).data, data::workspaces(app).data) else { return };
    let Some(w) = data::find_ws(&workspaces, &ws) else { return };
    let Some(p) = data::project_of(&projects, w.project_id) else { return };
    let device_name = w.remote.as_ref().map(|r| crate::remote::device_name(app, r.device_id));
    let device = device_name.as_deref();
    let branch = w.branch.clone();
    let full = format!(
        "{} / {}{}{}",
        p.name,
        w.name,
        device.map(|d| format!(" · {d}")).unwrap_or_default(),
        branch.as_ref().map(|b| format!(" ({b})")).unwrap_or_default()
    );
    let painter = ui.painter_at(area);
    let cy = area.center().y;
    let sep_w = widgets::text_width(ui, "/", font(12.5)) + 12.0;
    let branch_w = branch.as_ref().map(|b| widgets::text_width(ui, b, mono(11.5)) + 6.0).unwrap_or(0.0);
    let dev_w = device.map(|d| widgets::text_width(ui, d, font(11.5)) + 30.0).unwrap_or(0.0);
    let p_w = widgets::text_width(ui, &p.name, medium(12.5));
    let w_w = widgets::text_width(ui, &w.name, font(12.5));
    // Shrink the project and workspace names first; the branch stays readable.
    let fixed = sep_w + branch_w + dev_w;
    let room = (area.width() - fixed).max(0.0);
    let (pw, ww) = if p_w + w_w <= room {
        (p_w, w_w)
    } else {
        let half = room / 2.0;
        if p_w < half {
            (p_w, room - p_w)
        } else if w_w < half {
            (room - w_w, w_w)
        } else {
            (half, half)
        }
    };
    let mut x = area.left();
    widgets::paint_clipped(
        ui,
        Rect::from_min_size(Pos2::new(x, area.top()), Vec2::new(pw, area.height())),
        &p.name,
        medium(12.5),
        c::TEXT_STRONG,
        true,
    );
    x += pw + 6.0;
    painter.text(Pos2::new(x, cy), Align2::LEFT_CENTER, "/", font(12.5), c::TEXT_OFF);
    x += sep_w - 6.0;
    widgets::paint_clipped(
        ui,
        Rect::from_min_size(Pos2::new(x, area.top()), Vec2::new(ww, area.height())),
        &w.name,
        font(12.5),
        c::TEXT_2,
        true,
    );
    x += ww + 6.0;
    if let Some(d) = device {
        let bw = widgets::device_badge(ui, Pos2::new(x, cy), d, false);
        let badge = Rect::from_min_size(Pos2::new(x, cy - 8.5), Vec2::new(bw, 17.0));
        let b = ui.interact(badge, ui.scope_id().with("crumb-device"), Sense::click());
        if b.on_hover_text(format!("{d} · 이 공간 정보")).clicked() {
            crate::views::devices::open_space_panel(app, ws);
        }
        x += bw + 6.0;
    }
    if let Some(b) = &branch {
        painter.text(Pos2::new(x, cy), Align2::LEFT_CENTER, b, mono(11.5), c::TEXT_3);
    }
    let resp = ui.interact(area, ui.scope_id().with("crumb"), Sense::hover());
    resp.on_hover_text(full);
}

fn search_box(app: &mut App, ui: &mut egui::Ui, bar: Rect, right_x: f32) -> Rect {
    let avail = right_x - (bar.left() + LIGHTS_END) - 24.0;
    let w = avail.clamp(120.0, 340.0);
    let mut cx = bar.center().x;
    // Keep it centered unless that would collide with the right side.
    if cx + w / 2.0 > right_x - 12.0 {
        cx = right_x - 12.0 - w / 2.0;
    }
    let rect = Rect::from_center_size(Pos2::new(cx, bar.center().y), Vec2::new(w, 28.0));
    let resp = ui.interact(rect, ui.scope_id().with("cmd-search"), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::SearchInput, true, "명령 검색"));
    let bg = if resp.hovered() { c::CONTROL } else { c::ROW };
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(size::RADIUS), bg);
    painter.text(
        Pos2::new(rect.left() + 16.0, rect.center().y),
        Align2::CENTER_CENTER,
        icons::SEARCH,
        icon_font(13.0),
        c::TEXT_3,
    );
    let hint_w = widgets::text_width(ui, "⌘K", font(11.5));
    let text_rect = Rect::from_min_max(
        Pos2::new(rect.left() + 31.0, rect.top()),
        Pos2::new(rect.right() - hint_w - 18.0, rect.bottom()),
    );
    widgets::paint_clipped(ui, text_rect, "프로젝트, 세션, 명령 검색", font(12.5), c::TEXT_3, true);
    painter.text(Pos2::new(rect.right() - 10.0, rect.center().y), Align2::RIGHT_CENTER, "⌘K", font(11.5), c::TEXT_3);
    crate::theme::focus_ring(ui, &resp, CornerRadius::same(size::RADIUS));
    if resp.clicked() {
        super::palette::open(app, ui.ctx());
    }
    rect
}

/// Activity pill, connection state and settings, right-aligned. Returns the left edge used.
fn right_side(app: &mut App, ui: &mut egui::Ui, bar: Rect, welcome: bool) -> f32 {
    let cy = bar.center().y;
    let mut x = bar.right() - 10.0;
    if !welcome {
        // Settings.
        let r = Rect::from_min_size(Pos2::new(x - 28.0, cy - 14.0), Vec2::splat(28.0));
        let in_settings = app.ui.page == Page::Settings;
        let resp = ui.interact(r, ui.scope_id().with("settings"), Sense::click());
        resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, in_settings, "설정"));
        let hovered = resp.hovered() || resp.has_focus();
        if hovered || in_settings {
            ui.painter().rect_filled(
                r,
                CornerRadius::same(size::RADIUS),
                if in_settings { c::CONTROL } else { c::ROW },
            );
        }
        let fg = if hovered || in_settings { c::TEXT_STRONG } else { c::TEXT_2 };
        ui.painter().text(r.center(), Align2::CENTER_CENTER, icons::SLIDERS_HORIZONTAL, icon_font(15.0), fg);
        crate::theme::focus_ring(ui, &resp, CornerRadius::same(size::RADIUS));
        if resp.on_hover_text("설정 (⌘,)").clicked() {
            if in_settings {
                app.ui.page = Page::Work;
            } else {
                actions::open_settings(app, None);
            }
        }
        x = r.left() - 4.0;
        x = activity_pill(app, ui, x, cy);
    }
    connection(app, ui, x, cy)
}

fn activity_pill(app: &mut App, ui: &mut egui::Ui, right: f32, cy: f32) -> f32 {
    let counts = data::activity_counts(app);
    let (needs, running) = counts.unwrap_or((0, 0));
    let needs_txt = format!("확인 {needs}");
    let run_txt = match counts {
        Some(_) => format!("실행 {running}"),
        None => "실행 —".to_string(),
    };
    let needs_w = if needs > 0 { widgets::text_width(ui, &needs_txt, medium(12.0)) + 18.0 + 10.0 } else { 0.0 };
    let run_w = widgets::text_width(ui, &run_txt, font(12.0)) + 13.0;
    let w = needs_w + run_w + 20.0;
    let r = Rect::from_min_size(Pos2::new(right - w, cy - 14.0), Vec2::new(w, 28.0));
    let resp = ui.interact(r, ui.scope_id().with("activity"), Sense::click());
    let label = match counts {
        Some(_) => format!("전체 활동: 확인할 일 {needs}개, 실행 중 {running}개"),
        None => "전체 활동: 실행 상태를 아직 확인하지 못했어요".to_string(),
    };
    let l2 = label.clone();
    resp.widget_info(move || egui::WidgetInfo::labeled(egui::Role::Button, true, &l2));
    let open = app.ui.right == Some(RightPanel::Activity);
    let mut bg = if needs > 0 { c::EMBER_BG } else { c::ROW };
    if resp.hovered() || open {
        bg = if needs > 0 { c::EMBER_BG.gamma_multiply(1.4) } else { c::CONTROL };
    }
    ui.painter().rect_filled(r, CornerRadius::same(size::RADIUS), bg);
    let mut x = r.left() + 10.0;
    if needs > 0 {
        ui.painter().text(
            Pos2::new(x + 6.5, cy),
            Align2::CENTER_CENTER,
            icons::CIRCLE_ALERT,
            icon_font(13.0),
            c::EMBER,
        );
        ui.painter().text(Pos2::new(x + 18.0, cy), Align2::LEFT_CENTER, &needs_txt, medium(12.0), c::EMBER);
        x += needs_w;
    }
    let dot_color = if running > 0 { c::MINT } else { c::TEXT_OFF };
    // Breathe only while something is actually working: an idle live shell must not keep the
    // window repainting (battery).
    let working = running > 0 && crate::terminal::any_busy(app);
    widgets::dot(ui, Pos2::new(x + 3.5, cy), dot_color, working, 3.5);
    ui.painter().text(Pos2::new(x + 13.0, cy), Align2::LEFT_CENTER, &run_txt, font(12.0), c::TEXT_2);
    crate::theme::focus_ring(ui, &resp, CornerRadius::same(size::RADIUS));
    if resp.on_hover_text(label).clicked() {
        app.ui.page = Page::Work;
        actions::toggle_right(app, RightPanel::Activity);
    }
    r.left() - 8.0
}

/// Quiet host connection state; never blocks the window.
fn connection(app: &mut App, ui: &mut egui::Ui, right: f32, cy: f32) -> f32 {
    let st = app.hub.state(HostKey::Local);
    let reason = match &st {
        ConnState::Connected(_) => return right,
        ConnState::Connecting => None,
        ConnState::Disconnected { reason, .. } => Some(reason.clone()),
    };
    // Retries alternate between "connecting" and "disconnected"; describe the situation instead
    // of each attempt so the label does not flicker.
    let why = reason.map(|r| format!(" ({r})")).unwrap_or_default();
    let (text, tip, color, glyph) = if app.shell.ever_connected {
        (
            "연결 끊김 · 다시 연결 중",
            format!("실행 관리자와 연결이 끊겼어요{why}. 실행 중인 CLI는 계속돼요. 자동으로 다시 연결해요."),
            c::GREY,
            icons::UNPLUG,
        )
    } else {
        (
            "연결하는 중…",
            format!("이 컴퓨터의 Runwick 실행 관리자에 연결하고 있어요{why}"),
            c::TEXT_3,
            icons::LOADER_CIRCLE,
        )
    };
    let w = widgets::text_width(ui, text, font(12.0)) + 20.0;
    let r = Rect::from_min_size(Pos2::new(right - w, cy - 11.0), Vec2::new(w, 22.0));
    ui.painter().text(Pos2::new(r.left() + 6.0, cy), Align2::CENTER_CENTER, glyph, icon_font(12.0), color);
    ui.painter().text(Pos2::new(r.left() + 16.0, cy), Align2::LEFT_CENTER, text, font(12.0), color);
    ui.interact(r, ui.scope_id().with("conn"), Sense::hover()).on_hover_text(tip);
    r.left() - 8.0
}
