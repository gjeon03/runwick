//! S04 협업 상세 (CollabPanel.dc.html): the work a session handed out, one row per worker with
//! its newest request — task, role, real CLI, workspace/branch, current action, last report —
//! plus hold/resume of new assignments, follow-ups (queue or interrupt), cancel, and results.

use super::{Place, look, look_live, overview, place, requests_of};
use crate::App;
use crate::client::HostKey;
use crate::icons;
use crate::shell::{actions, data, dialog, dialog_title, widgets};
use crate::state::{RightPanel, SettingsSection};
use crate::theme::{self, Tone, c, font, icon, medium, mono, semibold, ty};
use egui::{CornerRadius, Margin, RichText, Sense};
use runwick_core::api::activity::{self, Group};
use runwick_core::api::collab::*;
use runwick_core::api::projects::ById;
use runwick_core::api::sessions::{Phase, SessionId, SessionInfo, SessionLive, SessionsStop, Stop};

struct Row {
    worker: SessionInfo,
    live: Option<SessionLive>,
    latest: Option<Request>,
    history: Vec<Request>,
    place: Place,
    direct: bool,
    group: Option<Group>,
    /// The worker's newest answer from its conversation record, when it did not report one.
    answer: Option<String>,
}

enum Act {
    Peek(SessionId),
    Hold(bool),
    ToggleHistory,
    StopAll,
    Cancel(RequestId),
    Check(RequestId),
    Release(SessionId),
    Result(RequestId),
    Retry(RequestId),
    Follow(Option<SessionId>),
    Send { worker: SessionId, now: bool },
    SaveWay,
}

