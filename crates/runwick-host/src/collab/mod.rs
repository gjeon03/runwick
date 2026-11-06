//! Delegation between sessions (S04/S05/S11): requests, delivery into worker terminals, turn
//! tracking, structured results, holds and direct conversation.
//!
//! - `db`: tables; `engine`: delivery and tracking loop; `changes`: what a worker changed (git);
//!   `guidance`: settings and the launch guidance for agent CLIs.
//!
//! The same operations serve the app and the `runwick` agent command, so both see one state.

mod changes;
mod db;
pub mod engine;
mod guidance;
mod identity;

use crate::host::{Ctx, Host};
use crate::router::Router;
use crate::store::Migration;
use engine::Runtime;
pub(crate) use identity::parent_pid;
pub use identity::{Caller, peer_session};
use runwick_core::api::collab::*;
use runwick_core::api::projects::{ById, WorkspaceId, WorkspaceKind};
use runwick_core::api::sessions::{Restart, SessionId, StartSession, Tool};
use runwick_core::rpc::{RpcError, codes, topic};
use std::sync::Arc;
use std::time::{Duration, Instant};
use uuid::Uuid;

pub const MIGRATIONS: &[Migration] = &[("collab_1", db::MIGRATION)];

/// Longest instruction accepted (a task description, not a file transfer).
const MAX_PROMPT: usize = 100 * 1024;
const DEFAULT_WAIT: u64 = 600;
const MAX_WAIT: u64 = 3600;

pub fn register(r: &mut Router) {
    // Every method that changes something first asks who is calling (`identity`): a window is
    // the person, a verified session is its agent, anything else may only read. What a request
    // says about itself (`source`, `parent_id`, `worker_id`) is never trusted.
    r.add::<CollabIdentify, _, _>(|ctx: Ctx, p: Identify| async move {
        // A stolen token proves nothing from a process outside its run (see `identity`).
        let id = ctx
            .host
            .sessions
            .session_for_token(p.token.trim())
            .filter(|id| identity::token_fits(&ctx.host, &ctx.client, *id));
        if let Some(id) = id {
            ctx.client.set_session(id);
        }
        Ok(id)
    });
    r.add::<CollabDelegate, _, _>(|ctx: Ctx, p: Delegate| async move {
        delegate_as(&ctx.host, identity::caller(&ctx.host, &ctx.client), p).await
    });
    r.add::<CollabSend, _, _>(|ctx: Ctx, p: SendTo| async move {
        send(&ctx.host, identity::caller(&ctx.host, &ctx.client), p)
    });
    r.add::<CollabWait, _, _>(|ctx: Ctx, p: WaitFor| async move { wait(&ctx.host, p).await });
    r.add::<CollabGet, _, _>(|ctx: Ctx, p: ById| async move { view(&ctx.host, p.id).await });
    r.add::<CollabList, _, _>(|ctx: Ctx, q: RequestQuery| async move { db::list(&ctx.host.store, &q) });
    r.add::<CollabReport, _, _>(|ctx: Ctx, p: Report| async move {
        report(&ctx.host, identity::caller(&ctx.host, &ctx.client), p).await
    });
    r.add::<CollabCancel, _, _>(|ctx: Ctx, p: ById| async move {
        let r = db::get(&ctx.host.store, p.id)?;
        person_or_owner(identity::caller(&ctx.host, &ctx.client), r.parent_id)?;
        cancel(&ctx.host, p.id)
    });
    r.add::<CollabHold, _, _>(|ctx: Ctx, p: Hold| async move {
        person_or_owner(identity::caller(&ctx.host, &ctx.client), Some(p.parent_id))?;
        db::set_hold(&ctx.host.store, p.parent_id, p.held)?;
        changed(&ctx.host);
        Ok(())
    });
    r.add::<CollabDirect, _, _>(|ctx: Ctx, p: Direct| async move {
        person_only(identity::caller(&ctx.host, &ctx.client))?;
        direct(&ctx.host, p)
    });
    r.add::<CollabAck, _, _>(|ctx: Ctx, p: Ack| async move {
        person_only(identity::caller(&ctx.host, &ctx.client))?;
        let mut r = db::get(&ctx.host.store, p.id)?;
        r.acknowledged = p.acknowledged;
        db::save(&ctx.host.store, &r)?;
        changed(&ctx.host);
        Ok(r)
    });
    r.add::<CollabRetry, _, _>(|ctx: Ctx, p: Retry| async move {
        retry(&ctx.host, identity::caller(&ctx.host, &ctx.client), p).await
    });
    r.add::<CollabCheck, _, _>(|ctx: Ctx, p: ById| async move {
        let r = db::get(&ctx.host.store, p.id)?;
        person_or_owner(identity::caller(&ctx.host, &ctx.client), r.parent_id)?;
        check(&ctx.host, p.id).await
    });
    r.add::<CollabOverview, _, _>(|ctx: Ctx, _: ()| async move { overview(&ctx.host) });
    r.add::<CollabSettingsGet, _, _>(|ctx: Ctx, _: ()| async move { Ok(guidance::load(&ctx.host.store)) });
    r.add::<CollabSettingsSet, _, _>(|ctx: Ctx, s: CollabSettings| async move {
        person_only(identity::caller(&ctx.host, &ctx.client))?;
        let s = guidance::save(&ctx.host.store, s)?;
        ctx.host.bus.changed(&[topic::SETTINGS, topic::COLLAB]);
        rt(&ctx.host).poke();
        Ok(s)
    });
    r.add::<CollabDiff, _, _>(|ctx: Ctx, q: DiffQuery| async move {
        // File contents are for the person's result view: an agent reads its own files itself,
        // within its CLI's permissions, not through the host.
        person_only(identity::caller(&ctx.host, &ctx.client))?;
        let r = db::get(&ctx.host.store, q.id)?;
        let cwd =
            engine::worker_cwd(&ctx.host, r.worker_id).ok_or_else(|| RpcError::not_found("작업자 기록이 없어요"))?;
        Ok(changes::diff(&cwd, r.start_rev.as_deref(), &q.path).await)
    });
}

