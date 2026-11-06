//! Settings › 자동 작업 (Automation.dc.html): per-project opt-in for AI wiki tidying and
//! timeline summaries, the battery policy and the current power source, and recent automatic
//! and requested jobs with their reason, status, result and stop/retry. Automatic work only
//! ever leaves proposals; nothing is applied to documents by itself.

use crate::App;
use crate::client::HostKey;
use crate::icons;
use crate::shell::actions;
use crate::state::{RightPanel, TabKind};
use crate::theme::{self, Tone, c, font, icon, medium, semibold, ty};
use crate::views::history::fmt;
use egui::{Align2, CornerRadius, Pos2, Rect, RichText, Sense, Vec2};
use runwick_core::api::jobs::*;

#[derive(Default)]
pub struct State {}

const HOST: HostKey = HostKey::Local;

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    ui.spacing_mut().item_spacing.y = 8.0;
    ui.label(RichText::new("자동 작업").font(semibold(ty::TITLE)).color(c::TEXT_STRONG));
    ui.label(
        RichText::new("대화 저장과 키워드 색인은 AI 없이 항상 동작해요. AI가 하는 정리는 프로젝트별로 직접 켭니다.")
            .font(font(ty::BODY))
            .color(c::TEXT_2),
    );
    ui.add_space(10.0);
    power_card(app, ui);
    ui.add_space(16.0);
    projects(app, ui);
    ui.add_space(18.0);
    jobs(app, ui);
}

fn power_card(app: &mut App, ui: &mut egui::Ui) {
    let ov = app.hub.query::<JobsOverview>(HOST, &());
    let (tone, glyph, title, detail) = match ov.data.as_ref().map(|o| o.power) {
        Some(PowerSource::Battery) => (
            Tone::Check,
            icons::BATTERY_LOW,
            "배터리 사용 중이라 새 자동 분석을 멈췄어요.",
            "전원을 연결하면 다시 시작합니다. 직접 누른 요청과 CLI 작업은 계속 실행돼요.",
        ),
        Some(PowerSource::Ac) => (
            Tone::Running,
            icons::PLUG,
            "전원 연결됨.",
            "켜 둔 프로젝트는 대화 기록이 쌓이고 작업이 한가해지면 정리해요.",
        ),
        Some(PowerSource::Unknown) => (
            Tone::Waiting,
            icons::CIRCLE_HELP,
            "전원 상태 확인 불가.",
            "이 컴퓨터에서는 전원 상태를 읽을 수 없어 전원이 연결된 것으로 보고 진행해요.",
        ),
        None => (Tone::Waiting, icons::HOURGLASS, "전원 상태를 확인하는 중…", ""),
    };
    theme::card_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.label(icon(glyph, 17.0, tone.fg()));
            ui.vertical(|ui| {
                ui.set_width(ui.available_width() - 110.0);
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    ui.label(RichText::new(title).font(medium(ty::BODY)).color(c::TEXT_STRONG));
                    ui.label(RichText::new(detail).font(font(ty::BODY)).color(c::TEXT_2));
                });
                ui.label(
                    RichText::new("배터리에서는 새 자동 분석을 시작하지 않아요. 이미 시작한 정리는 그 결과까지 저장하고, 다음 정리를 보류해요.")
                        .font(font(ty::SMALL))
                        .color(c::TEXT_3),
                );
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if theme::secondary_button(ui, "활동에서 보기").clicked() {
                    app.ui.page = crate::state::Page::Work;
                    app.ui.right = Some(RightPanel::Activity);
                }
            });
        });
    });
}

/// Switch like the design's: ember when on, with the accessible name and state.
fn switch(ui: &mut egui::Ui, on: bool, name: &str, enabled: bool) -> egui::Response {
    let (rect, resp) =
        ui.allocate_exact_size(Vec2::new(34.0, 20.0), if enabled { Sense::click() } else { Sense::hover() });
    let n = name.to_string();
    resp.widget_info(move || egui::WidgetInfo::selected(egui::Role::Switch, enabled, on, &n));
    let bg = if on { c::EMBER } else { c::LINE_STRONG };
    let dot = if on { c::ON_EMBER } else { c::TEXT_2 };
    let alpha = if enabled { 1.0 } else { 0.5 };
    ui.painter().rect_filled(rect, CornerRadius::same(10), bg.gamma_multiply(alpha));
    let x = if on { rect.right() - 10.0 } else { rect.left() + 10.0 };
    ui.painter().circle_filled(Pos2::new(x, rect.center().y), 8.0, dot.gamma_multiply(alpha));
    theme::focus_ring(ui, &resp, CornerRadius::same(10));
    resp
}

