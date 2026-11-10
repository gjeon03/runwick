//! Session runtime: CLI processes in PTYs, the host's terminal model of each, attach with an
//! exact snapshot followed by live output, input ownership, and live status.
//!
//! - `term` / `snapshot`: the terminal model and its serialization
//! - `run`: starting and watching a process; `stop`: ending all of it; `pty`: the
//!   pseudo-terminal itself
//! - `env`: the environment of managed terminals (login environment, stripped markers, and the
//!   single hook for the global environment feature: [`Manager::set_env_hook`])
//! - `live`: `session.live` events, prompt recognition, titles
//! - `record`: the `sessions` table

#[cfg(test)]
mod archive_tests;
mod attention;
mod codex;
pub mod env;
mod live;
mod pty;
mod record;
mod rt;
mod run;
mod screens;
mod snapshot;
mod stop;
pub mod term;

pub use live::visible;

use crate::host::{Client, ClientId, Ctx, Host};
use crate::router::Router;
use crate::store::Migration;
use env::EnvOverlay;
use rt::{Attachment, Rt};
use runwick_core::api::host::{Leftover, ShutdownReport};
use runwick_core::api::projects::ById;
use runwick_core::api::sessions::*;
use runwick_core::rpc::{RpcError, codes, topic};
use runwick_core::wire::Frame;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};
use term::Size;
use uuid::Uuid;

pub const MIGRATIONS: &[Migration] = &[(
    "0003_sessions",
    "CREATE TABLE sessions (
        id BLOB PRIMARY KEY,
        project_id BLOB NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
        workspace_id BLOB NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
        title TEXT NOT NULL,
        title_user_set INTEGER NOT NULL DEFAULT 0,
        tool TEXT NOT NULL,
        provider_conv_id TEXT,
        parent_id BLOB,
        role TEXT,
        cwd TEXT NOT NULL,
        created_at TEXT NOT NULL,
        last_active_at TEXT,
        ended_at TEXT,
        exit_code INTEGER,
        hidden INTEGER NOT NULL DEFAULT 0,
        archived INTEGER NOT NULL DEFAULT 0,
        env_version INTEGER NOT NULL DEFAULT 0,
        -- 1 while a run is alive; after a host restart these are offered for resume.
        was_running INTEGER NOT NULL DEFAULT 0
    );
    CREATE INDEX sessions_workspace ON sessions(workspace_id);
    CREATE INDEX sessions_project ON sessions(project_id);
    CREATE INDEX sessions_parent ON sessions(parent_id);",
)];

/// Grace period between hang-up and kill when stopping a run.
const STOP_GRACE: Duration = Duration::from_secs(3);

/// Supplies the global environment variables for new runs (see `env`).
pub type EnvHook = Arc<dyn Fn(&Host) -> EnvOverlay + Send + Sync>;

/// Told about every run that started (on the starting thread, after the record has the run's
/// environment version), with the overlay it got. Session refresh remembers each run's settings.
pub type StartHook = Arc<dyn Fn(&Host, &Session, &EnvOverlay) + Send + Sync>;

/// Extra instructions given to an agent CLI at launch (collaboration guidance), or `None`.
pub type GuidanceHook = Arc<dyn Fn(&Host, &Session) -> Option<String> + Send + Sync>;

#[derive(Default)]
pub struct Manager {
    rts: Mutex<HashMap<SessionId, Arc<Rt>>>,
    wake: tokio::sync::Notify,
    env_hook: RwLock<Option<EnvHook>>,
    start_hook: RwLock<Option<StartHook>>,
    guidance_hook: RwLock<Option<GuidanceHook>>,
    /// Session token of each live run (see `paths::ENV_SESSION_TOKEN`) → its session.
    tokens: Mutex<HashMap<String, SessionId>>,
}

