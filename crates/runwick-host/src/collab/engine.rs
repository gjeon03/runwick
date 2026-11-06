//! Delivery and tracking of delegated work.
//!
//! While any request is open the loop looks at each one about once a second; with none open it
//! sleeps until a request changes (no idle wake-ups).
//!
//! - **Delivery** (queued → delivered): only when the worker's CLI is running and quiet, shows
//!   no prompt for the person, nobody typed into it just now, a person is not talking to it
//!   directly, its parent is not held, earlier requests to it are finished, and — for work an
//!   agent assigned — the automatic assignment limit has room ([`in_use`]). The text goes in as
//!   a bracketed paste, then Enter.
//! - **Tracking**: output that keeps coming after the delivery means the worker took it
//!   (running); a recognised prompt means it needs the person; the conversation record's turn
//!   end — or, without one, a quiet screen after running — means it answered (done). No
//!   response within a timeout is "전달 확인 필요" (uncertain) and is never re-sent.
//! - A worker whose process ended before answering fails the request; what it changed stays.

use super::{changes, db, guidance};
use crate::host::Host;
use runwick_core::Ts;
use runwick_core::api::collab::{ReqStatus, Request, Source, Via};
use runwick_core::api::sessions::{AttentionKind, Phase, SessionId, SessionLive, Tool};
use runwick_core::api::timeline::{EventKind, EventLink, NewEvent};
use runwick_core::rpc::topic;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Timings of delivery and tracking (tests use shorter ones).
#[derive(Debug, Clone)]
pub struct Timing {
    pub tick: Duration,
    /// Output must still be coming this long after a typed delivery to count as taken.
    pub confirm_typed: Duration,
    /// The same for a first prompt given at launch (the CLI draws its start screen first).
    pub confirm_launch: Duration,
    pub uncertain_typed: Duration,
    pub uncertain_launch: Duration,
    /// A running worker quiet this long (without a recognised prompt) has answered.
    pub quiet_done: Duration,
    /// Deliver only after the worker has been quiet this long.
    pub ready_quiet: Duration,
    /// Deliver only when the person has not typed into the worker for this long.
    pub human_quiet: Duration,
    /// `--now` stops a worker's answer only when the person has not typed into it for this long.
    pub interrupt_quiet: Duration,
    /// A line the person left unsent this long is shown as "사람이 입력 중이라 대기".
    pub typed_line_notice: Duration,
    pub enter_delay: Duration,
    /// After a worker answered, how long `wait` gives its conversation record to catch up.
    pub answer_grace: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            tick: Duration::from_secs(1),
            confirm_typed: Duration::from_secs(2),
            confirm_launch: Duration::from_secs(6),
            uncertain_typed: Duration::from_secs(45),
            uncertain_launch: Duration::from_secs(120),
            quiet_done: Duration::from_secs(10),
            ready_quiet: Duration::from_millis(1500),
            human_quiet: Duration::from_secs(5),
            interrupt_quiet: Duration::from_secs(60),
            typed_line_notice: Duration::from_secs(120),
            enter_delay: Duration::from_millis(150),
            answer_grace: Duration::from_secs(8),
        }
    }
}

pub struct Runtime {
    pub timing: Timing,
    wake: tokio::sync::Notify,
    /// Bumped on every change; `collab.wait` watches it.
    version: tokio::sync::watch::Sender<u64>,
    /// Requests whose worker was already asked to stop its current answer (`--now`).
    interrupted: Mutex<HashSet<uuid::Uuid>>,
    /// Serialises "is a slot free?" with starting the worker.
    pub assign: tokio::sync::Mutex<()>,
}

impl Runtime {
    fn new(timing: Timing) -> Self {
        Self {
            timing,
            wake: tokio::sync::Notify::new(),
            version: tokio::sync::watch::channel(0).0,
            interrupted: Mutex::new(HashSet::new()),
            assign: tokio::sync::Mutex::new(()),
        }
    }

    /// Something changed: look again now and tell waiters.
    pub fn poke(&self) {
        self.wake.notify_one();
        self.version.send_modify(|v| *v += 1);
    }

    pub fn subscribe(&self) -> tokio::sync::watch::Receiver<u64> {
        self.version.subscribe()
    }
}

static RUNTIMES: Mutex<Vec<(usize, Arc<Runtime>)>> = Mutex::new(Vec::new());

/// The collaboration runtime of this host (one per host instance).
pub fn of(host: &Arc<Host>) -> Arc<Runtime> {
    install(host, Timing::default())
}