pub fn show(app: &mut App, ui: &mut egui::Ui, host: HostKey, session: SessionId) {
    let q = data::all_sessions(app, host);
    let Some(list) = q.data.clone() else {
        let text = match &q.error {
            Some(e) => format!("세션 목록을 불러오지 못했어요: {}", e.message),
            None => "불러오는 중…".into(),
        };
        pad(ui, |ui| widgets::empty_note(ui, &text));
        return;
    };
    let Some(parent) = list.iter().find(|s| s.session.id == session).cloned() else {
        pad(ui, |ui| widgets::empty_note(ui, "이 세션을 찾을 수 없어요"));
        return;
    };
    let rq = requests_of(app, host, session);
    let reqs = rq.data.clone().map(|v| v.as_ref().clone()).unwrap_or_default();
    let ov = overview(app, host).data;
    let held = ov.as_ref().is_some_and(|o| o.held_parents.contains(&session));
    let direct: Vec<SessionId> = ov.as_ref().map(|o| o.direct_workers.clone()).unwrap_or_default();

    let mut rows: Vec<Row> = list
        .iter()
        .filter(|s| s.session.parent_id == Some(session) && !s.session.archived)
        .map(|w| {
            let history: Vec<Request> = reqs.iter().filter(|r| r.worker_id == w.session.id).cloned().collect();
            let live = data::live_of(app, host, w);
            let latest = history.first().cloned();
            let group = activity::row_group(&w.session, live.as_ref(), latest.as_ref());
            // Without a report, the answer comes from the record (`collab.get` reads it).
            let unreported = latest.as_ref().filter(|r| {
                matches!(r.status, ReqStatus::Done | ReqStatus::Failed)
                    && r.result.as_ref().is_none_or(|x| x.summary.is_none())
            });
            let answer = unreported.and_then(|r| {
                let v = app.hub.query::<CollabGet>(host, &ById { id: r.id }).data?;
                v.answer.as_deref().map(|a| super::result::first_line(a, 120))
            });
            Row {
                place: place(app, host, &w.session),
                // Direct control only means something while the worker's run is alive: an ended or
                // stopped worker offers no "다시 맡기기" and is not counted as "직접 조작".
                direct: direct.contains(&w.session.id)
                    && live.as_ref().is_some_and(|l| matches!(l.phase, Phase::Running | Phase::Starting)),
                worker: w.clone(),
                live,
                latest,
                history,
                group,
                answer,
            }
        })
        .collect();
    rows.sort_by_key(|r| (r.group.unwrap_or(Group::Waiting), std::cmp::Reverse(r.worker.session.created_at)));

    let parent_live = data::live_of(app, host, &parent);
    let open = rows.iter().filter(|r| r.latest.as_ref().is_some_and(|x| x.status.is_open())).count();
    let need = rows.iter().filter(|r| matches!(r.group, Some(Group::Attention | Group::Recover))).count();
    let direct_n = rows.iter().filter(|r| r.direct).count();
    let summary = if held {
        let running = rows.iter().filter(|r| r.latest.as_ref().is_some_and(|x| x.status.holds_slot())).count();
        let queued = rows.iter().flat_map(|r| &r.history).filter(|x| x.status == ReqStatus::Queued).count();
        format!("새 배정 보류 중 · 실행 중 {running} · 미전달 대기 {queued}")
    } else {
        let mut s = format!("{} · 협업 {open}개 진행", data::status(&parent.session, parent_live.as_ref()).label);
        if need > 0 {
            s.push_str(&format!(" · 확인 필요 {need}"));
        }
        if direct_n > 0 {
            s.push_str(&format!(" · 직접 조작 {direct_n}"));
        }
        s
    };

    let mut acts: Vec<Act> = Vec::new();
    let peeked = app.feat.collab.peek.as_ref().map(|p| p.worker);
    let history_open = app.feat.collab.history_open;
    let follow_open = app.feat.collab.follow_open;

    // Header.
    egui::Frame::new().inner_margin(Margin { left: 14, right: 14, top: 14, bottom: 12 }).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = 2.0;
        theme::truncated(ui, &parent.session.title, semibold(ty::PANEL_TITLE), c::TEXT_STRONG);
        ui.label(RichText::new(&summary).font(font(12.5)).color(c::TEXT_2));
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let (glyph, text) = if held { (icons::PLAY, "배정 재개") } else { (icons::PAUSE, "새 배정 보류") };
            let b = if held {
                egui::Button::new(super::with_icon(glyph, text, medium(12.5), c::ON_EMBER, false)).fill(c::EMBER)
            } else {
                egui::Button::new(super::with_icon(glyph, text, font(12.5), c::TEXT_STRONG, false)).fill(c::CONTROL)
            };
            let resp = ui.add(b.min_size(egui::vec2(0.0, 28.0)).corner_radius(CornerRadius::same(6)));
            let tip = if held {
                "대기 중인 요청을 다시 전달해요. 전달 전에 입력권과 실행 상태를 다시 확인해요"
            } else {
                "이미 시작한 일은 계속되고, 아직 전달하지 않은 요청만 멈춰요"
            };
            if resp.on_hover_text(tip).clicked() {
                acts.push(Act::Hold(!held));
            }
            let more = widgets::icon_btn(ui, icons::ELLIPSIS, "협업 더 보기: 요청 원문, 시도 이력, 협업 중단", 28.0, 28.0, 14.0);
            egui::Popup::menu(&more).show(|ui| {
                ui.set_min_width(220.0);
                let t = if history_open { "요청 원문·시도 이력 접기" } else { "요청 원문·시도 이력 펼치기" };
                if widgets::menu_item(ui, icons::SCROLL_TEXT, t).clicked() {
                    acts.push(Act::ToggleHistory);
                }
                widgets::menu_sep(ui);
                if widgets::menu_item_danger(ui, icons::CIRCLE_STOP, "협업 중단…").clicked() {
                    acts.push(Act::StopAll);
                }
            });
        });
        if held {
            ui.add_space(6.0);
            ui.add(
                egui::Label::new(
                    RichText::new("이미 시작한 작업은 계속되고 결과는 저장됩니다. 새 전달만 멈춘 상태예요. 재개할 때 경로와 입력권을 다시 확인합니다.")
                        .font(font(12.0))
                        .color(c::TEXT_3),
                )
                .wrap(),
            );
        }
    });

    // Rows, then the footer pinned to the bottom.
    let footer_h = 40.0;
    let avail = ui.available_height() - footer_h;
    egui::ScrollArea::vertical().max_height(avail.max(80.0)).auto_shrink([false, false]).show(ui, |ui| {
        egui::Frame::new().inner_margin(Margin { left: 8, right: 8, top: 0, bottom: 8 }).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 4.0;
            if rq.data.is_none() && rq.error.is_none() && rows.is_empty() {
                widgets::empty_note(ui, "불러오는 중…");
            } else if let Some(e) = rq.error.as_ref().filter(|_| rq.data.is_none()) {
                widgets::empty_note(ui, &format!("맡긴 일을 불러오지 못했어요: {}", e.message));
            } else if rows.is_empty() {
                widgets::empty_note(
                    ui,
                    "이 세션이 맡긴 일이 아직 없어요. CLI에게 \"검토는 Codex에 맡겨\"처럼 말하면 여기에 쌓여요.",
                );
            }
            for row in &rows {
                let selected = peeked == Some(row.worker.session.id);
                let editing = follow_open == Some(row.worker.session.id);
                row_ui(app, ui, row, selected, editing, history_open, &mut acts);
            }
        });
    });

    // Footer.
    let (line, assigned) = match &ov {
        Some(o) => (activity::slots_line(o), true),
        None => ("자동 배정 한도 확인 중…".to_string(), false),
    };
    ui.painter().hline(ui.max_rect().x_range(), ui.cursor().top(), egui::Stroke::new(1.0, c::LINE));
    egui::Frame::new().inner_margin(Margin::symmetric(14, 10)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            let resp = ui.label(RichText::new(line).font(font(12.0)).color(c::TEXT_3));
            if assigned {
                resp.on_hover_text("관측할 수 없는 CLI 내장 작업은 제외한 수입니다");
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let link = ui.add(
                    egui::Label::new(RichText::new("작업 방식 저장").font(font(12.0)).color(c::TEXT_2).underline())
                        .sense(Sense::click()),
                );
                if link.on_hover_text("역할·지시문·선호 도구를 다시 쓸 수 있게 저장해요").clicked()
                {
                    acts.push(Act::SaveWay);
                }
            });
        });
    });

    for a in acts {
        apply(app, host, session, a);
    }
}

