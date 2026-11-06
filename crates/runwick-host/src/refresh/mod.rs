//! Session refresh (S12): apply changed global variables and MCP configuration to an existing
//! conversation by restarting its run with resume. The Runwick session, tab, pins and records
//! stay; other sessions are never touched.
//!
//! Every run start is observed ([`boot`] installs the sessions start hook): the environment
//! version, secrets that could not be read, and the fingerprints of the MCP servers the run's
//! CLI uses are kept in `refresh_runs`. A plan compares that with the current settings.
//!
//! - `job`: scheduling ("after the response"), the steps, verification and the job table

pub mod job;

use crate::host::{Ctx, Host};
use crate::mcp::config::{self, Snapshot};
use crate::router::Router;
use crate::sessions::{self, env::EnvOverlay};
use crate::store::{Migration, Store, db_err, ts_to_sql};
use runwick_core::api::refresh::*;
use runwick_core::api::sessions::{AttentionKind, Phase, Session, SessionId, SessionLive, Tool};
use runwick_core::rpc::RpcError;
use rusqlite::{OptionalExtension, params};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;

pub const MIGRATIONS: &[Migration] = &[(
    "0202_refresh_runs",
    "-- Settings each session's latest run started with (names and fingerprints, never values).
    CREATE TABLE refresh_runs (
        session_id BLOB PRIMARY KEY,
        started_at TEXT NOT NULL,
        env_version INTEGER NOT NULL,
        -- JSON list of secret names that could not be read for this run.
        env_unreadable TEXT NOT NULL DEFAULT '[]',
        -- JSON {\"provider:scope:name\": fingerprint} of the MCP servers the CLI uses.
        mcp TEXT NOT NULL DEFAULT '{}'
    );",
)];

/// Typing this recently may have left an unsent draft in the CLI.
const RECENT_INPUT: Duration = Duration::from_secs(60);

/// What a session's latest run started with.
#[derive(Debug, Clone, PartialEq)]
pub struct RunRecord {
    pub env_version: u64,
    pub env_unreadable: Vec<String>,
    pub mcp: Snapshot,
}

impl RunRecord {
    pub fn save(&self, store: &Store, session: SessionId) -> Result<(), RpcError> {
        let unreadable = serde_json::to_string(&self.env_unreadable).unwrap_or_else(|_| "[]".into());
        let mcp = serde_json::to_string(&self.mcp).unwrap_or_else(|_| "{}".into());
        store
            .with(|c| {
                c.execute(
                    "INSERT INTO refresh_runs (session_id, started_at, env_version, env_unreadable, mcp)
                     VALUES (?1, ?2, ?3, ?4, ?5)
                     ON CONFLICT(session_id) DO UPDATE SET started_at = excluded.started_at,
                        env_version = excluded.env_version, env_unreadable = excluded.env_unreadable,
                        mcp = excluded.mcp",
                    params![session, ts_to_sql(&runwick_core::now()), self.env_version as i64, unreadable, mcp],
                )
            })
            .map_err(db_err)?;
        Ok(())
    }

    pub fn load(store: &Store, session: SessionId) -> Result<Option<RunRecord>, RpcError> {
        store
            .with(|c| {
                c.query_row(
                    "SELECT env_version, env_unreadable, mcp FROM refresh_runs WHERE session_id = ?1",
                    [session],
                    |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)),
                )
                .optional()
            })
            .map_err(db_err)
            .map(|row| {
                row.map(|(v, u, m)| RunRecord {
                    env_version: v as u64,
                    env_unreadable: serde_json::from_str(&u).unwrap_or_default(),
                    mcp: serde_json::from_str(&m).unwrap_or_default(),
                })
            })
    }
}

/// Remember what every new run starts with (installed once at host start).
pub fn boot(host: &Arc<Host>) {
    host.sessions.set_start_hook(Arc::new(|host: &Host, s: &Session, overlay: &EnvOverlay| {
        let files = config::Files::from_login_env();
        let rec = RunRecord {
            env_version: overlay.version,
            env_unreadable: crate::env::service().last_unreadable(),
            mcp: config::snapshot(&files, s.tool, &s.cwd),
        };
        if let Err(e) = rec.save(&host.store, s.id) {
            tracing::warn!(session = %s.id, "cannot remember the run's settings: {}", e.message);
        }
        host.bus.changed(&[TOPIC]);
    }));
}

