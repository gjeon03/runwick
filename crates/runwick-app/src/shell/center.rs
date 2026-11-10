//! Center: the selected workspace's tab bar, a notice after switching workspaces, and the
//! active tab (or two side by side). Terminal panes get a session bar underneath.

use super::{data, session_bar, split_picker, start, tabs, widgets};
use crate::App;
use crate::icons;
use crate::state::{Pane, Tab, TabKind, WsKey};
use crate::theme::{c, font, size, ty};
use egui::{CornerRadius, Pos2, Rect, Stroke, UiBuilder, Vec2};
use runwick_core::api::sessions::Tool;
use runwick_core::josa;
use std::collections::HashMap;
use std::time::{Duration, Instant};

const NOTICE_FOR: Duration = Duration::from_secs(15);
const PANE_MIN: f32 = 280.0;
const PANE_HEADER: f32 = 30.0;
const FRESH_FOR: Duration = Duration::from_secs(10);

pub struct State {
    /// Center rect of the last frame (focus checks, overlays).
    pub rect: Option<Rect>,
    pub(super) picker: Option<split_picker::Picker>,
    /// A CLI start is in flight.
    pub starting: Option<Tool>,
    pub revealed_tab: Option<crate::state::TabId>,
    /// Area a new terminal would get, for the initial PTY size.
    term_area: Vec2,
    ctx: Option<egui::Context>,
    focused_tab: Option<(WsKey, crate::state::TabId)>,
    focus_content: bool,
    /// Sessions whose tab was just opened; a cached list may not contain them yet.
    fresh: HashMap<uuid::Uuid, Instant>,
    notice: Option<Notice>,
}

struct Notice {
    from: WsKey,
    to: WsKey,
    at: Instant,
}

impl Default for State {
    fn default() -> Self {
        Self {
            rect: None,
            picker: None,
            starting: None,
            revealed_tab: None,
            term_area: Vec2::new(900.0, 560.0),
            ctx: None,
            focused_tab: None,
            focus_content: false,
            fresh: HashMap::new(),
            notice: None,
        }
    }
}

/// Use this frame's window size before shortcuts, so a resize cannot close a hidden pane.
pub(super) fn prepare_panes(app: &mut App, ctx: &egui::Context) {
    let Some(ws) = app.ui.selected_ws else { return };
    let win_w = ctx.content_rect().width();
    let rail = win_w <= size::NARROW_WINDOW + 0.5 || app.ui.sidebar_collapsed;
    let sidebar = if rail { size::SIDEBAR_COLLAPSED } else { app.ui.sidebar_width };
    let right = app.ui.right_width.clamp(size::RIGHT_PANEL_MIN, size::RIGHT_PANEL_MAX);
    let docked = app.ui.right.is_some() && win_w - sidebar - right >= size::CENTER_MIN;
    let width = win_w - sidebar - if docked { right } else { 0.0 };
    if let Some(set) = app.ui.tabsets.get_mut(&crate::state::ws_storage_key(&ws)) {
        set.set_right_hidden(width < PANE_MIN * 2.0);
    }
}

impl State {
    /// The egui context seen by the center last frame (for actions that run outside a `ui`).
    pub fn ctx(&self) -> Option<egui::Context> {
        self.ctx.clone()
    }

    /// Columns and rows for a new terminal in the current center.
    pub fn terminal_grid(&self) -> (u16, u16) {
        match &self.ctx {
            Some(ctx) => crate::terminal::grid_for(ctx, self.term_area),
            None => (100, 30),
        }
    }

    /// A tab for this session was just opened (keep it while the session list catches up).
    pub fn note_opened(&mut self, session: uuid::Uuid) {
        self.fresh.retain(|_, at| at.elapsed() < FRESH_FOR);
        self.fresh.insert(session, Instant::now());
    }

