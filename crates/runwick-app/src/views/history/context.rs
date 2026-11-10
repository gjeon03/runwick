//! Reading a message with the messages around it, loading more in both directions. Live run
//! and resume are separate actions. The focused message (the one opened) shows its actions
//! below it; another message shows them next to its time while the pointer is over it or it has
//! keyboard focus (Tab reaches each message, ↑/↓ move between them, Enter selects one): pin the
//! whole message or a selected part, copy, or start a wiki page that quotes it.

use super::{Opened, fmt, note};
use crate::App;
use crate::client::HostKey;
use crate::shell::widgets;
use crate::shell::{actions, data};
use crate::state::WsKey;
use crate::theme::{self, c, font, medium, semibold, ty};
use egui::{CornerRadius, Margin, RichText};
use runwick_core::api::projects::ById;
use runwick_core::api::records::{
    Anchor, Context, ContextQuery, Message, PinQuery, PinSource, PinsList, RecordsContext, RecordsStatus, Role,
    StatusQuery,
};
use runwick_core::api::sessions::{Phase, SessionInfo, SessionsGet};
use std::sync::Arc;

const MORE: u32 = 20;
const MAX_WIDTH: f32 = 720.0;
/// Height of a message's time line (also holds its buttons while the pointer is over it).
const INLINE_H: f32 = 22.0;

pub fn show(app: &mut App, ui: &mut egui::Ui, in_panel: bool) {
    let Some(open) = app.feat.history.reader.open.clone() else {
        empty(ui, "검색 결과나 핀의 '원문'을 고르면 여기에서 앞뒤 문맥을 읽어요.");
        return;
    };
    let q = ContextQuery { anchor: open.anchor, before: open.before, after: open.after };
    let res = app.hub.query::<RecordsContext>(open.host, &q);
    let cx = match (res.data, &app.feat.history.reader.shown) {
        (Some(cx), _) => {
            app.feat.history.reader.shown = Some(cx.clone());
            cx
        }
        // A longer read of the same session is loading: keep showing what is there.
        (None, Some(prev)) if res.error.is_none() && same_session(prev, &open) => prev.clone(),
        (None, _) => {
            match res.error {
                Some(e) if e.code == runwick_core::rpc::codes::NOT_FOUND => empty(
                    ui,
                    "기록에서 이 메시지를 찾을 수 없어요. 원본 기록이 지워졌을 수 있어요. 핀은 저장된 사본으로 계속 읽을 수 있어요.",
                ),
                Some(e) => empty(ui, &format!("기록을 읽지 못했어요: {}", e.message)),
                None => empty(ui, "기록을 불러오는 중…"),
            }
            return;
        }
    };
    let info = app.hub.query::<SessionsGet>(open.host, &ById { id: cx.session_id }).data;
    header(app, ui, &open, &cx, info.as_deref(), in_panel);
    let foot_h = 30.0;
    let body = ui.available_rect_before_wrap();
    let list = egui::Rect::from_min_max(body.min, egui::pos2(body.right(), (body.bottom() - foot_h).max(body.top())));
    ui.scope_builder(egui::UiBuilder::new().max_rect(list).id_salt(("context", cx.session_id)), |ui| {
        messages(app, ui, &open, &cx, in_panel);
    });
    let foot = egui::Rect::from_min_max(egui::pos2(body.left(), list.bottom()), body.max);
    footer(app, ui, foot, open.host, &cx);
}

pub(super) fn same_session(prev: &Context, open: &Opened) -> bool {
    match open.anchor {
        Anchor::Message { id } => prev.messages.iter().any(|m| m.id == id),
        Anchor::SessionEnd { id } => prev.session_id == id,
    }
}

fn empty(ui: &mut egui::Ui, text: &str) {
    ui.add_space(32.0);
    ui.vertical_centered(|ui| {
        ui.set_max_width(420.0);
        ui.add(egui::Label::new(RichText::new(text).font(font(ty::BODY)).color(c::TEXT_3)).wrap());
    });
}

/// The workspace key and local project of a session's workspace, for opening its tab.
fn session_place(
    app: &mut App,
    host: HostKey,
    cx: &Context,
) -> Option<(runwick_core::api::projects::ProjectId, WsKey)> {
    let key = WsKey { host, id: cx.workspace_id };
    let list = data::workspaces(app).data?;
    let w = data::find_ws(&list, &key)?;
    Some((w.project_id, key))
}

