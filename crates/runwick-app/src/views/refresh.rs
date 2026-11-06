//! Session refresh (S12, Refresh.dc.html / RefreshPartial.dc.html): what changed since the run
//! started (keys only, never values), what stays, when to apply, then the steps and checks of the
//! refresh. Also the result card shown after the dialog is closed ([`show_overlay`]), the
//! "적용 대기" list used by the settings pages ([`pending_list`]) and [`pending`] for the
//! session bar's refresh dot.

use crate::App;
use crate::client::HostKey;
use crate::icons;
use crate::shell::{actions, dialog, dialog_title, widgets};
use crate::state::{Modal, WsKey};
use crate::theme::{self, Tone, c, font, icon, medium, mono, semibold, ty};
use egui::{Align2, CornerRadius, Pos2, RichText, Sense, Stroke, Vec2};
use runwick_core::api::env::{EnvChange, EnvChangeKind};
use runwick_core::api::mcp::{McpChange, McpChangeKind};
use runwick_core::api::refresh::*;
use runwick_core::api::sessions::{ScreenQuery, SessionId, SessionsScreenText, Tool};

#[derive(Default)]
pub struct State {
    /// Choice of "when" in the open dialog, per session.
    when: Option<(SessionId, When)>,
    /// A start request is in flight for this session.
    starting: Option<SessionId>,
    /// Refreshes started from this window whose result has not been closed yet.
    watched: Vec<(HostKey, SessionId)>,
}

/// The session's run started with older settings (or a refresh is under way): the session bar
/// shows a dot on its refresh button.
pub fn pending(app: &mut App, host: HostKey, session: SessionId) -> bool {
    app.hub.query::<RefreshPending>(host, &()).data.is_some_and(|l| l.iter().any(|p| p.session == session))
}

/// A scheduled or running refresh of the session, if any.
pub fn job_phase(app: &mut App, host: HostKey, session: SessionId) -> Option<JobPhase> {
    let list = app.hub.query::<RefreshPending>(host, &()).data?;
    list.iter().find(|p| p.session == session).and_then(|p| p.job)
}

// ---------------------------------------------------------------------------------------------
// Dialog

pub fn show_modal(app: &mut App, ctx: &egui::Context, host: HostKey, session: SessionId) -> bool {
    let q = app.hub.query::<RefreshPlan>(host, &Target { session });
    let Some(plan) = q.data else {
        let msg = match &q.error {
            Some(e) => format!("새로고침할 수 있는지 확인하지 못했어요: {}", e.message),
            None => "새로고침할 수 있는지 확인하는 중…".to_string(),
        };
        let (close_clicked, close) = dialog(ctx, "refresh", 560.0, |ui| {
            dialog_title(ui, "세션 새로고침", Some(&msg));
            footer(ui, |ui| theme::secondary_button(ui, "닫기").clicked())
        });
        return !(close || close_clicked);
    };
    match plan.job.clone().filter(|j| result_first(j.phase, plan.has_changes())) {
        Some(job) => show_job(app, ctx, host, &plan, &job),
        None => show_plan(app, ctx, host, &plan),
    }
}

/// Whether the dialog shows the session's last refresh rather than a new plan: while it waits or
/// runs, after a failure (its retry leads to the plan), and after a result as long as nothing
/// changed since. A cancelled reservation always goes back to the plan.
fn result_first(phase: JobPhase, new_changes: bool) -> bool {
    match phase {
        JobPhase::Scheduled | JobPhase::Running | JobPhase::Failed => true,
        JobPhase::Done | JobPhase::Partial => !new_changes,
        JobPhase::Cancelled => false,
    }
}

fn subtitle(plan: &Plan) -> String {
    let place = match (plan.project_name.is_empty(), plan.workspace_name.is_empty()) {
        (false, false) => format!(" · {} / {}", plan.project_name, plan.workspace_name),
        (false, true) => format!(" · {}", plan.project_name),
        _ => String::new(),
    };
    format!("{} · {}{place}", plan.title, plan.tool.label())
}