pub fn register(r: &mut Router) {
    r.add::<RefreshPlan, _, _>(|ctx: Ctx, t: Target| async move {
        let host = ctx.host.clone();
        tokio::task::spawn_blocking(move || plan(&host, t.session)).await.map_err(RpcError::internal)?
    });
    r.add::<RefreshStart, _, _>(start);
    r.add::<RefreshCancel, _, _>(|ctx: Ctx, t: Target| async move {
        if job::cancel(t.session)? {
            ctx.host.bus.changed(&[TOPIC]);
        }
        Ok(())
    });
    r.add::<RefreshStatus, _, _>(|_ctx: Ctx, t: Target| async move { Ok(job::get(t.session)) });
    r.add::<RefreshDismiss, _, _>(|ctx: Ctx, t: Target| async move {
        job::dismiss(t.session);
        ctx.host.bus.changed(&[TOPIC]);
        Ok(())
    });
    r.add::<RefreshPending, _, _>(|ctx: Ctx, _: ()| async move {
        let host = ctx.host.clone();
        tokio::task::spawn_blocking(move || pending(&host)).await.map_err(RpcError::internal)?
    });
}

async fn start(ctx: Ctx, p: StartParams) -> Result<Job, RpcError> {
    let host = ctx.host.clone();
    let session = p.session;
    let plan = tokio::task::spawn_blocking(move || plan(&host, session)).await.map_err(RpcError::internal)??;
    if p.verify_only {
        if plan.state == RunState::Ended {
            return Err(RpcError::invalid("실행 중이 아니라 다시 확인할 수 없어요"));
        }
    } else if plan.method.is_none() {
        let why: Vec<&str> = plan.blockers.iter().map(|b| b.message.as_str()).collect();
        return Err(RpcError::invalid(why.join(" ")));
    }
    let scheduled = p.when == When::AfterResponse && !p.verify_only;
    // A retry names the servers the previous result left open, so their outcome is shown again.
    let mut changed_mcp = job::changed_names(&plan);
    if let Some(prev) = &plan.job {
        changed_mcp.extend(job::unresolved_names(prev));
    }
    let job = Job {
        id: Uuid::now_v7(),
        session,
        when: p.when,
        phase: if scheduled { JobPhase::Scheduled } else { JobPhase::Running },
        steps: job::steps_for(plan.method, p.verify_only),
        checks: Vec::new(),
        created_at: runwick_core::now(),
        finished_at: None,
        message: None,
    };
    job::insert(job.clone())?;
    tracing::info!(%session, when = ?p.when, verify_only = p.verify_only, "session refresh requested");
    ctx.host.bus.changed(&[TOPIC]);
    let exec = job::Exec {
        session,
        job: job.id,
        when: p.when,
        method: plan.method,
        verify_only: p.verify_only,
        cols: p.cols,
        rows: p.rows,
        changed_mcp,
    };
    tokio::spawn(job::run(ctx.host.clone(), exec));
    Ok(job)
}

/// A prompt only the person can answer (a bell alone is not one).
pub fn needs_answer(l: &SessionLive) -> bool {
    l.attention.as_ref().is_some_and(|a| a.kind != AttentionKind::Notify)
}

pub fn run_state(live: Option<&SessionLive>) -> RunState {
    match live {
        Some(l) if l.phase == Phase::Starting => RunState::Starting,
        Some(l) if l.phase == Phase::Running && needs_answer(l) => RunState::NeedsAnswer,
        Some(l) if l.phase == Phase::Running && l.busy => RunState::Busy,
        Some(l) if l.phase == Phase::Running => RunState::Idle,
        _ => RunState::Ended,
    }
}