fn running(app: &App, host: HostKey, info: &SessionInfo) -> bool {
    data::live_of(app, host, info).is_some_and(|l| matches!(l.phase, Phase::Running | Phase::Starting))
}

fn header(app: &mut App, ui: &mut egui::Ui, open: &Opened, cx: &Context, info: Option<&SessionInfo>, in_panel: bool) {
    let host = open.host;
    let alive = info.is_some_and(|i| running(app, host, i));
    let resumable = info.is_some_and(|i| i.session.tool.is_agent() && i.session.provider_conv_id.is_some());
    let ws_name = data::workspaces(app)
        .data
        .and_then(|l| data::find_ws(&l, &WsKey { host, id: cx.workspace_id }).map(|w| w.name.clone()));
    let mut meta = vec![cx.tool.label().to_string()];
    if let Some(n) = ws_name {
        meta.push(n);
    }
    match (open.anchor, cx.anchor.and_then(|a| cx.messages.iter().find(|m| m.id == a))) {
        (Anchor::Message { .. }, Some(m)) => meta.push(format!("{} 메시지로 이동함", fmt::when_full(m.at))),
        (Anchor::SessionEnd { .. }, _) => meta.push("최근 대화".into()),
        _ => {}
    }
    let mut act = None;
    egui::Frame::new().inner_margin(Margin::symmetric(if in_panel { 12 } else { 20 }, 12)).show(ui, |ui| {
        ui.horizontal_wrapped(|ui| {
            ui.vertical(|ui| {
                ui.set_max_width((ui.available_width() - 260.0).max(200.0));
                theme::truncated(ui, cx.session_title.clone(), semibold(15.0), c::TEXT_STRONG);
                theme::truncated(ui, meta.join(" · "), font(ty::SMALL), c::TEXT_3);
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let resume = ui
                    .add_enabled(
                        resumable && !alive,
                        egui::Button::new(RichText::new("같은 대화 재개").font(medium(12.5))),
                    )
                    .on_disabled_hover_text(if alive {
                        "실행 중이에요. '살아 있는 실행 열기'로 이어서 보세요"
                    } else {
                        "저장된 대화 ID가 없어 같은 대화를 재개할 수 없어요"
                    });
                if resume.clicked() {
                    act = Some(true);
                }
                let live = ui
                    .add_enabled(alive, egui::Button::new(RichText::new("살아 있는 실행 열기").font(medium(12.5))))
                    .on_disabled_hover_text("지금 실행 중이 아니에요");
                if live.clicked() {
                    act = Some(false);
                }
            });
        });
    });
    ui.painter().hline(ui.max_rect().x_range(), ui.cursor().top() - 0.5, egui::Stroke::new(1.0, c::LINE));
    let Some(resume) = act else { return };
    let Some((project, ws)) = session_place(app, host, cx) else {
        app.toast_error("이 세션의 작업 공간을 찾을 수 없어요");
        return;
    };
    if resume {
        actions::restart_session(app, host, cx.session_id, true);
    }
    let hidden = info.is_some_and(|i| i.session.hidden);
    actions::open_session(app, project, ws, cx.session_id, hidden);
    crate::terminal::focus(app, host, cx.session_id);
}