fn denied(message: &str) -> RpcError {
    RpcError::new(codes::DENIED, message)
}

const UNVERIFIED: &str = "Runwick 창이나 Runwick 터미널 안의 runwick 명령만 할 수 있어요 / only a Runwick window or the runwick command inside a Runwick terminal can do this";

/// Only the person (a Runwick window).
fn person_only(caller: Caller) -> Result<(), RpcError> {
    match caller {
        Caller::Window => Ok(()),
        _ => Err(denied("Runwick 창에서만 할 수 있어요 / only from a Runwick window")),
    }
}

/// The person, or the agent of the session that owns the work (`owner`). Returns who it is.
fn person_or_owner(caller: Caller, owner: Option<SessionId>) -> Result<Source, RpcError> {
    match caller {
        Caller::Window => Ok(Source::Person),
        Caller::Session(me) if owner == Some(me) => Ok(Source::Agent),
        Caller::Session(_) => Err(denied("자기가 맡긴 일만 바꿀 수 있어요 / only work you delegated")),
        Caller::Unverified => Err(denied(UNVERIFIED)),
    }
}

/// Start the delivery loop (called once after the host starts).
pub fn boot(host: &Arc<Host>) {
    guidance::install(host);
    let rt = rt(host);
    let host = host.clone();
    tokio::spawn(async move {
        recover_after_restart(&host).await;
        engine::run(host, rt).await;
    });
}

fn rt(host: &Arc<Host>) -> Arc<Runtime> {
    engine::of(host)
}

fn changed(host: &Arc<Host>) {
    host.bus.changed(&[topic::COLLAB]);
    rt(host).poke();
}

/// A new host has no runs: delivered work cannot be followed any more, and queued work must
/// not be delivered before the person says so (its parents are held).
async fn recover_after_restart(host: &Arc<Host>) {
    let Ok(open) = db::open(&host.store) else {
        return;
    };
    let mut touched = false;
    for mut r in open {
        if r.status.holds_slot() && host.sessions.live(host, r.worker_id).is_none() {
            r.status = ReqStatus::Failed;
            r.reason = Some("호스트가 다시 시작되어 이 실행을 더 따라갈 수 없어요".into());
            r.updated_at = runwick_core::now();
            r.finished_at = Some(r.updated_at);
            engine::finalize(host, &mut r).await;
            let _ = db::save(&host.store, &r);
            touched = true;
        } else if r.status == ReqStatus::Queued
            && let Some(p) = r.parent_id
        {
            let _ = db::set_hold(&host.store, p, true);
            touched = true;
        }
    }
    if touched {
        tracing::info!("collab: requests reviewed after a host restart");
        host.bus.changed(&[topic::COLLAB]);
    }
}