impl Manager {
    /// A new secret for a run of `id` (any earlier one of that session stops counting).
    pub fn issue_token(&self, id: SessionId) -> String {
        let token = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
        let mut t = self.tokens.lock().unwrap_or_else(|e| e.into_inner());
        t.retain(|_, s| *s != id);
        t.insert(token.clone(), id);
        token
    }

    /// The run of `id` ended: its token stops counting.
    pub fn revoke_token(&self, id: SessionId) {
        self.tokens.lock().unwrap_or_else(|e| e.into_inner()).retain(|_, s| *s != id);
    }

    /// The session a token was issued to, while its run lives.
    pub fn session_for_token(&self, token: &str) -> Option<SessionId> {
        self.tokens.lock().unwrap_or_else(|e| e.into_inner()).get(token).copied()
    }
    /// Sessions whose process is alive right now.
    pub fn running_ids(&self) -> Vec<Uuid> {
        self.all().into_iter().filter(|rt| run::is_running(rt)).map(|rt| rt.id).collect()
    }

    /// Install the global-environment provider. Applied to runs started afterwards.
    pub fn set_env_hook(&self, hook: EnvHook) {
        if let Ok(mut h) = self.env_hook.write() {
            *h = Some(hook);
        }
    }

    /// Install the run-start observer (see [`StartHook`]).
    pub fn set_start_hook(&self, hook: StartHook) {
        if let Ok(mut h) = self.start_hook.write() {
            *h = Some(hook);
        }
    }

    fn started(&self, host: &Host, session: &Session, overlay: &EnvOverlay) {
        let hook = self.start_hook.read().ok().and_then(|h| h.clone());
        if let Some(f) = hook {
            f(host, session, overlay);
        }
    }

    /// Runtime state of a session for other host features; `None` when it has no runtime here.
    pub fn live(&self, host: &Host, id: SessionId) -> Option<SessionLive> {
        let rt = self.get(id)?;
        let st = rt.lock();
        st.has_runtime().then(|| st.live(id, &host.bus, Instant::now()))
    }

    /// The CLI shows its own input prompt and no dialog (see `attention::ready_for_input`).
    pub fn ready_for_input(&self, id: SessionId) -> bool {
        let Some(rt) = self.get(id) else { return false };
        let st = rt.lock();
        st.run.is_some() && attention::ready_for_input(rt.tool, &st.term.screen_lines(), st.term.cursor_row())
    }

    /// The person left typed text in the session's input without sending it.
    pub fn typed_line(&self, id: SessionId) -> bool {
        self.get(id).is_some_and(|rt| rt.lock().typed_line)
    }

    /// The person handed the session back ("다시 맡기기"): what they typed no longer holds it.
    pub fn forget_typed_line(&self, id: SessionId) {
        if let Some(rt) = self.get(id) {
            rt.lock().typed_line = false;
        }
    }

    /// Time since the person last typed into the session, if they have.
    pub fn since_input(&self, id: SessionId) -> Option<Duration> {
        self.get(id)?.lock().last_input.map(|t| t.elapsed())
    }

    /// Install the launch-guidance provider (see [`GuidanceHook`]). Applied to runs started afterwards.
    pub fn set_guidance_hook(&self, hook: GuidanceHook) {
        if let Ok(mut h) = self.guidance_hook.write() {
            *h = Some(hook);
        }
    }

    fn guidance(&self, host: &Host, session: &Session) -> Option<String> {
        let hook = self.guidance_hook.read().ok().and_then(|h| h.clone());
        hook.and_then(|f| f(host, session))
    }

    /// Type into a running session on the host's behalf (delegation). Not counted as the
    /// person's input. Returns false when nothing is running.
    pub fn type_into(&self, id: SessionId, data: &[u8]) -> bool {
        let Some(rt) = self.get(id) else { return false };
        rt.lock().send_input(data.to_vec())
    }

