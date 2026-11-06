//! `messages/<project>/<session>.jsonl`: a session's collected conversation, one message per
//! line, written only by the computer that ran the session. Other computers add the messages
//! they do not have yet (by id), so records are a union and never duplicated.

use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const DIR: &str = "messages";

pub fn path_of(project: Uuid, session: Uuid) -> String {
    format!("{DIR}/{project}/{session}.jsonl")
}

/// One line. Field order is fixed so the same messages always give the same text.
#[derive(Debug, Serialize, Deserialize)]
struct Line {
    id: Uuid,
    seq: i64,
    provider_msg_id: String,
    role: String,
    text: String,
    at: String,
    collected_at: String,
    #[serde(default)]
    sidechain: bool,
    #[serde(default)]
    tool_name: Option<String>,
}

/// Message files of this computer's sessions: `(path, signature, session)`. The signature
/// (count and last row) changes whenever messages are added, so unchanged files are skipped.
pub fn own(c: &Connection) -> rusqlite::Result<Vec<(String, String, Uuid)>> {
    let mut st = c.prepare(&format!(
        "SELECT m.session_id, s.project_id, COUNT(*), MAX(m.rid)
         FROM messages m JOIN sessions s ON s.id = m.session_id JOIN projects p ON p.id = s.project_id
         WHERE s.origin_host IS NULL AND {}
         GROUP BY m.session_id",
        super::projects::SYNCED
    ))?;
    let rows = st.query_map([], |r| {
        let session: Uuid = r.get(0)?;
        let project: Uuid = r.get(1)?;
        Ok((path_of(project, session), format!("{}:{}", r.get::<_, i64>(2)?, r.get::<_, i64>(3)?), session))
    })?;
    rows.collect()
}

pub fn render(c: &Connection, session: Uuid) -> rusqlite::Result<String> {
    let mut st = c.prepare(
        "SELECT id, seq, provider_msg_id, role, text, at, collected_at, sidechain, tool_name
         FROM messages WHERE session_id = ?1 ORDER BY seq, rid",
    )?;
    let rows = st.query_map([session], |r| {
        Ok(Line {
            id: r.get(0)?,
            seq: r.get(1)?,
            provider_msg_id: r.get(2)?,
            role: r.get(3)?,
            text: r.get(4)?,
            at: r.get(5)?,
            collected_at: r.get(6)?,
            sidechain: r.get(7)?,
            tool_name: r.get(8)?,
        })
    })?;
    let mut out = String::new();
    for line in rows {
        out.push_str(&serde_json::to_string(&line?).unwrap_or_default());
        out.push('\n');
    }
    Ok(out)
}

/// Add another computer's messages to its session record. Returns how many were new.
/// Messages per transaction: a big file never holds the store for long.
const CHUNK: usize = 1000;

/// Add another computer's messages to its session record. Returns how many were new.
pub fn import(store: &crate::store::Store, path: &str, bytes: &[u8]) -> Result<u64, String> {
    let session = super::uuid_at(path, 2).ok_or("파일 이름이 세션 id가 아니에요")?;
    let db = |e: rusqlite::Error| e.to_string();
    let Some(origin) = store.with(|c| super::sessions::origin_host(c, session)).map_err(db)? else {
        return Err("세션 기록이 아직 없어요".into());
    };
    if origin.is_none() {
        // This computer's own session: its file is written here.
        return Ok(0);
    }
    let lines = parse(bytes)?;
    let mut added = 0;
    for chunk in lines.chunks(CHUNK) {
        added += store.tx(|c| insert(c, session, chunk)).map_err(db)?;
        super::breathe();
    }
    Ok(added)
}

/// Every line of a file, checked before anything is written (a damaged file adds nothing).
fn parse(bytes: &[u8]) -> Result<Vec<Line>, String> {
    let text = std::str::from_utf8(bytes).map_err(|_| "UTF-8 텍스트가 아니에요".to_string())?;
    let mut lines = Vec::new();
    for (n, raw) in text.lines().enumerate().filter(|(_, l)| !l.trim().is_empty()) {
        let line: Line = serde_json::from_str(raw).map_err(|_| format!("{}번째 줄을 읽지 못했어요", n + 1))?;
        if !matches!(line.role.as_str(), "user" | "assistant" | "tool") {
            return Err(format!("{}번째 줄의 역할을 알 수 없어요", n + 1));
        }
        for t in [&line.at, &line.collected_at] {
            chrono::DateTime::parse_from_rfc3339(t).map_err(|_| format!("{}번째 줄의 시각 형식이 아니에요", n + 1))?;
        }
        lines.push(line);
    }
    Ok(lines)
}

fn insert(c: &Connection, session: Uuid, lines: &[Line]) -> rusqlite::Result<u64> {
    let (project, workspace): (Uuid, Uuid) =
        c.query_row("SELECT project_id, workspace_id FROM sessions WHERE id = ?1", [session], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?;
    let mut st = c.prepare_cached(
        "INSERT OR IGNORE INTO messages (id, session_id, project_id, workspace_id, provider_msg_id, seq, role, text,
             at, collected_at, sidechain, tool_name)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
    )?;
    let mut added = 0;
    for line in lines {
        added += st.execute(params![
            line.id,
            session,
            project,
            workspace,
            line.provider_msg_id,
            line.seq,
            line.role,
            line.text,
            line.at,
            line.collected_at,
            line.sidechain,
            line.tool_name
        ])? as u64;
    }
    Ok(added)
}

/// Message ids in a file (for previews: how many are new here).
pub fn ids(bytes: &[u8]) -> Vec<Uuid> {
    let text = String::from_utf8_lossy(bytes);
    text.lines().filter_map(|l| serde_json::from_str::<Line>(l).ok()).map(|l| l.id).collect()
}