/// First line of an instruction, shortened, as the title (no AI call for titles).
pub fn excerpt(text: &str, max: usize) -> String {
    let line = text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    let mut out: String = line.chars().take(max).collect();
    if line.chars().count() > max {
        out.push('…');
    }
    out
}

/// Longest report summary, check command and "not done" item kept (a report, not a log dump).
const MAX_SUMMARY: usize = 16 * 1024;
const MAX_ITEM: usize = 2 * 1024;
const MAX_ITEMS: usize = 200;
/// Longest check log kept (its end).
pub const MAX_LOG: usize = 64 * 1024;

/// Text another program gave us, as plain text: no control characters (escape sequences could
/// end a bracketed paste early and act as keys in a terminal); line breaks and tabs stay.
pub fn plain(text: &str) -> String {
    text.replace("\r\n", "\n")
        .chars()
        .map(|c| if c == '\r' { '\n' } else { c })
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
        .collect()
}

/// The first `max` bytes of `text` (at a character boundary).
fn head_of(text: &str, max: usize) -> String {
    let mut end = text.len().min(max);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}

/// The last `max` bytes of `text` (at a character boundary).
fn tail_of(text: &str, max: usize) -> String {
    let mut start = text.len().saturating_sub(max);
    while !text.is_char_boundary(start) {
        start += 1;
    }
    text[start..].to_string()
}

fn check_prompt(prompt: &str) -> Result<(), RpcError> {
    if prompt.trim().is_empty() {
        return Err(RpcError::invalid("맡길 내용이 비어 있어요"));
    }
    if prompt.len() > MAX_PROMPT {
        return Err(RpcError::invalid("맡길 내용이 너무 길어요 (100KB까지). 파일 경로를 알려 주세요"));
    }
    Ok(())
}

fn new_request(parent: Option<SessionId>, worker: SessionId, tool: Tool, prompt: &str) -> Request {
    let now = runwick_core::now();
    Request {
        id: Uuid::now_v7(),
        parent_id: parent,
        worker_id: worker,
        previous_id: None,
        attempt: 1,
        title: excerpt(prompt, 60),
        role: None,
        tool,
        prompt: prompt.trim().to_string(),
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
        notes: Vec::new(),
    }
}

/// Start delegated work. `p.source` and `p.parent_id` must already be decided by the host
/// (`delegate_as`); a client's claim is never passed here unchecked.
pub async fn delegate(host: &Arc<Host>, p: Delegate) -> Result<Delegated, RpcError> {
    delegate_attempt(host, p, None).await
}

/// A delegation from `caller`: a window delegates as the person (to the parent it names, if
/// any); a verified session delegates as its agent and becomes the parent, whatever the request
/// says; anyone else is refused.
async fn delegate_as(host: &Arc<Host>, caller: Caller, mut p: Delegate) -> Result<Delegated, RpcError> {
    match caller {
        Caller::Window => p.source = Source::Person,
        Caller::Session(me) => {
            p.parent_id = Some(me);
            p.source = Source::Agent;
            p.workspace_id = Some(agent_workspace(host, me, p.workspace_id)?);
        }
        Caller::Unverified => return Err(denied(UNVERIFIED)),
    }
    delegate(host, p).await
}

/// Where an agent may start a worker: its own workspace, or a separated (worktree) workspace of
/// its own project. Another project's or workspace's folder is the person's to choose (a window);
/// `--split` then branches from the agent's own workspace.
fn agent_workspace(host: &Host, me: SessionId, asked: Option<WorkspaceId>) -> Result<WorkspaceId, RpcError> {
    let mine = crate::sessions::session_record(&host.store, me)?;
    let Some(asked) = asked.filter(|w| *w != mine.workspace_id) else {
        return Ok(mine.workspace_id);
    };
    let ws = crate::projects::get_workspace(&host.store, asked)?;
    if ws.project_id == mine.project_id && ws.kind == WorkspaceKind::Worktree {
        return Ok(asked);
    }
    Err(denied(
        "다른 프로젝트나 공간에는 Runwick 창에서만 일을 맡길 수 있어요 / only a Runwick window can delegate into another project or workspace",
    ))
}