    /// Paste text into a running session like a terminal would: bracketed when the program
    /// asked for it, so a multi-line instruction arrives as one input. Enter is sent separately.
    pub fn paste(&self, id: SessionId, text: &str) -> bool {
        let Some(rt) = self.get(id) else { return false };
        let mut st = rt.lock();
        let bracketed = st.term.mode().contains(alacritty_terminal::term::TermMode::BRACKETED_PASTE);
        // Control characters (ESC above all) could end the paste early and act as keys.
        let clean: String = text.chars().filter(|c| !c.is_control() || matches!(c, '\n' | '\t')).collect();
        let data = if bracketed {
            format!("\x1b[200~{clean}\x1b[201~").into_bytes()
        } else {
            // Without bracketed paste every newline would submit a line: send one line.
            clean.split('\n').map(str::trim).filter(|l| !l.is_empty()).collect::<Vec<_>>().join(" ").into_bytes()
        };
        st.send_input(data)
    }

    /// Plain text of the session's screen (and up to `scrollback` lines above it).
    pub fn screen_text(&self, id: SessionId, scrollback: usize) -> Option<String> {
        Some(self.get(id)?.lock().term.text(scrollback))
    }

    fn overlay(&self, host: &Host) -> EnvOverlay {
        let hook = self.env_hook.read().ok().and_then(|h| h.clone());
        hook.map(|f| f(host)).unwrap_or_default()
    }

    /// The global-environment overlay a run started now would get. The host's own AI jobs use
    /// it so their CLI call sees the same variables as a CLI started in a terminal.
    pub fn env_overlay(&self, host: &Host) -> EnvOverlay {
        self.overlay(host)
    }

    fn get(&self, id: SessionId) -> Option<Arc<Rt>> {
        self.rts.lock().ok()?.get(&id).cloned()
    }

    /// Runtime state of a session, created on first use. A session without a run on this host
    /// starts from its saved last screen, if there is one.
    fn get_or_create(&self, id: SessionId, tool: Tool, size: Size) -> Arc<Rt> {
        let mut map = self.rts.lock().unwrap_or_else(|e| e.into_inner());
        map.entry(id)
            .or_insert_with(|| match screens::load(id) {
                Some((saved, bytes)) => {
                    let rt = Rt::new(id, tool, saved);
                    rt.lock().term.advance(&bytes);
                    Arc::new(rt)
                }
                None => Arc::new(Rt::new(id, tool, size)),
            })
            .clone()
    }

    fn all(&self) -> Vec<Arc<Rt>> {
        self.rts.lock().map(|m| m.values().cloned().collect()).unwrap_or_default()
    }

    /// Wake the live-state task (after output or any runtime change).
    pub fn wake(&self) {
        self.wake.notify_one();
    }

    async fn woken(&self) {
        self.wake.notified().await
    }
}