fn show_plan(app: &mut App, ctx: &egui::Context, host: HostKey, plan: &Plan) -> bool {
    let st = &mut app.feat.refresh;
    let mut when = match st.when {
        Some((s, w)) if s == plan.session => w,
        _ => When::AfterResponse,
    };
    let working = matches!(plan.state, RunState::Busy | RunState::NeedsAnswer | RunState::Starting);
    if !working {
        when = When::Now;
    }
    let starting = st.starting == Some(plan.session);
    let mut start = false;
    let mut cancel = false;
    let mut copy_screen = false;
    let ((), close) = dialog(ctx, "refresh", 560.0, |ui| {
        dialog_title(ui, "세션 새로고침", Some(&subtitle(plan)));
        ui.spacing_mut().item_spacing.y = 6.0;
        if !plan.blockers.is_empty() {
            label(ui, "새로고침할 수 없어요");
            for b in &plan.blockers {
                note(ui, Tone::Recover, &b.message);
            }
            ui.add_space(6.0);
            changes(ui, plan);
            ui.add_space(4.0);
            footer(ui, |ui| cancel = theme::secondary_button(ui, "닫기").clicked());
            return;
        }
        changes(ui, plan);
        ui.add_space(8.0);
        kept(ui, plan);
        if working {
            ui.add_space(8.0);
            let legend = match (plan.state, plan.tool) {
                (RunState::NeedsAnswer, _) => "CLI가 답을 기다리고 있어요. 언제 적용할까요?",
                (_, Tool::Shell) => "셸에서 명령이 실행 중이에요. 언제 적용할까요?",
                (RunState::Starting, _) => "CLI가 시작하는 중이에요. 언제 적용할까요?",
                _ => "지금 응답을 만들고 있어요. 언제 적용할까요?",
            };
            label(ui, legend);
            let shell = plan.tool == Tool::Shell;
            let (after_title, after_desc) = match plan.state {
                _ if shell => (
                    "실행 중인 명령이 끝나면 적용",
                    "출력이 멈추고 조용해지면 셸을 다시 시작해요. 언제든 예약을 취소할 수 있습니다.",
                ),
                RunState::NeedsAnswer => {
                    ("현재 응답이 끝나면 적용", "답을 마치고 응답이 끝나면 적용해요. 언제든 예약을 취소할 수 있습니다.")
                }
                _ => (
                    "현재 응답이 끝나면 적용",
                    "그동안 입력과 작업은 계속할 수 있어요. 언제든 예약을 취소할 수 있습니다.",
                ),
            };
            if choice(ui, when == When::AfterResponse, after_title, Some("추천"), after_desc) {
                when = When::AfterResponse;
            }
            let now_desc = match plan.state {
                _ if shell => "실행 중인 명령을 멈추고 셸을 바로 다시 시작해요.",
                RunState::NeedsAnswer => {
                    "답하지 않고 기다리던 화면을 닫은 뒤 바로 다시 시작해요. 입력 중이던 내용은 먼저 복사해 두세요."
                }
                _ => "진행 중인 응답을 멈춥니다. 입력 중이던 내용은 먼저 복사해 두세요.",
            };
            if choice(ui, when == When::Now, "중단하고 바로 적용", None, now_desc) {
                when = When::Now;
            }
        }
        if when == When::Now && plan.input_recent {
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                note_inline(ui, Tone::Check, "방금 입력한 내용이 있어요. 보내지 않은 입력은 다시 시작하면 사라져요.");
                if theme::link_button(ui, "화면 내용 복사").clicked() {
                    copy_screen = true;
                }
            });
        }
        ui.add_space(6.0);
        footer(ui, |ui| {
            let primary = match (when, working, plan.method) {
                (When::AfterResponse, true, Some(Method::RestartShell)) => "끝나면 다시 시작 예약",
                (When::AfterResponse, true, _) => "응답 후 적용 예약",
                (When::Now, true, _) => "중단하고 적용",
                (_, _, Some(Method::RestartShell)) => "셸 다시 시작",
                _ => "새로고침",
            };
            let label = if starting { "요청하는 중…" } else { primary };
            let b = ui.add_enabled(
                !starting,
                egui::Button::new(RichText::new(label).font(medium(ty::BODY)).color(c::ON_EMBER))
                    .fill(c::EMBER)
                    .corner_radius(CornerRadius::same(7))
                    .min_size(Vec2::new(0.0, 32.0)),
            );
            start = b.clicked();
            cancel = theme::secondary_button(ui, "취소").clicked();
        });
    });
    app.feat.refresh.when = Some((plan.session, when));
    if copy_screen {
        let ctx2 = ctx.clone();
        app.hub.call::<SessionsScreenText>(host, ScreenQuery { id: plan.session, scrollback: 0 }, move |app, res| {
            match res {
                Ok(text) => {
                    ctx2.copy_text(text);
                    app.toast_info("화면 내용을 복사했어요");
                }
                Err(e) => app.toast_error(&format!("화면 내용을 읽지 못했어요: {}", e.message)),
            }
        });
    }
    if start && !starting {
        request(app, host, plan.session, when, false);
    }
    !(close || cancel)
}