async fn delegate_attempt(host: &Arc<Host>, p: Delegate, previous: Option<&Request>) -> Result<Delegated, RpcError> {
    let mut p = p;
    p.prompt = plain(&p.prompt);
    p.title = p.title.as_deref().map(plain);
    p.role = p.role.as_deref().map(plain);
    check_prompt(&p.prompt)?;
    let settings = guidance::load(&host.store);
    if !p.tool.is_agent() {
        return Err(RpcError::invalid("셸에는 일을 맡길 수 없어요. claude 또는 codex를 고르세요"));
    }
    if !settings.allowed_tools.contains(&p.tool) {
        return Err(RpcError::new(
            codes::DENIED,
            format!(
                "{} 설정 › 협업에서 맡길 수 있는 도구로 허용되지 않았어요",
                runwick_core::josa::eun_neun(p.tool.label())
            ),
        ));
    }
    let key = p.key.as_deref().map(str::trim).filter(|k| !k.is_empty());
    if let Some(k) = key
        && let Some(existing) = db::by_key(&host.store, p.parent_id, k)?
    {
        let worker = crate::sessions::session_info(host, existing.worker_id)?;
        return Ok(Delegated { request: existing, worker, reused: true });
    }
    let parent = match p.parent_id {
        Some(id) => Some(
            crate::sessions::session_record(&host.store, id)
                .map_err(|_| RpcError::not_found("일을 맡기는 세션을 찾을 수 없어요"))?,
        ),
        None => None,
    };
    let mut workspace_id = p
        .workspace_id
        .or(parent.as_ref().map(|s| s.workspace_id))
        .ok_or_else(|| RpcError::invalid("어느 작업 공간에서 일할지 알 수 없어요"))?;
    if let Some(branch) = p.split.as_deref().map(str::trim).filter(|b| !b.is_empty()) {
        workspace_id = crate::worktree::split_for(host, workspace_id, branch).await?.id;
    }
    let ws = crate::projects::get_workspace(&host.store, workspace_id)?;

    let rt = rt(host);
    let _slot = rt.assign.lock().await;
    let held = p.parent_id.is_some_and(|pid| db::held_parents(&host.store).is_ok_and(|h| h.contains(&pid)));
    // The limit is for work agents assign; a person's own request goes first.
    let slots = engine::slot_workers(&db::open(&host.store)?);
    let full = p.source == Source::Agent
        && engine::in_use(&slots, &engine::busy_agents(host), p.parent_id) >= settings.auto_limit as usize;
    let mut req = new_request(p.parent_id, Uuid::nil(), p.tool, &p.prompt);
    req.title =
        p.title.as_deref().map(str::trim).filter(|t| !t.is_empty()).map(|t| excerpt(t, 80)).unwrap_or(req.title);
    req.role = p.role.as_deref().map(str::trim).filter(|r| !r.is_empty()).map(str::to_string);
    req.source = p.source;
    if let Some(prev) = previous {
        req.previous_id = Some(prev.id);
        req.attempt = prev.attempt + 1;
    }
    req.start_rev = changes::head(std::path::Path::new(&ws.path)).await;
    // The first instruction is typed once the worker's CLI shows its input prompt (never on its
    // command line, where other users could read it in the process list).
    let start = StartSession {
        workspace_id,
        tool: p.tool,
        title: Some(req.title.clone()),
        initial_prompt: None,
        parent_id: p.parent_id,
        role: req.role.clone(),
        cols: p.cols,
        rows: p.rows,
    };
    let worker = crate::sessions::start_session(host.clone(), start).await?;
    req.worker_id = worker.session.id;
    let start_error = worker.live.as_ref().and_then(|l| l.error.clone());
    if let Some(e) = start_error {
        req.status = ReqStatus::Failed;
        req.reason = Some(format!("작업자를 시작하지 못했어요: {e}"));
        req.finished_at = Some(runwick_core::now());
    } else {
        req.reason = Some(if held {
            "새 배정 보류 중이에요 · 배정을 재개하면 전달해요".into()
        } else if full {
            engine::limit_reason(settings.auto_limit as usize)
        } else {
            "작업자 CLI가 시작되는 중이에요".into()
        });
    }
    db::insert(&host.store, &req, key)?;
    drop(_slot);
    let who = req.role.clone().unwrap_or_else(|| p.tool.label().to_string());
    engine::note_timeline(host, &req, &format!("{who}에게 맡김: {}", req.title), "start");
    changed(host);
    tracing::info!(request = %req.id, worker = %req.worker_id, status = ?req.status, "collab: delegated");
    Ok(Delegated { request: req, worker, reused: false })
}