pub fn register(r: &mut Router) {
    r.add::<SessionsList, _, _>(|ctx: Ctx, q: SessionQuery| async move {
        let rows = record::list(&ctx.host.store, &q)?;
        Ok(rows.into_iter().map(|s| info(&ctx.host, s)).collect())
    });
    r.add::<SessionsGet, _, _>(
        |ctx: Ctx, p: ById| async move { Ok(info(&ctx.host, record::get(&ctx.host.store, p.id)?)) },
    );
    r.add::<SessionsStart, _, _>(start);
    r.add::<SessionsAttach, _, _>(attach);
    r.add::<SessionsDetach, _, _>(|ctx: Ctx, p: ById| async move {
        if let Some(rt) = ctx.host.sessions.get(p.id) {
            detach(&rt, ctx.client.id);
            ctx.host.sessions.wake();
        }
        Ok(())
    });
    r.add::<SessionsResize, _, _>(|ctx: Ctx, p: Resize| async move {
        let rt = ctx.host.sessions.get(p.id).ok_or_else(|| RpcError::not_found("열려 있지 않은 세션이에요"))?;
        let mut st = rt.lock();
        if !st.may_control(ctx.client.id) {
            return Err(RpcError::new(codes::DENIED, "다른 곳에서 입력 중이라 크기를 바꾸지 않았어요"));
        }
        resize(&mut st, Size::clamped(p.cols, p.rows));
        drop(st);
        ctx.host.sessions.wake();
        Ok(())
    });
    r.add::<SessionsStop, _, _>(|ctx: Ctx, p: Stop| async move {
        let Some(rt) = ctx.host.sessions.get(p.id) else { return Ok(()) };
        let grace = if p.force { Duration::ZERO } else { STOP_GRACE };
        // A successful response means cleanup finished, so the window can safely archive next.
        if let Some(out) = stop::stop(&rt, grace).await {
            if !out.leftovers.is_empty() {
                tracing::warn!(session = %rt.id, leftovers = ?out.leftovers, "processes survived the stop");
            }
            rt.lock().leftovers = out.leftovers;
            ctx.host.sessions.wake();
        }
        Ok(())
    });
    r.add::<SessionsRestart, _, _>(restart);
    r.add::<SessionsUpdate, _, _>(update);
    r.add::<SessionsTakeInput, _, _>(|ctx: Ctx, p: ById| async move {
        let rt = ctx.host.sessions.get(p.id).ok_or_else(|| RpcError::not_found("열려 있지 않은 세션이에요"))?;
        let mut st = rt.lock();
        let Some(a) = st.attachment_mut(ctx.client.id) else {
            return Err(RpcError::invalid("세션을 먼저 열어 주세요"));
        };
        a.interactive = true;
        st.owner = Some(ctx.client.id);
        drop(st);
        ctx.host.sessions.wake();
        Ok(())
    });
    r.add::<SessionsView, _, _>(|ctx: Ctx, p: View| async move {
        if let Some(rt) = ctx.host.sessions.get(p.id) {
            let mut st = rt.lock();
            if let Some(a) = st.attachment_mut(ctx.client.id) {
                a.viewing = p.viewing;
            }
            if p.viewing {
                st.unread = false;
                st.bell_at = None;
            }
            drop(st);
            ctx.host.sessions.wake();
        }
        Ok(())
    });
    r.add::<SessionsScreenText, _, _>(|ctx: Ctx, q: ScreenQuery| async move {
        record::get(&ctx.host.store, q.id)?;
        Ok(ctx.host.sessions.get(q.id).map(|rt| rt.lock().term.text(q.scrollback)).unwrap_or_default())
    });
}

/// A session's record, for other host features.
pub fn session_record(store: &crate::store::Store, id: SessionId) -> Result<Session, RpcError> {
    Ok(record::get(store, id)?.session)
}

/// Record plus runtime state, for other host features.
pub fn session_info(host: &Host, id: SessionId) -> Result<SessionInfo, RpcError> {
    Ok(info(host, record::get(&host.store, id)?))
}

/// Record plus runtime state.
fn info(host: &Host, stored: record::Stored) -> SessionInfo {
    let mut session = stored.session;
    let rt = host.sessions.get(session.id);
    let (live, running) = match &rt {
        Some(rt) => {
            let st = rt.lock();
            let live = st.has_runtime().then(|| st.live(rt.id, &host.bus, Instant::now()));
            // Until the end is announced (after it is recorded) the run still counts as running.
            (live, st.run.is_some() || matches!(st.phase, Phase::Starting | Phase::Running))
        }
        None => (None, false),
    };
    session.interrupted = stored.was_running && !running;
    SessionInfo { session, live }
}

async fn start(ctx: Ctx, p: StartSession) -> Result<SessionInfo, RpcError> {
    start_session(ctx.host.clone(), p).await
}