fn messages(app: &mut App, ui: &mut egui::Ui, open: &Opened, cx: &Arc<Context>, in_panel: bool) {
    let pins = app.hub.query::<PinsList>(open.host, &PinQuery { session_id: Some(cx.session_id), project_id: None });
    let mut load = (0u32, 0u32);
    let mut pin = None;
    let mut excerpt_done = None;
    let mut copy = None;
    let mut wiki = None;
    let mut select = None;
    let mut hover = None;
    let mut keyboard = None;
    // Focusable rows in order (for ↑/↓) and which one has focus.
    let mut rows: Vec<egui::Id> = Vec::new();
    let mut focused_row = None;
    let hovered = app.feat.history.reader.hover_msg;
    let keyboard_before = app.feat.history.reader.keyboard_msg;
    let focus_actions = app.feat.history.reader.focus_actions.take();
    let anchor = cx.anchor;
    let anchor_msg = anchor.filter(|_| matches!(open.anchor, Anchor::Message { .. }));
    let selected =
        app.feat.history.reader.select_msg.filter(|id| cx.messages.iter().any(|m| m.id == *id)).or(anchor_msg);
    let scroll_to = app.feat.history.reader.scrolled != Some(open.anchor);
    egui::ScrollArea::vertical().id_salt(("ctx-scroll", cx.session_id)).auto_shrink([false, false]).show(ui, |ui| {
        let w = ui.available_width();
        let pad = if in_panel { 12.0 } else { 20.0 };
        let col_w = (w - 2.0 * pad).min(MAX_WIDTH);
        let left = ((w - col_w) / 2.0).max(pad);
        egui::Frame::new().inner_margin(Margin { left: left as i8, right: 0, top: 16, bottom: 16 }).show(ui, |ui| {
            ui.set_max_width(col_w);
            ui.spacing_mut().item_spacing.y = 16.0;
            if cx.has_before {
                ui.vertical_centered(|ui| {
                    if fmt::quiet_link(ui, "이전 메시지 더 읽기").clicked() {
                        load.0 = MORE;
                    }
                });
            }
            if cx.messages.is_empty() {
                note(ui, "아직 수집된 대화가 없어요.");
            }
            for m in &cx.messages {
                let is_anchor = anchor_msg == Some(m.id);
                let look = Look {
                    is_anchor,
                    selected: selected == Some(m.id),
                    hovered: hovered == Some(m.id),
                    keyboard: keyboard_before == Some(m.id),
                    pinned: pins.data.as_ref().is_some_and(|list| super::message_pin(list, m.id).is_some()),
                    focus_actions: focus_actions == Some(m.id),
                };
                let r = message(app, ui, cx, m, look, &open.terms);
                if is_anchor && scroll_to {
                    r.rect_resp.scroll_to_me(Some(egui::Align::Center));
                }
                if ui.rect_contains_pointer(r.rect_resp.rect) {
                    hover = Some(m.id);
                }
                if r.keyboard {
                    keyboard = Some(m.id);
                }
                if let Some(row) = &r.row {
                    if row.has_focus() {
                        focused_row = Some(rows.len());
                    }
                    rows.push(row.id);
                }
                if let Some(a) = r.act {
                    match a {
                        MsgAct::Pin => pin = Some(m.id),
                        MsgAct::Excerpt(text) => excerpt_done = Some((m.id, text)),
                        MsgAct::Copy => copy = Some(m.text.clone()),
                        MsgAct::Wiki => wiki = Some(m.clone()),
                        MsgAct::Select => select = Some(m.id),
                    }
                }
            }
            if matches!(open.anchor, Anchor::SessionEnd { .. }) && scroll_to {
                ui.scroll_to_cursor(Some(egui::Align::BOTTOM));
            }
            if cx.has_after {
                ui.vertical_centered(|ui| {
                    if fmt::quiet_link(ui, "다음 메시지 더 읽기").clicked() {
                        load.1 = MORE;
                    }
                });
            }
        });
    });
    app.feat.history.reader.scrolled = Some(open.anchor);
    if hover != hovered || keyboard != keyboard_before {
        app.feat.history.reader.hover_msg = hover;
        app.feat.history.reader.keyboard_msg = keyboard;
        ui.ctx().request_repaint();
    }
    // ↑/↓ move between messages; Enter selects one and puts focus on its first button.
    if let Some(i) = focused_row {
        let (down, up) = ui.input_mut(|inp| {
            (
                inp.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown),
                inp.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp),
            )
        });
        let to = if down {
            rows.get(i + 1)
        } else if up {
            i.checked_sub(1).and_then(|j| rows.get(j))
        } else {
            None
        };
        if let Some(&id) = to {
            ui.memory_mut(|mem| mem.request_focus(id));
        }
    }
    if let Some(id) = select {
        app.feat.history.reader.select_msg = Some(id);
        app.feat.history.reader.focus_actions = Some(id);
        ui.ctx().request_repaint();
    }
    if let Some(m) = wiki {
        to_wiki(app, open.host, cx, &m);
    }
    if load != (0, 0)
        && let Some(o) = app.feat.history.reader.open.as_mut()
    {
        o.before += load.0;
        o.after += load.1;
    }
    if let Some(text) = copy {
        ui.ctx().copy_text(text);
        app.toast(crate::state::ToastKind::Success, "메시지를 복사했어요");
    }
    let host = open.host;
    if let Some(id) = pin {
        super::toggle_message_pin(app, host, cx.session_id, id);
    }
    if let Some((id, text)) = excerpt_done {
        app.feat.history.reader.excerpt = None;
        let src = PinSource::Excerpt { message_id: id, text };
        super::create_pin(app, host, src);
    }
}