/// Ask the host to refresh (or only verify again).
fn request(app: &mut App, host: HostKey, session: SessionId, when: When, verify_only: bool) {
    let (cols, rows) = app.shell.center.terminal_grid();
    app.feat.refresh.starting = Some(session);
    let p = StartParams { session, when, verify_only, cols, rows };
    app.hub.call::<RefreshStart>(host, p, move |app, res| {
        let st = &mut app.feat.refresh;
        if st.starting == Some(session) {
            st.starting = None;
        }
        match res {
            Ok(_) => {
                if !st.watched.contains(&(host, session)) {
                    st.watched.push((host, session));
                }
            }
            Err(e) => app.toast_error(&format!("새로고침하지 못했어요: {}", e.message)),
        }
    });
}

fn changes(ui: &mut egui::Ui, plan: &Plan) {
    let env_label = if plan.env_from == plan.env_to {
        "적용할 변경".to_string()
    } else {
        format!("적용할 변경 · 공통 환경변수 v{} → v{}", plan.env_from, plan.env_to)
    };
    label(ui, &env_label);
    box_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = 4.0;
        let mut any = false;
        for c in &plan.env_changes {
            any = true;
            change_row(ui, &c.key, env_change_text(c));
        }
        match &plan.mcp_changes {
            Some(list) => {
                for m in list {
                    any = true;
                    change_row(ui, &format!("MCP {}", m.name), &mcp_change_text(m));
                }
            }
            None if plan.tool.is_agent() => {
                change_row(ui, "MCP", "실행을 시작할 때의 설정을 몰라 비교하지 못했어요");
            }
            None => {}
        }
        if !any {
            ui.label(
                RichText::new("바뀐 설정이 없어요. 다시 시작하면 MCP 연결만 새로 맺어요.")
                    .font(font(12.5))
                    .color(c::TEXT_2),
            );
        }
    });
}

pub fn env_change_text(c: &EnvChange) -> &'static str {
    match c.kind {
        EnvChangeKind::Added => "새로 추가",
        EnvChangeKind::Changed => "값 변경 · 값은 표시하지 않아요",
        EnvChangeKind::Removed => "삭제",
        EnvChangeKind::Unreadable => "값을 읽지 못해 이 실행에 빠져 있어요",
    }
}

pub fn mcp_change_text(m: &McpChange) -> String {
    let what = match m.kind {
        McpChangeKind::Added => "새로 추가",
        McpChangeKind::Changed => "설정 변경",
        McpChangeKind::Removed => "삭제",
    };
    format!("{what} · {}", m.scope.label())
}

fn change_row(ui: &mut egui::Ui, name: &str, what: &str) {
    ui.horizontal(|ui| {
        let w = ui.available_width();
        ui.allocate_ui_with_layout(
            Vec2::new((w * 0.5).max(120.0), 18.0),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| theme::truncated(ui, name, mono(12.5), c::TEXT_STRONG),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(RichText::new(what).font(font(12.0)).color(c::TEXT_2));
        });
    });
}