/// `sessions.start` for other host features (delegation, worktrees): a new session and its run.
pub async fn start_session(host: Arc<Host>, p: StartSession) -> Result<SessionInfo, RpcError> {
    let ws = crate::projects::get_workspace(&host.store, p.workspace_id)?;
    if ws.remote.is_some() {
        return Err(RpcError::invalid("다른 기기의 작업 공간은 그 기기에서 실행해요"));
    }
    let title = p.title.as_deref().map(str::trim).filter(|t| !t.is_empty());
    let session = Session {
        id: Uuid::now_v7(),
        project_id: ws.project_id,
        workspace_id: ws.id,
        title: title.map(str::to_string).unwrap_or_else(|| p.tool.label().to_string()),
        title_user_set: title.is_some(),
        tool: p.tool,
        provider_conv_id: (p.tool == Tool::Claude).then(|| Uuid::new_v4().to_string()),
        parent_id: p.parent_id,
        role: p.role.clone(),
        cwd: ws.path.clone(),
        created_at: runwick_core::now(),
        last_active_at: None,
        ended_at: None,
        exit_code: None,
        hidden: false,
        archived: false,
        env_version: 0,
        interrupted: false,
        origin_device: None,
    };
    record::insert(&host.store, &session)?;
    let rt = host.sessions.get_or_create(session.id, session.tool, Size::clamped(p.cols, p.rows));
    let launch_session = session.clone();
    let prompt = p.initial_prompt.clone();
    launch(&host, &rt, move |host, rt, overlay| {
        run::start(
            host,
            rt,
            run::Launch { session: &launch_session, resume: false, initial_prompt: prompt.as_deref() },
            overlay,
        )
    })
    .await;
    host.bus.changed(&[topic::SESSIONS]);
    Ok(info(&host, record::get(&host.store, session.id)?))
}

/// Start a run on a blocking thread; a failure becomes the session's visible error.
async fn launch<F>(host: &Arc<Host>, rt: &Arc<Rt>, f: F)
where
    F: FnOnce(&Arc<Host>, &Arc<Rt>, &EnvOverlay) -> Result<(), run::StartError> + Send + 'static,
{
    let (h, r) = (host.clone(), rt.clone());
    let result = tokio::task::spawn_blocking(move || {
        let overlay = h.sessions.overlay(&h);
        f(&h, &r, &overlay)
    })
    .await
    .unwrap_or_else(|e| Err((StartErrorKind::SpawnFailed, format!("실행 준비 중 오류: {e}"))));
    if let Err((kind, message)) = result {
        tracing::warn!(session = %rt.id, ?kind, "start failed: {message}");
        let mut st = rt.lock();
        st.phase = runwick_core::api::sessions::Phase::Exited;
        st.error = Some((kind, message));
    }
    host.sessions.wake();
}

async fn attach(ctx: Ctx, p: Attach) -> Result<SessionLive, RpcError> {
    let stored = record::get(&ctx.host.store, p.id)?;
    let host = ctx.host.clone();
    let client = ctx.client.clone();
    let rt = host.sessions.get_or_create(p.id, stored.session.tool, Size::clamped(p.cols, p.rows));
    let live = tokio::task::spawn_blocking(move || {
        let mut st = rt.lock();
        if p.interactive && st.owner.is_none() {
            st.owner = Some(client.id);
        }
        // Snapshot and registration happen under one lock: the client receives exactly the
        // state before the next chunk of output, then that chunk.
        st.attached.retain(|a| a.client.id != client.id);
        st.attached.push(Attachment { client: client.clone(), interactive: p.interactive, viewing: false });
        // The snapshot below supersedes output this client may have lost.
        client.clear_lag(rt.id);
        if st.may_control(client.id) && p.interactive {
            resize(&mut st, Size::clamped(p.cols, p.rows));
        }
        let snap = st.term.snapshot();
        for f in Frame::output_chunks(rt.id, &snap) {
            client.send(f);
        }
        st.live(rt.id, &host.bus, Instant::now())
    })
    .await
    .map_err(RpcError::internal)?;
    ctx.host.sessions.wake();
    Ok(live)
}

