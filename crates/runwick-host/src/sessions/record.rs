//! Session records in SQLite (table from migration `0003_sessions`).

use crate::store::{Store, db_err, opt_ts, ts_from_sql, ts_to_sql};
use runwick_core::Ts;
use runwick_core::api::sessions::{Session, SessionId, SessionQuery, Tool};
use runwick_core::rpc::RpcError;
use rusqlite::{OptionalExtension, Row, params};
use uuid::Uuid;

pub fn tool_to_sql(t: Tool) -> &'static str {
    match t {
        Tool::Claude => "claude",
        Tool::Codex => "codex",
        Tool::Shell => "shell",
    }
}

fn tool_from_sql(s: &str) -> Tool {
    match s {
        "claude" => Tool::Claude,
        "codex" => Tool::Codex,
        _ => Tool::Shell,
    }
}

/// A record plus the host-side flag that is not part of the API type.
pub struct Stored {
    pub session: Session,
    /// The run was alive when the host last stopped (or is alive now).
    pub was_running: bool,
}

fn from_row(r: &Row) -> rusqlite::Result<Stored> {
    Ok(Stored {
        session: Session {
            id: r.get("id")?,
            project_id: r.get("project_id")?,
            workspace_id: r.get("workspace_id")?,
            // Titles reach tabs, the sidebar and the menu bar: invisible direction and
            // zero-width characters are left out whoever set them (first message, program).
            title: super::live::visible(&r.get::<_, String>("title")?),
            title_user_set: r.get("title_user_set")?,
            tool: tool_from_sql(&r.get::<_, String>("tool")?),
            provider_conv_id: r.get("provider_conv_id")?,
            parent_id: r.get("parent_id")?,
            role: r.get("role")?,
            cwd: r.get("cwd")?,
            created_at: ts_from_sql(&r.get::<_, String>("created_at")?),
            last_active_at: opt_ts(r.get("last_active_at")?),
            ended_at: opt_ts(r.get("ended_at")?),
            exit_code: r.get("exit_code")?,
            hidden: r.get("hidden")?,
            archived: r.get("archived")?,
            env_version: r.get::<_, i64>("env_version")? as u64,
            interrupted: false,
            origin_device: r.get("origin_device").unwrap_or(None),
        },
        was_running: r.get("was_running")?,
    })
}

pub fn insert(store: &Store, s: &Session) -> Result<(), RpcError> {
    store
        .with(|c| {
            c.execute(
                "INSERT INTO sessions (id, project_id, workspace_id, title, title_user_set, tool, provider_conv_id,
                    parent_id, role, cwd, created_at, last_active_at, hidden, archived, env_version, was_running)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?11, 0, 0, ?12, 0)",
                params![
                    s.id,
                    s.project_id,
                    s.workspace_id,
                    s.title,
                    s.title_user_set,
                    tool_to_sql(s.tool),
                    s.provider_conv_id,
                    s.parent_id,
                    s.role,
                    s.cwd,
                    ts_to_sql(&s.created_at),
                    s.env_version as i64,
                ],
            )
        })
        .map_err(db_err)?;
    Ok(())
}

pub fn get(store: &Store, id: SessionId) -> Result<Stored, RpcError> {
    store
        .with(|c| c.query_row("SELECT * FROM sessions WHERE id = ?1", [id], from_row).optional())
        .map_err(db_err)?
        .ok_or_else(|| RpcError::not_found("세션을 찾을 수 없어요"))
}

pub fn list(store: &Store, q: &SessionQuery) -> Result<Vec<Stored>, RpcError> {
    store
        .with(|c| {
            let mut st = c.prepare(
                "SELECT * FROM sessions
                 WHERE (?1 IS NULL OR project_id = ?1) AND (?2 IS NULL OR workspace_id = ?2)
                   AND (?3 OR hidden = 0) AND (?4 OR archived = 0)
                 ORDER BY created_at, id",
            )?;
            let rows =
                st.query_map(params![q.project_id, q.workspace_id, q.include_hidden, q.include_archived], from_row)?;
            rows.collect()
        })
        .map_err(db_err)
}

