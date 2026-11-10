//! Collaboration views: the session's 협업 panel (S04), the worker peek and direct conversation
//! (S05), and results (S11, as a tab or in the right panel).
//!
//! Everything shown comes from the host (`collab.*`, sessions and their live state); a status is
//! always a tone, an icon and words. "응답 완료" is never shown as verified.

mod panel;
mod peek;
mod result;

use crate::App;
use crate::client::{HostKey, Query};
use crate::icons;
use crate::shell::data;
use crate::state::{PanelDetail, PanelTarget, RightPanel, TabId, WsKey};
use crate::theme::Tone;
use runwick_core::api::collab::{CollabList, CollabOverview, Overview, ReqStatus, Request, RequestQuery};
use runwick_core::api::projects::{ProjectId, WorkspaceKind};
use runwick_core::api::sessions::{Attention, Session, SessionId};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

#[derive(Default)]
pub struct State {
    peek: Option<peek::Peek>,
    /// "협업 중단" confirmation for a parent session.
    stop_all: Option<(HostKey, SessionId)>,
    /// File selected in a result, per request.
    result_file: HashMap<Uuid, String>,
    /// Logs of checks expanded in a result.
    open_logs: HashSet<(Uuid, usize)>,
    read_versions: HashMap<(HostKey, Uuid, runwick_core::Ts), ReadAttempt>,
}

struct ReadAttempt {
    at: std::time::Instant,
    pending_or_done: bool,
}

impl ReadAttempt {
    fn can_retry(&self) -> bool {
        !self.pending_or_done && self.at.elapsed() >= std::time::Duration::from_secs(5)
    }
}

pub fn peek_is_open(app: &App) -> bool {
    app.feat.collab.peek.is_some()
}

pub fn open_workers(app: &mut App, host: HostKey, session: SessionId) {
    app.ui.push_panel(
        RightPanel::Workers,
        PanelDetail::WorkerList { host, session },
        Some(PanelTarget { host, session }),
    );
}

pub fn open_result(app: &mut App, host: HostKey, request: Uuid) {
    let target = app
        .hub
        .query::<runwick_core::api::collab::CollabGet>(host, &runwick_core::api::projects::ById { id: request })
        .data
        .map(|v| PanelTarget { host, session: v.request.worker_id });
    app.ui.push_panel(RightPanel::Workers, PanelDetail::Result { host, request }, target);
}

pub fn open_worker(app: &mut App, host: HostKey, session: SessionId) {
    app.ui.push_panel(RightPanel::Workers, PanelDetail::Worker { host, session }, Some(PanelTarget { host, session }));
}

pub fn show_worker(app: &mut App, ui: &mut egui::Ui, host: HostKey, session: SessionId) {
    let q =
        app.hub.query::<CollabList>(host, &RequestQuery { worker_id: Some(session), limit: 1, ..Default::default() });
    if let Some(r) = q.data.as_ref().and_then(|l| l.first()) {
        if let Some(frame) = app.ui.panel_mut().stacks.get_mut(&RightPanel::Workers).and_then(|s| s.last_mut()) {
            frame.detail = PanelDetail::Result { host, request: r.id };
        }
        result::show_panel(app, ui, host, r.id);
    } else {
        crate::shell::widgets::empty_note(
            ui,
            &q.error.map(|e| e.message).unwrap_or_else(|| {
                if q.data.is_some() {
                    "아직 보고된 결과가 없어요".into()
                } else {
                    "작업자를 불러오는 중…".into()
                }
            }),
        );
        ui.horizontal_wrapped(|ui| {
            if crate::theme::secondary_button(ui, "화면 보기").clicked() {
                peek::open(app, host, session, None);
            }
            if crate::theme::secondary_button(ui, "직접 대화").clicked() {
                direct(app, host, session);
            }
            if crate::theme::secondary_button(ui, "멈추기").clicked() {
                app.shell.modal.ask_stop(host, session);
            }
        });
    }
}