fn projects(app: &mut App, ui: &mut egui::Ui) {
    ui.label(RichText::new("프로젝트별 자동 분석").font(semibold(14.0)).color(c::TEXT_STRONG));
    let rows = app.hub.query::<AutomationList>(HOST, &());
    let connected = app.hub.state(HOST).is_connected();
    theme::card_frame().inner_margin(egui::Margin::same(0)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        let col = 110.0;
        let head = |ui: &mut egui::Ui| {
            let (r, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 38.0), Sense::hover());
            let p = ui.painter();
            p.text(Pos2::new(r.left() + 14.0, r.center().y), Align2::LEFT_CENTER, "프로젝트", medium(12.0), c::TEXT_3);
            p.text(
                Pos2::new(r.right() - 14.0 - col * 2.0, r.center().y),
                Align2::LEFT_CENTER,
                "위키 정리",
                medium(12.0),
                c::TEXT_3,
            );
            p.text(
                Pos2::new(r.right() - 14.0 - col, r.center().y),
                Align2::LEFT_CENTER,
                "타임라인 요약",
                medium(12.0),
                c::TEXT_3,
            );
        };
        head(ui);
        let Some(list) = rows.data.as_deref() else {
            pad(ui, |ui| match &rows.error {
                Some(e) => error(ui, &format!("설정을 불러오지 못했어요: {}", e.message)),
                None => loading(ui),
            });
            return;
        };
        if list.is_empty() {
            pad(ui, |ui| {
                ui.label(
                    RichText::new("열린 프로젝트가 없어요. 폴더를 열면 여기에서 켤 수 있어요.")
                        .font(font(12.5))
                        .color(c::TEXT_3),
                );
            });
        }
        for row in list {
            let (r, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 48.0), Sense::hover());
            ui.painter().hline(r.x_range(), r.top(), egui::Stroke::new(1.0, c::LINE));
            let name_w = r.width() - col * 2.0 - 40.0;
            let name_r = Rect::from_min_size(Pos2::new(r.left() + 14.0, r.top() + 6.0), Vec2::new(name_w, 36.0));
            ui.scope_builder(egui::UiBuilder::new().max_rect(name_r), |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                theme::truncated(ui, &row.project_name, medium(ty::BODY), c::TEXT_STRONG);
                let last = match (row.last_wiki_at, row.last_timeline_at) {
                    (Some(a), Some(b)) => format!("마지막 정리 {}", fmt::when(a.max(b))),
                    (Some(a), None) | (None, Some(a)) => {
                        format!("마지막 정리 {}", fmt::when(a))
                    }
                    (None, None) => "아직 정리한 적 없어요".into(),
                };
                ui.label(RichText::new(last).font(font(11.5)).color(c::TEXT_3));
            });
            for (i, (on, what)) in [(row.wiki, "위키 정리"), (row.timeline, "타임라인 요약")].into_iter().enumerate()
            {
                let x = r.right() - 14.0 - col * (2 - i) as f32;
                let sr = Rect::from_min_size(Pos2::new(x, r.center().y - 10.0), Vec2::new(34.0, 20.0));
                let resp = ui.scope_builder(egui::UiBuilder::new().max_rect(sr), |ui| {
                    switch(ui, on, &format!("{} {what}", row.project_name), connected)
                });
                if resp.inner.clicked() {
                    let mut set = SetAutomation { project_id: row.project_id, ..Default::default() };
                    if i == 0 {
                        set.wiki = Some(!on);
                    } else {
                        set.timeline = Some(!on);
                    }
                    app.hub.call::<AutomationSet>(HOST, set, |app, res| {
                        if let Err(e) = res {
                            app.toast_error(&e.message);
                        }
                    });
                }
            }
        }
    });
    ui.add_space(4.0);
    for line in [
        "자동 반영은 제공하지 않아요. AI 정리는 항상 제안으로 남고, 문서에 넣는 건 직접 해요.",
        "켜 둔 프로젝트는 새 대화 기록이 20건 넘게 쌓이고, 15분 넘게 한가하고, 전원에 연결돼 있을 때 정리해요. 위키 정리는 6시간에 한 번까지이고, 검토하지 않은 제안이 있으면 기다려요.",
        "구독 사용량을 씁니다. 설치된 Claude Code(없으면 Codex)를 내 계정으로 실행하고, 비용으로 바꿔 표시하지 않아요. 의미 검색(임베딩)은 아직 제공하지 않아요.",
    ] {
        ui.label(RichText::new(line).font(font(ty::SMALL)).color(c::TEXT_3));
    }
}

