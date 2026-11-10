//! One explicit target picker shared by split controls and pane headers.

use super::{data, modal, tabs, widgets};
use crate::App;
use crate::icons;
use crate::state::{Pane, TabKind, WsKey};
use crate::theme::{self, c, font, icon_font};
use egui::{Align2, CornerRadius, Pos2, Rect, Sense, Vec2};
use runwick_core::api::sessions::Tool;

pub(super) struct Picker {
    ws: WsKey,
    pane: Pane,
    mode: Mode,
    return_focus: Option<egui::Id>,
}

enum Mode {
    Targets,
    File { focus: bool, search: Box<crate::views::files::FilePicker> },
    Cli(Tool),
}

pub(super) fn open(app: &mut App, ws: WsKey, pane: Pane) {
    let return_focus = app.shell.center.ctx().and_then(|ctx| ctx.memory(|m| m.focused()));
    app.shell.center.picker = Some(Picker { ws, pane, mode: Mode::Targets, return_focus });
}

pub(super) fn show(app: &mut App, ctx: &egui::Context) {
    let Some(mut picker) = app.shell.center.picker.take() else { return };
    let ws = picker.ws;
    let pane = picker.pane;
    if app.ui.selected_ws != Some(ws) {
        return;
    }
    if let Mode::Cli(tool) = picker.mode {
        if modal::show_session_chooser(app, ctx, ws, Some((pane, tool))) {
            app.shell.center.picker = Some(picker);
        } else if let Some(id) = picker.return_focus {
            ctx.memory_mut(|m| m.request_focus(id));
        }
        return;
    }
    let set = app.ui.tabset(&ws).cloned().unwrap_or_default();
    let sessions = tabs::session_map(app, ws);
    let lines = data::tool_lines(app, ws.host, true);
    let mut chosen = None;
    let mut file = None;
    let mut next_mode = None;
    let mut cancel = false;
    let ((), close) = modal::dialog(ctx, "split-target", 380.0, |ui| {
        modal::dialog_title(
            ui,
            "옆에 열 것 고르기",
            Some(if pane == Pane::Left { "왼쪽에 열어요" } else { "오른쪽에 열어요" }),
        );
        if let Mode::File { focus, search } = &mut picker.mode {
            if widgets::menu_item(ui, icons::ARROW_LEFT, "목록으로 돌아가기").clicked() {
                next_mode = Some(Mode::Targets);
            }
            file = crate::views::files::pick_file(app, ui, ws, search, std::mem::take(focus));
        } else {
            ui.label(egui::RichText::new("새 CLI").font(font(12.0)).color(c::TEXT_3));
            for (tool, line, _) in &lines {
                let name = if *tool == Tool::Shell { "셸" } else { tool.label() };
                let enabled = line.startable && app.shell.center.starting.is_none();
                let response = ui
                    .add_enabled_ui(enabled, |ui| target_row(ui, &tabs::Mark::Text(data::tool_mark(*tool)), name))
                    .inner
                    .on_hover_text(&line.text);
                if response.clicked() {
                    next_mode = Some(Mode::Cli(*tool));
                }
            }
            ui.separator();
            ui.label(egui::RichText::new("열린 탭").font(font(12.0)).color(c::TEXT_3));
            egui::ScrollArea::vertical().max_height((ctx.content_rect().height() - 350.0).clamp(80.0, 240.0)).show(
                ui,
                |ui| {
                    let current = set.pane_tab(pane).or(set.active);
                    let mut count = 0;
                    for tab in set.tabs.iter().filter(|tab| Some(tab.id) != current) {
                        count += 1;
                        let look = tabs::look(app, tab, &sessions);
                        if target_row(ui, &look.mark, &look.title).on_hover_text(&look.tip).clicked() {
                            chosen = Some(tab.id);
                        }
                    }
                    if count == 0 {
                        widgets::empty_note(ui, "다른 열린 탭이 없어요");
                    }
                },
            );
            ui.separator();
            if widgets::menu_item(ui, icons::FOLDER_OPEN, "파일 열기…").clicked() {
                next_mode = Some(Mode::File { focus: true, search: Box::default() });
            }
        }
        if theme::secondary_button(ui, "취소").clicked() {
            cancel = true;
        }
    });
    if let Some(id) = chosen {
        app.ui.tabset_mut(&ws).show_in_pane(id, pane);
    } else if let Some(path) = file {
        crate::views::docs::focus_editor(app, ws, &path);
        app.ui.open_tab_in_pane(ws, TabKind::Doc { ws, path }, pane);
    } else if close || cancel {
        if let Some(id) = picker.return_focus {
            ctx.memory_mut(|m| m.request_focus(id));
        }
    } else {
        if let Some(mode) = next_mode {
            picker.mode = mode;
        }
        app.shell.center.picker = Some(picker);
    }
}

pub(super) fn target_row(ui: &mut egui::Ui, mark: &tabs::Mark, title: &str) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 30.0), Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, ui.is_enabled(), title));
    if response.hovered() || response.has_focus() {
        ui.painter().rect_filled(rect, CornerRadius::same(5), c::CONTROL);
    }
    let badge = Rect::from_center_size(Pos2::new(rect.left() + 16.0, rect.center().y), Vec2::new(20.0, 18.0));
    paint_mark(ui, badge, mark);
    widgets::paint_clipped(
        ui,
        Rect::from_min_max(Pos2::new(badge.right() + 8.0, rect.top()), rect.max),
        title,
        font(12.5),
        c::TEXT_STRONG,
        true,
    );
    theme::focus_ring(ui, &response, CornerRadius::same(5));
    crate::terminal::verify::target(title, rect);
    response
}

pub(super) fn paint_mark(ui: &egui::Ui, badge: Rect, mark: &tabs::Mark) {
    match mark {
        tabs::Mark::Text(mark) => theme::paint_tool_badge(ui.painter(), badge, mark, c::CONTROL),
        tabs::Mark::Icon(icon) => {
            ui.painter().text(badge.center(), Align2::CENTER_CENTER, *icon, icon_font(12.0), c::TEXT_2);
        }
    }
}