pub fn direct(app: &mut App, host: HostKey, worker: SessionId) {
    use runwick_core::api::collab::{CollabDirect, Direct};
    use runwick_core::api::projects::ById;
    use runwick_core::api::sessions::{SessionsGet, SessionsTakeInput};
    app.hub.call::<SessionsGet>(host, ById { id: worker }, move |app, res| {
        let info = match res {
            Ok(info) => info,
            Err(e) => {
                app.toast_error(&e.message);
                return;
            }
        };
        let p = place(app, host, &info.session);
        let (Some(project), Some(ws)) = (p.project_id, p.ws) else {
            app.toast_error("작업자의 공간을 찾을 수 없어요");
            return;
        };
        app.hub.call::<CollabDirect>(host, Direct { worker_id: worker, on: true }, move |app, res| {
            if let Err(e) = res {
                app.toast_error(&e.message);
                return;
            }
            crate::shell::actions::open_session(app, project, ws, worker, info.session.hidden);
            app.hub.call::<SessionsTakeInput>(host, ById { id: worker }, move |app, res| match res {
                Ok(()) => crate::terminal::focus(app, host, worker),
                Err(e) => app.toast_error(&format!("입력권을 가져오지 못했어요: {}", e.message)),
            });
        });
    });
}

pub fn show_panel(app: &mut App, ui: &mut egui::Ui, host: HostKey, session: SessionId) {
    panel::show(app, ui, host, session);
}

pub fn show_result_tab(app: &mut App, ui: &mut egui::Ui, host: HostKey, request: Uuid, _tab: TabId) {
    result::show_tab(app, ui, host, request);
}

pub fn show_result_panel(app: &mut App, ui: &mut egui::Ui, host: HostKey, request: Uuid) {
    result::show_panel(app, ui, host, request);
}

/// Overlays drawn above the workroom every frame (worker peek, "협업 중단" confirmation).
pub fn show_overlays(app: &mut App, ctx: &egui::Context) {
    peek::show(app, ctx);
    panel::show_stop_all(app, ctx);
}

/// Verification aid (`RUNWICK_UI_OVERLAY`, see shell): `peek:<worker>`, `direct:<worker>` (takes
/// input like the button), `stale:<worker>` (opened from a prompt that is gone), `stop-all:<parent>`,
/// `remove:<worktree workspace>`, `quit`, `resume`, `split`. Returns whether the spec was one of these.
pub fn verify_overlay(app: &mut App, spec: &str) -> bool {
    let (what, id) = spec.split_once(':').unwrap_or((spec, ""));
    let id: Option<Uuid> = id.parse().ok();
    match (what, id) {
        ("peek", Some(w)) => peek::open(app, HostKey::Local, w, None),
        ("direct", Some(w)) => {
            peek::open(app, HostKey::Local, w, None);
            app.hub.send::<runwick_core::api::collab::CollabDirect>(
                HostKey::Local,
                runwick_core::api::collab::Direct { worker_id: w, on: true },
            );
        }
        ("stale", Some(w)) => {
            let gone = Attention {
                kind: runwick_core::api::sessions::AttentionKind::Approval,
                text: "Do you want to proceed?".into(),
                since: runwick_core::now(),
            };
            peek::open(app, HostKey::Local, w, Some(gone));
        }
        ("stop-all", Some(p)) => app.feat.collab.stop_all = Some((HostKey::Local, p)),
        ("remove", Some(ws)) => {
            let key = WsKey { host: HostKey::Local, id: ws };
            let Some(project) = data::workspaces(app).data.and_then(|l| data::find_ws(&l, &key).map(|w| w.project_id))
            else {
                return false;
            };
            crate::views::split::open_merge(app, project, key, true);
        }
        ("quit", None) => app.ui.modal = Some(crate::state::Modal::Quit),
        ("resume", None) => app.ui.modal = Some(crate::state::Modal::Resume),
        ("split", None) => match app.ui.selected_ws {
            Some(ws) => app.ui.modal = Some(crate::state::Modal::SplitWork { ws }),
            None => return false,
        },
        _ => return false,
    }
    true
}

/// How a request is shown: tone, icon and words.
pub struct Look {
    pub tone: Tone,
    pub icon: &'static str,
    pub label: String,
    pub pulse: bool,
}

