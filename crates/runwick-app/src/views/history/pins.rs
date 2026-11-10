//! Pins of the current scope: source links, inline titles and optional memo editing.

use super::{PinDraft, PinField, ScopeKind, fmt, note, scope_ctx};
use crate::App;
use crate::client::HostKey;
use crate::icons;
use crate::shell::widgets;
use crate::theme::{self, c, font, medium};
use egui::{CornerRadius, Margin, RichText};
use runwick_core::api::projects::ById;
use runwick_core::api::records::{
    MessageId, Pin, PinId, PinKind, PinQuery, PinUpdate, PinsList, PinsRemove, PinsUpdate,
};

/// The same outlined/filled pin in message actions and the session bar.
pub(crate) fn toggle_button(ui: &mut egui::Ui, pinned: bool, tooltip: &str, height: f32) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(28.0, height), egui::Sense::click());
    resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, ui.is_enabled(), pinned, tooltip));
    if resp.hovered() || resp.has_focus() {
        ui.painter().rect_filled(rect, CornerRadius::same(6), c::CONTROL_HOVER);
    }
    let color = if pinned { c::TEXT_STRONG } else { c::TEXT_2 };
    let origin = rect.center() - egui::vec2(7.0, 7.0);
    let point = |x, y| origin + egui::vec2(x, y);
    if pinned {
        ui.painter().rect_filled(egui::Rect::from_min_max(point(4.0, 1.0), point(10.0, 7.0)), 0, color);
        ui.painter().add(egui::Shape::convex_polygon(
            vec![point(4.0, 7.0), point(10.0, 7.0), point(12.0, 10.0), point(2.0, 10.0)],
            color,
            egui::Stroke::NONE,
        ));
    }
    let stroke = egui::Stroke::new(1.3, color);
    ui.painter().add(egui::Shape::closed_line(
        vec![point(4.0, 1.0), point(10.0, 1.0), point(10.0, 7.0), point(12.0, 10.0), point(2.0, 10.0), point(4.0, 7.0)],
        stroke,
    ));
    ui.painter().line_segment([point(7.0, 10.0), point(7.0, 14.0)], stroke);
    theme::focus_ring(ui, &resp, CornerRadius::same(6));
    resp.on_hover_text(tooltip)
}

fn query(app: &mut App) -> (HostKey, PinQuery, String) {
    let sc = scope_ctx(app);
    let (kind, _, label) = super::search::effective(app.feat.history.scope, &sc);
    let q = match kind {
        ScopeKind::Session => PinQuery { session_id: sc.session.as_ref().map(|s| s.0), project_id: None },
        ScopeKind::Project => PinQuery { project_id: sc.project.as_ref().map(|p| p.0), session_id: None },
        ScopeKind::All => PinQuery::default(),
    };
    (sc.host, q, label)
}

/// Number of pins in the current scope, once known.
pub fn count(app: &mut App) -> Option<usize> {
    let (host, q, _) = query(app);
    app.hub.query::<PinsList>(host, &q).data.map(|l| l.len())
}

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    let (host, q, label) = query(app);
    let res = app.hub.query::<PinsList>(host, &q);
    egui::Frame::new().inner_margin(Margin { left: 12, right: 12, top: 8, bottom: 8 }).show(ui, |ui| {
        let n = res.data.as_ref().map(|l| l.len());
        let line = match n {
            Some(n) => format!("{label}의 핀 {n}개 · 범위는 검색과 같아요"),
            None => format!("{label}의 핀"),
        };
        note(ui, &line);
    });
    let Some(list) = res.data else {
        egui::Frame::new().inner_margin(Margin::symmetric(12, 0)).show(ui, |ui| match res.error {
            Some(e) => note(ui, &format!("핀을 불러오지 못했어요: {}", e.message)),
            None => note(ui, "핀을 불러오는 중…"),
        });
        return;
    };
    if list.is_empty() {
        egui::Frame::new().inner_margin(Margin::symmetric(12, 0)).show(ui, |ui| {
            note(ui, "대화 기록에서 메시지의 핀을 누르면 여기에 모여요.");
        });
        return;
    }
    let highlight = app.feat.history.highlight_pin;
    egui::ScrollArea::vertical().id_salt("history-pins").auto_shrink([false, false]).show(ui, |ui| {
        egui::Frame::new().inner_margin(Margin { left: 8, right: 8, top: 0, bottom: 8 }).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 4.0;
            for pin in list.iter() {
                let resp = ui.push_id((host, pin.id), |ui| card(app, ui, host, pin, highlight == Some(pin.id))).inner;
                if highlight == Some(pin.id) {
                    resp.scroll_to_me(Some(egui::Align::Center));
                }
            }
        });
    });
    if highlight.is_some() {
        app.feat.history.highlight_pin = None;
    }
}