/// What decides whether and how a session can be refreshed.
#[derive(Debug, Clone)]
pub struct Facts {
    pub tool: Tool,
    pub state: RunState,
    pub cwd: String,
    pub has_conversation: bool,
    /// Claude only: the provider saved the conversation (a transcript exists).
    pub transcript: Option<bool>,
    pub folder_exists: bool,
    pub cli_found: bool,
}

pub fn decide(f: &Facts) -> (Option<Method>, Vec<Blocker>) {
    let mut blockers = Vec::new();
    let mut block = |kind, message: String| blockers.push(Blocker { kind, message });
    if f.state == RunState::Ended {
        block(BlockerKind::NotRunning, "실행 중이 아니에요. 다음에 실행하면 새 설정을 그대로 써요.".into());
    }
    if !f.folder_exists {
        block(BlockerKind::FolderMissing, format!("작업 폴더를 찾을 수 없어요: {}", f.cwd));
    }
    if f.tool.is_agent() && !f.cli_found {
        block(BlockerKind::CliMissing, format!("{} CLI를 찾지 못해 다시 시작할 수 없어요.", f.tool.label()));
    }
    let method = match f.tool {
        Tool::Shell => Some(Method::RestartShell),
        Tool::Claude if f.has_conversation && f.transcript == Some(false) => Some(Method::StartFresh),
        _ if f.has_conversation => Some(Method::Resume),
        _ => {
            block(
                BlockerKind::NoConversation,
                format!("{} 대화 ID를 아직 확인하지 못해 같은 대화로 다시 연결할 수 없어요.", f.tool.label()),
            );
            None
        }
    };
    if blockers.is_empty() { (method, blockers) } else { (None, blockers) }
}

/// Transcript folders of Claude Code (`$CLAUDE_CONFIG_DIR/projects` or `~/.claude/projects`).
fn claude_projects_dir() -> PathBuf {
    let env = sessions::env::login_env();
    env.get("CLAUDE_CONFIG_DIR")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| runwick_core::paths::user_home().join(".claude"))
        .join("projects")
}

/// Claude Code saved conversation `id` (its transcript exists). Claude names the folder after
/// the working directory with every non-alphanumeric character replaced by `-`.
pub fn claude_transcript_exists(id: &str, cwd: &str) -> bool {
    transcript_in(&claude_projects_dir(), id, cwd)
}

fn transcript_in(root: &Path, id: &str, cwd: &str) -> bool {
    let file = format!("{id}.jsonl");
    let slug: String = cwd.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    if root.join(slug).join(&file).is_file() {
        return true;
    }
    // The folder can differ (symlinked paths): look in every project folder.
    std::fs::read_dir(root).map(|rd| rd.flatten().any(|e| e.path().join(&file).is_file())).unwrap_or(false)
}

fn names(host: &Host, s: &Session) -> (String, String) {
    let project = crate::projects::get_project(&host.store, s.project_id).map(|p| p.name).unwrap_or_default();
    let workspace = crate::projects::get_workspace(&host.store, s.workspace_id).map(|w| w.name).unwrap_or_default();
    (project, workspace)
}