fn job_status(j: &Job) -> (Tone, String) {
    match j.status {
        JobStatus::Queued if j.retry_at.is_some() => {
            (Tone::Check, format!("다시 시도 대기 · {}", j.retry_at.map(fmt::hm).unwrap_or_default()))
        }
        JobStatus::Queued => (Tone::Waiting, "실행 대기".into()),
        JobStatus::Running => (Tone::Running, "실행 중".into()),
        JobStatus::Held => (Tone::Check, format!("보류 · {}", j.held_reason.as_deref().unwrap_or("조건 대기"))),
        JobStatus::Done => (Tone::Running, format!("성공 · {}", fmt::when_full(j.finished_at.unwrap_or(j.created_at)))),
        JobStatus::Failed => {
            (Tone::Recover, format!("실패 · {}", fmt::when_full(j.finished_at.unwrap_or(j.created_at))))
        }
        JobStatus::Stopped => {
            (Tone::Waiting, format!("멈춤 · {}", fmt::when_full(j.finished_at.unwrap_or(j.created_at))))
        }
    }
}

fn jobs(app: &mut App, ui: &mut egui::Ui) {
    ui.label(RichText::new("최근 자동 작업").font(semibold(14.0)).color(c::TEXT_STRONG));
    let q = app.hub.query::<JobsList>(HOST, &JobQuery { project_id: None, active_only: false, limit: 20 });
    let ov = app.hub.query::<JobsOverview>(HOST, &()).data;
    theme::card_frame().inner_margin(egui::Margin::same(0)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        let Some(list) = q.data.as_deref() else {
            pad(ui, |ui| match &q.error {
                Some(e) => error(ui, &format!("작업 목록을 불러오지 못했어요: {}", e.message)),
                None => loading(ui),
            });
            return;
        };
        if list.is_empty() {
            pad(ui, |ui| {
                ui.label(
                    RichText::new("아직 자동 작업이 없어요. 프로젝트에서 켜거나 위키·타임라인의 ‘지금 정리’를 누르면 여기에 보여요.")
                        .font(font(12.5))
                        .color(c::TEXT_3),
                );
            });
        }
        for (i, j) in list.iter().enumerate() {
            if i > 0 {
                ui.painter().hline(ui.max_rect().x_range(), ui.cursor().top(), egui::Stroke::new(1.0, c::LINE));
            }
            egui::Frame::new().inner_margin(egui::Margin { left: 14, right: 10, top: 9, bottom: 9 }).show(ui, |ui| {
                ui.set_width(ui.available_width());
                job_row(app, ui, j);
            });
        }
    });
    ui.add_space(4.0);
    let limits = ov
        .map(|o| format!("동시에 {}개까지 실행하고, 그중 자동 작업은 {}개까지예요.", o.max_running, o.max_auto))
        .unwrap_or_default();
    ui.label(
        RichText::new(format!("{limits} 실패하면 2분, 10분 뒤 두 번까지 다시 시도해요. 이 컴퓨터의 Runwick이 다시 시작되면 하던 작업은 멈추고 저절로 다시 실행하지 않아요."))
            .font(font(ty::SMALL))
            .color(c::TEXT_3),
    );
}

