//! `collab/<project>/<request>.json`: finished delegation records (done, failed, cancelled) of
//! sessions this computer ran, written as stored. Other computers keep them as read-only
//! history next to the synced session; requests still in progress never travel, so no computer
//! acts on another computer's work.
//!
//! Rows are copied column by column (whatever columns the table has), so the delegation feature
//! can grow its table without changing this file.

use super::Cx;
use rusqlite::types::{Value as Sql, ValueRef};
use rusqlite::{Connection, OptionalExtension, params_from_iter};
use serde_json::{Map, Value};
use uuid::Uuid;

pub const DIR: &str = "collab";
const TABLE: &str = "collab_requests";
/// Statuses of finished requests (the only ones that travel).
const FINISHED: [&str; 3] = ["done", "failed", "cancelled"];

pub fn path_of(project: Uuid, request: Uuid) -> String {
    format!("{DIR}/{project}/{request}.json")
}

/// `(name, declared type)` of the table's columns.
fn columns(c: &Connection) -> rusqlite::Result<Vec<(String, String)>> {
    let mut st = c.prepare(&format!("PRAGMA table_info({TABLE})"))?;
    let rows = st.query_map([], |r| Ok((r.get::<_, String>(1)?, r.get::<_, String>(2)?.to_uppercase())))?;
    rows.collect()
}

pub fn render_own(c: &Connection) -> rusqlite::Result<Vec<(String, Value)>> {
    let cols = columns(c)?;
    if cols.is_empty() {
        return Ok(Vec::new());
    }
    let list = cols.iter().map(|(n, _)| format!("r.\"{n}\"")).collect::<Vec<_>>().join(", ");
    let mut st = c.prepare(&format!(
        "SELECT s.project_id, {list} FROM {TABLE} r JOIN sessions s ON s.id = r.worker_id JOIN projects p ON p.id = s.project_id
         WHERE s.origin_host IS NULL AND r.status IN ('done', 'failed', 'cancelled') AND {}",
        super::projects::SYNCED
    ))?;
    let rows = st.query_map([], |r| {
        let project: Uuid = r.get(0)?;
        let mut m = Map::new();
        for (i, (name, _)) in cols.iter().enumerate() {
            m.insert(name.clone(), to_json(r.get_ref(i + 1)?));
        }
        Ok((project, m))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (project, m) = row?;
        let Some(id) = m.get("id").and_then(Value::as_str).and_then(|s| s.parse::<Uuid>().ok()) else {
            continue;
        };
        out.push((path_of(project, id), Value::Object(m)));
    }
    Ok(out)
}

fn to_json(v: ValueRef) -> Value {
    match v {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(i) => Value::from(i),
        ValueRef::Real(f) => Value::from(f),
        ValueRef::Text(t) => Value::String(String::from_utf8_lossy(t).to_string()),
        ValueRef::Blob(b) => match Uuid::from_slice(b) {
            Ok(u) => Value::String(u.to_string()),
            Err(_) => Value::Null,
        },
    }
}

/// Import another computer's finished request. Returns false when it is not imported here
/// (this computer's own, still running, or its session is not here).
pub fn apply(cx: &Cx, c: &Connection, v: &Value) -> Result<bool, String> {
    let db = |e: rusqlite::Error| e.to_string();
    let obj = v.as_object().ok_or("기록 형식이 아니에요")?;
    let status = obj.get("status").and_then(Value::as_str).unwrap_or("");
    if !FINISHED.contains(&status) {
        return Ok(false);
    }
    let worker = obj
        .get("worker_id")
        .and_then(Value::as_str)
        .and_then(|s| s.parse::<Uuid>().ok())
        .ok_or("작업한 세션 id가 없어요")?;
    match super::sessions::origin_host(c, worker).map_err(db)? {
        Some(Some(origin)) if origin != cx.device.host_id => {}
        Some(_) => return Ok(false),
        None => return Err("작업한 세션 기록이 아직 없어요".into()),
    }
    // A request of this computer is never replaced by a file that claims the same id.
    let id = obj.get("id").and_then(Value::as_str).and_then(|s| s.parse::<Uuid>().ok()).ok_or("요청 id가 없어요")?;
    let mine: Option<bool> = c
        .query_row(
            &format!(
                "SELECT s.origin_host IS NULL FROM {TABLE} r JOIN sessions s ON s.id = r.worker_id WHERE r.id = ?1"
            ),
            [id],
            |r| r.get(0),
        )
        .optional()
        .map_err(db)?;
    if mine == Some(true) {
        return Err("이 컴퓨터의 기록과 같은 id라 덮어쓰지 않았어요".into());
    }
    let cols = columns(c).map_err(db)?;
    let mut names = Vec::new();
    let mut values: Vec<Sql> = Vec::new();
    for (name, ty) in &cols {
        let Some(x) = obj.get(name) else { continue };
        names.push(format!("\"{name}\""));
        values.push(match x {
            Value::Null => Sql::Null,
            Value::Bool(b) => Sql::Integer(*b as i64),
            Value::Number(n) => n.as_i64().map(Sql::Integer).or_else(|| n.as_f64().map(Sql::Real)).unwrap_or(Sql::Null),
            Value::String(s) if ty == "BLOB" => {
                Sql::Blob(s.parse::<Uuid>().map_err(|_| format!("{name} 값이 id가 아니에요"))?.as_bytes().to_vec())
            }
            Value::String(s) => Sql::Text(s.clone()),
            other => Sql::Text(other.to_string()),
        });
    }
    if !names.iter().any(|n| n == "\"id\"") {
        return Err("요청 id가 없어요".into());
    }
    let marks = vec!["?"; names.len()].join(", ");
    c.execute(
        &format!("INSERT OR REPLACE INTO {TABLE} ({}) VALUES ({marks})", names.join(", ")),
        params_from_iter(values),
    )
    .map_err(db)?;
    Ok(true)
}