pub fn plan(host: &Host, id: SessionId) -> Result<Plan, RpcError> {
    let info = sessions::session_info(host, id)?;
    let s = info.session;
    let live = host.sessions.live(host, id).or(info.live);
    let state = run_state(live.as_ref());
    let login = sessions::env::login_env();
    let cli = match s.tool {
        Tool::Claude => Some("claude"),
        Tool::Codex => Some("codex"),
        Tool::Shell => None,
    };
    let facts = Facts {
        tool: s.tool,
        state,
        cwd: s.cwd.clone(),
        has_conversation: s.provider_conv_id.is_some(),
        transcript: match (s.tool, &s.provider_conv_id) {
            (Tool::Claude, Some(c)) => Some(claude_transcript_exists(c, &s.cwd)),
            _ => None,
        },
        folder_exists: Path::new(&s.cwd).is_dir(),
        cli_found: cli.is_none_or(|n| crate::tools::resolve(n, login.path()).is_some()),
    };
    let (method, blockers) = decide(&facts);
    let run = RunRecord::load(&host.store, id)?;
    let unreadable = run.as_ref().map(|r| r.env_unreadable.clone()).unwrap_or_default();
    let env_to = crate::env::current_version(&host.store)?;
    let env_changes = crate::env::changes_since(&host.store, s.env_version, &unreadable)?.unwrap_or_default();
    let files = config::Files::from_login_env();
    let mcp_changes = run.map(|r| config::diff(&r.mcp, &config::snapshot(&files, s.tool, &s.cwd)));
    let (project_name, workspace_name) = names(host, &s);
    Ok(Plan {
        session: id,
        project_id: s.project_id,
        workspace_id: s.workspace_id,
        title: s.title,
        tool: s.tool,
        project_name,
        workspace_name,
        state,
        method,
        blockers,
        env_from: s.env_version,
        env_to,
        env_changes,
        mcp_changes,
        job: job::get(id),
        input_recent: host.sessions.since_input(id).is_some_and(|d| d < RECENT_INPUT),
    })
}