pub fn look(r: &Request, direct: bool) -> Look {
    let l = |tone, icon, label: &str, pulse| Look { tone, icon, label: label.to_string(), pulse };
    if direct && r.status.is_open() {
        return l(Tone::Direct, icons::KEYBOARD, "직접 조작 중", false);
    }
    match r.status {
        ReqStatus::Queued if r.reason.as_deref().is_some_and(|x| x.contains("보류")) => {
            l(Tone::Waiting, icons::CIRCLE_PAUSE, "미전달 대기", false)
        }
        ReqStatus::Queued => l(Tone::Waiting, icons::CIRCLE_PAUSE, "대기", false),
        ReqStatus::Delivered => l(Tone::Waiting, icons::HOURGLASS, "전달됨 · 응답 대기", false),
        ReqStatus::Running => l(Tone::Running, icons::CIRCLE_DOT, "진행 중", true),
        ReqStatus::NeedsInput if r.reason.as_deref().is_some_and(|x| x.starts_with("승인")) => {
            l(Tone::Attention, icons::HAND, "승인 필요", false)
        }
        ReqStatus::NeedsInput => l(Tone::Attention, icons::HAND, "답변 필요", false),
        ReqStatus::Done => {
            let checks = r.result.as_ref().map(|x| x.checks.as_slice()).unwrap_or(&[]);
            let label = if checks.is_empty() {
                "응답 완료"
            } else if checks.iter().any(|c| !c.passed) {
                "응답 완료 · 검증 실패 있음"
            } else {
                "응답 완료 · 검증 보고됨"
            };
            l(Tone::Info, icons::CIRCLE_CHECK, label, false)
        }
        ReqStatus::Failed => l(Tone::Recover, icons::TRIANGLE_ALERT, "실패", false),
        ReqStatus::Cancelled => l(Tone::Waiting, icons::CIRCLE_X, "취소됨", false),
        ReqStatus::Uncertain => l(Tone::Attention, icons::CIRCLE_ALERT, "전달 확인 필요", false),
    }
}

/// [`look`] with what the worker's screen shows now: a prompt only the person can answer wins
/// over the request's own state while the work is open (the same ember words the sidebar uses).
pub fn look_live(r: &Request, direct: bool, live: Option<&runwick_core::api::sessions::SessionLive>) -> Look {
    if r.status.is_open()
        && let Some(label) = live.and_then(attention_words)
    {
        return Look { tone: Tone::Attention, icon: icons::HAND, label: label.into(), pulse: false };
    }
    look(r, direct)
}

/// "승인 필요" / "인증·신뢰 확인 필요" / "선택 필요 · …" while the worker shows a prompt only the
/// person can answer (a bell alone is not one).
pub fn attention_words(l: &runwick_core::api::sessions::SessionLive) -> Option<&'static str> {
    use runwick_core::api::sessions::AttentionKind;
    match l.attention.as_ref()?.kind {
        AttentionKind::Approval => Some("승인 필요"),
        AttentionKind::Auth => Some("인증·신뢰 확인 필요"),
        AttentionKind::Choice => Some(runwick_core::api::activity::CHOICE_NEEDED),
        AttentionKind::Notify => None,
    }
}

/// Where a session works, for display.
#[derive(Debug, Clone, Default)]
pub struct Place {
    pub project: String,
    pub project_id: Option<ProjectId>,
    pub workspace: String,
    pub ws: Option<WsKey>,
    pub branch: Option<String>,
    pub kind: Option<WorkspaceKind>,
}

impl Place {
    /// `payments-api / 기본 공간 / main`
    pub fn line(&self) -> String {
        let mut s = format!("{} / {}", self.project, self.workspace);
        if let Some(b) = &self.branch {
            s.push_str(&format!(" / {b}"));
        }
        s
    }
}

pub fn place(app: &mut App, host: HostKey, s: &Session) -> Place {
    let key = WsKey { host, id: s.workspace_id };
    let wl = data::workspaces(app).data;
    let pl = data::projects(app).data;
    let w = wl.as_ref().and_then(|l| data::find_ws(l, &key));
    let project_id = w.map(|w| w.project_id).or((host == HostKey::Local).then_some(s.project_id));
    let p = pl.as_ref().and_then(|l| project_id.and_then(|id| data::project_of(l, id)));
    Place {
        project: p.map(|p| p.name.clone()).unwrap_or_else(|| "프로젝트 확인 불가".into()),
        project_id,
        workspace: w.map(|w| w.name.clone()).unwrap_or_else(|| "공간 확인 불가".into()),
        ws: w.map(|_| key),
        branch: w.and_then(|w| w.branch.clone()),
        kind: w.map(|w| w.kind),
    }
}