/// Create the runtime with `timing` unless it exists already.
pub fn install(host: &Arc<Host>, timing: Timing) -> Arc<Runtime> {
    let key = Arc::as_ptr(host) as usize;
    let mut all = RUNTIMES.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((_, r)) = all.iter().find(|(k, _)| *k == key) {
        return r.clone();
    }
    let r = Arc::new(Runtime::new(timing));
    all.push((key, r.clone()));
    r
}

pub async fn run(host: Arc<Host>, rt: Arc<Runtime>) {
    loop {
        let any_open = tick(&host, &rt).await;
        if any_open {
            tokio::select! {
                _ = tokio::time::sleep(rt.timing.tick) => {}
                _ = rt.wake.notified() => {}
            }
        } else {
            rt.wake.notified().await;
        }
    }
}

fn after(ts: Ts, d: Duration) -> Ts {
    ts + chrono::Duration::from_std(d).unwrap_or_else(|_| chrono::Duration::seconds(0))
}

fn elapsed(since: Ts, now: Ts) -> Duration {
    (now - since).to_std().unwrap_or_default()
}

pub fn worker_cwd(host: &Host, worker: SessionId) -> Option<PathBuf> {
    crate::sessions::session_record(&host.store, worker).ok().map(|s| PathBuf::from(s.cwd))
}

/// Look at every open request once. Returns whether any request is open.
async fn tick(host: &Arc<Host>, rt: &Runtime) -> bool {
    let open = match db::open(&host.store) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("collab: cannot read requests: {}", e.message);
            return true;
        }
    };
    if open.is_empty() {
        return false;
    }
    // Deliveries and new delegations decide on free slots one at a time.
    let _assign = rt.assign.lock().await;
    let ctx = Ctx {
        limit: guidance::load(&host.store).auto_limit as usize,
        held: db::held_parents(&host.store).unwrap_or_default().into_iter().collect(),
        direct: super::live_direct(host).unwrap_or_default().into_iter().collect(),
        busy: busy_agents(host),
    };
    let mut slots = slot_workers(&open);
    let working = folders_at_work(host);
    let mut first_in_line: HashSet<SessionId> = HashSet::new();
    let mut changed = false;
    for mut r in open {
        let before = (r.status, r.reason.clone());
        let was_open = r.status.holds_slot();
        let live = host.sessions.live(host, r.worker_id);
        if r.status == ReqStatus::Queued {
            let first = first_in_line.insert(r.worker_id);
            match wait_reason(host, rt, &r, live.as_ref(), first, &slots, &ctx) {
                Some(reason) => r.reason = Some(reason),
                None => {
                    deliver(host, rt, &mut r).await;
                    if r.status.holds_slot() {
                        slots.insert(r.worker_id);
                    }
                }
            }
        } else {
            track(host, rt, &mut r, live.as_ref());
        }
        let shared = (was_open || r.status.holds_slot())
            && !r.result.as_ref().is_some_and(|x| x.shared_folder)
            && shares_folder(host, &r, &working);
        if shared {
            r.result.get_or_insert_with(Default::default).shared_folder = true;
        }
        if shared || (r.status, &r.reason) != (before.0, &before.1) {
            let now = runwick_core::now();
            r.updated_at = now;
            if !r.status.is_open() {
                r.finished_at = Some(now);
                finalize(host, &mut r).await;
            }
            if let Err(e) = db::save(&host.store, &r) {
                tracing::warn!("collab: cannot save request: {}", e.message);
            }
            changed = true;
        }
    }
    if changed {
        host.bus.changed(&[topic::COLLAB]);
        rt.version.send_modify(|v| *v += 1);
    }
    true
}

struct Ctx {
    limit: usize,
    held: HashSet<SessionId>,
    direct: HashSet<SessionId>,
    busy: HashSet<SessionId>,
}

/// Agent CLI runs observed responding right now, whoever gave them their turn (a person typing
/// directly included). Runs a CLI starts on its own (built-in subagents) cannot be observed.
pub fn busy_agents(host: &Arc<Host>) -> HashSet<SessionId> {
    host.sessions
        .running_ids()
        .into_iter()
        .filter(|id| host.sessions.live(host, *id).is_some_and(|l| l.busy))
        .filter(|id| crate::sessions::session_record(&host.store, *id).is_ok_and(|s| s.tool.is_agent()))
        .collect()
}

/// Runs counted against the automatic assignment limit for new work from `asker`: workers
/// holding an assignment slot plus other agent runs observed responding. The asking session is
/// not counted — it waits for its workers, and counting it could keep them from ever starting.
pub fn in_use(slots: &HashSet<SessionId>, busy: &HashSet<SessionId>, asker: Option<SessionId>) -> usize {
    slots.union(busy).filter(|id| Some(**id) != asker).count()
}