/// Running sessions whose run started with older settings (or have a refresh under way).
pub fn pending(host: &Host) -> Result<Vec<Pending>, RpcError> {
    let files = config::Files::from_login_env();
    let mut snapshots: HashMap<(Tool, String), Snapshot> = HashMap::new();
    let mut out = Vec::new();
    for id in host.sessions.running_ids() {
        let Ok(info) = sessions::session_info(host, id) else {
            continue;
        };
        let s = info.session;
        let run = RunRecord::load(&host.store, id)?;
        let unreadable = run.as_ref().map(|r| r.env_unreadable.clone()).unwrap_or_default();
        let env_changes = crate::env::changes_since(&host.store, s.env_version, &unreadable)?.unwrap_or_default();
        let mcp_changes = match &run {
            Some(r) if s.tool.is_agent() => {
                let now = snapshots
                    .entry((s.tool, s.cwd.clone()))
                    .or_insert_with(|| config::snapshot(&files, s.tool, &s.cwd));
                config::diff(&r.mcp, now)
            }
            _ => Vec::new(),
        };
        let job = job::get(id).map(|j| j.phase).filter(|p| p.is_active());
        if env_changes.is_empty() && mcp_changes.is_empty() && job.is_none() {
            continue;
        }
        let live = host.sessions.live(host, id).or(info.live);
        let (project_name, workspace_name) = names(host, &s);
        out.push(Pending {
            session: id,
            project_id: s.project_id,
            workspace_id: s.workspace_id,
            title: s.title,
            tool: s.tool,
            project_name,
            workspace_name,
            state: run_state(live.as_ref()),
            env_from: s.env_version,
            env_changes,
            mcp_changes,
            job,
        });
    }
    out.sort_by(|a, b| (&a.project_name, &a.title).cmp(&(&b.project_name, &b.title)));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(tool: Tool, state: RunState) -> Facts {
        Facts {
            tool,
            state,
            cwd: "/w".into(),
            has_conversation: true,
            transcript: (tool == Tool::Claude).then_some(true),
            folder_exists: true,
            cli_found: true,
        }
    }

    fn kinds(b: &[Blocker]) -> Vec<BlockerKind> {
        b.iter().map(|b| b.kind).collect()
    }

    #[test]
    fn planning_matrix() {
        // Agents with a saved conversation resume it, whatever they are doing.
        for state in [RunState::Idle, RunState::Busy, RunState::NeedsAnswer, RunState::Starting] {
            assert_eq!(decide(&facts(Tool::Claude, state)), (Some(Method::Resume), vec![]));
            assert_eq!(decide(&facts(Tool::Codex, state)).0, Some(Method::Resume));
        }
        // Claude before its first message: nothing to lose, start again in the same session.
        let f = Facts { transcript: Some(false), ..facts(Tool::Claude, RunState::Idle) };
        assert_eq!(decide(&f).0, Some(Method::StartFresh));
        // Codex whose thread id was never found cannot promise the same conversation.
        let f = Facts { has_conversation: false, ..facts(Tool::Codex, RunState::Busy) };
        let (m, b) = decide(&f);
        assert_eq!((m, kinds(&b)), (None, vec![BlockerKind::NoConversation]));
        // Shells restart; their state is lost (the UI says so).
        assert_eq!(decide(&facts(Tool::Shell, RunState::Idle)).0, Some(Method::RestartShell));
        // Not running: nothing to refresh.
        let (m, b) = decide(&facts(Tool::Claude, RunState::Ended));
        assert_eq!((m, kinds(&b)), (None, vec![BlockerKind::NotRunning]));
        // Folder and CLI problems are reported together with their reasons.
        let f = Facts { folder_exists: false, cli_found: false, ..facts(Tool::Codex, RunState::Idle) };
        let (m, b) = decide(&f);
        assert_eq!((m, kinds(&b)), (None, vec![BlockerKind::FolderMissing, BlockerKind::CliMissing]));
        assert!(b[0].message.contains("/w"));
        // A shell does not need a CLI.
        let f = Facts { cli_found: false, ..facts(Tool::Shell, RunState::Busy) };
        assert_eq!(decide(&f).0, Some(Method::RestartShell));
    }

    #[test]
    fn run_state_reads_live_state() {
        let live = |phase, busy, attention: Option<AttentionKind>| SessionLive {
            id: Uuid::nil(),
            phase,
            busy,
            attention: attention.map(|kind| runwick_core::api::sessions::Attention {
                kind,
                text: String::new(),
                since: runwick_core::now(),
            }),
            last_output_at: None,
            cols: 80,
            rows: 24,
            pid: Some(1),
            input_owner: None,
            unread: false,
            error: None,
            error_kind: None,
            input_owner_client: None,
            foreground_job: false,
            input_prompt: false,
            leftovers: Vec::new(),
        };
        assert_eq!(run_state(None), RunState::Ended);
        assert_eq!(run_state(Some(&live(Phase::Running, true, None))), RunState::Busy);
        assert_eq!(run_state(Some(&live(Phase::Running, false, None))), RunState::Idle);
        assert_eq!(run_state(Some(&live(Phase::Running, true, Some(AttentionKind::Approval)))), RunState::NeedsAnswer);
        assert_eq!(run_state(Some(&live(Phase::Running, false, Some(AttentionKind::Notify)))), RunState::Idle);
        assert_eq!(run_state(Some(&live(Phase::Exited, false, None))), RunState::Ended);
        assert_eq!(run_state(Some(&live(Phase::Starting, false, None))), RunState::Starting);
    }

    #[test]
    fn transcripts_are_found_by_folder_name_or_by_scanning() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path();
        std::fs::create_dir_all(root.join("-Users-me-my-app")).unwrap();
        std::fs::write(root.join("-Users-me-my-app").join("abc.jsonl"), "{}").unwrap();
        assert!(transcript_in(root, "abc", "/Users/me/my.app"));
        std::fs::create_dir_all(root.join("-private-tmp-x")).unwrap();
        std::fs::write(root.join("-private-tmp-x").join("def.jsonl"), "{}").unwrap();
        assert!(transcript_in(root, "def", "/tmp/x"));
        assert!(!transcript_in(root, "zzz", "/Users/me/my.app"));
    }

    #[test]
    fn run_records_round_trip() {
        let store = Store::open_in_memory().unwrap();
        let id = Uuid::new_v4();
        assert_eq!(RunRecord::load(&store, id).unwrap(), None);
        let mut rec = RunRecord { env_version: 3, env_unreadable: vec!["TOKEN".into()], mcp: Snapshot::new() };
        rec.mcp.insert("claude:user:github".into(), "ab12".into());
        rec.save(&store, id).unwrap();
        assert_eq!(RunRecord::load(&store, id).unwrap(), Some(rec.clone()));
        rec.env_version = 4;
        rec.save(&store, id).unwrap();
        assert_eq!(RunRecord::load(&store, id).unwrap().unwrap().env_version, 4);
    }
}