fn pad(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::new().inner_margin(Margin::same(12)).show(ui, add);
}

/// The sentence under the row title: what is happening now.
fn now_line(row: &Row) -> String {
    if row.direct {
        let waiting = row.history.iter().filter(|r| r.status == ReqStatus::Queued).count();
        return if waiting > 0 {
            format!("자동 지시 {waiting}건은 다시 맡길 때까지 대기합니다")
        } else {
            "내가 입력하고 있어요. 자동 지시는 다시 맡길 때까지 대기합니다".into()
        };
    }
    if let Some(words) = row.live.as_ref().and_then(super::attention_words) {
        return format!("{words} · CLI 화면에서 답해 주세요");
    }
    let Some(r) = &row.latest else {
        return data::status(&row.worker.session, row.live.as_ref()).label;
    };
    if let Some(reason) = &r.reason {
        return reason.clone();
    }
    match r.status {
        ReqStatus::Running => "응답 생성 중".into(),
        ReqStatus::Done | ReqStatus::Failed => r
            .result
            .as_ref()
            .and_then(|x| x.summary.as_deref())
            .map(|s| super::result::first_line(s, 120))
            .or_else(|| row.answer.clone())
            .unwrap_or_else(|| "결과 보기에서 답과 바뀐 파일을 확인하세요".into()),
        ReqStatus::Delivered => "전달했어요 · 응답을 기다리는 중".into(),
        _ => r.status.label().into(),
    }
}