fn kept(ui: &mut egui::Ui, plan: &Plan) {
    if plan.method == Some(Method::RestartShell) {
        // What is lost comes before what stays.
        note(
            ui,
            Tone::Check,
            "셸은 대화를 이어갈 수 없어 새로 시작해요. 실행 중이던 명령과 셸 상태(변수, 이동한 폴더)는 사라져요.",
        );
        ui.add_space(4.0);
    }
    label(ui, "그대로 유지돼요");
    let text = match plan.method {
        Some(Method::StartFresh) => {
            "핀과 기록, 작업 공간. 아직 저장된 대화가 없어 CLI는 새 대화로 시작해요. 다른 세션은 재시작하지 않습니다."
        }
        Some(Method::RestartShell) => "탭과 작업 공간. 다른 세션은 재시작하지 않습니다.",
        _ => "같은 대화, 핀과 기록, 작업 공간. 다른 세션은 재시작하지 않습니다.",
    };
    ui.add(egui::Label::new(RichText::new(text).font(font(ty::BODY)).color(c::TEXT)).wrap());
}

/// A radio card. Returns whether it was clicked.
fn choice(ui: &mut egui::Ui, on: bool, title: &str, tag: Option<&str>, desc: &str) -> bool {
    let w = ui.available_width();
    let desc_galley = ui.painter().layout(desc.to_string(), font(12.5), c::TEXT_2, w - 44.0);
    let h = 22.0 + desc_galley.size().y + 12.0;
    let (r, resp) = ui.allocate_exact_size(Vec2::new(w, h), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::RadioButton, true, on, title));
    let bg = if on {
        c::CONTROL
    } else if resp.hovered() {
        c::PANEL_2
    } else {
        c::ROW
    };
    ui.painter().rect_filled(r, CornerRadius::same(8), bg);
    if on {
        ui.painter().rect_stroke(r, CornerRadius::same(8), Stroke::new(1.0, c::EMBER), egui::StrokeKind::Inside);
    }
    let dot = Pos2::new(r.left() + 19.0, r.top() + 19.0);
    ui.painter().circle_stroke(dot, 6.5, Stroke::new(1.5, if on { c::EMBER } else { c::TEXT_3 }));
    if on {
        ui.painter().circle_filled(dot, 3.5, c::EMBER);
    }
    let tx = r.left() + 34.0;
    let title_g = ui.painter().layout_no_wrap(title.to_string(), medium(ty::BODY), c::TEXT_STRONG);
    let tw = title_g.size().x;
    ui.painter().galley(Pos2::new(tx, r.top() + 10.0), title_g, c::TEXT_STRONG);
    if let Some(t) = tag {
        ui.painter().text(Pos2::new(tx + tw + 6.0, r.top() + 19.0), Align2::LEFT_CENTER, t, font(12.0), c::EMBER);
    }
    ui.painter().galley(Pos2::new(tx, r.top() + 30.0), desc_galley, c::TEXT_2);
    theme::focus_ring(ui, &resp, CornerRadius::same(8));
    resp.clicked()
}

// ---------------------------------------------------------------------------------------------
// Progress and result

fn job_title(phase: JobPhase) -> &'static str {
    match phase {
        JobPhase::Scheduled => "응답이 끝나면 새로고침해요",
        JobPhase::Running => "새로고침 중",
        JobPhase::Done => "새로고침했어요",
        JobPhase::Partial => "일부만 적용됐어요",
        JobPhase::Failed => "새로고침하지 못했어요",
        JobPhase::Cancelled => "새로고침을 취소했어요",
    }
}

/// What the person can do with a job's result.
#[derive(Default)]
struct Acts {
    cancel_schedule: bool,
    retry_verify: bool,
    retry_full: bool,
    go_auth: bool,
    close: bool,
}

/// What a scheduled refresh is waiting for, from the run's current state.
fn waiting_for(state: RunState, tool: Tool) -> (Tone, &'static str) {
    match state {
        RunState::NeedsAnswer => {
            (Tone::Attention, "CLI가 답을 기다리고 있어요. 터미널에서 답하고 응답이 끝나면 적용해요.")
        }
        RunState::Busy if tool == Tool::Shell => {
            (Tone::Waiting, "실행 중인 명령이 끝나 출력이 멈추면 셸을 다시 시작해요.")
        }
        RunState::Busy | RunState::Starting => {
            (Tone::Waiting, "응답이 끝나 출력이 멈추면 적용해요. 그동안 입력과 작업은 계속할 수 있어요.")
        }
        _ => (Tone::Waiting, "조용해지면 곧 적용해요. 방금 입력한 내용이 있으면 조금 더 기다려요."),
    }
}