pub fn requests_of(app: &mut App, host: HostKey, parent: SessionId) -> Query<Vec<Request>> {
    app.hub.query::<CollabList>(host, &RequestQuery { parent_id: Some(parent), limit: 300, ..Default::default() })
}

pub fn overview(app: &mut App, host: HostKey) -> Query<Overview> {
    app.hub.query::<CollabOverview>(host, &())
}

/// Text of a live prompt, to compare later ("이 승인 요청은 이미 끝났어요").
pub fn attention_key(a: &Attention) -> (String, i64) {
    (a.text.clone(), a.since.timestamp_millis())
}

/// Text with a Lucide glyph (icon font) before or after it, for buttons and labels.
pub fn with_icon(
    glyph: &str,
    text: &str,
    font: egui::FontId,
    color: egui::Color32,
    glyph_last: bool,
) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    let icon_fmt = egui::TextFormat {
        font_id: crate::theme::icon_font(font.size),
        color,
        valign: egui::Align::Center,
        ..Default::default()
    };
    let text_fmt = egui::TextFormat { font_id: font, color, valign: egui::Align::Center, ..Default::default() };
    if glyph_last {
        job.append(text, 0.0, text_fmt);
        job.append(glyph, 6.0, icon_fmt);
    } else {
        job.append(glyph, 0.0, icon_fmt);
        job.append(text, 6.0, text_fmt);
    }
    job
}

pub fn short(rev: &str) -> String {
    rev.chars().take(7).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use runwick_core::api::collab::{Source, Via};
    use runwick_core::api::sessions::{AttentionKind, Phase, SessionLive, Tool};

    fn request(status: ReqStatus) -> Request {
        let now = runwick_core::now();
        Request {
            id: Uuid::now_v7(),
            parent_id: None,
            worker_id: Uuid::now_v7(),
            previous_id: None,
            attempt: 1,
            title: "t".into(),
            role: None,
            tool: Tool::Claude,
            prompt: "p".into(),
            source: Source::Agent,
            via: Via::Typed,
            interrupt: false,
            status,
            reason: Some("작업자가 답변을 기다려요".into()),
            start_rev: None,
            created_at: now,
            delivered_at: None,
            finished_at: None,
            updated_at: now,
            result: None,
            acknowledged: false,
            notes: vec![],
        }
    }

    fn live(kind: Option<AttentionKind>) -> SessionLive {
        SessionLive {
            id: Uuid::nil(),
            phase: Phase::Running,
            busy: false,
            attention: kind.map(|kind| Attention {
                kind,
                text: "Trust this folder?".into(),
                since: runwick_core::now(),
            }),
            last_output_at: None,
            cols: 80,
            rows: 24,
            pid: None,
            input_owner: None,
            unread: false,
            error: None,
            error_kind: None,
            input_owner_client: None,
            foreground_job: false,
            input_prompt: false,
            leftovers: Vec::new(),
        }
    }

    #[test]
    fn failed_read_marks_can_retry_without_flooding_or_repeating_successful_requests() {
        let mut attempt = ReadAttempt { at: std::time::Instant::now(), pending_or_done: false };
        assert!(!attempt.can_retry());
        attempt.at -= std::time::Duration::from_secs(6);
        assert!(attempt.can_retry());
        attempt.pending_or_done = true;
        assert!(!attempt.can_retry());
    }

    #[test]
    fn a_prompt_on_the_worker_screen_wins_over_waiting() {
        let queued = request(ReqStatus::Queued);
        let lk = look_live(&queued, false, Some(&live(Some(AttentionKind::Auth))));
        assert_eq!((lk.tone, lk.label.as_str()), (Tone::Attention, "인증·신뢰 확인 필요"));
        let lk = look_live(&queued, false, Some(&live(Some(AttentionKind::Approval))));
        assert_eq!(lk.label, "승인 필요");
        // A bell is not a question; finished work keeps its own state.
        assert_eq!(look_live(&queued, false, Some(&live(Some(AttentionKind::Notify)))).label, "대기");
        let lk = look_live(&queued, false, Some(&live(Some(AttentionKind::Choice))));
        assert_eq!((lk.tone, lk.label.as_str()), (Tone::Attention, "선택 필요 · CLI 화면에서 골라 주세요"));
        let done = request(ReqStatus::Done);
        assert!(look_live(&done, false, Some(&live(Some(AttentionKind::Approval)))).label.starts_with("응답 완료"));
    }
}
