//! One project: the resume card (last work with its three separate actions, own memo, AI
//! summary, remaining items) and the project's days.

use super::{event_row, fmt, handover, kind_chips, retarget, summary_box, summary_switch};
use crate::App;
use crate::client::HostKey;
use crate::shell::actions;
use crate::state::{TabId, WsKey};
use crate::theme::{self, Seg, c, font, medium, semibold, ty};
use chrono::Duration;
use egui::{CornerRadius, Margin, RichText, Vec2};
use runwick_core::api::projects::{ById, ProjectId};
use runwick_core::api::timeline::{
    NoteItem, Range, RangeQuery, Resume, SaveNote, TimelineRange, TimelineResume, TimelineSaveNote,
};

/// Memo being edited and the remaining-item field.
#[derive(Default)]
pub struct NoteState {
    project: Option<ProjectId>,
    memo: Option<String>,
    focus_memo: bool,
    new_item: String,
}

const SPANS: [i64; 4] = [7, 14, 30, 60];

pub fn show(app: &mut App, ui: &mut egui::Ui, project: ProjectId, tab: TabId) {
    if app.feat.timeline.note.project != Some(project) {
        app.feat.timeline.note = NoteState { project: Some(project), ..Default::default() };
    }
    let resume = app.hub.query::<TimelineResume>(HostKey::Local, &ById { id: project });
    let span = if app.feat.timeline.span == 0 { 7 } else { app.feat.timeline.span };
    let today = fmt::today();
    let q = RangeQuery {
        from: today - Duration::days(span - 1),
        to: today,
        project_id: Some(project),
        utc_offset_min: fmt::utc_offset_min(),
    };
    let range = app.hub.query::<TimelineRange>(HostKey::Local, &q);
    egui::ScrollArea::vertical().id_salt(("timeline-project", tab)).auto_shrink([false, false]).show(ui, |ui| {
        let w = ui.available_width();
        let col = (w - 56.0).clamp(300.0, 880.0);
        let left = ((w - col) / 2.0).max(16.0);
        egui::Frame::new().inner_margin(Margin { left: left as i8, right: 0, top: 24, bottom: 28 }).show(ui, |ui| {
            ui.set_max_width(col);
            ui.set_min_width(col);
            ui.spacing_mut().item_spacing.y = 24.0;
            match (&resume.data, &resume.error) {
                (Some(r), _) => {
                    resume_card(app, ui, r);
                    if app.feat.timeline.handover.as_ref().is_some_and(|d| d.project == project) {
                        handover::show(app, ui, r);
                    }
                }
                (None, Some(e)) => {
                    theme::chip(ui, theme::Tone::Recover, "이어하기 정보를 읽지 못했어요");
                    ui.label(RichText::new(&e.message).font(font(ty::SMALL)).color(c::TEXT_2));
                }
                (None, None) => {
                    ui.label(RichText::new("이어하기 정보를 불러오는 중…").font(font(ty::BODY)).color(c::TEXT_3));
                }
            }
            let name = resume.data.as_ref().map(|r| r.project_name.clone()).unwrap_or_default();
            days(app, ui, &name, range.data.as_deref(), range.error.map(|e| e.message), span, tab);
        });
    });
}

/// A button that may be the area's one ember action, or disabled with a reason.
fn action(ui: &mut egui::Ui, text: &str, enabled: bool, primary: bool, why_not: &str) -> bool {
    let (fill, fg) = if primary && enabled { (c::EMBER, c::ON_EMBER) } else { (c::CONTROL, c::TEXT) };
    let b =
        egui::Button::new(RichText::new(text).font(medium(12.5)).color(fg)).fill(fill).min_size(Vec2::new(0.0, 32.0));
    ui.add_enabled(enabled, b).on_disabled_hover_text(why_not).clicked()
}

