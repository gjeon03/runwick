//! `roles/<id>.json`: a role's instructions, preferred tool and checklist. Names are unique
//! (ignoring case); a role from another computer whose name is already used by a different role
//! here gets " (다른 컴퓨터)" added.

use super::{opt_str, str_field, ts_text};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use uuid::Uuid;

pub const DIR: &str = "roles";

pub fn path_of(id: Uuid) -> String {
    format!("{DIR}/{id}.json")
}

pub fn render(c: &Connection, out: &mut BTreeMap<String, Value>) -> rusqlite::Result<()> {
    let mut st =
        c.prepare("SELECT id, name, instructions, preferred_tool, procedure, created_at, updated_at FROM roles")?;
    let rows = st.query_map([], |r| {
        let id: Uuid = r.get(0)?;
        let procedure: Value = serde_json::from_str(&r.get::<_, String>(4)?).unwrap_or_else(|_| json!([]));
        Ok((
            path_of(id),
            json!({
                "id": id.to_string(),
                "name": r.get::<_, String>(1)?,
                "instructions": r.get::<_, String>(2)?,
                "preferred_tool": r.get::<_, Option<String>>(3)?,
                "procedure": procedure,
                "created_at": r.get::<_, String>(5)?,
                "updated_at": r.get::<_, String>(6)?,
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
    let id = super::uuid_at(path, 1).ok_or("파일 이름이 역할 id가 아니에요")?;
    let db = |e: rusqlite::Error| e.to_string();
    let Some(v) = merged else {
        c.execute("DELETE FROM roles WHERE id = ?1", [id]).map_err(db)?;
        return Ok(());
    };
    if super::uuid_field(v, "id")? != id {
        return Err("파일 이름과 id가 달라요".into());
    }
    let wanted = str_field(v, "name")?.trim().to_string();
    if wanted.is_empty() {
        return Err("역할 이름이 비어 있어요".into());
    }
    let name = free_name(c, id, &wanted).map_err(db)?;
    let steps: Vec<String> = v
        .get("procedure")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
        .unwrap_or_default();
    let tool = opt_str(v, "preferred_tool").filter(|t| matches!(t.as_str(), "claude" | "codex" | "shell"));
    c.execute(
        "INSERT INTO roles (id, name, instructions, preferred_tool, procedure, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(id) DO UPDATE SET name = excluded.name, instructions = excluded.instructions,
             preferred_tool = excluded.preferred_tool, procedure = excluded.procedure,
             created_at = excluded.created_at, updated_at = excluded.updated_at",
        params![
            id,
            name,
            opt_str(v, "instructions").unwrap_or_default(),
            tool,
            serde_json::to_string(&steps).unwrap_or_else(|_| "[]".into()),
            ts_text(v, "created_at")?,
            ts_text(v, "updated_at")?,
        ],
    )
    .map_err(db)?;
    Ok(())
}

fn free_name(c: &Connection, id: Uuid, wanted: &str) -> rusqlite::Result<String> {
    let taken = |name: &str| -> rusqlite::Result<bool> {
        c.query_row("SELECT 1 FROM roles WHERE name = ?1 COLLATE NOCASE AND id <> ?2", params![name, id], |_| Ok(()))
            .optional()
            .map(|o| o.is_some())
    };
    if !taken(wanted)? {
        return Ok(wanted.to_string());
    }
    let mut n = 1;
    loop {
        let candidate =
            if n == 1 { format!("{wanted} (다른 컴퓨터)") } else { format!("{wanted} (다른 컴퓨터 {n})") };
        if !taken(&candidate)? {
            return Ok(candidate);
        }
        n += 1;
    }
}

pub fn updated_at(c: &Connection, id: Uuid) -> rusqlite::Result<Option<String>> {
    c.query_row("SELECT updated_at FROM roles WHERE id = ?1", [id], |r| r.get(0)).optional()
}
