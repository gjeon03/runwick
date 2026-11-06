//! Running one refresh: wait for the response to end (when asked), then save → restart →
//! reconnect → verify, reporting each step. One job per session; a newer job or a cancel makes
//! an older task stop at its next step (the job id is checked before every change).

use super::{RunRecord, claude_transcript_exists};
use crate::host::Host;
use crate::sessions;
use runwick_core::api::mcp::{McpChangeKind, McpHealth};
use runwick_core::api::refresh::*;
use runwick_core::api::sessions::{Phase, Restart, SessionId, Tool};
use runwick_core::rpc::RpcError;
use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use uuid::Uuid;

/// Typing within this window may be an unsent draft: wait longer before restarting.
pub const INPUT_QUIET: Duration = Duration::from_secs(15);
const POLL: Duration = Duration::from_millis(750);
/// Consecutive quiet observations needed before "after the response" applies.
const SETTLE_POLLS: u32 = 2;
/// A restarted CLI must still be alive this long after the restart to count as reconnected.
const ALIVE_FOR: Duration = Duration::from_secs(3);

static JOBS: Mutex<Option<HashMap<SessionId, Job>>> = Mutex::new(None);

pub fn get(session: SessionId) -> Option<Job> {
    JOBS.lock().ok()?.as_ref()?.get(&session).cloned()
}

/// A refresh is waiting or running for this session (new assignments should wait).
pub fn locked(session: SessionId) -> bool {
    get(session).is_some_and(|j| j.phase.is_active())
}

/// Store a new job unless one is already active.
pub fn insert(job: Job) -> Result<(), RpcError> {
    let mut g = JOBS.lock().map_err(|_| RpcError::internal("작업 상태를 읽지 못했어요"))?;
    let map = g.get_or_insert_with(HashMap::new);
    if map.get(&job.session).is_some_and(|j| j.phase.is_active()) {
        return Err(RpcError::new(
            runwick_core::rpc::codes::CONFLICT,
            "이 세션은 이미 새로고침을 기다리거나 진행 중이에요",
        ));
    }
    map.insert(job.session, job);
    Ok(())
}

/// Change the job if it is still the current one for its session. Returns false otherwise.
pub fn update(host: &Host, session: SessionId, id: Uuid, f: impl FnOnce(&mut Job)) -> bool {
    let changed = match JOBS.lock() {
        Ok(mut g) => match g.as_mut().and_then(|m| m.get_mut(&session)) {
            Some(j) if j.id == id && j.phase != JobPhase::Cancelled => {
                f(j);
                true
            }
            _ => false,
        },
        Err(_) => false,
    };
    if changed {
        host.bus.changed(&[TOPIC]);
    }
    changed
}

pub fn cancel(session: SessionId) -> Result<bool, RpcError> {
    let mut g = JOBS.lock().map_err(|_| RpcError::internal("작업 상태를 읽지 못했어요"))?;
    match g.as_mut().and_then(|m| m.get_mut(&session)) {
        Some(j) if j.phase == JobPhase::Scheduled => {
            j.phase = JobPhase::Cancelled;
            j.finished_at = Some(runwick_core::now());
            j.message = Some("예약을 취소했어요. 실행은 그대로예요".into());
            Ok(true)
        }
        Some(j) if j.phase == JobPhase::Running => {
            Err(RpcError::new(runwick_core::rpc::codes::BUSY, "이미 다시 시작하고 있어 취소할 수 없어요"))
        }
        _ => Ok(false),
    }
}

/// Forget a finished job (its result was closed).
pub fn dismiss(session: SessionId) {
    if let Ok(mut g) = JOBS.lock()
        && let Some(m) = g.as_mut()
        && m.get(&session).is_some_and(|j| !j.phase.is_active())
    {
        m.remove(&session);
    }
}