fn job_body(ui: &mut egui::Ui, job: &Job, plan: Option<&Plan>) {
    if let Some(m) = &job.message {
        ui.add(egui::Label::new(RichText::new(m).font(font(12.5)).color(c::TEXT_2)).wrap());
    } else if let (JobPhase::Scheduled, Some(p)) = (job.phase, plan) {
        let (tone, text) = waiting_for(p.state, p.tool);
        note(ui, tone, text);
    }
    ui.add_space(4.0);
    ui.spacing_mut().item_spacing.y = 7.0;
    for s in &job.steps {
        let (glyph, color, state) = match s.state {
            StepState::Done => (icons::CHECK, c::MINT, "완료"),
            StepState::Running => (icons::LOADER_CIRCLE, c::SKY, "진행 중"),
            StepState::Pending => (icons::CLOCK, c::TEXT_OFF, "대기"),
            StepState::Failed => (icons::CIRCLE_X, c::ROSE, "실패"),
            StepState::Skipped => (icons::MINUS, c::TEXT_OFF, "건너뜀"),
        };
        let fg = if s.state == StepState::Pending { c::TEXT_3 } else { c::TEXT };
        item(ui, glyph, color, state, s.kind.label(), fg, s.message.as_deref());
    }
    for k in &job.checks {
        let (glyph, color, state, fg) = match k.state {
            CheckState::Ok => (icons::CHECK, c::MINT, "확인됨", c::TEXT),
            CheckState::NeedsAuth => (icons::CIRCLE_ALERT, c::EMBER, "인증 필요", c::TEXT_STRONG),
            CheckState::Failed => (icons::CIRCLE_X, c::ROSE, "실패", c::TEXT_STRONG),
            CheckState::Unknown => (icons::CIRCLE_HELP, c::GREY, "확인 불가", c::TEXT_2),
        };
        item(ui, glyph, color, state, &k.label, fg, k.detail.as_deref());
    }
}

fn item(
    ui: &mut egui::Ui,
    glyph: &str,
    color: egui::Color32,
    state: &str,
    text: &str,
    fg: egui::Color32,
    sub: Option<&str>,
) {
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        ui.label(icon(glyph, 13.0, color)).on_hover_text(state);
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 1.0;
            ui.add(egui::Label::new(RichText::new(text).font(font(ty::BODY)).color(fg)).wrap());
            if let Some(s) = sub {
                ui.add(egui::Label::new(RichText::new(s).font(font(12.0)).color(c::TEXT_3)).wrap());
            }
        });
    });
}

fn job_actions(ui: &mut egui::Ui, job: &Job, busy: bool, acts: &mut Acts) {
    let needs_auth = job.checks.iter().any(|k| k.state == CheckState::NeedsAuth);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        match job.phase {
            JobPhase::Scheduled => {
                acts.cancel_schedule = theme::secondary_button(ui, "예약 취소").clicked();
            }
            JobPhase::Partial => {
                if needs_auth {
                    acts.go_auth = theme::primary_button(ui, &auth_label(job)).clicked();
                }
                acts.retry_verify =
                    ui.add_enabled_ui(!busy, |ui| theme::secondary_button(ui, "이 세션에서 다시 시도")).inner.clicked();
            }
            JobPhase::Failed => {
                acts.retry_full =
                    ui.add_enabled_ui(!busy, |ui| theme::secondary_button(ui, "다시 시도")).inner.clicked();
            }
            _ => {}
        }
        let close_label = if job.phase.is_active() { "닫기 (계속 진행돼요)" } else { "닫기" };
        acts.close = theme::link_button(ui, close_label).clicked();
    });
}

/// The primary action of a partial result: "<server> 인증"/"<server> 승인" for one server
/// (as in the design), otherwise a general label.
fn auth_label(job: &Job) -> String {
    let open: Vec<&CheckItem> = job.checks.iter().filter(|k| k.state == CheckState::NeedsAuth).collect();
    match open.as_slice() {
        [one] => match &one.mcp {
            Some(name) if one.label.ends_with("승인 필요") => format!("{name} 승인"),
            Some(name) => format!("{name} 인증"),
            None => "터미널에서 인증".into(),
        },
        _ => "터미널에서 인증".into(),
    }
}

