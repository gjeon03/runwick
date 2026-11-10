//! Tab bar of the selected workspace (38): tool mark, title, state dot, close. Closing a
//! terminal tab only hides it; the run continues. `+` opens the CLI chooser, the split button
//! shows two tabs side by side.

use super::{actions, data, split_picker, widgets};
use crate::App;
use crate::icons;
use crate::state::{Pane, Tab, TabId, TabKind, WsKey};
use crate::theme::{self, Tone, c, font, icon_font, size, ty};
use egui::{Align2, CornerRadius, Pos2, Rect, Sense, Stroke, Vec2};
use runwick_core::api::sessions::SessionInfo;
use std::collections::HashMap;

/// How a tab is labelled and what its dot says.
pub struct TabLook {
    pub title: String,
    pub mark: Mark,
    pub dot: Option<(Tone, &'static str, bool)>,
    pub tip: String,
}

pub enum Mark {
    Text(&'static str),
    Icon(&'static str),
}

#[derive(Clone, Copy)]
pub(super) struct TabDrag {
    pub ws: WsKey,
    pub id: TabId,
}

pub fn look(app: &App, tab: &Tab, sessions: &HashMap<uuid::Uuid, SessionInfo>) -> TabLook {
    match &tab.kind {
        TabKind::Terminal { host, session } => match sessions.get(session) {
            Some(s) => {
                let live = data::live_of(app, *host, s);
                let st = data::status(&s.session, live.as_ref());
                let unread = live.as_ref().is_some_and(runwick_core::api::activity::has_new_response);
                let dot = match st.tone {
                    Tone::Attention => Some((Tone::Attention, "확인 필요", false)),
                    Tone::Recover => Some((Tone::Recover, "복구 필요", false)),
                    _ if unread => Some((Tone::Info, "새 응답", false)),
                    Tone::Running => Some((Tone::Running, "실행 중", st.pulse)),
                    _ => None,
                };
                TabLook {
                    title: s.session.title.clone(),
                    mark: Mark::Text(data::tool_mark(s.session.tool)),
                    dot,
                    tip: format!(
                        "{}\n{} · {}\n탭을 닫아도 실행은 계속돼요",
                        s.session.title,
                        s.session.tool.label(),
                        st.label
                    ),
                }
            }
            None => TabLook {
                title: "세션".into(),
                mark: Mark::Icon(icons::TERMINAL),
                dot: None,
                tip: "세션 정보를 불러오는 중".into(),
            },
        },
        TabKind::Doc { ws, path } => {
            let name = std::path::Path::new(path)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| path.clone());
            let name = runwick_core::api::files::display_path(&name).to_string();
            let dot = crate::views::docs::tab_dot(app, *ws, path);
            let mark = Mark::Icon(crate::views::files::file_icon(path, false, false));
            TabLook { title: name, mark, dot, tip: path.clone() }
        }
        TabKind::Wiki { page, .. } => {
            // The open page's title (and its draft state) as the wiki view knows it.
            let (t, dot) = crate::views::wiki::tab_look(app, tab.id)
                .unwrap_or_else(|| (page.as_ref().map_or_else(|| "위키".into(), |p| format!("위키: {p}")), None));
            TabLook { tip: t.clone(), title: t, mark: Mark::Icon(icons::BOOK_OPEN), dot }
        }
        TabKind::WikiGraph { .. } => simple("위키 관계 그래프", icons::WAYPOINTS),
        TabKind::Timeline { .. } => simple("타임라인", icons::CHART_GANTT),
        TabKind::History => simple(&crate::views::history::tab_title(app), icons::HISTORY),
        TabKind::Result { .. } => simple("결과", icons::LIST_CHECKS),
        TabKind::Merge { .. } => simple("통합", icons::GIT_MERGE),
        TabKind::Start => simple("새 세션", icons::PLUS),
    }
}

fn simple(title: &str, glyph: &'static str) -> TabLook {
    TabLook { title: title.into(), mark: Mark::Icon(glyph), dot: None, tip: title.into() }
}

/// Sessions of a workspace by id (for tab titles and states).
pub fn session_map(app: &mut App, ws: WsKey) -> HashMap<uuid::Uuid, SessionInfo> {
    data::sessions_in(app, ws).data.map(|l| l.iter().map(|s| (s.session.id, s.clone())).collect()).unwrap_or_default()
}

pub fn show(app: &mut App, ui: &mut egui::Ui, ws: WsKey) {
    let bar = ui.max_rect();
    ui.painter().rect_filled(bar, CornerRadius::ZERO, c::NAV);
    ui.painter().hline(bar.x_range(), bar.bottom() - 0.5, Stroke::new(1.0, c::LINE));
    let set = app.ui.tabset(&ws).cloned().unwrap_or_default();
    let split_on = set.split.is_some();
    let split_label = if split_on { "분할 닫기" } else { "나란히 보기" };
    let split_w = widgets::text_width(ui, split_label, font(ty::SMALL)) + 37.0;
    let tools_w = 6.0 + 26.0 + 4.0 + split_w + 6.0;
    let tabs_rect = Rect::from_min_max(bar.min, Pos2::new(bar.right() - tools_w, bar.bottom() - 1.0));
    let tools_rect =
        Rect::from_min_max(Pos2::new(tabs_rect.right(), bar.top()), Pos2::new(bar.right(), bar.bottom() - 1.0));
    ui.painter().vline(tools_rect.left() + 0.5, tools_rect.y_range(), Stroke::new(1.0, c::LINE));
    let sessions = session_map(app, ws);
    let mut close = None;
    let mut activate = None;
    let mut split_with = None;
    let mut rename = None;
    let mut reorder = None;
    if !set.tabs.is_empty() {
        let n = set.tabs.len() as f32;
        let tab_w = (tabs_rect.width() / n).clamp(90.0, 220.0);
        let content_w = tab_w * n;
        ui.scope_builder(egui::UiBuilder::new().max_rect(tabs_rect), |ui| {
            egui::ScrollArea::horizontal()
                .id_salt(("tabs", ws.id))
                .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    let (strip, _) = ui.allocate_exact_size(Vec2::new(content_w, tabs_rect.height()), Sense::hover());
                    for (i, tab) in set.tabs.iter().enumerate() {
                        let r = Rect::from_min_size(
                            Pos2::new(strip.left() + i as f32 * tab_w, strip.top()),
                            Vec2::new(tab_w, strip.height()),
                        );
                        let active = set.focused_tab() == Some(tab.id);
                        let in_split = set.active == Some(tab.id) || set.split == Some(tab.id);
                        let lk = look(app, tab, &sessions);
                        let (hit, resp) = tab_ui(ui, r, tab, &lk, active, in_split, i);
                        resp.dnd_set_drag_payload(TabDrag { ws, id: tab.id });
                        match hit {
                            TabHit::Activate => activate = Some(tab.id),
                            TabHit::Close => close = Some(tab.id),
                            TabHit::None => {}
                        }
                        if active && app.shell.center.revealed_tab != Some(tab.id) {
                            // Keep the active tab in view when it changes (shortcuts, sidebar).
                            ui.scroll_to_rect(r, None);
                            app.shell.center.revealed_tab = Some(tab.id);
                        }
                        resp.context_menu(|ui| {
                            if set.active != Some(tab.id)
                                && widgets::menu_item(ui, icons::COLUMNS_2, "나란히 보기").clicked()
                            {
                                split_with = Some(tab.id);
                            }
                            if ui
                                .add_enabled_ui(i > 0, |ui| widgets::menu_item(ui, icons::ARROW_LEFT, "왼쪽으로 이동"))
                                .inner
                                .clicked()
                            {
                                reorder = Some((tab.id, i - 1));
                            }
                            if ui
                                .add_enabled_ui(i + 1 < set.tabs.len(), |ui| {
                                    widgets::menu_item(ui, icons::ARROW_RIGHT, "오른쪽으로 이동")
                                })
                                .inner
                                .clicked()
                            {
                                reorder = Some((tab.id, i + 2));
                            }
                            if let TabKind::Terminal { host, session } = &tab.kind
                                && widgets::menu_item(ui, icons::PENCIL, "이름 바꾸기").clicked()
                            {
                                rename = Some((*host, *session));
                            }
                            let close_label = if matches!(tab.kind, TabKind::Terminal { .. }) {
                                "탭 닫기 (실행 유지)"
                            } else {
                                "탭 닫기"
                            };
                            if widgets::menu_item(ui, icons::X, close_label).clicked() {
                                close = Some(tab.id);
                            }
                        });
                    }
                    if let Some(drag) = egui::DragAndDrop::payload::<TabDrag>(ui.ctx()).filter(|d| d.ws == ws)
                        && let Some(pos) = ui.input(|i| i.pointer.interact_pos()).filter(|p| tabs_rect.contains(*p))
                    {
                        let gap = insertion_gap(pos.x, strip.left(), tab_w, set.tabs.len());
                        let x =
                            (strip.left() + gap as f32 * tab_w).clamp(tabs_rect.left() + 1.0, tabs_rect.right() - 1.0);
                        ui.painter().vline(
                            x,
                            tabs_rect.shrink2(Vec2::new(0.0, 4.0)).y_range(),
                            Stroke::new(3.0, c::EMBER),
                        );
                        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
                        // Scroll overflowing tabs while dragging near either visible edge.
                        let dx = if pos.x < tabs_rect.left() + 24.0 {
                            10.0
                        } else if pos.x > tabs_rect.right() - 24.0 {
                            -10.0
                        } else {
                            0.0
                        };
                        if dx != 0.0 {
                            ui.scroll_with_delta(Vec2::new(dx, 0.0));
                            ui.ctx().request_repaint();
                        }
                        if ui.input(|i| i.pointer.any_released()) {
                            reorder = Some((drag.id, gap));
                            egui::DragAndDrop::clear_payload(ui.ctx());
                        }
                    }
                });
        });
    }
    // Tools: new session, side by side.
    let plus =
        Rect::from_center_size(Pos2::new(tools_rect.left() + 6.0 + 13.0, tools_rect.center().y), Vec2::splat(26.0));
    let split_r = Rect::from_min_size(Pos2::new(plus.right() + 4.0, plus.top()), Vec2::new(split_w, 26.0));
    ui.scope_builder(egui::UiBuilder::new().max_rect(plus), |ui| {
        if widgets::icon_btn(ui, icons::PLUS, "새 CLI 세션 (⌘T)", 26.0, 26.0, 14.0).clicked() {
            actions::new_session(app);
        }
    });
    ui.scope_builder(egui::UiBuilder::new().max_rect(split_r), |ui| {
        let tip = if split_on {
            "분할을 닫아도 두 탭은 유지돼요"
        } else {
            "옆에 열 CLI, 탭, 파일을 골라요"
        };
        let resp = widgets::pill_button(
            ui,
            Some(icons::COLUMNS_2),
            split_label,
            26.0,
            if split_on { c::CONTROL } else { c::NAV },
        )
        .on_hover_text(tip);
        if split_on {
            ui.painter().rect_stroke(
                split_r,
                CornerRadius::same(6),
                Stroke::new(1.0, c::LINE_STRONG),
                egui::StrokeKind::Inside,
            );
        }
        if resp.clicked() {
            if split_on {
                app.ui.tabset_mut(&ws).close_pane(Pane::Right);
            } else {
                split_picker::open(app, ws, Pane::Right);
            }
        }
    });
    if let Some(id) = activate {
        app.ui.tabset_mut(&ws).select_tab(id);
    }
    if let Some(id) = split_with {
        app.ui.tabset_mut(&ws).show_in_pane(id, Pane::Right);
    }
    if let Some((id, gap)) = reorder {
        app.ui.tabset_mut(&ws).reorder(id, gap);
        app.shell.center.revealed_tab = None;
    }
    if let Some((host, id)) = rename {
        app.ui.modal = Some(crate::state::Modal::Rename { target: crate::state::RenameTarget::Session { host, id } });
    }
    if let Some(id) = close {
        actions::close_tab(app, ws, id);
    }
}