/// The steps a method goes through ("필요 없는 단계는 표시하지 않음").
pub fn steps_for(method: Option<Method>, verify_only: bool) -> Vec<Step> {
    let kinds: &[StepKind] = match (verify_only, method) {
        (true, _) | (false, None) => &[StepKind::Verify],
        (false, Some(Method::Resume)) => {
            &[StepKind::SaveRecord, StepKind::Restart, StepKind::Reconnect, StepKind::Verify]
        }
        (false, Some(Method::StartFresh)) => &[StepKind::Restart, StepKind::Alive, StepKind::Verify],
        (false, Some(Method::RestartShell)) => &[StepKind::Restart, StepKind::Verify],
    };
    kinds.iter().map(|k| Step { kind: *k, state: StepState::Pending, message: None }).collect()
}

// ---------------------------------------------------------------------------------------------
// Waiting for the response to end

/// What the scheduler sees of a run at one moment.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Observed {
    pub alive: bool,
    pub busy: bool,
    /// A prompt only the person can answer is on screen.
    pub needs_answer: bool,
    pub since_input: Option<Duration>,
}

/// Source of [`Observed`]; the host reads live state, tests use a script.
pub trait Signals: Send + Sync {
    fn observe(&self, session: SessionId) -> Observed;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Waited {
    Ready,
    /// The run ended while waiting: the next start uses the new settings anyway.
    Ended,
    Cancelled,
}

/// Wait until the run is quiet: no output, no prompt waiting, no recent typing, observed
/// `settle` times in a row, polling every `poll`.
pub async fn wait_until_quiet(
    signals: &dyn Signals,
    session: SessionId,
    still_wanted: impl Fn() -> bool,
    poll: Duration,
    settle: u32,
) -> Waited {
    let mut quiet_in_a_row = 0;
    loop {
        if !still_wanted() {
            return Waited::Cancelled;
        }
        let o = signals.observe(session);
        if !o.alive {
            return Waited::Ended;
        }
        let quiet = !o.busy && !o.needs_answer && o.since_input.is_none_or(|d| d >= INPUT_QUIET);
        quiet_in_a_row = if quiet { quiet_in_a_row + 1 } else { 0 };
        if quiet_in_a_row >= settle {
            return Waited::Ready;
        }
        tokio::time::sleep(poll).await;
    }
}

struct HostSignals(Arc<Host>);

impl Signals for HostSignals {
    fn observe(&self, session: SessionId) -> Observed {
        let live = self.0.sessions.live(&self.0, session);
        let alive =
            live.as_ref().is_some_and(|l| matches!(l.phase, Phase::Starting | Phase::Running) && l.pid.is_some());
        Observed {
            alive,
            busy: live.as_ref().is_some_and(|l| l.busy || l.phase == Phase::Starting),
            needs_answer: live.as_ref().is_some_and(super::needs_answer),
            since_input: self.0.sessions.since_input(session),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Execution

pub struct Exec {
    pub session: SessionId,
    pub job: Uuid,
    pub when: When,
    pub method: Option<Method>,
    pub verify_only: bool,
    pub cols: u16,
    pub rows: u16,
    /// MCP servers whose definition changed (their connection decides a partial result).
    pub changed_mcp: BTreeSet<String>,
}

pub async fn run(host: Arc<Host>, x: Exec) {
    if x.when == When::AfterResponse && !x.verify_only {
        let signals = HostSignals(host.clone());
        let wanted = || get(x.session).is_some_and(|j| j.id == x.job && j.phase == JobPhase::Scheduled);
        match wait_until_quiet(&signals, x.session, wanted, POLL, SETTLE_POLLS).await {
            Waited::Ready => {}
            Waited::Cancelled => return,
            Waited::Ended => {
                finish(
                    &host,
                    &x,
                    JobPhase::Cancelled,
                    "실행이 끝나 예약을 거뒀어요. 다음 실행부터 새 설정을 써요".into(),
                );
                return;
            }
        }
    }
    if !update(&host, x.session, x.job, |j| j.phase = JobPhase::Running) {
        return;
    }
    let steps = steps_for(x.method, x.verify_only);
    for step in steps.iter().map(|s| s.kind) {
        set_step(&host, &x, step, StepState::Running, None);
        let result = match step {
            StepKind::SaveRecord => save_record(&host, &x).await,
            StepKind::Restart => restart(&host, &x).await,
            StepKind::Reconnect | StepKind::Alive => reconnect(&host, &x).await,
            StepKind::Verify => Ok(verify(&host, &x).await),
        };
        match result {
            Ok(msg) => set_step(&host, &x, step, StepState::Done, msg),
            Err(msg) => {
                set_step(&host, &x, step, StepState::Failed, Some(msg.clone()));
                let tail = if step == StepKind::SaveRecord {
                    " 실행은 그대로예요."
                } else {
                    " 기록과 핀은 그대로예요."
                };
                finish(&host, &x, JobPhase::Failed, format!("새로고침하지 못했어요: {msg}.{tail}"));
                return;
            }
        }
    }
    let job = get(x.session).filter(|j| j.id == x.job);
    let open: Vec<String> = job
        .iter()
        .flat_map(|j| j.checks.iter())
        .filter(|c| matches!(c.state, CheckState::NeedsAuth | CheckState::Failed))
        .map(|c| c.label.clone())
        .collect();
    if open.is_empty() {
        let msg = done_message(x.method, x.verify_only);
        finish(&host, &x, JobPhase::Done, msg.into());
    } else {
        finish(&host, &x, JobPhase::Partial, partial_message(x.method, x.verify_only, &open));
    }
}

/// Summary of a refresh in which every step and check passed.
pub fn done_message(method: Option<Method>, verify_only: bool) -> &'static str {
    match method {
        _ if verify_only => "다시 확인했어요. 남은 문제가 없어요",
        Some(Method::RestartShell) => "새 셸로 다시 시작했어요",
        Some(Method::StartFresh) => "새로고침했어요. 저장된 대화가 없어 CLI는 새 대화로 시작했어요",
        _ => "새로고침했어요. 같은 대화에서 계속 입력할 수 있어요",
    }
}

/// Summary when the run is back but some checks did not pass (`open` = their labels).
pub fn partial_message(method: Option<Method>, verify_only: bool, open: &[String]) -> String {
    let head = match method {
        _ if verify_only => "다시 확인했어요.",
        Some(Method::Resume) => "같은 대화로 다시 연결했어요.",
        Some(Method::StartFresh) => "CLI를 다시 시작했어요.",
        Some(Method::RestartShell) => "새 셸로 다시 시작했어요.",
        None => "다시 확인했어요.",
    };
    format!("{head} 남은 항목: {}", open.join(", "))
}

fn set_step(host: &Host, x: &Exec, kind: StepKind, state: StepState, message: Option<String>) {
    update(host, x.session, x.job, |j| {
        if let Some(s) = j.steps.iter_mut().find(|s| s.kind == kind) {
            s.state = state;
            s.message = message;
        }
    });
}

fn finish(host: &Host, x: &Exec, phase: JobPhase, message: String) {
    update(host, x.session, x.job, |j| {
        j.phase = phase;
        j.finished_at = Some(runwick_core::now());
        j.message = Some(message);
        for s in j.steps.iter_mut().filter(|s| s.state == StepState::Pending) {
            s.state = StepState::Skipped;
        }
    });
}

async fn blocking<T: Send + 'static>(host: &Arc<Host>, f: impl FnOnce(&Host) -> T + Send + 'static) -> Option<T> {
    let h = host.clone();
    tokio::task::spawn_blocking(move || f(&h)).await.ok()
}

async fn save_record(host: &Arc<Host>, x: &Exec) -> Result<Option<String>, String> {
    let id = x.session;
    let s = blocking(host, move |h| sessions::session_info(h, id)).await.ok_or("세션을 읽지 못했어요")?;
    let s = s.map_err(|e| e.message)?.session;
    let conv = s.provider_conv_id.clone().ok_or("저장된 대화 ID가 없어요")?;
    match s.tool {
        Tool::Claude => {
            let (cwd, c) = (s.cwd.clone(), conv.clone());
            let saved = blocking(host, move |_| claude_transcript_exists(&c, &cwd)).await.unwrap_or(false);
            if saved {
                Ok(Some("Claude Code가 이 대화를 저장해 두었어요".into()))
            } else {
                Err("대화 기록 파일을 찾지 못해 같은 대화를 이어갈 수 없어요".into())
            }
        }
        _ => Ok(Some(format!("Codex 대화 {}", short(&conv)))),
    }
}

fn short(id: &str) -> String {
    id.chars().take(8).collect()
}

async fn restart(host: &Arc<Host>, x: &Exec) -> Result<Option<String>, String> {
    // Keep the run's current size so attached windows do not jump.
    let live = host.sessions.live(host, x.session);
    let (cols, rows) = live.as_ref().map(|l| (l.cols, l.rows)).unwrap_or((x.cols, x.rows));
    let resume = x.method == Some(Method::Resume);
    let p = Restart { id: x.session, resume, cols, rows };
    let info = sessions::restart_session(host.clone(), p).await.map_err(|e| e.message)?;
    if let Some(err) = info.live.as_ref().and_then(|l| l.error.clone()) {
        return Err(err);
    }
    Ok(Some(match x.method {
        Some(Method::RestartShell) => "새 셸을 시작했어요. 이전 셸의 명령은 끝났어요".into(),
        _ => "같은 세션에서 CLI를 다시 시작했어요".into(),
    }))
}

async fn reconnect(host: &Arc<Host>, x: &Exec) -> Result<Option<String>, String> {
    let id = x.session;
    let before = blocking(host, move |h| sessions::session_info(h, id).map(|i| i.session.provider_conv_id))
        .await
        .and_then(Result::ok)
        .flatten();
    tokio::time::sleep(ALIVE_FOR).await;
    let info = blocking(host, move |h| sessions::session_info(h, id)).await.ok_or("세션을 읽지 못했어요")?;
    let info = info.map_err(|e| e.message)?;
    let live = host.sessions.live(host, id);
    let alive = live.as_ref().is_some_and(|l| matches!(l.phase, Phase::Starting | Phase::Running));
    if !alive {
        let why = live
            .and_then(|l| l.error)
            .or_else(|| info.session.exit_code.map(|c| format!("종료 코드 {c}")))
            .unwrap_or_else(|| "이유를 알 수 없어요".into());
        return Err(format!("다시 시작한 CLI가 바로 끝났어요 ({why})"));
    }
    match x.method {
        Some(Method::Resume) => {
            if info.session.provider_conv_id != before || before.is_none() {
                return Err("대화 ID가 이전과 달라 성공으로 보지 않아요".into());
            }
            Ok(Some("대화 ID가 이전과 같음을 확인했어요".into()))
        }
        _ => Ok(Some("저장된 대화가 없어 같은 세션에서 새로 시작했어요".into())),
    }
}

/// Add check items; returns a note on how they were checked.
async fn verify(host: &Arc<Host>, x: &Exec) -> Option<String> {
    let id = x.session;
    let facts = blocking(host, move |h| {
        let info = sessions::session_info(h, id).ok()?;
        let current = crate::env::current_version(&h.store).ok()?;
        let run = RunRecord::load(&h.store, id).ok().flatten();
        Some((info.session, current, run))
    })
    .await
    .flatten();
    let Some((session, current, run)) = facts else {
        let detail = Some("세션을 읽지 못했어요".into());
        add_check(host, x, CheckItem { label: "설정 확인".into(), state: CheckState::Unknown, detail, mcp: None });
        return None;
    };
    let unreadable = run.as_ref().map(|r| r.env_unreadable.clone()).unwrap_or_default();
    let env = if !unreadable.is_empty() {
        CheckItem {
            label: format!("공통 환경변수 v{}", session.env_version),
            state: CheckState::Failed,
            detail: Some(format!("{} 값을 읽지 못해 빠졌어요. 키체인 잠금을 확인해 주세요", unreadable.join(", "))),
            mcp: None,
        }
    } else if session.env_version < current {
        CheckItem {
            label: format!("공통 환경변수 v{}", session.env_version),
            state: CheckState::Failed,
            detail: Some(format!("그사이 v{current}로 다시 바뀌었어요. 한 번 더 새로고침해 주세요")),
            mcp: None,
        }
    } else {
        CheckItem {
            label: format!("공통 환경변수 v{} 적용", session.env_version),
            state: CheckState::Ok,
            detail: None,
            mcp: None,
        }
    };
    add_check(host, x, env);
    if !session.tool.is_agent() {
        return None;
    }
    let (tool, cwd) = (session.tool, session.cwd.clone());
    let before = crate::mcp::last_result(tool, &Some(cwd.clone()));
    let result = blocking(host, move |h| crate::mcp::check_now(h, tool, Some(cwd))).await?;
    host.bus.changed(&[runwick_core::api::mcp::TOPIC]);
    for c in mcp_checks(tool, &result, before.as_ref(), &x.changed_mcp) {
        add_check(host, x, c);
    }
    Some(match tool {
        Tool::Claude => "MCP 상태는 ‘claude mcp list’로 따로 확인했어요".into(),
        _ => "MCP는 ‘codex mcp list’ 기준이에요. 연결 시험은 Codex 안에서만 돼요".into(),
    })
}

fn add_check(host: &Host, x: &Exec, item: CheckItem) {
    update(host, x.session, x.job, |j| j.checks.push(item));
}

/// Check items from a status result. Servers whose definition changed (or that a previous
/// result left open) and servers that were connected at the previous check but are not now are
/// listed one by one and decide the result; the rest are summarised as background information.
pub fn mcp_checks(
    tool: Tool,
    result: &runwick_core::api::mcp::McpCheckResult,
    before: Option<&runwick_core::api::mcp::McpCheckResult>,
    changed: &BTreeSet<String>,
) -> Vec<CheckItem> {
    if let Some(e) = &result.error {
        return vec![CheckItem {
            label: "MCP 상태".into(),
            state: CheckState::Unknown,
            detail: Some(e.clone()),
            mcp: None,
        }];
    }
    let was_connected = |name: &str| {
        before.is_some_and(|b| b.statuses.iter().any(|s| s.name == name && s.health == McpHealth::Connected))
    };
    let auth_hint = match tool {
        Tool::Claude => "Claude에서 /mcp를 열어 인증을 마치면 연결돼요",
        _ => "‘codex mcp login <이름>’으로 로그인해 주세요",
    };
    let mut items = Vec::new();
    let (mut ok_rest, mut off_rest, mut unknown) = (Vec::new(), Vec::new(), Vec::new());
    for s in &result.statuses {
        // Changed servers are reported by name; so is one that was connected before and is not
        // now (a regression). Unchanged servers that still work are only counted.
        let tracked = changed.contains(&s.name) || was_connected(&s.name);
        let item = match s.health {
            McpHealth::Disabled => continue,
            McpHealth::Connected if changed.contains(&s.name) => CheckItem {
                label: format!("MCP {} 연결", s.name),
                state: CheckState::Ok,
                detail: None,
                mcp: Some(s.name.clone()),
            },
            McpHealth::Connected => {
                ok_rest.push(s.name.clone());
                continue;
            }
            McpHealth::Unknown => {
                unknown.push(s.name.clone());
                continue;
            }
            _ if !tracked => {
                off_rest.push(s.name.clone());
                continue;
            }
            McpHealth::NeedsAuth => CheckItem {
                label: format!("MCP {} 인증 필요", s.name),
                state: CheckState::NeedsAuth,
                detail: Some(auth_hint.into()),
                mcp: Some(s.name.clone()),
            },
            McpHealth::PendingApproval => CheckItem {
                label: format!("MCP {} 승인 필요", s.name),
                state: CheckState::NeedsAuth,
                detail: Some("Claude에서 이 프로젝트의 MCP 사용을 승인해 주세요".into()),
                mcp: Some(s.name.clone()),
            },
            McpHealth::Failed => CheckItem {
                label: format!("MCP {} 연결 실패", s.name),
                state: CheckState::Failed,
                detail: s.detail.clone(),
                mcp: Some(s.name.clone()),
            },
        };
        items.push(item);
    }
    if !ok_rest.is_empty() {
        items.push(CheckItem {
            label: format!("MCP {}개 연결", ok_rest.len()),
            state: CheckState::Ok,
            detail: Some(ok_rest.join(", ")),
            mcp: None,
        });
    }
    if !off_rest.is_empty() {
        // "Unrelated" only when each of them was observed not connected before this refresh.
        let seen_off_before = |name: &String| {
            before.is_some_and(|b| b.statuses.iter().any(|s| &s.name == name && s.health != McpHealth::Connected))
        };
        let (label, when) = if off_rest.iter().all(seen_off_before) {
            (format!("연결되지 않은 MCP {}개 · 이번 변경과 무관", off_rest.len()), "새로고침 전에도 연결되지 않았어요")
        } else {
            (
                format!("연결되지 않은 MCP {}개", off_rest.len()),
                "새로고침 전 상태를 확인하지 않아 이번 변경 때문인지 알 수 없어요",
            )
        };
        items.push(CheckItem {
            label,
            state: CheckState::Unknown,
            detail: Some(format!("{} · {when}", off_rest.join(", "))),
            mcp: None,
        });
    }
    if !unknown.is_empty() {
        items.push(CheckItem {
            label: format!("MCP {}개 확인 불가", unknown.len()),
            state: CheckState::Unknown,
            detail: Some(unknown.join(", ")),
            mcp: None,
        });
    }
    items
}

/// MCP servers a previous result left unresolved (checked again by name on a retry).
pub fn unresolved_names(job: &Job) -> BTreeSet<String> {
    job.checks.iter().filter(|c| c.state != CheckState::Ok).filter_map(|c| c.mcp.clone()).collect()
}

/// Names of servers a refresh changes for this tool.
pub fn changed_names(plan: &Plan) -> BTreeSet<String> {
    plan.mcp_changes
        .iter()
        .flatten()
        .filter(|c| c.provider == plan.tool && c.kind != McpChangeKind::Removed)
        .map(|c| c.name.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use runwick_core::api::mcp::{McpCheckResult, McpStatus};
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Plays back one observation per poll, then repeats the last.
    struct Script {
        seq: Vec<Observed>,
        n: AtomicUsize,
    }

    impl Signals for Script {
        fn observe(&self, _: SessionId) -> Observed {
            let i = self.n.fetch_add(1, Ordering::SeqCst).min(self.seq.len() - 1);
            self.seq[i]
        }
    }

    fn obs(alive: bool, busy: bool, needs_answer: bool, since_input: Option<u64>) -> Observed {
        Observed { alive, busy, needs_answer, since_input: since_input.map(Duration::from_secs) }
    }

    async fn wait(seq: Vec<Observed>, wanted_for: usize) -> (Waited, usize) {
        let s = Script { seq, n: AtomicUsize::new(0) };
        let calls = AtomicUsize::new(0);
        let w = wait_until_quiet(
            &s,
            Uuid::nil(),
            || calls.fetch_add(1, Ordering::SeqCst) < wanted_for,
            Duration::from_millis(1),
            2,
        )
        .await;
        (w, s.n.load(Ordering::SeqCst))
    }

    #[tokio::test]
    async fn applies_only_after_output_stops_and_stays_quiet() {
        let busy = obs(true, true, false, Some(60));
        let quiet = obs(true, false, false, Some(60));
        // busy, busy, quiet, busy (the response continued), quiet, quiet → ready on the 6th look.
        let (w, looks) = wait(vec![busy, busy, quiet, busy, quiet, quiet], 100).await;
        assert_eq!((w, looks), (Waited::Ready, 6));
    }

    #[tokio::test]
    async fn waits_for_prompts_and_recent_typing() {
        let prompt = obs(true, false, true, None);
        let typing = obs(true, false, false, Some(2));
        let quiet = obs(true, false, false, None);
        let (w, looks) = wait(vec![prompt, prompt, typing, typing, quiet, quiet], 100).await;
        assert_eq!((w, looks), (Waited::Ready, 6));
    }

    #[tokio::test]
    async fn stops_when_the_run_ends_or_the_reservation_is_cancelled() {
        let busy = obs(true, true, false, None);
        let ended = obs(false, false, false, None);
        assert_eq!(wait(vec![busy, busy, ended], 100).await.0, Waited::Ended);
        assert_eq!(wait(vec![busy], 5).await, (Waited::Cancelled, 5));
    }

    #[test]
    fn result_messages_never_claim_the_same_conversation_without_resume() {
        assert!(done_message(Some(Method::Resume), false).contains("같은 대화"));
        for (m, v) in
            [(Some(Method::StartFresh), false), (Some(Method::RestartShell), false), (Some(Method::RestartShell), true)]
        {
            assert!(!done_message(m, v).contains("같은 대화"), "{m:?} {v}");
            assert!(!partial_message(m, v, &["x".into()]).contains("같은 대화"), "{m:?} {v}");
        }
        assert_eq!(done_message(Some(Method::RestartShell), true), "다시 확인했어요. 남은 문제가 없어요");
        assert_eq!(
            partial_message(Some(Method::Resume), false, &["MCP linear 인증 필요".into()]),
            "같은 대화로 다시 연결했어요. 남은 항목: MCP linear 인증 필요"
        );
    }

    #[test]
    fn steps_match_the_method() {
        let kinds = |m, v| steps_for(m, v).into_iter().map(|s| s.kind).collect::<Vec<_>>();
        assert_eq!(kinds(Some(Method::Resume), false).len(), 4);
        assert_eq!(kinds(Some(Method::StartFresh), false), [StepKind::Restart, StepKind::Alive, StepKind::Verify]);
        assert_eq!(kinds(Some(Method::RestartShell), false), [StepKind::Restart, StepKind::Verify]);
        assert_eq!(kinds(Some(Method::Resume), true), [StepKind::Verify]);
    }

    #[test]
    fn only_changed_or_previously_connected_servers_decide_the_result() {
        let st = |name: &str, health| McpStatus { name: name.into(), health, detail: None };
        let result = McpCheckResult {
            provider: Tool::Claude,
            cwd: None,
            checked_at: runwick_core::now(),
            statuses: vec![
                st("linear", McpHealth::NeedsAuth),
                st("github", McpHealth::Connected),
                st("gmail", McpHealth::NeedsAuth),
                st("db", McpHealth::Failed),
                st("off", McpHealth::Disabled),
            ],
            error: None,
        };
        let before = McpCheckResult {
            statuses: vec![st("db", McpHealth::Connected), st("gmail", McpHealth::NeedsAuth)],
            ..result.clone()
        };
        let changed: BTreeSet<String> = ["linear".to_string()].into();
        let items = mcp_checks(Tool::Claude, &result, Some(&before), &changed);
        let got: Vec<(&str, CheckState)> = items.iter().map(|i| (i.label.as_str(), i.state)).collect();
        assert_eq!(
            got,
            vec![
                ("MCP linear 인증 필요", CheckState::NeedsAuth),
                ("MCP db 연결 실패", CheckState::Failed),
                ("MCP 1개 연결", CheckState::Ok),
                ("연결되지 않은 MCP 1개 · 이번 변경과 무관", CheckState::Unknown),
            ]
        );
        assert_eq!(items[0].mcp.as_deref(), Some("linear"));
        // Still connected and unchanged: counted, not listed by name.
        let again = McpCheckResult { statuses: vec![st("github", McpHealth::Connected)], ..result.clone() };
        let quiet = mcp_checks(Tool::Claude, &again, Some(&again), &BTreeSet::new());
        assert_eq!(quiet.iter().map(|i| i.label.as_str()).collect::<Vec<_>>(), ["MCP 1개 연결"]);
        // Without an earlier check nothing is called unrelated to the change.
        let unseen = mcp_checks(Tool::Claude, &result, None, &changed);
        assert!(unseen.iter().any(|i| i.label == "연결되지 않은 MCP 2개"), "{unseen:?}");
        assert!(unseen.iter().all(|i| !i.label.contains("무관")), "{unseen:?}");
        let job = Job {
            id: Uuid::nil(),
            session: Uuid::nil(),
            when: When::Now,
            phase: JobPhase::Partial,
            steps: vec![],
            checks: items.clone(),
            created_at: runwick_core::now(),
            finished_at: None,
            message: None,
        };
        let open: Vec<String> = unresolved_names(&job).into_iter().collect();
        assert_eq!(open, ["db", "linear"]);
        let err = McpCheckResult { error: Some("시간 초과".into()), statuses: vec![], ..result };
        assert_eq!(mcp_checks(Tool::Claude, &err, None, &changed)[0].state, CheckState::Unknown);
    }
}