fn resume_card(app: &mut App, ui: &mut egui::Ui, r: &Resume) {
    let mut act: Option<u8> = None;
    egui::Frame::new().fill(c::PANEL_2).corner_radius(CornerRadius::same(14)).inner_margin(Margin::same(18)).show(
        ui,
        |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 14.0;
            ui.horizontal_wrapped(|ui| {
                ui.vertical(|ui| {
                    ui.set_max_width((ui.available_width() - 380.0).max(240.0));
                    ui.spacing_mut().item_spacing.y = 3.0;
                    match &r.last {
                        Some(l) => {
                            let local = fmt::local(l.last_at);
                            let sub = format!(
                                "{} 마지막 작업 · {} · {}",
                                fmt::day_label(local.date_naive()),
                                local.format("%H:%M"),
                                l.session.tool.label()
                            );
                            ui.label(RichText::new(sub).font(font(12.0)).color(c::TEXT_3));
                            theme::truncated(ui, l.session.title.clone(), semibold(20.0), c::TEXT_STRONG);
                        }
                        None => {
                            ui.label(RichText::new("이어하기").font(font(12.0)).color(c::TEXT_3));
                            ui.label(
                                RichText::new("아직 이 프로젝트에서 시작한 세션이 없어요")
                                    .font(semibold(17.0))
                                    .color(c::TEXT_STRONG),
                            );
                        }
                    }
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    let running = r.last.as_ref().is_some_and(|l| l.running);
                    let resumable = r.last.as_ref().is_some_and(|l| {
                        !l.running
                            && l.session.tool.is_agent()
                            && l.session.provider_conv_id.is_some()
                            && l.workspace_exists
                    });
                    let handover_primary = !running && !resumable;
                    // Right to left: the last added is leftmost.
                    if action(ui, "새 대화에 인계", true, handover_primary, "") {
                        act = Some(2);
                    }
                    let why_resume = if running {
                        "실행 중이에요. '살아 있는 실행 열기'를 쓰세요"
                    } else if r.last.as_ref().is_some_and(|l| !l.workspace_exists) {
                        "작업 폴더를 찾을 수 없어 재개할 수 없어요"
                    } else {
                        "재개할 수 있는 대화 ID가 없어요"
                    };
                    if action(ui, "같은 대화 재개", resumable, resumable, why_resume) {
                        act = Some(1);
                    }
                    if action(ui, "살아 있는 실행 열기", running, running, "지금 실행 중이 아니에요")
                    {
                        act = Some(0);
                    }
                });
            });
            if !r.root_exists {
                ui.horizontal_wrapped(|ui| {
                    theme::chip(
                        ui,
                        theme::Tone::Check,
                        "프로젝트 폴더를 찾을 수 없어요 · 기록은 그대로 읽을 수 있어요",
                    );
                });
            }
            let w = ui.available_width();
            let cols = if w >= 3.0 * 240.0 + 32.0 { 3 } else { 1 };
            let col_w = if cols == 3 { (w - 32.0) / 3.0 } else { w };
            let draw = |app: &mut App, ui: &mut egui::Ui, i: usize| match i {
                0 => memo(app, ui, r),
                1 => summary(app, ui, r),
                _ => items(app, ui, r),
            };
            if cols == 3 {
                ui.horizontal_top(|ui| {
                    ui.spacing_mut().item_spacing.x = 16.0;
                    for i in 0..3 {
                        ui.allocate_ui(Vec2::new(col_w, 0.0), |ui| {
                            ui.set_width(col_w);
                            ui.vertical(|ui| draw(app, ui, i));
                        });
                    }
                });
            } else {
                for i in 0..3 {
                    ui.vertical(|ui| draw(app, ui, i));
                }
            }
        },
    );
    match (act, &r.last) {
        (Some(2), _) => handover::open(app, r),
        (Some(a), Some(l)) => {
            let ws = WsKey { host: HostKey::Local, id: l.session.workspace_id };
            if a == 1 {
                actions::restart_session(app, HostKey::Local, l.session.id, true);
            }
            actions::open_session(app, r.project_id, ws, l.session.id, l.session.hidden);
            crate::terminal::focus(app, HostKey::Local, l.session.id);
        }
        _ => {}
    }
}

fn save(app: &mut App, project: ProjectId, memo: String, items: Vec<NoteItem>) {
    app.hub.call::<TimelineSaveNote>(HostKey::Local, SaveNote { project_id: project, memo, items }, |app, res| {
        if let Err(e) = res {
            app.toast_error(&format!("메모를 저장하지 못했어요: {}", e.message));
        }
    });
}

