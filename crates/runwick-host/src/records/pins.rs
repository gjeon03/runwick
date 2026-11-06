//! Pins: a snapshot of the pinned text plus where it came from. Removing a pin never touches the
//! message, and a pin outlives its message (shown as "원문 연결 불가").

use super::db::{self, tool_from_sql, tool_to_sql};
use crate::store::{Store, db_err, ts_from_sql, ts_to_sql};
use runwick_core::Ts;
use runwick_core::api::projects::ProjectId;
use runwick_core::api::records::{Message, NewPin, Pin, PinId, PinKind, PinQuery, PinSource, PinUpdate};
use runwick_core::api::sessions::{SessionId, Tool};
use runwick_core::rpc::RpcError;
use rusqlite::{Connection, OptionalExtension, Row, params};
use uuid::Uuid;

/// Longest pinned text kept.
const MAX_TEXT: usize = 20_000;
const MAX_TITLE: usize = 200;
const MAX_NOTE: usize = 4_000;

fn kind_to_sql(k: PinKind) -> &'static str {
    match k {
        PinKind::Message => "message",
        PinKind::Excerpt => "excerpt",
        PinKind::Terminal => "terminal",
    }
}

fn kind_from_sql(s: &str) -> PinKind {
    match s {
        "message" => PinKind::Message,
        "excerpt" => PinKind::Excerpt,
        _ => PinKind::Terminal,
    }
}

const COLS: &str =
    "p.id, p.kind, p.title, p.note, p.text, p.message_id, p.session_id, p.project_id, p.session_title, p.tool,
    p.source_at, p.created_at, p.updated_at,
    (p.kind <> 'terminal' AND EXISTS (SELECT 1 FROM messages WHERE messages.id = p.message_id))";

fn from_row(r: &Row) -> rusqlite::Result<Pin> {
    Ok(Pin {
        id: r.get(0)?,
        kind: kind_from_sql(&r.get::<_, String>(1)?),
        title: r.get(2)?,
        note: r.get(3)?,
        text: r.get(4)?,
        message_id: r.get(5)?,
        session_id: r.get(6)?,
        project_id: r.get(7)?,
        session_title: crate::sessions::visible(&r.get::<_, String>(8)?),
        tool: r.get::<_, Option<String>>(9)?.as_deref().map(tool_from_sql),
        source_at: ts_from_sql(&r.get::<_, String>(10)?),
        created_at: ts_from_sql(&r.get::<_, String>(11)?),
        updated_at: ts_from_sql(&r.get::<_, String>(12)?),
        source_available: r.get(13)?,
    })
}