/// Where a pin came from, and whether that is still reachable (warning color when not).
pub fn source_line(pin: &Pin) -> (String, bool) {
    let kind = match pin.kind {
        PinKind::Message => "메시지 핀",
        PinKind::Excerpt => "발췌 핀",
        PinKind::Terminal => "터미널 발췌",
    };
    if pin.kind == PinKind::Terminal {
        return (format!("{kind} · 원문 메시지 연결 불가 · {}", pin.session_title), true);
    }
    if !pin.source_available {
        return (format!("{kind} · 원문 연결 불가 (저장된 사본) · {}", pin.session_title), true);
    }
    (format!("{kind} · {} · {}", pin.session_title, fmt::when(pin.source_at)), false)
}

fn card(app: &mut App, ui: &mut egui::Ui, host: HostKey, pin: &Pin, highlighted: bool) -> egui::Response {
    let editing = app.feat.history.edit_pin.as_ref().is_some_and(|d| d.id == pin.id);
    let confirming = app.feat.history.confirm_unpin == Some(pin.id);
    let fill = if highlighted || editing { c::CONTROL } else { c::ROW };
    egui::Frame::new()
        .fill(fill)
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::same(10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 6.0;
            title(app, ui, host, pin);
            let mut job = egui::text::LayoutJob::simple(pin.text.clone(), font(12.5), c::TEXT_2, ui.available_width());
            job.wrap.max_rows = 5;
            let body = ui.add(egui::Label::new(job).wrap());
            if body.hovered() && pin.text.lines().count() > 5 {
                body.on_hover_text(pin.text.chars().take(2000).collect::<String>());
            }
            if app.feat.history.edit_pin.as_ref().is_some_and(|d| d.id == pin.id && d.field == PinField::Note) {
                edit_note(app, ui);
            } else if !pin.note.is_empty() {
                ui.add(
                    egui::Label::new(RichText::new(format!("메모: {}", pin.note)).font(font(12.0)).color(c::TEXT))
                        .wrap(),
                );
            }
            let (src, warn) = source_line(pin);
            theme::truncated(ui, src, font(11.5), if warn { c::YELLOW } else { c::TEXT_3 });
            if confirming {
                confirm(app, ui, host, pin);
                return;
            }
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing = egui::vec2(4.0, 4.0);
                if pin.source_available
                    && let Some(id) = pin.message_id
                    && fmt::small_button(ui, "원문").clicked()
                {
                    super::open_message(app, host, id, Vec::new());
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let more = widgets::icon_btn(ui, icons::ELLIPSIS, "핀 더 보기", 28.0, 24.0, 14.0);
                    crate::terminal::verify::target("pin:더 보기", more.rect);
                    egui::Popup::menu(&more).show(|ui| menu(app, ui, host, pin));
                });
            });
        })
        .response
}

fn menu(app: &mut App, ui: &mut egui::Ui, host: HostKey, pin: &Pin) {
    if widgets::menu_item(ui, icons::PENCIL, "메모").clicked() {
        begin_edit(app, host, pin, PinField::Note);
        ui.close();
    }
    let in_handover = app.feat.timeline.handover_pins.contains(&pin.id);
    let handover = if in_handover { "인계에서 빼기" } else { "인계에 담기" };
    if widgets::menu_item(ui, icons::CLIPBOARD, handover).clicked() {
        let set = &mut app.feat.timeline.handover_pins;
        if in_handover {
            set.remove(&pin.id);
        } else {
            set.insert(pin.id);
            app.toast_info("인계 자료에 담았어요. 프로젝트 타임라인의 '새 대화에 인계'에서 보내요");
        }
        ui.close();
    }
    let wiki = widgets::menu_item(ui, icons::BOOK_OPEN, "위키 초안으로");
    crate::terminal::verify::target("pin:위키 초안으로", wiki.rect);
    if wiki.clicked() {
        super::draft_to_wiki(app, host, pin.session_id, wiki_seed(pin));
        ui.close();
    }
    widgets::menu_sep(ui);
    if widgets::menu_item(ui, icons::PIN_OFF, "핀 해제").clicked() {
        if needs_confirmation(pin) {
            app.feat.history.confirm_unpin = Some(pin.id);
        } else {
            remove(app, host, pin.id, None);
        }
        ui.close();
    }
}

fn needs_confirmation(pin: &Pin) -> bool {
    match pin.kind {
        PinKind::Message => false,
        PinKind::Terminal => true,
        PinKind::Excerpt => !pin.source_available || pin.message_id.is_none(),
    }
}