fn job_row(app: &mut App, ui: &mut egui::Ui, j: &Job) {
    let (tone, status) = job_status(j);
    let project = j.project_name.clone().unwrap_or_else(|| "프로젝트".into());
    ui.horizontal(|ui| {
        let w = ui.available_width();
        ui.vertical(|ui| {
            ui.set_width((w - 300.0).max(160.0));
            ui.spacing_mut().item_spacing.y = 1.0;
            theme::truncated(ui, format!("{} · {project}", j.kind.label()), medium(ty::BODY), c::TEXT_STRONG);
            let origin = match j.trigger {
                Trigger::Auto => "자동",
                Trigger::Requested => "직접 요청",
            };
            let why = if j.reason.starts_with(origin) { j.reason.clone() } else { format!("{origin} · {}", j.reason) };
            theme::truncated(ui, why, font(12.0), c::TEXT_3);
            let detail = match (&j.result, &j.error, j.status) {
                (Some(r), _, _) => Some((r.clone(), c::TEXT_2)),
                (_, Some(e), JobStatus::Failed | JobStatus::Queued) => Some((e.clone(), c::ROSE)),
                (_, Some(e), JobStatus::Stopped) => Some((e.clone(), c::TEXT_2)),
                _ => None,
            };
            if let Some((text, col)) = detail {
                theme::truncated(ui, text, font(12.0), col);
            }
            let mut meta = Vec::new();
            if let Some(p) = &j.provider {
                meta.push(match &j.model {
                    Some(m) => format!("{p} · {m}"),
                    None => p.clone(),
                });
            }
            if let Some(ms) = j.duration_ms {
                meta.push(format!("{:.0}초", ms as f64 / 1000.0));
            }
            if j.attempts > 1 {
                meta.push(format!("{}번째 시도", j.attempts));
            }
            if !meta.is_empty() {
                ui.label(RichText::new(meta.join(" · ")).font(font(11.5)).color(c::TEXT_3));
            }
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            job_actions(app, ui, j);
            ui.add_space(6.0);
            ui.vertical(|ui| {
                ui.set_width(170.0);
                theme::chip(ui, tone, &status).on_hover_text(&status);
            });
        });
    });
}

fn job_actions(app: &mut App, ui: &mut egui::Ui, j: &Job) {
    let id = j.id;
    let done = |app: &mut App, res: Result<Job, runwick_core::RpcError>| {
        if let Err(e) = res {
            app.toast_error(&e.message);
        }
    };
    match j.status {
        JobStatus::Held => {
            if fmt::small_button(ui, "지금 정리")
                .on_hover_text("배터리에서도 바로 실행해요 · 구독 사용량을 씁니다")
                .clicked()
            {
                let req = RequestJob { kind: j.kind, project_id: j.project_id, target: j.target.clone() };
                app.hub.call::<JobsRequest>(HOST, req, done);
            }
            if fmt::small_button(ui, "멈추기").clicked() {
                app.hub.call::<JobsStop>(HOST, JobRef { id }, done);
            }
        }
        JobStatus::Queued | JobStatus::Running => {
            if fmt::small_button(ui, "멈추기").clicked() {
                app.hub.call::<JobsStop>(HOST, JobRef { id }, done);
            }
        }
        JobStatus::Failed | JobStatus::Stopped => {
            if fmt::small_button(ui, "다시 실행").on_hover_text("구독 사용량을 씁니다").clicked() {
                app.hub.call::<JobsRetry>(HOST, JobRef { id }, done);
            }
        }
        JobStatus::Done => {
            if fmt::small_button(ui, "결과 보기").clicked() {
                open_result(app, j);
            }
        }
    }
}

/// Where a finished job's result lives: the project's wiki or timeline.
fn open_result(app: &mut App, j: &Job) {
    let project = j.project_id;
    let ws = crate::shell::data::workspaces(app)
        .data
        .and_then(|l| l.iter().find(|w| w.project_id == project && !w.archived).map(crate::shell::data::ws_key));
    let Some(ws) = ws else {
        app.toast_info("이 프로젝트의 작업 공간을 찾을 수 없어요");
        return;
    };
    actions::select_workspace(app, project, ws);
    app.ui.page = crate::state::Page::Work;
    let kind = match j.kind {
        JobKind::TimelineSummary => TabKind::Timeline { project: Some(project) },
        JobKind::WikiProposals | JobKind::WikiDraft => TabKind::Wiki { project, page: None },
    };
    actions::open_center_tab(app, kind);
}

fn pad(ui: &mut egui::Ui, f: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::new().inner_margin(egui::Margin { left: 14, right: 14, top: 4, bottom: 14 }).show(ui, f);
}

fn loading(ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        ui.add(egui::Spinner::new().size(12.0).color(c::TEXT_3));
        ui.label(RichText::new("불러오는 중…").font(font(12.5)).color(c::TEXT_3));
    });
}

fn error(ui: &mut egui::Ui, text: &str) {
    ui.horizontal_wrapped(|ui| {
        ui.label(icon(icons::CIRCLE_ALERT, 13.0, c::ROSE));
        ui.label(RichText::new(text).font(font(12.5)).color(c::ROSE));
    });
}