    /// Remember a workspace switch so the center can say where the other runs went.
    pub fn note_switch(&mut self, from: Option<WsKey>, to: WsKey) {
        self.notice = from.filter(|f| *f != to).map(|from| Notice { from, to, at: Instant::now() });
    }
}

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    egui::CentralPanel::default().frame(egui::Frame::new().fill(c::TERMINAL)).show(ui, |ui| {
        let rect = ui.max_rect();
        app.shell.center.rect = Some(rect);
        app.shell.center.ctx = Some(ui.ctx().clone());
        let Some(ws) = app.ui.selected_ws else {
            no_workspace(app, ui);
            return;
        };
        sync_tabs(app, ws);
        if let Some(set) = app.ui.tabsets.get_mut(&crate::state::ws_storage_key(&ws)) {
            set.set_right_hidden(rect.width() < PANE_MIN * 2.0);
        }
        // An empty workspace shows only the CLI chooser (S01), without a tab bar.
        let has_tabs = app.ui.tabset(&ws).is_some_and(|s| !s.tabs.is_empty());
        let tabs_h = if has_tabs { size::TABS } else { 0.0 };
        let tabs_rect = Rect::from_min_size(rect.min, Vec2::new(rect.width(), tabs_h));
        if has_tabs {
            ui.scope_builder(UiBuilder::new().max_rect(tabs_rect), |ui| tabs::show(app, ui, ws));
        }
        let mut body = Rect::from_min_max(Pos2::new(rect.left(), tabs_rect.bottom()), rect.max);
        if let Some(h) = notice(app, ui, body, ws) {
            body.min.y += h;
        }
        // A new terminal always gets the tab bar above it, even when started from the empty state.
        let tabs_missing = size::TABS - tabs_h;
        app.shell.center.term_area = Vec2::new(body.width(), body.height() - tabs_missing - size::SESSION_BAR);
        let set = app.ui.tabset(&ws).cloned().unwrap_or_default();
        let active = set.active.and_then(|id| set.tabs.iter().find(|t| t.id == id).cloned());
        let side = set.split.and_then(|id| set.tabs.iter().find(|t| t.id == id).cloned());
        let focused = set.focused_tab().map(|id| (ws, id));
        let focus_changed = focused != app.shell.center.focused_tab;
        app.shell.center.focused_tab = focused;
        app.shell.center.focus_content = focus_changed;
        if focus_changed && app.ui.modal.is_none() && !app.ui.palette_open && app.shell.center.picker.is_none() {
            // Do not keep sending keys to the previously focused terminal after tab selection.
            ui.memory_mut(|m| m.request_focus(egui::Id::unique("center-pane-focus")));
        }
        match (active, side) {
            (None, _) => {
                ui.scope_builder(UiBuilder::new().max_rect(body), |ui| start::show_start(app, ui, ws));
            }
            (Some(a), Some(b)) if body.width() >= PANE_MIN * 2.0 => split(app, ui, ws, body, a, b),
            (Some(a), _) => pane(app, ui, ws, body, a, Pane::Left),
        }
        tab_drop(app, ui, ws, body);
    });
}

fn no_workspace(app: &mut App, ui: &mut egui::Ui) {
    let loading = data::projects(app).is_first_load();
    ui.vertical_centered(|ui| {
        ui.add_space(ui.available_height() * 0.38);
        let text =
            if loading { "프로젝트를 불러오는 중…" } else { "왼쪽에서 작업 공간을 고르세요" };
        ui.label(egui::RichText::new(text).font(font(ty::BODY)).color(c::TEXT_3));
    });
}

/// First visit of a workspace: show tabs for its sessions that are not closed. Later, drop
/// terminal tabs whose session no longer exists.
fn sync_tabs(app: &mut App, ws: WsKey) {
    let Some(list) = data::sessions_in(app, ws).data else { return };
    let first_visit = app.ui.tabset(&ws).is_none();
    if first_visit {
        app.ui.tabset_mut(&ws);
        let mut open: Vec<_> =
            list.iter().filter(|s| !s.session.hidden && !s.session.archived && s.session.parent_id.is_none()).collect();
        open.sort_by_key(|s| s.session.created_at);
        for s in open {
            app.ui.open_tab(ws, TabKind::Terminal { host: ws.host, session: s.session.id });
        }
        return;
    }
    let fresh = &app.shell.center.fresh;
    let keep = |id: &uuid::Uuid| {
        fresh.get(id).is_some_and(|at| at.elapsed() < FRESH_FOR)
            || list.iter().any(|s| s.session.id == *id && !s.session.archived)
    };
    let set = app.ui.tabsets.entry(crate::state::ws_storage_key(&ws)).or_default();
    let before = set.tabs.len();
    set.tabs.retain(|t| match &t.kind {
        TabKind::Terminal { session, .. } => keep(session),
        _ => true,
    });
    if set.tabs.len() != before {
        super::layout::fix_active(set);
    }
}