fn show_job(app: &mut App, ctx: &egui::Context, host: HostKey, plan: &Plan, job: &Job) -> bool {
    let busy = app.feat.refresh.starting == Some(plan.session);
    let mut acts = Acts::default();
    let ((), close) = dialog(ctx, "refresh", 560.0, |ui| {
        dialog_title(ui, job_title(job.phase), Some(&subtitle(plan)));
        job_body(ui, job, Some(plan));
        ui.add_space(8.0);
        job_actions(ui, job, busy, &mut acts);
    });
    let keep = apply(app, host, plan.session, (plan.project_id, plan.workspace_id), job, acts);
    keep && !close
}

/// Carry out the chosen action. Returns whether the result stays open.
fn apply(
    app: &mut App,
    host: HostKey,
    session: SessionId,
    place: (uuid::Uuid, uuid::Uuid),
    job: &Job,
    acts: Acts,
) -> bool {
    if acts.cancel_schedule {
        app.hub.call::<RefreshCancel>(host, Target { session }, |app, res| {
            if let Err(e) = res {
                app.toast_error(&e.message);
            }
        });
        return true;
    }
    if acts.retry_verify {
        request(app, host, session, When::Now, true);
        return true;
    }
    if acts.retry_full {
        dismiss(app, host, session);
        return true;
    }
    if acts.go_auth {
        let ws = WsKey { host, id: place.1 };
        actions::open_session(app, place.0, ws, session, false);
        crate::terminal::focus(app, host, session);
        app.toast_info("터미널에서 인증이나 승인을 마친 뒤 ‘이 세션에서 다시 시도’를 누르세요");
        return false;
    }
    if acts.close {
        if !job.phase.is_active() {
            dismiss(app, host, session);
        }
        return false;
    }
    true
}

fn dismiss(app: &mut App, host: HostKey, session: SessionId) {
    app.feat.refresh.watched.retain(|w| *w != (host, session));
    app.hub.send::<RefreshDismiss>(host, Target { session });
}

/// Verification aid: `RUNWICK_UI_REFRESH_CARD=<session id>` shows that session's refresh card
/// as if the refresh had been started from this window.
fn watch_requested(app: &mut App) {
    static ASKED: std::sync::Once = std::sync::Once::new();
    ASKED.call_once(|| {
        let id = hook_env!("RUNWICK_UI_REFRESH_CARD").and_then(|v| v.parse::<SessionId>().ok());
        if let Some(id) = id {
            app.feat.refresh.watched.push((HostKey::Local, id));
        }
    });
}

/// Result card at the bottom right after the dialog was closed (RefreshPartial.dc.html):
/// progress of a running refresh and its result until closed. Call once per frame.
pub fn show_overlay(app: &mut App, ctx: &egui::Context) {
    watch_requested(app);
    if app.ui.modal.is_some() {
        return;
    }
    let watched = app.feat.refresh.watched.clone();
    let mut y = ctx.content_rect().bottom() - crate::theme::size::SESSION_BAR - 12.0;
    for (host, session) in watched {
        let q = app.hub.query::<RefreshStatus>(host, &Target { session });
        let Some(job) = q.data.as_ref().and_then(|j| (**j).clone()) else {
            if q.data.is_some() {
                // Dismissed elsewhere.
                app.feat.refresh.watched.retain(|w| *w != (host, session));
            }
            continue;
        };
        let plan = app.hub.query::<RefreshPlan>(host, &Target { session }).data;
        let busy = app.feat.refresh.starting == Some(session);
        let mut acts = Acts::default();
        let width = 400.0_f32.min(ctx.content_rect().width() - 24.0);
        let area = egui::Area::new(egui::Id::unique(("refresh-card", session)))
            .order(egui::Order::Foreground)
            .pivot(Align2::RIGHT_BOTTOM)
            .fixed_pos(Pos2::new(ctx.content_rect().right() - 12.0, y))
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(c::PANEL_2)
                    .stroke(Stroke::new(1.0, c::LINE_STRONG))
                    .corner_radius(CornerRadius::same(12))
                    .inner_margin(egui::Margin::same(14))
                    .shadow(egui::epaint::Shadow {
                        offset: [0, 12],
                        blur: 40,
                        spread: 0,
                        color: egui::Color32::from_black_alpha(120),
                    })
                    .show(ui, |ui| {
                        ui.set_width(width - 28.0);
                        ui.spacing_mut().item_spacing.y = 6.0;
                        ui.label(
                            RichText::new(job_title(job.phase)).font(semibold(ty::PANEL_TITLE)).color(c::TEXT_STRONG),
                        );
                        if let Some(p) = &plan {
                            ui.label(RichText::new(subtitle(p)).font(font(12.0)).color(c::TEXT_3));
                        }
                        job_body(ui, &job, plan.as_deref());
                        ui.add_space(4.0);
                        job_actions(ui, &job, busy, &mut acts);
                    });
            });
        y = area.response.rect.top() - 8.0;
        let place = plan.as_ref().map(|p| (p.project_id, p.workspace_id));
        match place {
            Some(place) => {
                if acts.retry_full {
                    // Re-plan in the dialog: the old result is dropped, the plan shows again.
                    app.ui.modal = Some(Modal::Refresh { host, session });
                }
                apply(app, host, session, place, &job, acts);
            }
            None if acts.close => dismiss(app, host, session),
            None => {}
        }
    }
}

