//! Pins of the current scope: snapshot text, memo, where it came from (or that the source is
//! gone), and actions — 원문, 인계에 담기, 위키 초안으로, 고치기, 핀 해제 (never deletes the
//! source).

use super::{PinDraft, ScopeKind, fmt, note, scope_ctx};
use crate::App;
use crate::client::HostKey;
use crate::theme::{self, c, font, medium};
use egui::{CornerRadius, Margin, RichText};
use runwick_core::api::projects::ById;
use runwick_core::api::records::{Pin, PinKind, PinQuery, PinUpdate, PinsList, PinsRemove, PinsUpdate};

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
            note(ui, "고정한 핀이 없어요. 대화 기록에서 '핀'을 누르거나, 세션 막대의 핀 버튼으로 마지막 응답이나 터미널 화면을 고정할 수 있어요.");
        });
        return;
    }
    let highlight = app.feat.history.highlight_pin;
    egui::ScrollArea::vertical().id_salt("history-pins").auto_shrink([false, false]).show(ui, |ui| {
        egui::Frame::new().inner_margin(Margin { left: 8, right: 8, top: 0, bottom: 8 }).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 4.0;
            for pin in list.iter() {
                let resp = card(app, ui, host, pin, highlight == Some(pin.id));
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
    let in_handover = app.feat.timeline.handover_pins.contains(&pin.id);
    let fill = if highlighted || editing { c::CONTROL } else { c::ROW };
    egui::Frame::new()
        .fill(fill)
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::same(10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 6.0;
            if editing {
                edit(app, ui);
                return;
            }
            theme::truncated(ui, pin.title.clone(), medium(13.0), c::TEXT_STRONG);
            let mut job = egui::text::LayoutJob::simple(pin.text.clone(), font(12.5), c::TEXT_2, ui.available_width());
            job.wrap.max_rows = 5;
            let body = ui.add(egui::Label::new(job).wrap());
            if body.hovered() && pin.text.lines().count() > 5 {
                body.on_hover_text(pin.text.chars().take(2000).collect::<String>());
            }
            if !pin.note.is_empty() {
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
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = egui::vec2(4.0, 4.0);
                if pin.source_available
                    && let Some(id) = pin.message_id
                    && fmt::small_button(ui, "원문").clicked()
                {
                    super::open_message(app, host, id, Vec::new());
                }
                let handover = if in_handover { "인계에서 빼기" } else { "인계에 담기" };
                if fmt::small_button(ui, handover)
                    .on_hover_text("프로젝트 타임라인의 '새 대화에 인계'에 이 핀을 담아요")
                    .clicked()
                {
                    let set = &mut app.feat.timeline.handover_pins;
                    if in_handover {
                        set.remove(&pin.id);
                    } else {
                        set.insert(pin.id);
                        app.toast_info("인계 자료에 담았어요. 프로젝트 타임라인의 '새 대화에 인계'에서 보내요");
                    }
                }
                let wiki = fmt::small_button(ui, "위키 초안으로");
                crate::terminal::verify::target("pin:위키 초안으로", wiki.rect);
                if wiki
                    .on_hover_text("이 핀을 인용한 새 위키 문서를 만들어요. 원문이 있으면 돌아가는 링크도 넣어요")
                    .clicked()
                {
                    super::draft_to_wiki(app, host, pin.session_id, wiki_seed(pin));
                }
                if fmt::small_button(ui, "고치기").clicked() {
                    app.feat.history.edit_pin = Some(PinDraft {
                        host,
                        id: pin.id,
                        title: pin.title.clone(),
                        note: pin.note.clone(),
                        focus: true,
                    });
                }
                if fmt::small_button(ui, "핀 해제").clicked() {
                    app.feat.history.confirm_unpin = Some(pin.id);
                }
            });
        })
        .response
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

fn edit(app: &mut App, ui: &mut egui::Ui) {
    let Some(d) = app.feat.history.edit_pin.as_mut() else {
        return;
    };
    fmt::label(ui, "제목");
    let title = ui.add(egui::TextEdit::singleline(&mut d.title).font(font(13.0)).desired_width(f32::INFINITY));
    if d.focus {
        title.request_focus();
        d.focus = false;
    }
    fmt::label(ui, "메모");
    ui.add(egui::TextEdit::multiline(&mut d.note).font(font(12.5)).desired_rows(2).desired_width(f32::INFINITY));
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
        let Some(d) = app.feat.history.edit_pin.clone() else {
            return;
        };
        if d.title.trim().is_empty() {
            app.toast_error("핀 제목을 비워 둘 수 없어요");
            return;
        }
        let upd = PinUpdate { id: d.id, title: Some(d.title.clone()), note: Some(d.note.clone()) };
        app.hub.call::<PinsUpdate>(d.host, upd, |app, res| match res {
            Ok(_) => {
                app.feat.history.edit_pin = None;
                app.toast(crate::state::ToastKind::Success, "핀을 저장했어요");
            }
            Err(e) => app.toast_error(&format!("핀을 저장하지 못했어요: {}", e.message)),
        });
    }
}

fn confirm(app: &mut App, ui: &mut egui::Ui, host: HostKey, pin: &Pin) {
    let text = if pin.kind == PinKind::Terminal || !pin.source_available {
        "핀을 해제하면 이 사본과 메모는 다시 볼 수 없어요."
    } else {
        "핀을 해제할까요? 원문 메시지는 기록에 그대로 남아요."
    };
    note(ui, text);
    ui.horizontal(|ui| {
        if theme::danger_button(ui, "핀 해제").clicked() {
            let id = pin.id;
            app.feat.history.confirm_unpin = None;
            app.feat.timeline.handover_pins.remove(&id);
            app.hub.call::<PinsRemove>(host, ById { id }, |app, res| match res {
                Ok(()) => app.toast_info("핀을 해제했어요"),
                Err(e) => app.toast_error(&format!("핀을 해제하지 못했어요: {}", e.message)),
            });
        }
        if theme::secondary_button(ui, "취소").clicked() {
            app.feat.history.confirm_unpin = None;
        }
    });
}