fn notice(app: &mut App, ui: &mut egui::Ui, body: Rect, ws: WsKey) -> Option<f32> {
    let n = app.shell.center.notice.as_ref()?;
    if n.to != ws || n.at.elapsed() > NOTICE_FOR {
        if n.to != ws {
            app.shell.center.notice = None;
        }
        return None;
    }
    ui.ctx().request_repaint_after(NOTICE_FOR.saturating_sub(n.at.elapsed()));
    let from = n.from;
    let list = data::workspaces(app).data?;
    let (to_w, from_w) = (data::find_ws(&list, &ws)?, data::find_ws(&list, &from)?);
    if to_w.project_id != from_w.project_id {
        return None;
    }
    let h = 40.0;
    let r = Rect::from_min_size(body.min, Vec2::new(body.width(), h));
    ui.painter().rect_filled(r, CornerRadius::ZERO, c::PANEL);
    ui.painter().hline(r.x_range(), r.bottom() - 0.5, Stroke::new(1.0, c::LINE));
    let mut back = false;
    let mut merge = false;
    let mut dismiss = false;
    let back_label = format!("{} 돌아가기", josa::euro_ro(&space_name(&from_w.name)));
    ui.scope_builder(
        UiBuilder::new()
            .max_rect(r.shrink2(Vec2::new(14.0, 7.0)))
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
        |ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            let right_w = 26.0
                + 8.0
                + widgets::text_width(ui, &back_label, font(ty::SMALL))
                + 40.0
                + if to_w.kind == runwick_core::api::projects::WorkspaceKind::Worktree { 80.0 } else { 0.0 };
            let text_w = (ui.available_width() - right_w).max(80.0);
            ui.allocate_ui(Vec2::new(text_w, 26.0), |ui| {
                let mut job = egui::text::LayoutJob::default();
                let f = font(12.5);
                job.append("파일과 탭이 ", 0.0, egui::TextFormat::simple(f.clone(), c::TEXT_2));
                job.append(&to_w.name, 0.0, egui::TextFormat::simple(f.clone(), c::TEXT_STRONG));
                let to_label = space_name(&to_w.name);
                job.append(
                    &format!(
                        "{}{} 바뀌었어요. {}의 CLI와 문서는 그대로 실행·보존 중입니다.",
                        &to_label[to_w.name.len()..],
                        josa::particle(&to_label, josa::Particle::EuroRo),
                        space_name(&from_w.name)
                    ),
                    0.0,
                    egui::TextFormat::simple(f, c::TEXT_2),
                );
                job.wrap = egui::text::TextWrapping::truncate_at_width(text_w);
                ui.add(egui::Label::new(job).selectable(false));
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if widgets::icon_btn(ui, icons::X, "안내 닫기", 26.0, 26.0, 12.0).clicked() {
                    dismiss = true;
                }
                if to_w.kind == runwick_core::api::projects::WorkspaceKind::Worktree
                    && widgets::pill_button(ui, None, "통합 준비", 26.0, c::CONTROL).clicked()
                {
                    merge = true;
                }
                if widgets::pill_button(ui, Some(icons::ARROW_LEFT), &back_label, 26.0, c::CONTROL).clicked() {
                    back = true;
                }
            });
        },
    );
    let project = from_w.project_id;
    if dismiss {
        app.shell.center.notice = None;
    }
    if merge {
        app.ui.open_tab(ws, TabKind::Merge { ws });
        app.shell.center.notice = None;
    }
    if back {
        super::actions::select_workspace(app, project, from);
        app.shell.center.notice = None;
    }
    Some(h)
}

/// A workspace as a space: "auth-tests 공간", but "기본 공간" stays as it is.
fn space_name(name: &str) -> String {
    if name.ends_with("공간") { name.to_string() } else { format!("{name} 공간") }
}

fn split(app: &mut App, ui: &mut egui::Ui, ws: WsKey, body: Rect, a: Tab, b: Tab) {
    let frac = app.ui.tabset(&ws).map_or(0.5, |s| s.split_ratio);
    let min_f = PANE_MIN / body.width();
    let frac = frac.clamp(min_f, 1.0 - min_f);
    let x = body.left() + (body.width() * frac).round();
    let left = Rect::from_min_max(body.min, Pos2::new(x, body.bottom()));
    let right = Rect::from_min_max(Pos2::new(x + 1.0, body.top()), body.max);
    pane(app, ui, ws, left, a, Pane::Left);
    pane(app, ui, ws, right, b, Pane::Right);
    ui.painter().vline(x + 0.5, body.y_range(), Stroke::new(1.0, c::LINE));
    let handle = Rect::from_x_y_ranges(x - 3.0..=x + 4.0, body.y_range());
    if let Some(dx) = super::resize_handle(ui, handle, "split-divider") {
        let f = (frac + dx / body.width()).clamp(min_f, 1.0 - min_f);
        app.ui.tabset_mut(&ws).split_ratio = f;
    }
}