/// What "위키 초안으로" quotes: the pin's words, its session and time, and a link to the
/// message while the records still have it.
fn wiki_seed(pin: &Pin) -> crate::views::wiki::Seed {
    let mut source = pin.session_title.clone();
    if pin.kind == PinKind::Terminal {
        source.push_str(" · 터미널 발췌");
    }
    source.push_str(&format!(" · {}", fmt::date_time(pin.source_at)));
    let link = pin.message_id.filter(|_| pin.source_available);
    crate::views::wiki::Seed::quote(&pin.title, &pin.text, &source, pin.session_id, link)
}

fn begin_edit(app: &mut App, host: HostKey, pin: &Pin, field: PinField) {
    if app.feat.history.edit_pin.as_ref().is_some_and(|d| d.field == PinField::Title) {
        save_edit(app);
    }
    let text = match field {
        PinField::Title => pin.title.clone(),
        PinField::Note => pin.note.clone(),
    };
    app.feat.history.edit_pin = Some(PinDraft { host, id: pin.id, field, original: text.clone(), text, focus: true });
}

#[derive(Debug, PartialEq, Eq)]
enum EditEnd {
    Keep,
    Save,
    Cancel,
}

fn title_input(ui: &mut egui::Ui, d: &mut PinDraft) -> EditEnd {
    let title = ui.add(
        egui::TextEdit::singleline(&mut d.text)
            .id_salt(("pin-title", d.host, d.id))
            .font(medium(13.0))
            .desired_width(f32::INFINITY),
    );
    if d.focus {
        title.request_focus();
        d.focus = false;
    }
    if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        EditEnd::Cancel
    } else if title.lost_focus() {
        EditEnd::Save
    } else {
        EditEnd::Keep
    }
}

fn title(app: &mut App, ui: &mut egui::Ui, host: HostKey, pin: &Pin) {
    let draft =
        app.feat.history.edit_pin.as_mut().filter(|d| d.host == host && d.id == pin.id && d.field == PinField::Title);
    if let Some(d) = draft {
        match title_input(ui, d) {
            EditEnd::Cancel => app.feat.history.edit_pin = None,
            EditEnd::Save => save_edit(app),
            EditEnd::Keep => {}
        }
    } else {
        let title = ui.add(
            egui::Label::new(RichText::new(&pin.title).font(medium(13.0)).color(c::TEXT_STRONG))
                .truncate()
                .sense(egui::Sense::click()),
        );
        crate::terminal::verify::target("pin:제목", title.rect);
        if title.on_hover_text("제목을 고쳐요").clicked() {
            begin_edit(app, host, pin, PinField::Title);
        }
    }
}

fn edit_note(app: &mut App, ui: &mut egui::Ui) {
    let Some(d) = app.feat.history.edit_pin.as_mut() else {
        return;
    };
    fmt::label(ui, "메모");
    let input =
        ui.add(egui::TextEdit::multiline(&mut d.text).font(font(12.5)).desired_rows(2).desired_width(f32::INFINITY));
    if d.focus {
        input.request_focus();
        d.focus = false;
    }
    let mut save = false;
    let mut cancel = false;
    ui.horizontal(|ui| {
        save = theme::primary_button(ui, "저장").clicked();
        cancel = theme::secondary_button(ui, "취소").clicked();
    });
    if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        cancel = true;
    }
    if cancel {
        app.feat.history.edit_pin = None;
    } else if save {
        save_edit(app);
    }
}

impl PinDraft {
    fn update(&self) -> Option<PinUpdate> {
        let text = self.text.trim();
        if text == self.original || (self.field == PinField::Title && text.is_empty()) {
            return None;
        }
        Some(match self.field {
            PinField::Title => PinUpdate { id: self.id, title: Some(text.into()), note: None },
            PinField::Note => PinUpdate { id: self.id, title: None, note: Some(text.into()) },
        })
    }
}

fn save_edit(app: &mut App) {
    let Some(d) = app.feat.history.edit_pin.take() else { return };
    let Some(update) = d.update() else { return };
    app.hub.call::<PinsUpdate>(d.host, update, move |app, res| match res {
        Ok(_) => {}
        Err(e) => {
            app.toast_error(&format!("핀을 저장하지 못했어요: {}", e.message));
            if app.feat.history.edit_pin.is_none() {
                app.feat.history.edit_pin = Some(PinDraft { focus: true, ..d });
            }
        }
    });
}

fn confirm(app: &mut App, ui: &mut egui::Ui, host: HostKey, pin: &Pin) {
    note(ui, "핀을 해제하면 이 사본과 메모는 다시 볼 수 없어요.");
    ui.horizontal(|ui| {
        if theme::danger_button(ui, "핀 해제").clicked() {
            app.feat.history.confirm_unpin = None;
            remove(app, host, pin.id, None);
        }
        if theme::secondary_button(ui, "취소").clicked() {
            app.feat.history.confirm_unpin = None;
        }
    });
}