enum MsgAct {
    Pin,
    Excerpt(String),
    Copy,
    Wiki,
    /// Enter on the row: make it the selected message.
    Select,
}

/// How a message is shown this frame.
#[derive(Clone, Copy)]
struct Look {
    /// The message the search led to (its words are highlighted).
    is_anchor: bool,
    /// The selected message: its actions sit under it (the anchor until Enter picks another).
    selected: bool,
    hovered: bool,
    /// The row or one of its buttons had keyboard focus last frame.
    keyboard: bool,
    pinned: bool,
    /// Move keyboard focus to the first action button.
    focus_actions: bool,
}

/// Where a message's actions are shown.
#[derive(Clone, Copy, PartialEq)]
enum Actions {
    /// Under the focused message, as in the design.
    Below,
    /// Next to the time of the message under the pointer (no layout shift).
    Inline,
    Hidden,
}

/// The person's name for who wrote a message.
fn author(cx: &Context, m: &Message) -> String {
    match m.role {
        Role::User => "나".to_string(),
        Role::Assistant => cx.tool.label().to_string(),
        Role::Tool => "도구".to_string(),
    }
}

/// "위키 초안으로": a new wiki page in the session's project quoting the message.
fn to_wiki(app: &mut App, host: HostKey, cx: &Context, m: &Message) {
    let source = format!("{} · {} · {}", cx.session_title, author(cx, m), fmt::date_time(m.at));
    let seed = crate::views::wiki::Seed::quote(&m.text, &m.text, &source, Some(cx.session_id), Some(m.id));
    match session_place(app, host, cx) {
        Some((project, ws)) => crate::views::wiki::new_page_from(app, project, ws, seed),
        None => app.toast_error("이 대화의 작업 공간을 찾을 수 없어 위키 문서를 만들 수 없어요"),
    }
}

/// The action buttons of a message; `h` is the button height. `focus_first` moves keyboard
/// focus to the first button. Returns the button pressed and whether one has keyboard focus.
fn toolbar(ui: &mut egui::Ui, pinned: bool, h: f32, floating: bool, focus_first: bool) -> (Option<Tool>, bool) {
    let mut out = None;
    let mut focused = false;
    let frame = egui::Frame::new()
        .fill(c::CONTROL)
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::same(if floating { 3 } else { 1 }));
    let frame = if floating {
        frame.shadow(egui::Shadow { offset: [0, 8], blur: 24, spread: 0, color: egui::Color32::from_black_alpha(100) })
    } else {
        frame
    };
    frame.show(ui, |ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        // The row is exactly as tall as the buttons (the time line must not grow on hover).
        ui.spacing_mut().interact_size.y = h;
        ui.horizontal(|ui| {
            let tip = if pinned { "핀 해제" } else { "메시지 전체를 핀에 고정해요" };
            let pin = super::pin_toggle_button(ui, pinned, tip, h);
            crate::terminal::verify::target("context:핀", pin.rect);
            if focus_first {
                pin.request_focus();
            }
            focused |= pin.has_focus();
            if pin.clicked() {
                out = Some(Tool::Pin);
            }
            let part = widgets::pill_button(ui, None, "일부만 핀", h, c::CONTROL);
            focused |= part.has_focus();
            if part.on_hover_text("고정할 부분을 드래그해 골라요").clicked() {
                out = Some(Tool::Excerpt);
            }
            let copy = widgets::pill_button(ui, None, "복사", h, c::CONTROL);
            focused |= copy.has_focus();
            if copy.clicked() {
                out = Some(Tool::Copy);
            }
            let wiki = widgets::pill_button(ui, None, "위키 초안으로", h, c::CONTROL);
            focused |= wiki.has_focus();
            if floating {
                crate::terminal::verify::target("context:위키 초안으로", wiki.rect);
            }
            if wiki
                .on_hover_text("이 메시지를 인용한 새 위키 문서를 만들어요. 원문으로 돌아가는 링크도 함께 넣어요")
                .clicked()
            {
                out = Some(Tool::Wiki);
            }
        });
    });
    (out, focused)
}