fn when_line(row: &Row) -> String {
    let Some(r) = &row.latest else {
        return String::new();
    };
    if row.direct {
        return "내가 입력 중".into();
    }
    match r.status {
        ReqStatus::Queued => format!("접수 {} · {}", r.receipt(), data::ago(r.created_at)),
        ReqStatus::NeedsInput => match row.live.as_ref().and_then(|l| l.attention.as_ref()) {
            Some(a) => format!("요청 {}", data::ago(a.since)),
            None => format!("요청 {}", data::ago(r.updated_at)),
        },
        ReqStatus::Running => match row.live.as_ref().and_then(|l| l.last_output_at) {
            Some(t) => format!("마지막 출력 {}", data::ago(t)),
            None => format!("전달 {}", data::ago(r.delivered_at.unwrap_or(r.created_at))),
        },
        ReqStatus::Delivered | ReqStatus::Uncertain => {
            format!("접수 {} · 전달 {}", r.receipt(), data::ago(r.delivered_at.unwrap_or(r.created_at)))
        }
        _ => format!("{} · {}", r.receipt(), data::ago(r.finished_at.unwrap_or(r.updated_at))),
    }
}

fn row_ui(
    app: &mut App,
    ui: &mut egui::Ui,
    row: &Row,
    selected: bool,
    editing: bool,
    history: bool,
    acts: &mut Vec<Act>,
) {
    let wid = row.worker.session.id;
    let lk = match &row.latest {
        Some(r) => look_live(r, row.direct, row.live.as_ref()),
        None => {
            let st = data::status(&row.worker.session, row.live.as_ref());
            super::Look { tone: st.tone, icon: st.tone.icon(), label: st.label, pulse: st.pulse }
        }
    };
    let title = row.latest.as_ref().map(|r| r.title.clone()).unwrap_or_else(|| row.worker.session.title.clone());
    let who = format!(
        "{} · {}",
        row.worker.session.role.clone().filter(|r| !r.is_empty()).unwrap_or_else(|| "역할 없음".into()),
        row.worker.session.tool.label()
    );
    let mut where_ = row.place.line();
    if let Some(rev) = row.latest.as_ref().and_then(|r| r.start_rev.as_deref()) {
        where_.push_str(&format!(" · 기준 {}", super::short(rev)));
    }
    // The row being previewed is outlined (a fill would hide the row's buttons).
    let ring = if selected { egui::Stroke::new(1.0, c::LINE_STRONG) } else { egui::Stroke::NONE };
    egui::Frame::new().fill(c::ROW).stroke(ring).corner_radius(CornerRadius::same(8)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        let head = egui::Frame::new().inner_margin(Margin::same(10)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 9.0;
                let fg = if lk.pulse { pulse(ui, lk.tone.fg()) } else { lk.tone.fg() };
                ui.add_space(0.0);
                ui.label(icon(lk.icon, 14.0, fg)).on_hover_text(&lk.label);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.horizontal(|ui| {
                        let state_w = widgets::text_width(ui, &lk.label, medium(12.0)) + 8.0;
                        let w = (ui.available_width() - state_w).max(40.0);
                        ui.allocate_ui(egui::vec2(w, 18.0), |ui| {
                            theme::truncated(ui, &title, medium(ty::BODY), c::TEXT_STRONG);
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(RichText::new(&lk.label).font(medium(12.0)).color(lk.tone.fg()));
                        });
                    });
                    ui.label(RichText::new(&who).font(font(12.0)).color(c::TEXT_2));
                    theme::truncated(ui, &where_, mono(11.0), c::TEXT_3);
                    ui.add_space(2.0);
                    ui.add(egui::Label::new(RichText::new(now_line(row)).font(font(12.5)).color(c::TEXT)).wrap());
                    // A message the worker sent back up: shown here, never typed anywhere.
                    let message = row
                        .latest
                        .as_ref()
                        .and_then(|r| r.notes.iter().rev().find(|n| n.text.starts_with(WORKER_MESSAGE)));
                    if let Some(n) = message {
                        let line = super::result::first_line(&n.text, 140);
                        ui.add(egui::Label::new(RichText::new(line).font(font(12.0)).color(c::VIOLET)).wrap())
                            .on_hover_text(&n.text);
                    }
                    let when = when_line(row);
                    if !when.is_empty() {
                        ui.label(RichText::new(when).font(font(11.5)).color(c::TEXT_3));
                    }
                });
            });
        });
        let click = ui.interact(head.response.rect, ui.scope_id().with(("collab-row", wid)), Sense::click());
        click.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, format!("{title} 미리보기 열기")));
        if click.on_hover_text("작업자 화면 미리보기 (키 입력은 전달되지 않아요)").clicked() {
            acts.push(Act::Peek(wid));
        }
        actions_ui(ui, row, editing, acts);
        if editing {
            follow_ui(app, ui, row, acts);
        }
        if history && !row.history.is_empty() {
            history_ui(ui, row);
        }
    });
}