fn pane(app: &mut App, ui: &mut egui::Ui, ws: WsKey, mut rect: Rect, tab: Tab, idx: Pane) {
    let split = app.ui.tabset(&ws).is_some_and(|s| s.split.is_some());
    let pane_rect = rect;
    if ui.rect_contains_pointer(rect) && ui.input(|i| i.pointer.any_pressed()) {
        app.ui.tabset_mut(&ws).focused_pane = idx;
        app.shell.center.focused_tab = Some((ws, tab.id));
        // Clicking the title focuses the content too; clicking an editor keeps its cursor.
        app.shell.center.focus_content =
            split && ui.input(|i| i.pointer.interact_pos().is_some_and(|p| p.y < rect.top() + PANE_HEADER));
    }
    if split {
        pane_header(app, ui, ws, &mut rect, &tab, idx);
    }
    let term = match tab.kind {
        TabKind::Terminal { host, session } => Some((host, session)),
        _ => None,
    };
    let content = if term.is_some() {
        Rect::from_min_max(rect.min, Pos2::new(rect.right(), rect.bottom() - size::SESSION_BAR))
    } else {
        rect
    };
    let bg = if term.is_some() { c::TERMINAL } else { c::DOC };
    ui.painter().rect_filled(content, CornerRadius::ZERO, bg);
    let mut child = ui.new_child(
        UiBuilder::new().max_rect(content).id_salt(("pane", tab.id)).layout(egui::Layout::top_down(egui::Align::Min)),
    );
    child.set_clip_rect(content.intersect(ui.clip_rect()));
    if let Some((host, session)) = term
        && app.ui.tabset(&ws).is_some_and(|s| s.focused_tab() == Some(tab.id))
        && (app.shell.center.focus_content || child.memory(|m| m.focused().is_none()))
        && app.ui.modal.is_none()
        && !app.ui.palette_open
        && app.shell.center.picker.is_none()
    {
        let scope = match host {
            crate::client::HostKey::Local => child.scope_id(),
            crate::client::HostKey::Device(_) => child.scope_id().with(("remote-term", session)),
        };
        child.memory_mut(|m| m.request_focus(scope.with(("terminal", host, session))));
    }
    if let TabKind::Doc { ws: doc_ws, path } = &tab.kind
        && app.shell.center.focus_content
        && app.ui.tabset(&ws).is_some_and(|s| s.focused_tab() == Some(tab.id))
        && app.ui.modal.is_none()
        && !app.ui.palette_open
        && app.shell.center.picker.is_none()
    {
        crate::views::docs::focus_editor(app, *doc_ws, path);
    }
    crate::views::show_tab(app, &mut child, ws, &tab);
    if let Some((host, session)) = term {
        let bar = Rect::from_min_max(Pos2::new(rect.left(), content.bottom()), rect.max);
        ui.scope_builder(UiBuilder::new().max_rect(bar).id_salt(("bar", tab.id)), |ui| {
            session_bar::show(app, ui, ws, host, session, tab.id)
        });
    }
    if let Some(focused) = ui.memory(|m| m.focused()).and_then(|id| ui.ctx().read_response(id))
        && focused.layer_id == ui.layer_id()
        && pane_rect.contains(focused.rect.center())
    {
        app.ui.tabset_mut(&ws).focused_pane = idx;
        app.shell.center.focused_tab = Some((ws, tab.id));
    }
}