/// A button of [`toolbar`].
enum Tool {
    Pin,
    Excerpt,
    Copy,
    Wiki,
}

struct MsgResp {
    rect_resp: egui::Response,
    act: Option<MsgAct>,
    /// The focusable row (tool lines have none).
    row: Option<egui::Response>,
    /// The row or one of its buttons has keyboard focus.
    keyboard: bool,
}

fn message(app: &mut App, ui: &mut egui::Ui, cx: &Context, m: &Message, look: Look, terms: &[String]) -> MsgResp {
    let mut act = None;
    let mut tool = None;
    let mut keyboard = false;
    let who = author(cx, m);
    // A focus stop for the whole message, registered before its buttons so Tab goes row →
    // buttons → next row. Its rect is the one drawn last frame (the size is known only after).
    let row = (m.role != Role::Tool).then(|| {
        let id = ui.make_persistent_id(("ctx-msg", m.id));
        let last = ui.ctx().data(|d| d.get_temp::<egui::Rect>(id));
        let rect = last
            .unwrap_or_else(|| egui::Rect::from_min_size(ui.cursor().min, egui::vec2(ui.available_width(), INLINE_H)));
        let row = ui.interact(rect, id, egui::Sense::focusable_noninteractive());
        let label = format!("{who} 메시지 · {}", fmt::when_full(m.at));
        row.widget_info(|| egui::WidgetInfo::labeled(egui::Role::ListItem, true, &label));
        if row.has_focus() {
            // ↑/↓ belong to the message list (handled in `messages`), not to egui's spatial moves.
            let filter = egui::EventFilter { vertical_arrows: true, ..Default::default() };
            ui.memory_mut(|mem| mem.set_focus_lock_filter(id, filter));
            if ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter)) {
                act = Some(MsgAct::Select);
            }
        }
        row
    });
    let row_focused = row.as_ref().is_some_and(|r| r.has_focus());
    let place = if look.selected {
        Actions::Below
    } else if look.hovered || look.keyboard || row_focused {
        Actions::Inline
    } else {
        Actions::Hidden
    };
    let pinned = look.pinned;
    let inner = ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 4.0;
        if m.role == Role::Tool {
            ui.label(RichText::new(format!("{} · {}", m.text, fmt::hm(m.at))).font(font(12.0)).color(c::TEXT_3));
            return;
        }
        let mut meta = format!("{who} · {}", fmt::when_full(m.at));
        if m.sidechain {
            meta.push_str(" · 하위 에이전트");
        }
        let excerpting = app.feat.history.reader.excerpt == Some(m.id);
        ui.horizontal(|ui| {
            // Same height with or without the buttons, so hovering never moves the text.
            ui.set_min_height(INLINE_H);
            ui.label(RichText::new(meta).font(font(ty::SMALL)).color(c::TEXT_3));
            if place == Actions::Inline && !excerpting {
                ui.add_space(6.0);
                let (t, focused) = toolbar(ui, pinned, INLINE_H - 2.0, false, false);
                (tool, keyboard) = (t, focused);
            }
        });
        if excerpting {
            if let Some(text) = excerpt_editor(ui, m) {
                act = Some(MsgAct::Excerpt(text));
            }
            if fmt::quiet_link(ui, "취소").clicked() {
                app.feat.history.reader.excerpt = None;
            }
            return;
        }
        let ranges = if look.is_anchor { fmt::find_terms(&m.text, terms) } else { Vec::new() };
        let color = if m.role == Role::User { c::TEXT_STRONG } else { c::TEXT };
        let job = fmt::highlighted(&m.text, &ranges, font(14.0), color);
        let label = egui::Label::new(job).wrap().selectable(true);
        if m.role == Role::User {
            egui::Frame::new()
                .fill(c::ROW)
                .corner_radius(CornerRadius::same(10))
                .inner_margin(Margin::symmetric(12, 10))
                .show(ui, |ui| {
                    ui.add(label);
                });
        } else {
            ui.add(label);
        }
        if place == Actions::Below {
            ui.add_space(4.0);
            let (t, focused) = toolbar(ui, pinned, 26.0, true, look.focus_actions);
            (tool, keyboard) = (t, focused);
        }
        match tool {
            Some(Tool::Pin) => act = Some(MsgAct::Pin),
            Some(Tool::Excerpt) => app.feat.history.reader.excerpt = Some(m.id),
            Some(Tool::Copy) => act = Some(MsgAct::Copy),
            Some(Tool::Wiki) => act = Some(MsgAct::Wiki),
            None => {}
        }
    });
    let rect = inner.response.rect;
    if look.selected {
        let bar = egui::Rect::from_min_max(
            egui::pos2(rect.left() - 10.0, rect.top()),
            egui::pos2(rect.left() - 7.0, rect.bottom()),
        );
        ui.painter().rect_filled(bar, CornerRadius::same(2), c::EMBER);
    }
    if let Some(row) = &row {
        ui.ctx().data_mut(|d| d.insert_temp(row.id, rect));
        if row.gained_focus() {
            ui.scroll_to_rect(rect, None);
        }
        if row_focused && ui.input(|i| !i.pointer.any_down()) {
            let ring = rect.expand2(egui::vec2(11.0, 5.0));
            ui.painter().rect_stroke(
                ring,
                CornerRadius::same(8),
                egui::Stroke::new(1.5, c::TEXT_STRONG),
                egui::StrokeKind::Outside,
            );
        }
    }
    MsgResp { rect_resp: inner.response, act, row, keyboard: keyboard || row_focused }
}