/// Workers holding an assignment slot (delivered, running, needs input or uncertain).
pub fn slot_workers(open: &[Request]) -> HashSet<SessionId> {
    open.iter().filter(|r| r.status.holds_slot()).map(|r| r.worker_id).collect()
}

/// Runs observed working right now (any tool) and their folders.
fn folders_at_work(host: &Arc<Host>) -> Vec<(SessionId, PathBuf)> {
    host.sessions
        .running_ids()
        .into_iter()
        .filter(|id| host.sessions.live(host, *id).is_some_and(|l| l.busy))
        .filter_map(|id| worker_cwd(host, id).map(|cwd| (id, cwd)))
        .collect()
}

/// Another run works in the worker's folder (or a folder inside it, or around it), so files
/// read from the folder may be its changes too. The session that asked for the work is not
/// counted: it waits for the worker (its spinner is output too) and knows what it changed.
fn shares_folder(host: &Host, r: &Request, working: &[(SessionId, PathBuf)]) -> bool {
    let Some(mine) = worker_cwd(host, r.worker_id) else {
        return false;
    };
    working.iter().any(|(id, cwd)| {
        *id != r.worker_id && Some(*id) != r.parent_id && (cwd.starts_with(&mine) || mine.starts_with(cwd))
    })
}

/// Why a queued request cannot be delivered now (`None`: deliver it).
fn wait_reason(
    host: &Arc<Host>,
    rt: &Runtime,
    r: &Request,
    live: Option<&SessionLive>,
    first: bool,
    slots: &HashSet<SessionId>,
    ctx: &Ctx,
) -> Option<String> {
    let t = &rt.timing;
    if !first || slots.contains(&r.worker_id) {
        return Some("앞서 맡긴 일이 끝나면 전달해요".into());
    }
    if r.parent_id.is_some_and(|p| ctx.held.contains(&p)) {
        return Some("새 배정 보류 중이에요 · 배정을 재개하면 전달해요".into());
    }
    if ctx.direct.contains(&r.worker_id) {
        return Some("직접 조작 중이에요 · 다시 맡기면 전달해요".into());
    }
    let Some(l) = live else {
        return Some("작업자가 실행 중이 아니에요 · 다시 실행하면 전달해요".into());
    };
    match l.phase {
        Phase::Starting => return Some("작업자 CLI가 시작되는 중이에요".into()),
        Phase::Exited | Phase::NotRunning => {
            return Some("작업자가 실행 중이 아니에요 · 다시 실행하면 전달해요".into());
        }
        Phase::Running => {}
    }
    // A bell alone is not a question; a dialog is.
    if l.attention.as_ref().is_some_and(|a| a.kind != AttentionKind::Notify) {
        return Some("작업자가 답변을 기다려요 · CLI 화면에서 답해 주세요".into());
    }
    if r.source == Source::Agent && in_use(slots, &ctx.busy, r.parent_id) >= ctx.limit {
        return Some(limit_reason(ctx.limit));
    }
    if host.sessions.since_input(r.worker_id).is_some_and(|d| d < t.human_quiet) {
        return Some("사람이 입력하고 있어 잠시 기다려요".into());
    }
    // Never press Enter on a line the person typed and did not send; after a while say so plainly.
    if host.sessions.typed_line(r.worker_id) {
        let long = host.sessions.since_input(r.worker_id).is_none_or(|d| d >= t.typed_line_notice);
        return Some(if long {
            "사람이 입력 중이라 대기 · 보내거나 지우거나 다시 맡기면 전달해요".into()
        } else {
            "사람이 입력하고 있어 잠시 기다려요".into()
        });
    }
    let now = runwick_core::now();
    let quiet = l.last_output_at.is_none_or(|o| elapsed(o, now) >= t.ready_quiet);
    if l.busy || !quiet {
        // `--now` stops the answer only when nobody has typed into the worker for a while.
        let person_near = host.sessions.since_input(r.worker_id).is_some_and(|d| d < t.interrupt_quiet);
        if r.interrupt && person_near {
            return Some("사람이 쓰던 작업자라 응답을 멈추지 않고 기다려요".into());
        }
        if r.interrupt {
            let first_ask = rt.interrupted.lock().map(|mut s| s.insert(r.id)).unwrap_or(false);
            if first_ask {
                // Esc stops the current answer in Claude Code and Codex.
                host.sessions.type_into(r.worker_id, b"\x1b");
            }
            return Some("현재 응답을 멈추도록 요청했어요 · 멈추면 전달해요".into());
        }
        return Some("작업자가 응답 중이에요 · 끝나면 전달해요".into());
    }
    // Typed only where the CLI shows its own input prompt; any other screen (a dialog, a
    // program it runs, something unknown) waits for the person.
    if !host.sessions.ready_for_input(r.worker_id) {
        return Some("작업자 화면이 입력 대기 상태로 보이지 않아요 · CLI 화면을 확인해 주세요".into());
    }
    None
}