/// A follow-up for a worker. A person (window) may instruct any agent session; an agent only
/// its own workers. A message an agent sends UP to the session that gave it work is never typed
/// there: it is kept on the work as a notice for the person ([`WORKER_MESSAGE`]).
pub fn send(host: &Arc<Host>, caller: Caller, mut p: SendTo) -> Result<Request, RpcError> {
    p.prompt = plain(&p.prompt);
    check_prompt(&p.prompt)?;
    let target = crate::sessions::session_record(&host.store, p.worker_id)
        .map_err(|_| RpcError::not_found("작업자 세션을 찾을 수 없어요"))?;
    let (parent, source) = match caller {
        Caller::Window => (p.parent_id.or(target.parent_id), Source::Person),
        Caller::Session(me) if target.parent_id == Some(me) => (Some(me), Source::Agent),
        Caller::Session(me)
            if crate::sessions::session_record(&host.store, me).is_ok_and(|s| s.parent_id == Some(target.id)) =>
        {
            return notice_up(host, me, &p.prompt);
        }
        Caller::Session(_) => {
            return Err(denied(
                "자기가 맡긴 작업자에게만 지시를 보낼 수 있어요 / you can only instruct your own workers",
            ));
        }
        Caller::Unverified => return Err(denied(UNVERIFIED)),
    };
    if !target.tool.is_agent() {
        return Err(RpcError::invalid("셸 세션에는 지시를 보낼 수 없어요"));
    }
    let key = p.key.as_deref().map(str::trim).filter(|k| !k.is_empty());
    if let Some(k) = key
        && let Some(existing) = db::by_key(&host.store, parent, k)?
    {
        return Ok(existing);
    }
    let mut req = new_request(parent, target.id, target.tool, &p.prompt);
    req.role = target.role.clone();
    req.source = source;
    req.interrupt = p.now;
    db::insert(&host.store, &req, key)?;
    changed(host);
    Ok(req)
}

/// A worker wrote to the session that gave it work: shown to the person on that work, never
/// typed into the parent's terminal. Returns the work it was noted on.
fn notice_up(host: &Arc<Host>, worker: SessionId, text: &str) -> Result<Request, RpcError> {
    let mut r = db::latest_for_worker(&host.store, worker, &[])?
        .ok_or_else(|| RpcError::not_found("맡겨진 일이 없어 메시지를 남길 곳이 없어요"))?;
    let text = head_of(text.trim(), MAX_ITEM);
    r.notes.push(Note { at: runwick_core::now(), text: format!("{WORKER_MESSAGE}{text}") });
    r.updated_at = runwick_core::now();
    db::save(&host.store, &r)?;
    changed(host);
    Ok(r)
}

/// Resolve a request id or a worker id (its newest request).
fn resolve(host: &Host, id: Uuid) -> Result<Request, RpcError> {
    if let Some(r) = db::find(&host.store, id)? {
        return Ok(r);
    }
    db::latest_for_worker(&host.store, id, &[])?.ok_or_else(|| RpcError::not_found("그 ID로 맡긴 일을 찾을 수 없어요"))
}

pub async fn wait(host: &Arc<Host>, p: WaitFor) -> Result<Waited, RpcError> {
    let secs = if p.timeout_secs == 0 { DEFAULT_WAIT } else { p.timeout_secs.min(MAX_WAIT) };
    let deadline = Instant::now() + Duration::from_secs(secs);
    let mut rx = rt(host).subscribe();
    let id = resolve(host, p.id)?.id;
    loop {
        rx.borrow_and_update();
        let r = db::get(&host.store, id)?;
        if !r.status.is_open() || r.status == ReqStatus::Uncertain {
            let (answer, from) = answer_after(host, &r, deadline).await;
            return Ok(Waited { request: r, timed_out: false, answer, answer_from: from });
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Ok(Waited { request: r, timed_out: true, answer: None, answer_from: None });
        }
        tokio::select! {
            _ = rx.changed() => {}
            _ = tokio::time::sleep(left.min(Duration::from_secs(5))) => {}
        }
    }
}

