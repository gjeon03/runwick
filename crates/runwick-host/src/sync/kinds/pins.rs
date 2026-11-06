//! `pins/<id>.json`: a pin's snapshot text, note and where it came from. Any computer may edit
//! or remove a pin; removals travel as tombstones.

use super::{opt_str, opt_uuid, str_field, ts_text};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use uuid::Uuid;

pub const DIR: &str = "pins";

pub fn path_of(id: Uuid) -> String {
    format!("{DIR}/{id}.json")
}

pub fn render(c: &Connection, out: &mut BTreeMap<String, Value>) -> rusqlite::Result<()> {
    let mut st = c.prepare(
        "SELECT id, kind, title, note, text, message_id, session_id, project_id, session_title, tool, source_at,
                created_at, updated_at FROM pins",
    )?;
    let rows = st.query_map([], |r| {
        let id: Uuid = r.get(0)?;
        Ok((
            path_of(id),
            json!({
                "id": id.to_string(),
                "kind": r.get::<_, String>(1)?,
                "title": r.get::<_, String>(2)?,
                "note": r.get::<_, String>(3)?,
                "text": r.get::<_, String>(4)?,
                "message_id": r.get::<_, Option<Uuid>>(5)?.map(|u| u.to_string()),
                "session_id": r.get::<_, Option<Uuid>>(6)?.map(|u| u.to_string()),
                "project_id": r.get::<_, Option<Uuid>>(7)?.map(|u| u.to_string()),
                "session_title": r.get::<_, String>(8)?,
                "tool": r.get::<_, Option<String>>(9)?,
                "source_at": r.get::<_, String>(10)?,
                "created_at": r.get::<_, String>(11)?,
                "updated_at": r.get::<_, String>(12)?,
            }),
        ))
    })?;
    for row in rows {
        let (p, v) = row?;
        out.insert(p, v);
    }
    Ok(())
}

pub fn apply(c: &Connection, path: &str, merged: Option<&Value>) -> Result<(), String> {
    let id = super::uuid_at(path, 1).ok_or("파일 이름이 핀 id가 아니에요")?;
    let db = |e: rusqlite::Error| e.to_string();
    let Some(v) = merged else {
        c.execute("DELETE FROM pins WHERE id = ?1", [id]).map_err(db)?;
        return Ok(());
    };
    if super::uuid_field(v, "id")? != id {
        return Err("파일 이름과 id가 달라요".into());
    }
    let kind = str_field(v, "kind")?;
    if !matches!(kind.as_str(), "message" | "excerpt" | "terminal") {
        return Err(format!("알 수 없는 핀 종류예요: {kind}"));
    }
    c.execute(
        "INSERT INTO pins (id, kind, title, note, text, message_id, session_id, project_id, session_title, tool,
             source_at, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
         ON CONFLICT(id) DO UPDATE SET kind = excluded.kind, title = excluded.title, note = excluded.note,
             text = excluded.text, message_id = excluded.message_id, session_id = excluded.session_id,
             project_id = excluded.project_id, session_title = excluded.session_title, tool = excluded.tool,
             source_at = excluded.source_at, created_at = excluded.created_at, updated_at = excluded.updated_at",
        params![
            id,
            kind,
            str_field(v, "title")?,
            opt_str(v, "note").unwrap_or_default(),
            str_field(v, "text")?,
            opt_uuid(v, "message_id"),
            opt_uuid(v, "session_id"),
            opt_uuid(v, "project_id"),
            opt_str(v, "session_title").unwrap_or_default(),
            opt_str(v, "tool").filter(|t| matches!(t.as_str(), "claude" | "codex" | "shell")),
            ts_text(v, "source_at")?,
            ts_text(v, "created_at")?,
            ts_text(v, "updated_at")?,
        ],
    )
    .map_err(db)?;
    Ok(())
}

/// `updated_at` of a pin here, for tombstones.
pub fn updated_at(c: &Connection, id: Uuid) -> rusqlite::Result<Option<String>> {
    use rusqlite::OptionalExtension;
    c.query_row("SELECT updated_at FROM pins WHERE id = ?1", [id], |r| r.get(0)).optional()
}