pub(super) fn remove(app: &mut App, host: HostKey, id: PinId, message: Option<MessageId>) {
    app.hub.call::<PinsRemove>(host, ById { id }, move |app, res| {
        if let Some(message) = message {
            app.feat.history.pending_pins.remove(&(host, message));
        }
        match res {
            Ok(()) => {
                app.feat.timeline.handover_pins.remove(&id);
                if app.feat.history.edit_pin.as_ref().is_some_and(|d| d.host == host && d.id == id) {
                    app.feat.history.edit_pin = None;
                }
                app.toast_info("핀을 해제했어요");
            }
            Err(e) => app.toast_error(&format!("핀을 해제하지 못했어요: {}", e.message)),
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn pin(kind: PinKind, source_available: bool) -> Pin {
        Pin {
            id: Uuid::new_v4(),
            kind,
            title: "Saved answer".into(),
            note: "Keep this memo".into(),
            text: "The original answer".into(),
            message_id: (kind != PinKind::Terminal).then(Uuid::new_v4),
            session_id: Some(Uuid::new_v4()),
            project_id: None,
            session_title: "Conversation".into(),
            tool: None,
            source_at: runwick_core::now(),
            created_at: runwick_core::now(),
            updated_at: runwick_core::now(),
            source_available,
        }
    }

    fn draft(field: PinField, text: &str) -> PinDraft {
        PinDraft {
            host: HostKey::Local,
            id: Uuid::new_v4(),
            field,
            text: text.into(),
            original: "Original title".into(),
            focus: true,
        }
    }

    #[test]
    fn only_copies_without_a_source_require_confirmation() {
        for available in [true, false] {
            assert!(!needs_confirmation(&pin(PinKind::Message, available)));
        }
        assert!(!needs_confirmation(&pin(PinKind::Excerpt, true)));
        assert!(needs_confirmation(&pin(PinKind::Excerpt, false)));
        assert!(needs_confirmation(&pin(PinKind::Terminal, false)));
    }

    #[test]
    fn message_toggle_ignores_excerpts_and_other_messages() {
        let whole = pin(PinKind::Message, true);
        let message = whole.message_id.unwrap();
        let mut excerpt = pin(PinKind::Excerpt, true);
        excerpt.message_id = Some(message);
        let other = pin(PinKind::Message, true);
        let mut list = vec![excerpt, other];
        assert_eq!(super::super::message_pin(&list, message), None);
        list.push(whole.clone());
        assert_eq!(super::super::message_pin(&list, message), Some(whole.id));
    }

    #[test]
    fn empty_or_unchanged_title_keeps_the_previous_title() {
        for text in ["", "   \n", "Original title", " Original title "] {
            assert!(draft(PinField::Title, text).update().is_none());
        }
    }

    #[test]
    fn title_and_memo_updates_do_not_overwrite_each_other() {
        let update = draft(PinField::Title, "  Renamed  ").update().unwrap();
        assert_eq!(update.title.as_deref(), Some("Renamed"));
        assert!(update.note.is_none());
        let update = draft(PinField::Note, "  ").update().unwrap();
        assert_eq!(update.note.as_deref(), Some(""));
        assert!(update.title.is_none());
    }

    fn frame(ctx: &egui::Context, d: &mut PinDraft, events: Vec<egui::Event>) -> EditEnd {
        let mut result = EditEnd::Keep;
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(500.0, 300.0))),
            events,
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |ui| result = title_input(ui, d));
        output.textures_delta.clear();
        result
    }

    fn key(key: egui::Key) -> egui::Event {
        egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }
    }

    #[test]
    fn inline_title_enter_saves_and_escape_cancels() {
        for (key_code, expected) in [(egui::Key::Enter, EditEnd::Save), (egui::Key::Escape, EditEnd::Cancel)] {
            let ctx = egui::Context::default();
            let mut d = draft(PinField::Title, "Renamed");
            assert_eq!(frame(&ctx, &mut d, vec![]), EditEnd::Keep);
            assert_eq!(frame(&ctx, &mut d, vec![key(key_code)]), expected);
        }
    }

    #[test]
    fn inline_title_focus_loss_saves() {
        let ctx = egui::Context::default();
        let mut d = draft(PinField::Title, "Renamed");
        assert_eq!(frame(&ctx, &mut d, vec![]), EditEnd::Keep);
        let pos = egui::pos2(400.0, 250.0);
        let event = egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        };
        assert_eq!(frame(&ctx, &mut d, vec![egui::Event::PointerMoved(pos), event]), EditEnd::Keep);
        let release = egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        };
        let result = frame(&ctx, &mut d, vec![release]);
        let result = if result == EditEnd::Keep { frame(&ctx, &mut d, vec![]) } else { result };
        assert_eq!(result, EditEnd::Save);
    }
}