fn memo(app: &mut App, ui: &mut egui::Ui, r: &Resume) {
    ui.spacing_mut().item_spacing.y = 6.0;
    ui.horizontal(|ui| {
        fmt::label(ui, "내 메모");
        if app.feat.timeline.note.memo.is_none()
            && ui
                .add(egui::Button::new(RichText::new("고치기").font(font(11.5)).color(c::TEXT_2)).frame(false).small())
                .clicked()
        {
            app.feat.timeline.note.memo = Some(r.note.memo.clone());
            app.feat.timeline.note.focus_memo = true;
        }
    });
    let mut done = None;
    let note = &mut app.feat.timeline.note;
    match note.memo.as_mut() {
        Some(draft) => {
            let resp = ui.add(
                egui::TextEdit::multiline(draft)
                    .font(font(13.0))
                    .desired_rows(4)
                    .desired_width(f32::INFINITY)
                    .hint_text("다음에 이어서 할 때 볼 메모"),
            );
            if note.focus_memo {
                resp.request_focus();
                note.focus_memo = false;
            }
            ui.horizontal(|ui| {
                if theme::secondary_button(ui, "저장").clicked() {
                    done = Some(true);
                }
                if ui.add(egui::Button::new(RichText::new("취소").font(font(12.0))).frame(false)).clicked() {
                    done = Some(false);
                }
            });
        }
        None => {
            let text = if r.note.memo.trim().is_empty() {
                "메모 없음 · '고치기'로 남겨 두세요"
            } else {
                r.note.memo.as_str()
            };
            let color = if r.note.memo.trim().is_empty() { c::TEXT_3 } else { c::TEXT_STRONG };
            ui.add(egui::Label::new(RichText::new(text).font(font(13.0)).color(color)).wrap());
            if let Some(t) = r.note.updated_at {
                ui.label(RichText::new(format!("{} 저장", fmt::when_full(t))).font(font(11.0)).color(c::TEXT_3));
            }
        }
    }
    match done {
        Some(true) => {
            let memo = app.feat.timeline.note.memo.take().unwrap_or_default();
            save(app, r.project_id, memo, r.note.items.clone());
        }
        Some(false) => app.feat.timeline.note.memo = None,
        None => {}
    }
}

fn summary(app: &mut App, ui: &mut egui::Ui, r: &Resume) {
    ui.spacing_mut().item_spacing.y = 6.0;
    fmt::label(ui, "요약");
    match &r.summary {
        Some(s) => {
            ui.label(RichText::new(format!("{} 기록 기준", fmt::day_label(s.day))).font(font(11.5)).color(c::TEXT_3));
            summary_box(ui, s);
        }
        None => {
            let today = fmt::today();
            super::jobs::no_summary(app, ui, r.project_id, today, today);
        }
    }
}

fn items(app: &mut App, ui: &mut egui::Ui, r: &Resume) {
    ui.spacing_mut().item_spacing.y = 4.0;
    fmt::label(ui, "남은 일");
    let mut changed: Option<Vec<NoteItem>> = None;
    if r.note.items.is_empty() {
        ui.label(RichText::new("적어 둔 일이 없어요").font(font(12.5)).color(c::TEXT_3));
    }
    for (i, it) in r.note.items.iter().enumerate() {
        ui.horizontal(|ui| {
            let mut done = it.done;
            if ui.checkbox(&mut done, "").on_hover_text(if done { "다시 남은 일로" } else { "끝냄으로 표시" }).changed()
            {
                let mut v = r.note.items.clone();
                v[i].done = done;
                changed = Some(v);
            }
            let color = if it.done { c::TEXT_3 } else { c::TEXT };
            let mut text = RichText::new(&it.text).font(font(12.5)).color(color);
            if it.done {
                text = text.strikethrough();
            }
            ui.add(egui::Label::new(text).wrap());
            if crate::shell::widgets::icon_btn(ui, crate::icons::X, "남은 일에서 지우기", 20.0, 20.0, 11.0).clicked()
            {
                let mut v = r.note.items.clone();
                v.remove(i);
                changed = Some(v);
            }
        });
    }
    ui.horizontal(|ui| {
        let field = ui.add(
            egui::TextEdit::singleline(&mut app.feat.timeline.note.new_item)
                .font(font(12.5))
                .hint_text("남은 일 추가")
                .desired_width(ui.available_width() - 48.0),
        );
        let enter = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        let add =
            ui.add_enabled(!app.feat.timeline.note.new_item.trim().is_empty(), egui::Button::new("추가")).clicked();
        if (enter || add) && !app.feat.timeline.note.new_item.trim().is_empty() {
            let mut v = r.note.items.clone();
            v.push(NoteItem {
                text: std::mem::take(&mut app.feat.timeline.note.new_item).trim().to_string(),
                done: false,
            });
            changed = Some(v);
        }
    });
    if let Some(v) = changed {
        save(app, r.project_id, r.note.memo.clone(), v);
    }
}