/// Send a fresh snapshot of every session whose output a client lost by falling behind (see
/// `Client::send`). Under each session's lock, so the snapshot sits exactly between the output
/// that was dropped and the output that follows.
pub fn resync(host: &Arc<Host>, client: &Arc<Client>) {
    for id in client.lagging() {
        let Some(rt) = host.sessions.get(id) else {
            client.clear_lag(id);
            continue;
        };
        let mut st = rt.lock();
        client.clear_lag(id);
        if st.attachment(client.id).is_none() {
            continue;
        }
        let snap = st.term.snapshot();
        for f in Frame::output_chunks(id, &snap) {
            if !client.send(f) {
                return;
            }
        }
    }
}

fn detach(rt: &Rt, client: ClientId) {
    let mut st = rt.lock();
    st.attached.retain(|a| a.client.id != client);
    if st.owner == Some(client) {
        st.owner = None;
    }
}

fn resize(st: &mut rt::RtState, size: Size) {
    if st.term.size() == size {
        return;
    }
    st.term.resize(size);
    if let Some(run) = &st.run
        && let Err(e) = pty::resize(&run.master, size.cols, size.rows)
    {
        tracing::debug!("resize failed: {e}");
    }
}

async fn restart(ctx: Ctx, p: Restart) -> Result<SessionInfo, RpcError> {
    restart_session(ctx.host.clone(), p).await
}

/// `sessions.restart` for other host features (session refresh): same session, new run.
pub async fn restart_session(host: Arc<Host>, p: Restart) -> Result<SessionInfo, RpcError> {
    let stored = record::get(&host.store, p.id)?;
    let mut session = stored.session;
    if let Some(device) = &session.origin_device {
        return Err(RpcError::new(
            codes::UNSUPPORTED,
            format!("{device}에서 동기화한 기록이라 여기서 다시 시작하거나 재개할 수 없어요"),
        ));
    }
    if p.resume && session.tool.is_agent() {
        let (store, s) = (host.clone(), session.clone());
        let saved = tokio::task::spawn_blocking(move || crate::records::conversation_saved(&store.store, &s))
            .await
            .unwrap_or(false);
        if !saved {
            return Err(RpcError::new(
                codes::UNSUPPORTED,
                "저장된 대화가 없어 같은 대화를 재개할 수 없어요. 새 대화로 시작하세요",
            ));
        }
    }
    if !std::path::Path::new(&session.cwd).is_dir() {
        return Err(RpcError::invalid(format!("작업 폴더를 찾을 수 없어요: {}", session.cwd)));
    }
    let size = Size::clamped(p.cols, p.rows);
    let rt = host.sessions.get_or_create(session.id, session.tool, size);
    stop::stop(&rt, STOP_GRACE).await;
    if run::is_running(&rt) {
        return Err(RpcError::new(codes::BUSY, "이전 실행이 아직 끝나지 않았어요"));
    }
    if !p.resume && session.tool.is_agent() {
        // A new conversation in the same session: Claude gets a fresh id, Codex is rediscovered.
        session.provider_conv_id = (session.tool == Tool::Claude).then(|| Uuid::new_v4().to_string());
        record::set_conv_id(&host.store, session.id, session.provider_conv_id.as_deref())?;
    }
    {
        // The previous output stays: the new run starts below a separator line, with the
        // terminal's modes (not its content) reset, here and in every attached client.
        let what = match (session.tool.is_agent(), p.resume) {
            (true, true) => "같은 대화 재개",
            (true, false) => "새 대화",
            (false, _) => "새로 실행",
        };
        let mut st = rt.lock();
        let mut out = Vec::new();
        if st.term.mode().contains(alacritty_terminal::term::TermMode::ALT_SCREEN) {
            // Back on the primary screen, at its own cursor.
            out.extend(st.term.advance(b"\x1b[?1049l").forward);
        }
        let at = (st.term.cursor_row(), st.term.cursor_col());
        out.extend(st.term.advance(&separator(what, chrono::Local::now(), at)).forward);
        st.attached.retain(|a| Frame::output_chunks(rt.id, &out).all(|f| a.client.send(f)));
        resize(&mut st, size);
        st.unread = false;
        st.bell_at = None;
        st.leftovers.clear();
    }
    let resume = p.resume && session.tool.is_agent();
    let tool = session.tool;
    let failures_before = attention::resume_failures(tool, &rt.lock().term.text(RESUME_SCROLLBACK));
    launch(&host, &rt, move |host, rt, overlay| {
        run::start(host, rt, run::Launch { session: &session, resume, initial_prompt: None }, overlay)
    })
    .await;
    if resume {
        tokio::spawn(watch_resume(host.clone(), rt.clone(), tool, failures_before));
    }
    host.bus.changed(&[topic::SESSIONS]);
    Ok(info(&host, record::get(&host.store, p.id)?))
}