fn pulse(ui: &egui::Ui, color: egui::Color32) -> egui::Color32 {
    if theme::reduce_motion() {
        return color;
    }
    let t = ui.input(|i| i.time) as f32;
    ui.ctx().request_repaint_after(std::time::Duration::from_millis(80));
    color.gamma_multiply(0.55 + 0.45 * (t * std::f32::consts::PI / 1.8).cos().abs())
}

fn small(ui: &mut egui::Ui, text: &str, primary: bool) -> egui::Response {
    let b = if primary {
        egui::Button::new(RichText::new(text).font(medium(12.0)).color(c::ON_EMBER)).fill(c::EMBER)
    } else {
        egui::Button::new(RichText::new(text).font(font(12.0)).color(c::TEXT_STRONG)).fill(c::CONTROL)
    };
    ui.add(b.min_size(egui::vec2(0.0, 26.0)).corner_radius(CornerRadius::same(6)))
}

fn actions_ui(ui: &mut egui::Ui, row: &Row, editing: bool, acts: &mut Vec<Act>) {
    let wid = row.worker.session.id;
    let alive = row.live.as_ref().is_some_and(|l| matches!(l.phase, Phase::Running | Phase::Starting));
    egui::Frame::new().inner_margin(Margin { left: 34, right: 10, top: 0, bottom: 10 }).show(ui, |ui| {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
            if row.direct
                && small(ui, "다시 맡기기", false)
                    .on_hover_text("자동 지시를 다시 전달해요. 내가 한 대화는 이 시도에 연결돼요")
                    .clicked()
            {
                acts.push(Act::Release(wid));
            }
            if let Some(r) = &row.latest {
                match r.status {
                    ReqStatus::Uncertain => {
                        if small(ui, "상태 조회", true).on_hover_text("다시 보내지 않고 지금 상태만 확인해요").clicked()
                        {
                            acts.push(Act::Check(r.id));
                        }
                        if small(ui, "CLI 화면 보기", false).clicked() {
                            acts.push(Act::Peek(wid));
                        }
                    }
                    ReqStatus::Queued => {
                        if small(ui, "이 요청 취소", false).clicked() {
                            acts.push(Act::Cancel(r.id));
                        }
                    }
                    ReqStatus::Done => {
                        if small(ui, "결과 보기", false).clicked() {
                            acts.push(Act::Result(r.id));
                        }
                    }
                    ReqStatus::Failed => {
                        if small(ui, "결과 보기", false).clicked() {
                            acts.push(Act::Result(r.id));
                        }
                        if small(ui, "이어서 수정", false).on_hover_text("같은 작업자가 시도 2로 이어서 해요").clicked()
                        {
                            acts.push(Act::Retry(r.id));
                        }
                    }
                    _ => {}
                }
            }
            if alive && !row.direct && !editing && small(ui, "후속 지시…", false).clicked() {
                acts.push(Act::Follow(Some(wid)));
            }
        });
    });
}