fn insertion_gap(x: f32, left: f32, tab_width: f32, count: usize) -> usize {
    (((x - left) / tab_width + 0.5).floor().max(0.0) as usize).min(count)
}

enum TabHit {
    None,
    Activate,
    Close,
}

fn tab_ui(
    ui: &mut egui::Ui,
    r: Rect,
    tab: &Tab,
    lk: &TabLook,
    active: bool,
    in_split: bool,
    index: usize,
) -> (TabHit, egui::Response) {
    let resp = ui.interact(r, ui.scope_id().with(("tab", tab.id)), Sense::click_and_drag());
    let title = lk.title.clone();
    resp.widget_info(move || egui::WidgetInfo::selected(egui::Role::Tab, true, active, &title));
    let painter = ui.painter_at(r);
    let bg = if active {
        Some(c::TERMINAL)
    } else if in_split {
        Some(c::WINDOW)
    } else if resp.hovered() {
        Some(c::PANEL_2)
    } else {
        None
    };
    if let Some(bg) = bg {
        painter.rect_filled(r, CornerRadius::ZERO, bg);
    }
    if active {
        // The active tab merges into the content below.
        painter.hline(r.x_range(), r.bottom() + 0.5, Stroke::new(2.0, c::TERMINAL));
    }
    painter.vline(r.right() - 0.5, r.y_range(), Stroke::new(1.0, c::LINE));
    let cy = r.center().y;
    let fg = if active || in_split { c::TEXT_STRONG } else { c::TEXT_2 };
    let badge = Rect::from_min_size(Pos2::new(r.left() + 10.0, cy - 9.0), Vec2::new(20.0, 18.0));
    match lk.mark {
        Mark::Text(m) => theme::paint_tool_badge(&painter, badge, m, c::CONTROL),
        Mark::Icon(g) => {
            painter.rect_filled(badge, CornerRadius::same(4), c::CONTROL);
            painter.text(badge.center(), Align2::CENTER_CENTER, g, icon_font(11.0), c::TEXT);
        }
    }
    let show_close = active || resp.hovered();
    let close_r = Rect::from_center_size(Pos2::new(r.right() - 16.0, cy), Vec2::splat(18.0));
    let mut right = r.right() - 10.0;
    if show_close {
        right = close_r.left() - 4.0;
    }
    if let Some((tone, _, pulse)) = lk.dot {
        widgets::dot(ui, Pos2::new(right - 3.5, cy), tone.fg(), pulse, 3.5);
        right -= 14.0;
    }
    let text_rect = Rect::from_min_max(Pos2::new(badge.right() + 7.0, r.top()), Pos2::new(right, r.bottom()));
    widgets::paint_clipped(ui, text_rect, &lk.title, font(12.5), fg, true);
    let mut hit = TabHit::None;
    if show_close {
        let cr = ui.interact(close_r, ui.scope_id().with(("tabclose", tab.id)), Sense::click());
        let is_term = matches!(tab.kind, TabKind::Terminal { .. });
        let tip = if is_term { "탭 닫기 (⌘W) — 실행은 계속돼요" } else { "탭 닫기 (⌘W)" };
        cr.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, tip));
        if cr.hovered() {
            painter.rect_filled(close_r, CornerRadius::same(4), c::CONTROL_HOVER);
        }
        painter.text(
            close_r.center(),
            Align2::CENTER_CENTER,
            icons::X,
            icon_font(12.0),
            if cr.hovered() { c::TEXT_STRONG } else { c::TEXT_3 },
        );
        if cr.on_hover_text(tip).clicked() {
            hit = TabHit::Close;
        }
    }
    theme::focus_ring(ui, &resp, CornerRadius::same(size::RADIUS_SM));
    let mut tip = lk.tip.clone();
    if index < 8 {
        tip.push_str(&format!("\n⌘{}", index + 1));
    }
    if let Some((_, name, _)) = lk.dot {
        tip = format!("{tip}\n상태: {name}");
    }
    let resp = resp.on_hover_text(tip);
    if matches!(hit, TabHit::None) {
        if resp.middle_clicked() {
            hit = TabHit::Close;
        } else if resp.clicked() {
            hit = TabHit::Activate;
        }
    }
    (hit, resp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insertion_marker_tracks_tab_midpoints_and_scrolled_strip() {
        assert_eq!(insertion_gap(20.0, 10.0, 100.0, 3), 0);
        assert_eq!(insertion_gap(59.0, 10.0, 100.0, 3), 0);
        assert_eq!(insertion_gap(60.0, 10.0, 100.0, 3), 1);
        assert_eq!(insertion_gap(310.0, 10.0, 100.0, 3), 3);
        assert_eq!(insertion_gap(40.0, -110.0, 100.0, 3), 2);
        assert_eq!(insertion_gap(-20.0, 10.0, 100.0, 3), 0);
        assert_eq!(insertion_gap(900.0, 10.0, 100.0, 3), 3);
    }
}