/// Why automatic work waits for a slot.
pub fn limit_reason(limit: usize) -> String {
    format!("자동 배정 한도 {limit}개가 모두 쓰이고 있어요 · 진행 중인 일이 끝나면 전달해요")
}

/// Text typed into the worker: the task, plus where to report for work another session gave a
/// Claude worker while guidance is on. Codex gets the task only: its default sandbox cannot reach
/// the host's socket, so `runwick report` would fail there; its answer is read from its record.
pub fn delivery_text(r: &Request, guidance_on: bool) -> String {
    let mut text = r.prompt.trim().to_string();
    if guidance_on && r.parent_id.is_some() && r.tool == Tool::Claude {
        text.push_str(&format!(
            "\n\n(Runwick {} · 다른 세션이 맡긴 일이에요. 끝나면 `runwick report --summary \"...\"`로 결과를 남겨 주세요.)",
            r.receipt()
        ));
    }
    text
}

async fn deliver(host: &Arc<Host>, rt: &Runtime, r: &mut Request) {
    if let Some(cwd) = worker_cwd(host, r.worker_id) {
        r.start_rev = changes::head(&cwd).await;
    }
    let text = delivery_text(r, guidance::load(&host.store).guidance);
    if !host.sessions.paste(r.worker_id, &text) {
        r.reason = Some("전달하지 못했어요 · 작업자가 실행 중이 아니에요".into());
        return;
    }
    tokio::time::sleep(rt.timing.enter_delay).await;
    host.sessions.type_into(r.worker_id, b"\r");
    r.status = ReqStatus::Delivered;
    r.via = Via::Typed;
    r.delivered_at = Some(runwick_core::now());
    r.reason = None;
    tracing::info!(request = %r.id, worker = %r.worker_id, "collab: delivered");
}

/// Why the worker waits, in words only: what its screen says stays on its screen (it is not
/// passed to the session that delegated, as text a program printed could pose as a request).
fn attention_reason(kind: AttentionKind) -> String {
    let what = match kind {
        AttentionKind::Approval => "승인 필요",
        AttentionKind::Auth => "인증·신뢰 확인 필요",
        AttentionKind::Notify => "확인 필요",
        AttentionKind::Choice => return runwick_core::api::activity::CHOICE_NEEDED.into(),
    };
    format!("{what} · CLI 화면에서 답해 주세요")
}

/// Follow a delivered request (pure on observed state, except reading the turn record).
fn track(host: &Arc<Host>, rt: &Runtime, r: &mut Request, live: Option<&SessionLive>) {
    let t = &rt.timing;
    let now = runwick_core::now();
    let Some(l) = live.filter(|l| matches!(l.phase, Phase::Running | Phase::Starting)) else {
        r.status = ReqStatus::Failed;
        r.reason = Some(match live.and_then(|l| l.error.clone()) {
            Some(e) => format!("작업자를 시작하지 못했어요: {e}"),
            None => match crate::sessions::session_record(&host.store, r.worker_id).ok().and_then(|s| s.exit_code) {
                Some(code) => format!("응답을 마치기 전에 작업자 실행이 끝났어요 (코드 {code})"),
                None => "응답을 마치기 전에 작업자 실행이 끝났어요".into(),
            },
        });
        return;
    };
    let delivered = r.delivered_at.unwrap_or(r.created_at);
    if let Some(a) = l.attention.as_ref().filter(|a| a.kind != AttentionKind::Notify) {
        r.status = ReqStatus::NeedsInput;
        r.reason = Some(attention_reason(a.kind));
        return;
    }
    let turn_ended = crate::records::turn_info(&host.store, r.worker_id)
        .ok()
        .and_then(|t| t.turn_ended_at)
        .is_some_and(|e| e > delivered);
    if turn_ended && !l.busy {
        r.status = ReqStatus::Done;
        r.reason = None;
        return;
    }
    let confirm = if r.via == Via::Launch { t.confirm_launch } else { t.confirm_typed };
    let uncertain = if r.via == Via::Launch { t.uncertain_launch } else { t.uncertain_typed };
    let taken = l.last_output_at.is_some_and(|o| o > after(delivered, confirm));
    match r.status {
        ReqStatus::Delivered | ReqStatus::Uncertain if taken => {
            r.status = ReqStatus::Running;
            r.reason = None;
        }
        ReqStatus::Delivered if now > after(delivered, uncertain) => {
            r.status = ReqStatus::Uncertain;
            r.reason = Some("전달했지만 응답을 확인하지 못했어요. 자동으로 다시 보내지 않았어요".into());
        }
        ReqStatus::NeedsInput => {
            r.status = ReqStatus::Running;
            r.reason = None;
        }
        ReqStatus::Running => {
            let quiet = l.last_output_at.is_none_or(|o| elapsed(o, now) >= t.quiet_done);
            if !l.busy && quiet {
                r.status = ReqStatus::Done;
                r.reason = None;
            }
        }
        _ => {}
    }
}