/// The message as selectable text; returns the selection when the person pins it.
fn excerpt_editor(ui: &mut egui::Ui, m: &Message) -> Option<String> {
    let id = ui.make_persistent_id(("excerpt", m.id));
    let mut text: &str = &m.text;
    egui::Frame::new().fill(c::ROW).corner_radius(CornerRadius::same(8)).inner_margin(Margin::same(10)).show(
        ui,
        |ui| {
            ui.add(
                egui::TextEdit::multiline(&mut text)
                    .id(id)
                    .font(font(14.0))
                    .text_color(c::TEXT)
                    .frame(egui::Frame::NONE)
                    .desired_width(f32::INFINITY),
            );
        },
    );
    let selected = egui::text_edit::TextEditState::load(ui.ctx(), id)
        .and_then(|s| s.cursor.char_range())
        .filter(|r| !r.is_empty())
        .map(|r| {
            let range = r.as_sorted_char_range();
            m.text.chars().skip(range.start.0).take(range.end.0 - range.start.0).collect::<String>()
        })
        .filter(|s| !s.trim().is_empty());
    let mut out = None;
    ui.horizontal(|ui| {
        let label = if selected.is_some() {
            "고른 부분 핀 고정"
        } else {
            "고정할 부분을 드래그해 고르세요"
        };
        if ui.add_enabled(selected.is_some(), egui::Button::new(RichText::new(label).font(medium(12.0)))).clicked() {
            out = selected.clone();
        }
    });
    out
}

fn footer(app: &mut App, ui: &mut egui::Ui, rect: egui::Rect, host: HostKey, cx: &Context) {
    ui.painter().rect_filled(rect, CornerRadius::ZERO, c::NAV);
    ui.painter().hline(rect.x_range(), rect.top() + 0.5, egui::Stroke::new(1.0, c::LINE));
    let status =
        app.hub.query::<RecordsStatus>(host, &StatusQuery { session_id: Some(cx.session_id), project_id: None }).data;
    let mut line = "기록 보기 · CLI 입력과 스크롤은 바뀌지 않아요".to_string();
    if let Some(s) = status.as_ref().and_then(|l| l.first()) {
        line.push_str(&format!(" · {}", s.state.label()));
        if let Some(t) = s.last_collected_at {
            line.push_str(&format!(" · 마지막 수집 {}", data::ago(t)));
        }
        if let Some(e) = &s.last_error {
            line.push_str(&format!(" · {e}"));
        }
    }
    let r = rect.shrink2(egui::vec2(12.0, 0.0));
    crate::shell::widgets::paint_clipped(ui, r, &line, font(12.0), c::TEXT_3, true);
}