fn pane_header(app: &mut App, ui: &mut egui::Ui, ws: WsKey, body: &mut Rect, tab: &Tab, pane: Pane) {
    let header = Rect::from_min_size(body.min, Vec2::new(body.width(), PANE_HEADER));
    body.min.y += PANE_HEADER;
    let set = app.ui.tabset(&ws).cloned().unwrap_or_default();
    let focused = set.focused_pane == pane;
    let sessions = tabs::session_map(app, ws);
    let look = tabs::look(app, tab, &sessions);
    ui.painter().rect_filled(header, CornerRadius::ZERO, if focused { c::CONTROL } else { c::NAV });
    ui.painter().hline(
        header.x_range(),
        header.bottom() - 1.0,
        Stroke::new(if focused { 2.0 } else { 1.0 }, if focused { c::EMBER } else { c::LINE }),
    );
    let badge = Rect::from_center_size(Pos2::new(header.left() + 18.0, header.center().y), Vec2::new(20.0, 18.0));
    split_picker::paint_mark(ui, badge, &look.mark);
    let controls_w = widgets::text_width(ui, "바꾸기", font(ty::SMALL)) + 20.0 + 4.0 + 24.0 + 8.0;
    let title = Rect::from_min_max(
        Pos2::new(badge.right() + 6.0, header.top()),
        Pos2::new(header.right() - controls_w - 6.0, header.bottom()),
    );
    widgets::paint_clipped(ui, title, &look.title, font(12.0), if focused { c::TEXT_STRONG } else { c::TEXT_2 }, true);
    let controls = Rect::from_min_size(
        Pos2::new(header.right() - controls_w, header.center().y - 12.0),
        Vec2::new(controls_w, 24.0),
    );
    ui.scope_builder(
        UiBuilder::new()
            .max_rect(controls)
            .id_salt(("pane-header", pane == Pane::Right))
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
        |ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            if widgets::pill_button(ui, None, "바꾸기", 24.0, c::CONTROL).clicked() {
                split_picker::open(app, ws, pane);
            }
            if widgets::icon_btn(ui, icons::X, "이 칸 닫기 — 탭은 유지돼요", 24.0, 24.0, 12.0).clicked() {
                app.ui.tabset_mut(&ws).close_pane(pane);
            }
        },
    );
    if set.right_hidden {
        let strip = Rect::from_min_size(body.min, Vec2::new(body.width(), 28.0));
        body.min.y += strip.height();
        ui.painter().rect_filled(strip, CornerRadius::ZERO, c::PANEL);
        ui.scope_builder(
            UiBuilder::new()
                .max_rect(strip.shrink2(Vec2::new(8.0, 2.0)))
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
            |ui| {
                ui.label(egui::RichText::new("다른 칸이 숨겨져 있어요").font(font(11.5)).color(c::TEXT_2));
                if widgets::pill_button(ui, Some(icons::COLUMNS_2), "바꿔 보기", 24.0, c::CONTROL).clicked() {
                    app.ui.tabset_mut(&ws).swap_panes();
                }
            },
        );
    }
}

fn tab_drop(app: &mut App, ui: &mut egui::Ui, ws: WsKey, body: Rect) {
    let Some(drag) = egui::DragAndDrop::payload::<tabs::TabDrag>(ui.ctx()).filter(|d| d.ws == ws) else { return };
    let Some(pos) = ui.input(|i| i.pointer.interact_pos()).filter(|p| body.contains(*p)) else { return };
    let set = app.ui.tabset(&ws).cloned().unwrap_or_default();
    let two_panes = set.split.is_some() && !set.right_hidden;
    let min_frac = (PANE_MIN / body.width()).min(0.5);
    let divider = body.left() + body.width() * set.split_ratio.clamp(min_frac, 1.0 - min_frac);
    let (pane, target) = if two_panes {
        if pos.x < divider {
            (Pane::Left, Rect::from_min_max(body.min, Pos2::new(divider, body.bottom())))
        } else {
            (Pane::Right, Rect::from_min_max(Pos2::new(divider, body.top()), body.max))
        }
    } else if !set.right_hidden && pos.x >= body.right() - 64.0 && set.active != Some(drag.id) {
        (Pane::Right, Rect::from_min_max(Pos2::new(body.center().x, body.top()), body.max))
    } else {
        return;
    };
    ui.painter().rect_filled(target, CornerRadius::ZERO, c::EMBER.gamma_multiply(0.12));
    ui.painter().rect_stroke(
        target.shrink(2.0),
        CornerRadius::same(4),
        Stroke::new(2.0, c::EMBER),
        egui::StrokeKind::Inside,
    );
    ui.painter().text(target.center(), egui::Align2::CENTER_CENTER, "여기에 열어요", font(13.0), c::TEXT_STRONG);
    if ui.input(|i| i.pointer.any_released()) {
        app.ui.tabset_mut(&ws).drop_in_pane(drag.id, pane);
        egui::DragAndDrop::clear_payload(ui.ctx());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn space_names_read_naturally() {
        assert_eq!(space_name("auth-tests"), "auth-tests 공간");
        assert_eq!(space_name("기본 공간"), "기본 공간");
        assert_eq!(josa::euro_ro(&space_name("기본 공간")), "기본 공간으로");
        assert_eq!(josa::euro_ro(&space_name("정산테스트")), "정산테스트 공간으로");
    }
}