/// Look at an uncertain request again on request. Never re-sends.
pub fn recheck(host: &Arc<Host>, rt: &Runtime, r: &mut Request) {
    let live = host.sessions.live(host, r.worker_id);
    track(host, rt, r, live.as_ref());
    if r.status == ReqStatus::Uncertain {
        r.reason = Some("다시 확인했지만 응답이 보이지 않아요. CLI 화면에서 직접 확인해 주세요".into());
    }
}

/// Read what the worker changed when its request ends, and note it on the timeline.
pub async fn finalize(host: &Arc<Host>, r: &mut Request) {
    let mut res = r.result.take().unwrap_or_default();
    if let Some(cwd) = worker_cwd(host, r.worker_id).filter(|c| c.is_dir()) {
        res.files = changes::files(&cwd, r.start_rev.as_deref()).await;
        res.head_rev = changes::head(&cwd).await;
        res.fingerprint = changes::fingerprint(&cwd).await;
    }
    res.base_rev = r.start_rev.clone();
    res.collected_at = Some(runwick_core::now());
    r.result = Some(res);
    let outcome = match r.status {
        ReqStatus::Done => "응답 완료",
        ReqStatus::Cancelled => "취소",
        _ => "실패",
    };
    note_timeline(host, r, &format!("{} · {outcome}", r.title), "end");
}

pub fn note_timeline(host: &Arc<Host>, r: &Request, title: &str, phase: &str) {
    let Ok(worker) = crate::sessions::session_record(&host.store, r.worker_id) else {
        return;
    };
    let ev = NewEvent {
        project_id: worker.project_id,
        kind: EventKind::Collab,
        title: title.to_string(),
        at: runwick_core::now(),
        session_id: r.parent_id.or(Some(r.worker_id)),
        link: Some(EventLink::Collab { request: r.id, session_id: Some(r.worker_id) }),
        dedupe_key: Some(format!("collab:{}:{phase}", r.id)),
    };
    match crate::timeline::record_event(&host.store, &ev) {
        Ok(()) => host.bus.changed(&[topic::TIMELINE]),
        Err(e) => tracing::debug!("collab: timeline note skipped: {}", e.message),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use runwick_core::api::collab::Source;
    use runwick_core::api::sessions::Tool;

    #[test]
    fn delivery_text_mentions_report_only_for_delegated_work_with_guidance() {
        let now = runwick_core::now();
        let mut r = Request {
            id: uuid::Uuid::now_v7(),
            parent_id: Some(uuid::Uuid::now_v7()),
            worker_id: uuid::Uuid::now_v7(),
            previous_id: None,
            attempt: 1,
            title: "검토".into(),
            role: None,
            tool: Tool::Codex,
            prompt: "  변경을 검토해 줘 \n".into(),
            source: Source::Agent,
            via: Via::Typed,
            interrupt: false,
            status: ReqStatus::Queued,
            reason: None,
            start_rev: None,
            created_at: now,
            delivered_at: None,
            finished_at: None,
            updated_at: now,
            result: None,
            acknowledged: false,
            notes: vec![],
        };
        let t = delivery_text(&r, true);
        assert_eq!(t, "변경을 검토해 줘", "Codex gets the task only (its sandbox cannot reach runwick)");
        r.tool = Tool::Claude;
        let t = delivery_text(&r, true);
        assert!(t.starts_with("변경을 검토해 줘\n\n(Runwick r-") && t.contains("runwick report"), "{t}");
        assert_eq!(delivery_text(&r, false), "변경을 검토해 줘");
        r.parent_id = None;
        assert_eq!(delivery_text(&r, true), "변경을 검토해 줘");
        assert_eq!(attention_reason(AttentionKind::Approval), "승인 필요 · CLI 화면에서 답해 주세요");
    }
}