fn days(
    app: &mut App,
    ui: &mut egui::Ui,
    name: &str,
    range: Option<&Range>,
    error: Option<String>,
    span: i64,
    tab: TabId,
) {
    ui.spacing_mut().item_spacing.y = 12.0;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = Vec2::new(6.0, 8.0);
        ui.label(RichText::new(format!("{name} 타임라인")).font(semibold(14.0)).color(c::TEXT_STRONG));
        ui.add_space(8.0);
        let short: String = name.chars().take(24).collect();
        if theme::segmented(ui, &[Seg::text("모든 프로젝트"), Seg::text(&short)], 1, 22.0) == Some(0) {
            retarget(app, tab, None);
        }
        let period = fmt::chip_button(ui, &format!("기간: {span}일"), span != 7, true);
        egui::Popup::menu(&period).show(|ui| {
            for s in SPANS {
                if ui.selectable_label(span == s, format!("{s}일")).clicked() {
                    app.feat.timeline.span = s;
                    ui.close();
                }
            }
        });
        kind_chips(app, ui);
        summary_switch(app, ui);
    });
    let Some(range) = range else {
        match error {
            Some(e) => {
                theme::chip(ui, theme::Tone::Recover, "타임라인을 읽지 못했어요");
                ui.label(RichText::new(e).font(font(ty::SMALL)).color(c::TEXT_2));
            }
            None => {
                ui.label(RichText::new("기록을 모으는 중…").font(font(ty::BODY)).color(c::TEXT_3));
            }
        }
        return;
    };
    if let Some(note) = range.projects.first().and_then(|p| p.commits_note.clone()) {
        ui.horizontal_wrapped(|ui| {
            theme::chip(ui, theme::Tone::Check, &note);
        });
    }
    let mut days: Vec<_> = range.days.iter().collect();
    days.sort_by_key(|d| std::cmp::Reverse(d.day));
    if days.is_empty() {
        ui.label(RichText::new(format!("최근 {span}일에는 남은 기록이 없어요.")).font(font(ty::BODY)).color(c::TEXT_3));
        return;
    }
    let off = app.feat.timeline.kinds_off.clone();
    let show_sum = !app.feat.timeline.hide_summaries;
    for d in days {
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 16.0;
            ui.allocate_ui(Vec2::new(72.0, 0.0), |ui| {
                ui.set_width(72.0);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    ui.label(RichText::new(fmt::day_label(d.day)).font(medium(13.0)).color(c::TEXT_STRONG));
                    let sub = if d.summary.is_none() && show_sum {
                        "요약 없음".to_string()
                    } else {
                        format!(
                            "{}월 {}일 {}",
                            chrono::Datelike::month(&d.day),
                            chrono::Datelike::day(&d.day),
                            fmt::weekday(d.day)
                        )
                    };
                    ui.label(RichText::new(sub).font(font(11.5)).color(c::TEXT_3));
                    ui.label(RichText::new(format!("기록 {}건", d.count)).font(font(11.5)).color(c::TEXT_3));
                });
            });
            let rest = ui.available_width();
            ui.vertical(|ui| {
                ui.set_width(rest);
                let (top, _) = ui.allocate_exact_size(Vec2::new(rest, 0.0), egui::Sense::hover());
                ui.spacing_mut().item_spacing.y = 2.0;
                if show_sum && let Some(s) = &d.summary {
                    summary_box(ui, s);
                    ui.add_space(4.0);
                }
                let shown: Vec<_> = d.events.iter().filter(|e| !off.contains(&e.kind)).collect();
                for e in &shown {
                    event_row(app, ui, e, d.project_id, true);
                }
                if shown.is_empty() {
                    ui.label(RichText::new("선택한 종류의 기록이 없어요.").font(font(12.0)).color(c::TEXT_3));
                }
                let bottom = ui.min_rect().bottom();
                ui.painter().vline(top.left() - 8.0, top.top()..=bottom, egui::Stroke::new(1.0, c::CONTROL_HOVER));
            });
        });
    }
}
