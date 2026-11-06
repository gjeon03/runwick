//! `sessions/<project>/<session>.json`: a session's record, written only by the computer that
//! ran it. Other computers keep it as a read-only record (`origin_host` set): it is never run or
//! resumed there, and its provider conversation id is not kept (the transcript is not there).

use super::{Cx, bool_field, opt_str, opt_ts_text, opt_uuid, str_field, ts_text, uuid_field};
use crate::store::ts_to_sql;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use uuid::Uuid;

pub const DIR: &str = "sessions";

pub fn path_of(project: Uuid, session: Uuid) -> String {
    format!("{DIR}/{project}/{session}.json")
}

/// Sessions this computer ran, as `(path, record)`.
pub fn render_own(cx: &Cx, c: &Connection) -> rusqlite::Result<Vec<(String, Value)>> {
    let mut st = c.prepare(&format!(
        "SELECT s.id, s.project_id, s.title, s.title_user_set, s.tool, s.provider_conv_id, s.parent_id, s.role, s.cwd,
                s.created_at, s.last_active_at, s.ended_at, s.exit_code, s.archived
         FROM sessions s JOIN projects p ON p.id = s.project_id
         WHERE s.origin_host IS NULL AND {}",
        super::projects::SYNCED
    ))?;
    let rows = st.query_map([], |r| {
        let id: Uuid = r.get(0)?;
        let project: Uuid = r.get(1)?;
        Ok((
            path_of(project, id),
            json!({
                "id": id.to_string(),
                "project_id": project.to_string(),
                "origin": { "host_id": cx.device.host_id.to_string(), "device": cx.device.name },
                "title": r.get::<_, String>(2)?,
                "title_user_set": r.get::<_, bool>(3)?,
                "tool": r.get::<_, String>(4)?,
                "provider_conv_id": r.get::<_, Option<String>>(5)?,
                "parent_id": r.get::<_, Option<Uuid>>(6)?.map(|u| u.to_string()),
                "role": r.get::<_, Option<String>>(7)?,
                "cwd": r.get::<_, String>(8)?,
                "created_at": r.get::<_, String>(9)?,
                "last_active_at": r.get::<_, Option<String>>(10)?,
                "ended_at": r.get::<_, Option<String>>(11)?,
                "exit_code": r.get::<_, Option<i64>>(12)?,
                "archived": r.get::<_, bool>(13)?,
            }),
        ))
    })?;
    rows.collect()
}

/// Origin of a session file: `(host_id, device name)`.
pub fn origin_of(v: &Value) -> Option<(Uuid, String)> {
    let o = v.get("origin")?;
    Some((opt_uuid(o, "host_id")?, opt_str(o, "device").unwrap_or_else(|| "다른 컴퓨터".into())))
}

/// Import another computer's session record (inserted or updated, never a local session).
/// Returns false when the file is this computer's own.
pub fn apply(cx: &Cx, c: &Connection, path: &str, v: &Value) -> Result<bool, String> {
    let (host, device) = origin_of(v).ok_or("만든 기기 정보가 없어요")?;
    if host == cx.device.host_id {
        return Ok(false);
    }
    let id = uuid_field(v, "id")?;
    let project = uuid_field(v, "project_id")?;
    if super::uuid_at(path, 2) != Some(id) || super::uuid_at(path, 1) != Some(project) {
        return Err("파일 위치와 id가 달라요".into());
    }
    let tool = str_field(v, "tool")?;
    if !matches!(tool.as_str(), "claude" | "codex" | "shell") {
        return Err(format!("알 수 없는 도구예요: {tool}"));
    }
    let db = |e: rusqlite::Error| e.to_string();
    if !super::project_exists(c, project).map_err(db)? {
        return Err("세션의 프로젝트가 아직 없어요".into());
    }
    let ws = default_workspace(c, project).map_err(db)?;
    c.execute(
        "INSERT INTO sessions (id, project_id, workspace_id, title, title_user_set, tool, provider_conv_id, parent_id,
             role, cwd, created_at, last_active_at, ended_at, exit_code, hidden, archived, env_version, was_running,
             origin_host, origin_device)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, ?7, ?8, ?9, ?10, ?11, ?12, ?13, 0, ?14, 0, 0, ?15, ?16)
         ON CONFLICT(id) DO UPDATE SET title = excluded.title, title_user_set = excluded.title_user_set,
             parent_id = excluded.parent_id, role = excluded.role, last_active_at = excluded.last_active_at,
             ended_at = excluded.ended_at, exit_code = excluded.exit_code, archived = excluded.archived,
             origin_device = excluded.origin_device
         WHERE sessions.origin_host IS NOT NULL",
        params![
            id,
            project,
            ws,
            str_field(v, "title")?,
            bool_field(v, "title_user_set"),
            tool,
            opt_uuid(v, "parent_id"),
            opt_str(v, "role"),
            str_field(v, "cwd").unwrap_or_default(),
            ts_text(v, "created_at")?,
            opt_ts_text(v, "last_active_at"),
            opt_ts_text(v, "ended_at"),
            v.get("exit_code").and_then(Value::as_i64),
            bool_field(v, "archived"),
            host,
            device,
        ],
    )
    .map_err(db)?;
    Ok(true)
}

/// The project's default workspace on this computer (made when the project has none).
pub fn default_workspace(c: &Connection, project: Uuid) -> rusqlite::Result<Uuid> {
    let found: Option<Uuid> = c
        .query_row(
            "SELECT id FROM workspaces WHERE project_id = ?1 AND kind = 'default' AND remote_device_id IS NULL
             ORDER BY created_at LIMIT 1",
            [project],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(id) = found {
        return Ok(id);
    }
    let root: String = c.query_row("SELECT root FROM projects WHERE id = ?1", [project], |r| r.get(0))?;
    let id = Uuid::now_v7();
    c.execute(
        "INSERT INTO workspaces (id, project_id, name, path, kind, created_at) VALUES (?1, ?2, ?3, ?4, 'default', ?5)",
        params![id, project, crate::projects::DEFAULT_WORKSPACE_NAME, root, ts_to_sql(&runwick_core::now())],
    )?;
    Ok(id)
}

/// Origin host of a local session record: None for this computer's own sessions.
pub fn origin_host(c: &Connection, session: Uuid) -> rusqlite::Result<Option<Option<Uuid>>> {
    c.query_row("SELECT origin_host FROM sessions WHERE id = ?1", [session], |r| r.get::<_, Option<Uuid>>(0)).optional()
}