fn follow_ui(app: &mut App, ui: &mut egui::Ui, row: &Row, acts: &mut Vec<Act>) {
    let wid = row.worker.session.id;
    let busy = row.live.as_ref().is_some_and(|l| l.busy)
        || row.latest.as_ref().is_some_and(|r| r.status.holds_slot() || r.status == ReqStatus::Queued);
    egui::Frame::new().inner_margin(Margin { left: 34, right: 10, top: 0, bottom: 10 }).show(ui, |ui| {
        let draft = app.feat.collab.drafts.entry(wid).or_default();
        ui.add(
            egui::TextEdit::multiline(draft)
                .desired_rows(3)
                .desired_width(f32::INFINITY)
                .font(font(12.5))
                .hint_text("후속 지시를 적어 주세요"),
        );
        let empty = draft.trim().is_empty();
        ui.add_space(4.0);
        if busy {
            ui.label(
                RichText::new("작업자가 응답 중이에요. 기본은 대기열에 넣고, 이전 요청은 취소하지 않아요.")
                    .font(font(12.0))
                    .color(c::TEXT_3),
            );
        }
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
            let main = if busy { "대기열에 추가" } else { "보내기" };
            if ui.add_enabled_ui(!empty, |ui| small(ui, main, false)).inner.clicked() {
                acts.push(Act::Send { worker: wid, now: false });
            }
            if busy
                && ui
                    .add_enabled_ui(!empty, |ui| small(ui, "중단하고 전송", false))
                    .inner
                    .on_hover_text("현재 응답을 멈추고 이 지시를 보내요. 사람이 입력 중이면 멈추지 않고 기다려요")
                    .clicked()
            {
                acts.push(Act::Send { worker: wid, now: true });
            }
            if small(ui, "취소", false).clicked() {
                acts.push(Act::Follow(None));
            }
        });
    });
}

fn history_ui(ui: &mut egui::Ui, row: &Row) {
    egui::Frame::new().inner_margin(Margin { left: 34, right: 10, top: 0, bottom: 10 }).show(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 6.0;
        for r in &row.history {
            let lk = look(r, false);
            ui.horizontal(|ui| {
                ui.label(RichText::new(r.receipt()).font(mono(11.0)).color(c::TEXT_3));
                ui.label(RichText::new(format!("시도 {}", r.attempt)).font(font(11.5)).color(c::TEXT_3));
                ui.label(RichText::new(&lk.label).font(font(11.5)).color(lk.tone.fg()));
                let via = match r.via {
                    Via::Launch => "시작할 때 전달",
                    Via::Typed => "입력으로 전달",
                };
                if r.delivered_at.is_some() {
                    ui.label(RichText::new(via).font(font(11.5)).color(c::TEXT_3));
                }
            });
            let text: String = r.prompt.lines().take(6).collect::<Vec<_>>().join("\n");
            egui::Frame::new().fill(c::PANEL).corner_radius(CornerRadius::same(6)).inner_margin(Margin::same(8)).show(
                ui,
                |ui| {
                    ui.set_width(ui.available_width());
                    ui.add(egui::Label::new(RichText::new(text).font(font(12.0)).color(c::TEXT_2)).wrap());
                },
            );
            for n in &r.notes {
                ui.label(RichText::new(format!("{} · {}", n.text, data::ago(n.at))).font(font(11.5)).color(c::VIOLET));
            }
        }
    });
}