/// The worker's answer for a request: its report, else its newest answer in the record after
/// the delivery, else the last lines of its screen.
fn answer(host: &Arc<Host>, r: &Request) -> (Option<String>, Option<SummaryFrom>) {
    if let Some(res) = &r.result
        && let Some(s) = &res.summary
        && res.summary_from == Some(SummaryFrom::Report)
    {
        return (Some(s.clone()), Some(SummaryFrom::Report));
    }
    let delivered = r.delivered_at.unwrap_or(r.created_at);
    if let Ok(t) = crate::records::turn_info(&host.store, r.worker_id)
        && let Some(m) = t.last_answer.filter(|m| m.at >= delivered)
    {
        return (Some(m.text), Some(SummaryFrom::Transcript));
    }
    if r.delivered_at.is_none() {
        return (None, None);
    }
    let screen = host.sessions.screen_text(r.worker_id, 0).unwrap_or_default();
    let lines: Vec<&str> = screen.lines().map(str::trim_end).filter(|l| !l.trim().is_empty()).collect();
    if lines.is_empty() {
        return (None, None);
    }
    (Some(lines[lines.len().saturating_sub(30)..].join("\n")), Some(SummaryFrom::Screen))
}

/// Like [`answer`], but give the conversation record a moment to catch up after a turn ended.
async fn answer_after(host: &Arc<Host>, r: &Request, deadline: Instant) -> (Option<String>, Option<SummaryFrom>) {
    let grace = (Instant::now() + rt(host).timing.answer_grace).min(deadline);
    loop {
        let (a, from) = answer(host, r);
        let settled = from != Some(SummaryFrom::Screen) || r.status != ReqStatus::Done || !r.tool.is_agent();
        if settled || Instant::now() >= grace {
            return (a, from);
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

async fn view(host: &Arc<Host>, id: Uuid) -> Result<RequestView, RpcError> {
    let request = resolve(host, id)?;
    let cwd = engine::worker_cwd(host, request.worker_id).unwrap_or_default();
    let folder_exists = cwd.is_dir();
    let checks = request.result.as_ref().map(|r| r.checks.clone()).unwrap_or_default();
    let checks_stale = if folder_exists && !checks.is_empty() {
        let now = changes::fingerprint(&cwd).await;
        checks.iter().any(|c| c.fingerprint.is_some() && c.fingerprint != now)
    } else {
        false
    };
    let (answer, answer_from) = answer(host, &request);
    Ok(RequestView {
        worker_cwd: cwd.to_string_lossy().into_owned(),
        folder_exists,
        checks_stale,
        answer,
        answer_from,
        request,
    })
}

/// The worker's own result. Only a command inside the worker's terminal (or a client that
/// cannot be placed) may report for it. Text is kept as plain text within limits; a check's log
/// is text the command read (the host never opens a path it is given).
pub async fn report(host: &Arc<Host>, caller: Caller, p: Report) -> Result<Request, RpcError> {
    match caller {
        Caller::Session(me) if me == p.worker_id => {}
        Caller::Session(_) => {
            return Err(denied("다른 세션의 일은 보고할 수 없어요 / you can only report your own work"));
        }
        Caller::Window => {}
        Caller::Unverified => return Err(denied(UNVERIFIED)),
    }
    if p.summary.trim().is_empty() {
        return Err(RpcError::invalid("--summary가 비어 있어요"));
    }
    let item = |t: &str| head_of(plain(t).trim(), MAX_ITEM);
    let statuses =
        [ReqStatus::Running, ReqStatus::NeedsInput, ReqStatus::Delivered, ReqStatus::Uncertain, ReqStatus::Done];
    let mut r = db::latest_for_worker(&host.store, p.worker_id, &statuses)?
        .ok_or_else(|| RpcError::not_found("이 세션에 맡겨진 일이 없어 보고할 곳이 없어요"))?;
    let cwd = engine::worker_cwd(host, p.worker_id).unwrap_or_default();
    let revision = changes::head(&cwd).await;
    let fingerprint = changes::fingerprint(&cwd).await;
    let now = runwick_core::now();
    let mut res = r.result.take().unwrap_or_default();
    res.summary = Some(head_of(plain(&p.summary).trim(), MAX_SUMMARY));
    res.summary_from = Some(SummaryFrom::Report);
    res.reported_files = p.files.iter().map(|f| item(f)).filter(|f| !f.is_empty()).take(MAX_ITEMS).collect();
    res.checks = p
        .checks
        .iter()
        .filter(|c| !c.command.trim().is_empty())
        .take(MAX_ITEMS)
        .map(|c| Check {
            command: item(&c.command),
            passed: c.passed,
            revision: revision.clone(),
            fingerprint: fingerprint.clone(),
            log: c.log.as_deref().map(|l| tail_of(&plain(l), MAX_LOG)).filter(|l| !l.trim().is_empty()),
            at: now,
        })
        .collect();
    res.not_done = p.not_done.iter().map(|s| item(s)).filter(|s| !s.is_empty()).take(MAX_ITEMS).collect();
    if r.status == ReqStatus::Done {
        // Reported after the turn ended: refresh what changed too.
        res.files = changes::files(&cwd, r.start_rev.as_deref()).await;
        res.head_rev = revision.clone();
        res.fingerprint = fingerprint;
    }
    r.result = Some(res);
    if matches!(r.status, ReqStatus::Delivered | ReqStatus::Uncertain) {
        r.status = ReqStatus::Running;
        r.reason = None;
    }
    r.updated_at = now;
    db::save(&host.store, &r)?;
    changed(host);
    Ok(r)
}

fn cancel(host: &Arc<Host>, id: Uuid) -> Result<Request, RpcError> {
    let mut r = db::get(&host.store, id)?;
    if r.status != ReqStatus::Queued {
        return Err(RpcError::new(
            codes::CONFLICT,
            "이미 전달된 일은 취소할 수 없어요. 멈추려면 작업자 실행을 종료하세요",
        ));
    }
    r.status = ReqStatus::Cancelled;
    r.reason = Some("사람이 취소했어요".into());
    r.updated_at = runwick_core::now();
    r.finished_at = Some(r.updated_at);
    db::save(&host.store, &r)?;
    // Nothing left for this worker: a direct conversation has nothing to hold back any more.
    let open =
        db::list(&host.store, &RequestQuery { worker_id: Some(r.worker_id), open_only: true, ..Default::default() })?;
    if open.is_empty() {
        db::set_direct(&host.store, r.worker_id, false)?;
    }
    changed(host);
    Ok(r)
}

/// Workers a person is talking to directly whose run is still alive. A run that ended ends the
/// direct conversation too (it is released, so nothing offers "다시 맡기기" for it).
pub(crate) fn live_direct(host: &Arc<Host>) -> Result<Vec<SessionId>, RpcError> {
    let mut alive = Vec::new();
    for id in db::direct_workers(&host.store)? {
        let running = host.sessions.live(host, id).is_some_and(|l| {
            matches!(
                l.phase,
                runwick_core::api::sessions::Phase::Running | runwick_core::api::sessions::Phase::Starting
            )
        });
        if running {
            alive.push(id);
        } else {
            db::set_direct(&host.store, id, false)?;
        }
    }
    Ok(alive)
}

const DIRECT_ON: &str = "직접 대화 시작";
const DIRECT_OFF: &str = "직접 대화 후 다시 맡김";

/// A person talks to a worker directly (automatic instructions wait) or gives it back. Both are
/// noted on the attempt the person stepped into, so what they added stays with that attempt.
fn direct(host: &Arc<Host>, p: Direct) -> Result<(), RpcError> {
    crate::sessions::session_record(&host.store, p.worker_id)?;
    if !p.on {
        // Handed back: what the person typed and left there no longer holds deliveries.
        host.sessions.forget_typed_line(p.worker_id);
    }
    if db::set_direct(&host.store, p.worker_id, p.on)? {
        let recent =
            db::list(&host.store, &RequestQuery { worker_id: Some(p.worker_id), limit: 50, ..Default::default() })?;
        let target = if p.on {
            recent.into_iter().next()
        } else {
            let stepped_in = recent.iter().position(|r| r.notes.last().is_some_and(|n| n.text == DIRECT_ON));
            match stepped_in {
                Some(i) => recent.into_iter().nth(i),
                None => recent.into_iter().next(),
            }
        };
        if let Some(mut r) = target {
            r.notes.push(Note { at: runwick_core::now(), text: if p.on { DIRECT_ON } else { DIRECT_OFF }.into() });
            db::save(&host.store, &r)?;
        }
    }
    changed(host);
    Ok(())
}

/// A new attempt of earlier work: by the person, or by the agent of the session that owns it.
async fn retry(host: &Arc<Host>, caller: Caller, p: Retry) -> Result<Delegated, RpcError> {
    let prev = db::get(&host.store, p.id)?;
    let source = person_or_owner(caller, prev.parent_id)?;
    let note = p.note.as_deref().map(plain);
    let note = note.as_deref().map(str::trim).filter(|n| !n.is_empty());
    match p.mode {
        RetryMode::Continue => {
            if host
                .sessions
                .live(host, prev.worker_id)
                .is_none_or(|l| l.phase != runwick_core::api::sessions::Phase::Running)
            {
                // Resume only a conversation that was saved; otherwise the CLI starts anew and
                // the instruction below carries the task.
                let restart = Restart {
                    id: prev.worker_id,
                    resume: conversation_saved(host, prev.worker_id),
                    cols: p.cols,
                    rows: p.rows,
                };
                crate::sessions::restart_session(host.clone(), restart).await?;
            }
            let mut prompt = format!(
                "이어서 수정해 주세요. 이전 시도({})에서 끝내지 못한 부분만 이어서 하고, 이미 끝낸 부분은 다시 하지 마세요. 이전 검증 결과는 지금 파일에 그대로 쓸 수 없어요.\n\n원래 맡긴 일:\n{}",
                prev.receipt(),
                prev.prompt
            );
            if let Some(n) = note {
                prompt.push_str(&format!("\n\n추가 지시:\n{n}"));
            }
            check_prompt(&prompt)?;
            let mut req = new_request(prev.parent_id, prev.worker_id, prev.tool, &prompt);
            req.title = prev.title.clone();
            req.role = prev.role.clone();
            req.source = source;
            req.previous_id = Some(prev.id);
            req.attempt = prev.attempt + 1;
            db::insert(&host.store, &req, None)?;
            changed(host);
            let worker = crate::sessions::session_info(host, prev.worker_id)?;
            Ok(Delegated { request: req, worker, reused: false })
        }
        RetryMode::Fresh => {
            let worker = crate::sessions::session_record(&host.store, prev.worker_id)?;
            let mut prompt = prev.prompt.clone();
            if let Some(n) = note {
                prompt.push_str(&format!("\n\n추가 지시:\n{n}"));
            }
            let again = Delegate {
                parent_id: prev.parent_id,
                workspace_id: Some(worker.workspace_id),
                tool: prev.tool,
                role: prev.role.clone(),
                title: Some(prev.title.clone()),
                prompt,
                split: None,
                key: None,
                source,
                cols: p.cols,
                rows: p.rows,
            };
            delegate_attempt(host, again, Some(&prev)).await
        }
    }
}

/// The worker's CLI conversation exists (an id and at least one stored answer), so `--resume`
/// can continue it. An id alone is not enough: Claude Code gets one before anything is saved.
fn conversation_saved(host: &Host, worker: SessionId) -> bool {
    let Ok(s) = crate::sessions::session_record(&host.store, worker) else {
        return false;
    };
    s.tool.is_agent()
        && s.provider_conv_id.is_some()
        && crate::records::turn_info(&host.store, worker).is_ok_and(|t| t.last_answer.is_some())
}

async fn check(host: &Arc<Host>, id: Uuid) -> Result<Request, RpcError> {
    let mut r = db::get(&host.store, id)?;
    if matches!(r.status, ReqStatus::Uncertain | ReqStatus::Delivered) {
        engine::recheck(host, &rt(host), &mut r);
        r.updated_at = runwick_core::now();
        if !r.status.is_open() {
            r.finished_at = Some(r.updated_at);
            engine::finalize(host, &mut r).await;
        }
        db::save(&host.store, &r)?;
        changed(host);
    }
    Ok(r)
}

fn overview(host: &Arc<Host>) -> Result<Overview, RpcError> {
    let busy = engine::busy_agents(host);
    let slots = engine::slot_workers(&db::open(&host.store)?);
    Ok(Overview {
        settings: guidance::load(&host.store),
        assigned: slots.len() as u32,
        observed_busy: busy.len() as u32,
        in_use: engine::in_use(&slots, &busy, None) as u32,
        held_parents: db::held_parents(&host.store)?,
        direct_workers: live_direct(host)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delivered_text_is_plain() {
        let evil = "fix it\x1b[201~\r\x1b[2J rm -rf /\x07\r\nnext\tline\u{9b}31m";
        let p = plain(evil);
        assert!(!p.chars().any(|c| c.is_control() && c != '\n' && c != '\t'), "{p:?}");
        assert_eq!(p, "fix it[201~\n[2J rm -rf /\nnext\tline31m");
        assert_eq!(tail_of("가나다", 4), "다");
        assert_eq!(head_of("가나다", 4), "가");
    }

    #[test]
    fn excerpts_use_the_first_non_empty_line() {
        assert_eq!(excerpt("\n  로그인 오류 고쳐줘  \n자세히는…", 60), "로그인 오류 고쳐줘");
        assert_eq!(excerpt("abcdef", 3), "abc…");
        assert_eq!(excerpt("", 5), "");
    }
}