/// A new run started: clear the previous end, remember the run is alive.
pub fn mark_started(store: &Store, id: SessionId, now: &Ts, env_version: u64) -> Result<(), RpcError> {
    store
        .with(|c| {
            c.execute(
                "UPDATE sessions SET was_running = 1, ended_at = NULL, exit_code = NULL, last_active_at = ?2,
                    env_version = ?3 WHERE id = ?1",
                params![id, ts_to_sql(now), env_version as i64],
            )
        })
        .map_err(db_err)?;
    Ok(())
}

pub fn mark_ended(store: &Store, id: SessionId, now: &Ts, exit_code: Option<i32>) -> Result<(), RpcError> {
    store
        .with(|c| {
            c.execute(
                "UPDATE sessions SET was_running = 0, ended_at = ?2, exit_code = ?3, last_active_at = ?2 WHERE id = ?1",
                params![id, ts_to_sql(now), exit_code],
            )
        })
        .map_err(db_err)?;
    Ok(())
}

pub fn touch(store: &Store, id: SessionId, at: &Ts) -> Result<(), RpcError> {
    store
        .with(|c| c.execute("UPDATE sessions SET last_active_at = ?2 WHERE id = ?1", params![id, ts_to_sql(at)]))
        .map_err(db_err)?;
    Ok(())
}

pub fn set_conv_id(store: &Store, id: SessionId, conv: Option<&str>) -> Result<(), RpcError> {
    store
        .with(|c| c.execute("UPDATE sessions SET provider_conv_id = ?2 WHERE id = ?1", params![id, conv]))
        .map_err(db_err)?;
    Ok(())
}

/// Claim a provider conversation id unless another session already holds it.
pub fn claim_conv_id(store: &Store, id: SessionId, conv: &str) -> Result<bool, RpcError> {
    let n = store
        .with(|c| {
            c.execute(
                "UPDATE sessions SET provider_conv_id = ?2
                 WHERE id = ?1 AND NOT EXISTS (SELECT 1 FROM sessions WHERE provider_conv_id = ?2 AND id <> ?1)",
                params![id, conv],
            )
        })
        .map_err(db_err)?;
    Ok(n == 1)
}

pub fn conv_id_taken(store: &Store, conv: &str) -> Result<bool, RpcError> {
    store
        .with(|c| {
            c.query_row("SELECT 1 FROM sessions WHERE provider_conv_id = ?1 LIMIT 1", [conv], |_| Ok(()))
                .optional()
                .map(|r| r.is_some())
        })
        .map_err(db_err)
}

/// Automatic title from the program (ignored once the person renamed the session).
pub fn set_auto_title(store: &Store, id: SessionId, title: &str) -> Result<bool, RpcError> {
    let n = store
        .with(|c| {
            c.execute(
                "UPDATE sessions SET title = ?2 WHERE id = ?1 AND title_user_set = 0 AND title <> ?2",
                params![id, title],
            )
        })
        .map_err(db_err)?;
    Ok(n == 1)
}

pub fn set_user_title(store: &Store, id: SessionId, title: &str) -> Result<(), RpcError> {
    store
        .with(|c| c.execute("UPDATE sessions SET title = ?2, title_user_set = 1 WHERE id = ?1", params![id, title]))
        .map_err(db_err)?;
    Ok(())
}

pub fn set_flag(store: &Store, id: SessionId, column: Flag, value: bool) -> Result<(), RpcError> {
    let sql = match column {
        Flag::Hidden => "UPDATE sessions SET hidden = ?2 WHERE id = ?1",
        Flag::Archived => "UPDATE sessions SET archived = ?2 WHERE id = ?1",
        Flag::WasRunning => "UPDATE sessions SET was_running = ?2 WHERE id = ?1",
    };
    store.with(|c| c.execute(sql, params![id, value])).map_err(db_err)?;
    Ok(())
}

#[derive(Debug, Clone, Copy)]
pub enum Flag {
    Hidden,
    Archived,
    WasRunning,
}

/// Ids of sessions whose run was alive when the host last stopped.
pub fn interrupted_ids(store: &Store) -> Result<Vec<Uuid>, RpcError> {
    store
        .with(|c| {
            let mut st = c.prepare("SELECT id FROM sessions WHERE was_running = 1")?;
            let rows = st.query_map([], |r| r.get(0))?;
            rows.collect()
        })
        .map_err(db_err)
}