// ---------------------------------------------------------------------------------------------
// "적용 대기" list for the settings pages

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingFilter {
    Env,
    Mcp,
}

/// Verification aid: `RUNWICK_UI_REFRESH=<session id>` opens that session's refresh dialog once
/// a settings page with the pending list is shown (dialogs are not part of `RUNWICK_UI_STATE`).
fn open_requested(app: &mut App) {
    static ASKED: std::sync::Once = std::sync::Once::new();
    ASKED.call_once(|| {
        if let Some(id) = hook_env!("RUNWICK_UI_REFRESH").and_then(|v| v.parse::<SessionId>().ok()) {
            app.ui.modal = Some(Modal::Refresh { host: HostKey::Local, session: id });
        }
    });
}

/// Sessions on this computer that run with older settings, each with its refresh entry.
pub fn pending_list(app: &mut App, ui: &mut egui::Ui, filter: PendingFilter, current_env: Option<u64>) {
    open_requested(app);
    let host = HostKey::Local;
    let q = app.hub.query::<RefreshPending>(host, &());
    let Some(list) = q.data else {
        let text = match &q.error {
            Some(e) => format!("적용 대기 세션을 확인하지 못했어요: {}", e.message),
            None => "실행 중인 세션을 확인하는 중…".into(),
        };
        widgets::empty_note(ui, &text);
        return;
    };
    let rows: Vec<&Pending> = list
        .iter()
        .filter(|p| match filter {
            PendingFilter::Env => !p.env_changes.is_empty(),
            PendingFilter::Mcp => !p.mcp_changes.is_empty(),
        })
        .collect();
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 2.0;
        let (title, desc) = if rows.is_empty() {
            match (filter, current_env) {
                (PendingFilter::Env, Some(v)) => {
                    (format!("실행 중인 세션은 모두 v{v} 설정을 쓰고 있어요"), "새로 시작하는 세션도 같은 값을 받아요.")
                }
                _ => ("새로고침이 필요한 세션이 없어요".to_string(), "실행 중인 세션은 모두 지금 설정으로 시작했어요."),
            }
        } else {
            (
                "저장됐어요. 실행 중인 세션에는 아직 적용 전입니다".to_string(),
                "저장만으로 세션을 재시작하지 않아요. 필요한 세션만 골라 새로고침하세요. 대화는 그대로 이어집니다.",
            )
        };
        ui.label(RichText::new(title).font(semibold(14.0)).color(c::TEXT_STRONG));
        ui.label(RichText::new(desc).font(font(12.5)).color(c::TEXT_2));
    });
    if rows.is_empty() {
        return;
    }
    ui.add_space(6.0);
    let mut open = None;
    egui::Frame::new().fill(c::PANEL).corner_radius(CornerRadius::same(12)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = 0.0;
        for (i, p) in rows.iter().enumerate() {
            if i > 0 {
                let r = ui.available_rect_before_wrap();
                ui.painter().hline(r.x_range(), r.top(), Stroke::new(1.0, c::LINE));
            }
            if pending_row(ui, p) {
                open = Some(p.session);
            }
        }
    });
    if let Some(session) = open {
        app.ui.modal = Some(Modal::Refresh { host, session });
    }
}

