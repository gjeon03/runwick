//! `timeline/<project>/memo.json` (the person's memo and remaining items) and
//! `timeline/<project>/summaries/<YYYY-MM-DD>.json` (day summaries). Observed events are
//! rebuilt from records on each computer and are not written here.

use super::{opt_str, str_field, ts_text};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use uuid::Uuid;

pub const DIR: &str = "timeline";

pub fn memo_path(project: Uuid) -> String {
    format!("{DIR}/{project}/memo.json")
}

pub fn summary_path(project: Uuid, day: &str) -> String {
    format!("{DIR}/{project}/summaries/{day}.json")
}

pub fn render(c: &Connection, out: &mut BTreeMap<String, Value>) -> rusqlite::Result<()> {
    let synced = super::projects::SYNCED;
    let mut st = c.prepare(&format!(
        "SELECT n.project_id, n.memo, n.items, n.updated_at FROM project_notes n JOIN projects p ON p.id = n.project_id
         WHERE {synced}"
    ))?;
    let rows = st.query_map([], |r| {
        let project: Uuid = r.get(0)?;
        let items: Value = serde_json::from_str(&r.get::<_, String>(2)?).unwrap_or_else(|_| json!([]));
        Ok((
            memo_path(project),
            json!({
                "project_id": project.to_string(),
                "memo": r.get::<_, String>(1)?,
                "items": items,
                "updated_at": r.get::<_, String>(3)?,
            }),
        ))
    })?;
    for row in rows {
        let (p, v) = row?;
        out.insert(p, v);
    }
    let mut st = c.prepare(&format!(
        "SELECT s.project_id, s.day, s.text, s.generated_at, s.basis_count, s.provider
         FROM timeline_summaries s JOIN projects p ON p.id = s.project_id WHERE {synced}"
    ))?;
    let rows = st.query_map([], |r| {
        let project: Uuid = r.get(0)?;
        let day: String = r.get(1)?;
        Ok((
            summary_path(project, &day),
            json!({
                "project_id": project.to_string(),
                "day": day,
                "text": r.get::<_, String>(2)?,
                "generated_at": r.get::<_, String>(3)?,
                "basis_count": r.get::<_, i64>(4)?,
                "provider": r.get::<_, Option<String>>(5)?,
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
    // Notes and summaries are not deleted by people; a missing file changes nothing.
    let Some(v) = merged else { return Ok(()) };
    let project = super::uuid_at(path, 1).ok_or("폴더 이름이 프로젝트 id가 아니에요")?;
    let db = |e: rusqlite::Error| e.to_string();
    if !super::project_exists(c, project).map_err(db)? {
        return Err("프로젝트가 아직 없어요".into());
    }
    let parts: Vec<&str> = path.split('/').collect();
    match parts.as_slice() {
        [_, _, "memo.json"] => {
            let items = v.get("items").cloned().unwrap_or_else(|| json!([]));
            if !items.is_array() {
                return Err("남은 일 목록 형식이 아니에요".into());
            }
            c.execute(
                "INSERT INTO project_notes (project_id, memo, items, updated_at) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(project_id) DO UPDATE SET memo = excluded.memo, items = excluded.items,
                     updated_at = excluded.updated_at",
                params![project, opt_str(v, "memo").unwrap_or_default(), items.to_string(), ts_text(v, "updated_at")?],
            )
            .map_err(db)?;
        }
        [_, _, "summaries", file] => {
            let day = str_field(v, "day")?;
            if file.strip_suffix(".json") != Some(day.as_str())
                || chrono::NaiveDate::parse_from_str(&day, "%Y-%m-%d").is_err()
            {
                return Err("요약 날짜가 올바르지 않아요".into());
            }
            c.execute(
                "INSERT INTO timeline_summaries (project_id, day, text, generated_at, basis_count, provider)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(project_id, day) DO UPDATE SET text = excluded.text, generated_at = excluded.generated_at,
                     basis_count = excluded.basis_count, provider = excluded.provider",
                params![
                    project,
                    day,
                    str_field(v, "text")?,
                    ts_text(v, "generated_at")?,
                    v.get("basis_count").and_then(Value::as_i64).unwrap_or(0),
                    opt_str(v, "provider"),
                ],
            )
            .map_err(db)?;
        }
        _ => return Err("알 수 없는 타임라인 파일이에요".into()),
    }
    Ok(())
}