async fn update(ctx: Ctx, u: SessionUpdate) -> Result<Session, RpcError> {
    let host = &ctx.host;
    record::get(&host.store, u.id)?;
    if u.archived == Some(true)
        && host.sessions.get(u.id).is_some_and(|rt| {
            let st = rt.lock();
            st.run.is_some() || matches!(st.phase, Phase::Starting | Phase::Running) || !st.leftovers.is_empty()
        })
    {
        return Err(RpcError::new(codes::BUSY, "실행이 남아 있어 보관하지 않았어요. 종료 상태를 확인해 주세요."));
    }
    if let Some(title) = &u.title {
        let title = live::visible(title);
        let title = title.trim();
        if title.is_empty() {
            return Err(RpcError::invalid("이름을 비워 둘 수 없어요"));
        }
        let title: String = title.chars().take(200).collect();
        record::set_user_title(&host.store, u.id, &title)?;
    }
    if let Some(h) = u.hidden {
        record::set_flag(&host.store, u.id, record::Flag::Hidden, h)?;
    }
    if let Some(a) = u.archived {
        record::set_flag(&host.store, u.id, record::Flag::Archived, a)?;
    }
    if u.clear_interrupted && host.sessions.get(u.id).is_none_or(|rt| !run::is_running(&rt)) {
        record::set_flag(&host.store, u.id, record::Flag::WasRunning, false)?;
    }
    if u.mark_read
        && let Some(rt) = host.sessions.get(u.id)
    {
        let mut st = rt.lock();
        st.unread = false;
        st.bell_at = None;
        drop(st);
        host.sessions.wake();
    }
    if u.title.is_some() || u.hidden.is_some() || u.archived.is_some() || u.clear_interrupted {
        host.bus.changed(&[topic::SESSIONS]);
    }
    Ok(info(host, record::get(&host.store, u.id)?).session)
}

/// Called once after the host starts (spawn background tasks here).
pub fn boot(host: &Arc<Host>) {
    env::start_capture();
    link_runwick_command();
    match record::interrupted_ids(&host.store) {
        Ok(ids) if !ids.is_empty() => {
            // Never started again automatically; they are offered for resume.
            tracing::info!(count = ids.len(), "sessions were running before the host stopped");
        }
        Ok(_) => {}
        Err(e) => tracing::warn!("cannot read sessions: {e}"),
    }
    tokio::spawn(live::run(host.clone()));
}

/// `<RUNWICK_HOME>/bin/runwick` → this executable, so agents in managed terminals can call it.
fn link_runwick_command() {
    let Ok(exe) = std::env::current_exe() else { return };
    let link = runwick_core::paths::bin_dir().join("runwick");
    if std::fs::read_link(&link).is_ok_and(|t| t == exe) {
        return;
    }
    let _ = std::fs::create_dir_all(runwick_core::paths::bin_dir());
    let _ = std::fs::remove_file(&link);
    if let Err(e) = std::os::unix::fs::symlink(&exe, &link) {
        tracing::warn!("cannot link {}: {e}", link.display());
    }
}