fn state_words(s: RunState, tool: Tool) -> &'static str {
    match s {
        RunState::Starting => "시작 중",
        RunState::Busy if tool == Tool::Shell => "명령 실행 중",
        RunState::Busy => "응답 생성 중",
        RunState::Idle => "입력 대기",
        RunState::NeedsAnswer => "답을 기다리는 중",
        RunState::Ended => "실행 종료",
    }
}

fn pending_row(ui: &mut egui::Ui, p: &Pending) -> bool {
    let mut clicked = false;
    egui::Frame::new().inner_margin(egui::Margin { left: 14, right: 8, top: 8, bottom: 8 }).show(ui, |ui| {
        ui.set_min_height(34.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 12.0;
            theme::tool_badge(ui, crate::shell::data::tool_mark(p.tool), 22.0, c::CONTROL);
            let action = match (p.job, p.tool) {
                (Some(JobPhase::Scheduled), _) => "예약됨",
                (Some(_), _) => "진행 중",
                (None, Tool::Shell) => "셸 다시 시작",
                (None, _) => "새로고침",
            };
            let btn_w = widgets::text_width(ui, action, font(12.0)) + 22.0;
            let text_w = (ui.available_width() - btn_w - 12.0).max(80.0);
            ui.vertical(|ui| {
                ui.set_width(text_w);
                ui.spacing_mut().item_spacing.y = 1.0;
                theme::truncated(ui, &p.title, medium(ty::BODY), c::TEXT_STRONG);
                let place = if p.workspace_name.is_empty() {
                    p.project_name.clone()
                } else {
                    format!("{} / {}", p.project_name, p.workspace_name)
                };
                let mut meta = format!("{place} · {}", state_words(p.state, p.tool));
                if p.tool == Tool::Shell {
                    meta.push_str(" · 일반 셸은 대화를 이어갈 수 없어 새로 시작해요");
                }
                theme::truncated(ui, meta, font(12.0), c::TEXT_3);
                let keys: Vec<String> = p
                    .env_changes
                    .iter()
                    .map(|c| c.key.clone())
                    .chain(p.mcp_changes.iter().map(|m| format!("MCP {}", m.name)))
                    .collect();
                theme::truncated(ui, format!("바뀐 항목: {}", keys.join(", ")), font(11.5), c::TEXT_3);
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let label = format!("{action}: {}", p.title);
                let resp = widgets::pill_button(ui, None, action, 28.0, c::CONTROL).on_hover_text(label);
                clicked = resp.clicked();
            });
        });
    });
    clicked
}

// ---------------------------------------------------------------------------------------------
// Small pieces

fn label(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).font(medium(12.0)).color(c::TEXT_3));
}

fn box_frame() -> egui::Frame {
    egui::Frame::new()
        .fill(c::CONTROL)
        .corner_radius(CornerRadius::same(8))
        .inner_margin(egui::Margin::symmetric(12, 10))
}

fn note(ui: &mut egui::Ui, tone: Tone, text: &str) {
    egui::Frame::new()
        .fill(tone.bg())
        .corner_radius(CornerRadius::same(8))
        .inner_margin(egui::Margin::symmetric(12, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            note_inline(ui, tone, text);
        });
}

fn note_inline(ui: &mut egui::Ui, tone: Tone, text: &str) {
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        ui.label(icon(tone.icon(), 12.0, tone.fg()));
        ui.add(egui::Label::new(RichText::new(text).font(font(12.5)).color(c::TEXT)).wrap());
    });
}

fn footer<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let r = ui.available_rect_before_wrap();
    ui.painter().hline(r.x_range(), r.top(), Stroke::new(1.0, c::LINE_STRONG));
    ui.add_space(10.0);
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), add).inner
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_finished_result_gives_way_to_a_plan_for_newer_changes() {
        for p in [JobPhase::Scheduled, JobPhase::Running, JobPhase::Failed] {
            assert!(result_first(p, true) && result_first(p, false), "{p:?}");
        }
        for p in [JobPhase::Done, JobPhase::Partial] {
            assert!(result_first(p, false) && !result_first(p, true), "{p:?}");
        }
        assert!(!result_first(JobPhase::Cancelled, false));
    }
}