fn clip(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

/// Default title: the first non-empty line, at most 40 characters.
fn default_title(text: &str) -> String {
    db::title_from(text).unwrap_or_else(|| "핀".to_string())
}

pub fn get(c: &Connection, id: PinId) -> rusqlite::Result<Option<Pin>> {
    c.query_row(&format!("SELECT {COLS} FROM pins p WHERE p.id = ?1"), [id], from_row).optional()
}

pub fn list(c: &Connection, q: &PinQuery, limit: Option<u32>) -> rusqlite::Result<Vec<Pin>> {
    let limit = limit.map(|l| format!("LIMIT {l}")).unwrap_or_default();
    let mut st = c.prepare(&format!(
        "SELECT {COLS} FROM pins p
         WHERE (?1 IS NULL OR p.project_id = ?1) AND (?2 IS NULL OR p.session_id = ?2)
         ORDER BY p.created_at DESC {limit}"
    ))?;
    let rows = st.query_map(params![q.project_id, q.session_id], from_row)?;
    rows.collect()
}

fn message(c: &Connection, id: Uuid) -> rusqlite::Result<Option<Message>> {
    c.query_row(&format!("SELECT {} FROM messages m WHERE m.id = ?1", db::MSG_COLS), [id], |r| {
        db::message_from_row(r, 0)
    })
    .optional()
}

fn session_head(c: &Connection, id: SessionId) -> rusqlite::Result<Option<(ProjectId, String, Tool)>> {
    c.query_row("SELECT project_id, title, tool FROM sessions WHERE id = ?1", [id], |r| {
        Ok((r.get(0)?, r.get(1)?, tool_from_sql(&r.get::<_, String>(2)?)))
    })
    .optional()
}

struct Draft {
    kind: PinKind,
    text: String,
    message_id: Option<Uuid>,
    session_id: SessionId,
    source_at: Ts,
}

/// Validate a new pin's source and take its snapshot.
fn draft(c: &Connection, source: &PinSource) -> Result<Draft, RpcError> {
    let from_message = |id: Uuid| -> Result<Message, RpcError> {
        message(c, id).map_err(db_err)?.ok_or_else(|| RpcError::not_found("기록에서 그 메시지를 찾을 수 없어요"))
    };
    let checked = |text: &str| -> Result<String, RpcError> {
        let t = text.trim_end();
        let t = t.trim_start_matches(['\n', '\r']);
        if t.trim().is_empty() {
            return Err(RpcError::invalid("고정할 내용이 비어 있어요"));
        }
        Ok(clip(t, MAX_TEXT))
    };
    Ok(match source {
        PinSource::Message { message_id } => {
            let m = from_message(*message_id)?;
            Draft {
                kind: PinKind::Message,
                text: clip(&m.text, MAX_TEXT),
                message_id: Some(m.id),
                session_id: m.session_id,
                source_at: m.at,
            }
        }
        PinSource::LatestAnswer { session_id } => {
            let m = db::latest_answer(c, *session_id)
                .map_err(db_err)?
                .ok_or_else(|| RpcError::not_found("이 세션에서 아직 수집된 응답이 없어요"))?;
            Draft {
                kind: PinKind::Message,
                text: clip(&m.text, MAX_TEXT),
                message_id: Some(m.id),
                session_id: m.session_id,
                source_at: m.at,
            }
        }
        PinSource::Excerpt { message_id, text } => {
            let m = from_message(*message_id)?;
            Draft {
                kind: PinKind::Excerpt,
                text: checked(text)?,
                message_id: Some(m.id),
                session_id: m.session_id,
                source_at: m.at,
            }
        }
        PinSource::Terminal { session_id, text } => Draft {
            kind: PinKind::Terminal,
            text: checked(text)?,
            message_id: None,
            session_id: *session_id,
            source_at: runwick_core::now(),
        },
    })
}

/// Create a pin. Pinning a whole message twice returns the existing pin.
pub fn create(store: &Store, p: &NewPin) -> Result<(Pin, bool), RpcError> {
    store
        .tx(|tx| {
            let d = match draft(tx, &p.source) {
                Ok(d) => d,
                Err(e) => return Ok(Err(e)),
            };
            if d.kind == PinKind::Message {
                let existing: Option<Uuid> = tx
                    .query_row(
                        "SELECT id FROM pins WHERE message_id = ?1 AND kind = 'message' LIMIT 1",
                        [d.message_id],
                        |r| r.get(0),
                    )
                    .optional()?;
                if let Some(id) = existing {
                    return Ok(get(tx, id)?.map(|pin| (pin, false)).ok_or_else(|| RpcError::not_found("핀")));
                }
            }
            let Some((project, session_title, tool)) = session_head(tx, d.session_id)? else {
                return Ok(Err(RpcError::not_found("세션을 찾을 수 없어요")));
            };
            let title = p.title.as_deref().map(str::trim).filter(|t| !t.is_empty()).map(|t| clip(t, MAX_TITLE));
            let title = title.unwrap_or_else(|| default_title(&d.text));
            let note = clip(p.note.as_deref().unwrap_or("").trim(), MAX_NOTE);
            let now = ts_to_sql(&runwick_core::now());
            let id = Uuid::now_v7();
            tx.execute(
                "INSERT INTO pins (id, kind, title, note, text, message_id, session_id, project_id, session_title, tool,
                     source_at, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?12)",
                params![
                    id,
                    kind_to_sql(d.kind),
                    title,
                    note,
                    d.text,
                    d.message_id,
                    d.session_id,
                    project,
                    session_title,
                    tool_to_sql(tool),
                    ts_to_sql(&d.source_at),
                    now,
                ],
            )?;
            Ok(get(tx, id)?.map(|pin| (pin, true)).ok_or_else(|| RpcError::internal("핀을 저장하지 못했어요")))
        })
        .map_err(db_err)?
}

pub fn update(store: &Store, u: &PinUpdate) -> Result<Pin, RpcError> {
    let title = match &u.title {
        Some(t) if t.trim().is_empty() => {
            return Err(RpcError::invalid("핀 제목을 비워 둘 수 없어요"));
        }
        Some(t) => Some(clip(t.trim(), MAX_TITLE)),
        None => None,
    };
    let note = u.note.as_deref().map(|n| clip(n.trim(), MAX_NOTE));
    store
        .with(|c| {
            c.execute(
                "UPDATE pins SET title = COALESCE(?2, title), note = COALESCE(?3, note), updated_at = ?4 WHERE id = ?1",
                params![u.id, title, note, ts_to_sql(&runwick_core::now())],
            )?;
            get(c, u.id)
        })
        .map_err(db_err)?
        .ok_or_else(|| RpcError::not_found("핀을 찾을 수 없어요"))
}

/// Remove a pin (the message stays). Returns whether it existed.
pub fn remove(store: &Store, id: PinId) -> Result<bool, RpcError> {
    let n = store.with(|c| c.execute("DELETE FROM pins WHERE id = ?1", [id])).map_err(db_err)?;
    Ok(n == 1)
}