fn apply(app: &mut App, host: HostKey, parent: SessionId, a: Act) {
    match a {
        Act::Peek(w) => super::peek::open(app, host, w, None),
        Act::Hold(held) => app.hub.send::<CollabHold>(host, Hold { parent_id: parent, held }),
        Act::ToggleHistory => app.feat.collab.history_open = !app.feat.collab.history_open,
        Act::StopAll => app.feat.collab.stop_all = Some((host, parent)),
        Act::Cancel(id) => app.hub.send::<CollabCancel>(host, ById { id }),
        Act::Check(id) => app.hub.call::<CollabCheck>(host, ById { id }, |app, res| match res {
            Ok(r) if r.status == ReqStatus::Uncertain => {
                app.toast_info("아직 응답이 보이지 않아요. CLI 화면에서 확인해 주세요")
            }
            Ok(r) => app.toast_info(&format!("지금 상태: {}", r.status.label())),
            Err(e) => app.toast_error(&e.message),
        }),
        Act::Release(w) => app.hub.send::<CollabDirect>(host, Direct { worker_id: w, on: false }),
        Act::Result(id) => app.ui.right = Some(RightPanel::Result { host, request: id }),
        Act::Retry(id) => super::result::retry(app, host, id, RetryMode::Continue),
        Act::Follow(w) => app.feat.collab.follow_open = w,
        Act::Send { worker, now } => {
            let prompt = app.feat.collab.drafts.get(&worker).cloned().unwrap_or_default();
            let p =
                SendTo { worker_id: worker, parent_id: Some(parent), prompt, now, key: None, source: Source::Person };
            app.hub.call::<CollabSend>(host, p, move |app, res| match res {
                Ok(r) => {
                    app.feat.collab.drafts.remove(&worker);
                    app.feat.collab.follow_open = None;
                    let receipt = r.receipt();
                    let note = r.reason.unwrap_or_else(|| "작업자가 쉬고 있으면 곧 전달해요".into());
                    app.toast_info(&format!("{receipt} 접수했어요 · {note}"));
                }
                Err(e) => app.toast_error(&e.message),
            });
        }
        Act::SaveWay => actions::open_settings(app, Some(SettingsSection::Roles)),
    }
}

/// "협업 중단": stop the workers this session started and cancel what is still queued. The
/// session itself and runs started separately keep going.
pub fn show_stop_all(app: &mut App, ctx: &egui::Context) {
    let Some((host, parent)) = app.feat.collab.stop_all else {
        return;
    };
    let list = data::all_sessions(app, host).data;
    let reqs = requests_of(app, host, parent).data;
    let running: Vec<SessionInfo> = list
        .iter()
        .flat_map(|l| l.iter())
        .filter(|s| s.session.parent_id == Some(parent))
        .filter(|s| data::status(&s.session, data::live_of(app, host, s).as_ref()).alive)
        .cloned()
        .collect();
    let queued: Vec<RequestId> =
        reqs.iter().flat_map(|l| l.iter()).filter(|r| r.status == ReqStatus::Queued).map(|r| r.id).collect();
    let mut go = false;
    let mut cancel = false;
    let ((), close) = dialog(ctx, "collab-stop-all", 460.0, |ui| {
        dialog_title(
            ui,
            "협업을 중단할까요?",
            Some(
                "이 세션이 맡긴 작업자를 멈추고 아직 전달하지 않은 요청을 취소해요. 이 세션과 따로 시작한 실행은 그대로예요. 결과와 기록은 남아요.",
            ),
        );
        if running.is_empty() && queued.is_empty() {
            ui.label(RichText::new("멈출 작업자도, 대기 중인 요청도 없어요").font(font(12.5)).color(c::TEXT_3));
        }
        for s in &running {
            ui.horizontal(|ui| {
                theme::status_dot(ui, Tone::Running, true);
                ui.label(RichText::new(&s.session.title).font(medium(12.5)).color(c::TEXT));
                ui.label(
                    RichText::new(format!("{} · 실행 종료", s.session.tool.label())).font(font(12.0)).color(c::TEXT_3),
                );
            });
        }
        if !queued.is_empty() {
            ui.label(
                RichText::new(format!("대기 중인 요청 {}개 취소", queued.len())).font(font(12.5)).color(c::TEXT_2),
            );
        }
        ui.label(
            RichText::new("실제로 멈췄는지는 각 작업자 상태에서 확인할 수 있어요.").font(font(12.0)).color(c::TEXT_3),
        );
        ui.add_space(8.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if theme::danger_button(ui, "협업 중단").clicked() {
                go = true;
            }
            if theme::secondary_button(ui, "취소").clicked() {
                cancel = true;
            }
        });
    });
    if go {
        app.hub.send::<CollabHold>(host, Hold { parent_id: parent, held: true });
        for id in queued {
            app.hub.send::<CollabCancel>(host, ById { id });
        }
        for s in running {
            app.hub.send::<SessionsStop>(host, Stop { id: s.session.id, force: false });
        }
        app.feat.collab.stop_all = None;
    } else if close || cancel {
        app.feat.collab.stop_all = None;
    }
}