/// Terminal input from a client.
pub fn on_input(host: &Arc<Host>, client: &Arc<Client>, session: Uuid, data: Vec<u8>) {
    let Some(rt) = host.sessions.get(session) else { return };
    let mut st = rt.lock();
    if !st.may_control(client.id) {
        return; // another client holds input
    }
    if st.owner.is_none() {
        st.owner = Some(client.id);
    }
    let typed = rt::typed_line_after(st.typed_line, &data);
    if st.send_input(data) {
        st.last_input = Some(Instant::now());
        st.typed_line = typed;
        st.bell_at = None;
    }
    drop(st);
    host.sessions.wake();
}

/// A client disconnected: drop its attachments and input ownership.
pub fn on_client_gone(host: &Arc<Host>, client: ClientId) {
    for rt in host.sessions.all() {
        detach(&rt, client);
    }
    host.sessions.wake();
}

/// Stop every managed run (used by "작업까지 종료").
/// Lines above the screen read when checking a resumed run's answer.
const RESUME_SCROLLBACK: usize = 200;

/// After `--resume`, the CLI may still say there is nothing to continue (the transcript went
/// away). A new such answer (earlier output stays on screen, so earlier ones are counted out)
/// becomes the session's start error: "cannot resume", never a silent exit.
async fn watch_resume(host: Arc<Host>, rt: Arc<Rt>, tool: Tool, before: usize) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(300)).await;
        let mut st = rt.lock();
        if attention::resume_failures(tool, &st.term.text(RESUME_SCROLLBACK)) > before {
            if st.error.is_none() {
                let msg = "저장된 대화를 찾지 못해 재개하지 못했어요. 새 대화로 시작하세요";
                st.error = Some((StartErrorKind::NotResumable, msg.into()));
            }
            drop(st);
            host.sessions.wake();
            return;
        }
        if st.run.is_none() {
            return;
        }
    }
}

/// Only sessions whose processes all ended count as stopped; survivors are listed by name.
pub async fn stop_all(host: &Arc<Host>) -> ShutdownReport {
    let running: Vec<Arc<Rt>> = host.sessions.all().into_iter().filter(|rt| run::is_running(rt)).collect();
    let tasks: Vec<_> =
        running.into_iter().map(|rt| tokio::spawn(async move { (rt.id, stop::stop(&rt, STOP_GRACE).await) })).collect();
    let mut report = ShutdownReport::default();
    for task in tasks {
        let Ok((id, Some(out))) = task.await else { continue };
        if !out.leftovers.is_empty() {
            report.leftovers.push(Leftover { session: id, processes: out.leftovers });
        } else {
            report.stopped.push(id);
            if out.killed {
                report.killed.push(id);
            }
        }
    }
    report
}

/// Bytes that end the previous run's terminal state without erasing its output: reset modes,
/// pen, scroll region and character sets (the last two move the cursor home, so it is put back
/// at `cursor` (row, column)), then a dim "── 새로 실행 · 14:02 ──" on a line of its own.
fn separator(what: &str, now: chrono::DateTime<chrono::Local>, cursor: (usize, usize)) -> Vec<u8> {
    let modes = "\x1b[r\x1b[?6l\x1b[0m\x1b(B\x1b)B\x1b[?25h\x1b[?7h\x1b[4l\x1b[?1l\x1b>\x1b[0 q\
                 \x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1004l\x1b[?1005l\x1b[?1006l\x1b[?2004l";
    let (row, col) = cursor;
    let newline = if col > 0 { "\r\n" } else { "" };
    format!("{modes}\x1b[{};{}H{newline}\x1b[2m── {what} · {} ──\x1b[0m\r\n", row + 1, col + 1, now.format("%H:%M"))
        .into_bytes()
}
