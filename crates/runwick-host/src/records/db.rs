//! Message rows, per-session transcript cursors and reads over them.

use super::parse::{Item, Parsed};
use crate::store::{opt_ts, ts_from_sql, ts_to_sql};
use runwick_core::Ts;
use runwick_core::api::projects::{ProjectId, WorkspaceId};
use runwick_core::api::records::{
    Anchor, CaptureState, CaptureStatus, Context, ContextQuery, Message, MessageId, Role, TurnInfo,
};
use runwick_core::api::sessions::{SessionId, Tool};
use rusqlite::{Connection, OptionalExtension, Row, params};
use uuid::Uuid;

pub fn role_to_sql(r: Role) -> &'static str {
    match r {
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    }
}

fn role_from_sql(s: &str) -> Role {
    match s {
        "user" => Role::User,
        "assistant" => Role::Assistant,
        _ => Role::Tool,
    }
}

pub fn tool_from_sql(s: &str) -> Tool {
    match s {
        "claude" => Tool::Claude,
        "codex" => Tool::Codex,
        _ => Tool::Shell,
    }
}

pub fn tool_to_sql(t: Tool) -> &'static str {
    match t {
        Tool::Claude => "claude",
        Tool::Codex => "codex",
        Tool::Shell => "shell",
    }
}

fn state_to_sql(s: CaptureState) -> &'static str {
    match s {
        CaptureState::Collecting => "collecting",
        CaptureState::UpToDate => "up_to_date",
        CaptureState::NoFile => "no_file",
        CaptureState::WaitingForId => "waiting_for_id",
        CaptureState::NotApplicable => "not_applicable",
        CaptureState::Error => "error",
    }
}

fn state_from_sql(s: &str) -> CaptureState {
    match s {
        "collecting" => CaptureState::Collecting,
        "up_to_date" => CaptureState::UpToDate,
        "no_file" => CaptureState::NoFile,
        "waiting_for_id" => CaptureState::WaitingForId,
        "not_applicable" => CaptureState::NotApplicable,
        _ => CaptureState::Error,
    }
}

/// Columns read by [`message_from_row`] (table alias `m`).
pub const MSG_COLS: &str = "m.id, m.session_id, m.project_id, m.workspace_id, m.role, m.text, m.at, m.collected_at, m.seq, m.sidechain, m.tool_name";
pub const MSG_COL_COUNT: usize = 11;

pub fn message_from_row(r: &Row, base: usize) -> rusqlite::Result<Message> {
    Ok(Message {
        id: r.get(base)?,
        session_id: r.get(base + 1)?,
        project_id: r.get(base + 2)?,
        workspace_id: r.get(base + 3)?,
        role: role_from_sql(&r.get::<_, String>(base + 4)?),
        text: r.get(base + 5)?,
        at: ts_from_sql(&r.get::<_, String>(base + 6)?),
        collected_at: ts_from_sql(&r.get::<_, String>(base + 7)?),
        seq: r.get(base + 8)?,
        sidechain: r.get(base + 9)?,
        tool_name: r.get(base + 10)?,
    })
}

/// What collection needs to know about a session.
#[derive(Debug, Clone)]
pub struct SessionRow {
    pub id: SessionId,
    pub project_id: ProjectId,
    pub workspace_id: WorkspaceId,
    pub tool: Tool,
    pub conv: Option<String>,
    pub cwd: String,
    pub created_at: Ts,
    pub title: String,
    pub title_user_set: bool,
}

pub fn session_row(c: &Connection, id: SessionId) -> rusqlite::Result<Option<SessionRow>> {
    c.query_row(
        "SELECT id, project_id, workspace_id, tool, provider_conv_id, cwd, created_at, title, title_user_set
         FROM sessions WHERE id = ?1",
        [id],
        |r| {
            Ok(SessionRow {
                id: r.get(0)?,
                project_id: r.get(1)?,
                workspace_id: r.get(2)?,
                tool: tool_from_sql(&r.get::<_, String>(3)?),
                conv: r.get(4)?,
                cwd: r.get(5)?,
                created_at: ts_from_sql(&r.get::<_, String>(6)?),
                title: r.get(7)?,
                title_user_set: r.get(8)?,
            })
        },
    )
    .optional()
}

/// Agent sessions that have a conversation id (candidates for catch-up).
pub fn agent_session_ids(c: &Connection) -> rusqlite::Result<Vec<SessionId>> {
    let mut st = c.prepare("SELECT id FROM sessions WHERE tool <> 'shell' ORDER BY created_at")?;
    let rows = st.query_map([], |r| r.get(0))?;
    rows.collect()
}

/// The read position in a session's transcript and its collection state.
#[derive(Debug, Clone, PartialEq)]
pub struct Source {
    /// Conversation id the cursor belongs to (a new conversation starts a new file).
    pub conv: Option<String>,
    pub path: Option<String>,
    /// Bytes read so far (always at a line boundary).
    pub offset: u64,
    /// Length and hash of the start of the file when first read, to notice a replaced file.
    pub head_len: u64,
    pub head_hash: Option<String>,
    pub next_seq: i64,
    pub state: CaptureState,
    pub last_error: Option<String>,
    pub last_collected_at: Option<Ts>,
    pub last_checked_at: Option<Ts>,
    pub turn_ended_at: Option<Ts>,
}

impl Source {
    pub fn new(state: CaptureState) -> Self {
        Self {
            conv: None,
            path: None,
            offset: 0,
            head_len: 0,
            head_hash: None,
            next_seq: 0,
            state,
            last_error: None,
            last_collected_at: None,
            last_checked_at: None,
            turn_ended_at: None,
        }
    }

    /// Read the file again from the start (dedupe keeps what is already stored).
    pub fn rewind(&mut self) {
        self.offset = 0;
        self.head_len = 0;
        self.head_hash = None;
    }
}

pub fn load_source(c: &Connection, id: SessionId) -> rusqlite::Result<Option<Source>> {
    c.query_row(
        "SELECT conv_id, path, offset, head_len, head_hash, next_seq, state, last_error, last_collected_at,
                last_checked_at, turn_ended_at
         FROM record_sources WHERE session_id = ?1",
        [id],
        |r| {
            Ok(Source {
                conv: r.get(0)?,
                path: r.get(1)?,
                offset: r.get::<_, i64>(2)?.max(0) as u64,
                head_len: r.get::<_, i64>(3)?.max(0) as u64,
                head_hash: r.get(4)?,
                next_seq: r.get(5)?,
                state: state_from_sql(&r.get::<_, String>(6)?),
                last_error: r.get(7)?,
                last_collected_at: opt_ts(r.get(8)?),
                last_checked_at: opt_ts(r.get(9)?),
                turn_ended_at: opt_ts(r.get(10)?),
            })
        },
    )
    .optional()
}

pub fn save_source(c: &Connection, id: SessionId, s: &Source) -> rusqlite::Result<()> {
    c.execute(
        "INSERT INTO record_sources (session_id, conv_id, path, offset, head_len, head_hash, next_seq, state,
             last_error, last_collected_at, last_checked_at, turn_ended_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
         ON CONFLICT(session_id) DO UPDATE SET conv_id = ?2, path = ?3, offset = ?4, head_len = ?5, head_hash = ?6,
             next_seq = ?7, state = ?8, last_error = ?9, last_collected_at = ?10, last_checked_at = ?11,
             turn_ended_at = ?12",
        params![
            id,
            s.conv,
            s.path,
            s.offset as i64,
            s.head_len as i64,
            s.head_hash,
            s.next_seq,
            state_to_sql(s.state),
            s.last_error,
            s.last_collected_at.as_ref().map(ts_to_sql),
            s.last_checked_at.as_ref().map(ts_to_sql),
            s.turn_ended_at.as_ref().map(ts_to_sql),
        ],
    )?;
    Ok(())
}

/// Store parsed items (duplicates are ignored) and advance the turn end. Returns the number of
/// new messages.
pub fn store_items(
    c: &Connection,
    s: &SessionRow,
    src: &mut Source,
    items: &[Item],
    now: &Ts,
) -> rusqlite::Result<usize> {
    let mut st = c.prepare_cached(
        "INSERT OR IGNORE INTO messages (id, session_id, project_id, workspace_id, provider_msg_id, seq, role, text,
             at, collected_at, sidechain, tool_name)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
    )?;
    let mut added = 0;
    let collected = ts_to_sql(now);
    for item in items {
        match item {
            Item::Message(Parsed { key, role, text, at, sidechain, tool_name }) => {
                let n = st.execute(params![
                    Uuid::now_v7(),
                    s.id,
                    s.project_id,
                    s.workspace_id,
                    key,
                    src.next_seq,
                    role_to_sql(*role),
                    text,
                    ts_to_sql(at),
                    collected,
                    sidechain,
                    tool_name,
                ])?;
                if n == 1 {
                    src.next_seq += 1;
                    added += 1;
                }
            }
            Item::TurnEnd(at) => {
                if src.turn_ended_at.is_none_or(|t| *at > t) {
                    src.turn_ended_at = Some(*at);
                }
            }
        }
    }
    Ok(added)
}

/// First user message of a session, for an automatic title.
pub fn first_user_text(c: &Connection, id: SessionId) -> rusqlite::Result<Option<String>> {
    c.query_row(
        "SELECT text FROM messages WHERE session_id = ?1 AND role = 'user' AND sidechain = 0 ORDER BY seq LIMIT 1",
        [id],
        |r| r.get(0),
    )
    .optional()
}

/// Title from the first user message: first line, trimmed, at most 40 characters.
pub fn title_from(text: &str) -> Option<String> {
    let line = text.lines().map(str::trim).find(|l| !l.is_empty() && *l != "[이미지]")?;
    let count = line.chars().count();
    Some(if count > 40 { format!("{}…", line.chars().take(39).collect::<String>()) } else { line.to_string() })
}

/// Set an automatic title only while the session still has its default title.
pub fn set_default_title(c: &Connection, s: &SessionRow, title: &str) -> rusqlite::Result<bool> {
    let n = c.execute(
        "UPDATE sessions SET title = ?2 WHERE id = ?1 AND title_user_set = 0 AND title = ?3",
        params![s.id, title, s.tool.label()],
    )?;
    Ok(n == 1)
}

pub struct StatusFilter {
    pub project_id: Option<ProjectId>,
    pub session_id: Option<SessionId>,
}

/// Collection state per session. `running` says which runs are alive.
pub fn statuses(c: &Connection, f: &StatusFilter, running: &[SessionId]) -> rusqlite::Result<Vec<CaptureStatus>> {
    let mut st = c.prepare(
        "SELECT s.id, s.tool, s.provider_conv_id, rs.state, rs.last_error, rs.last_collected_at, rs.last_checked_at,
                (SELECT COUNT(*) FROM messages WHERE session_id = s.id)
         FROM sessions s LEFT JOIN record_sources rs ON rs.session_id = s.id
         WHERE (?1 IS NULL OR s.project_id = ?1) AND (?2 IS NULL OR s.id = ?2)
         ORDER BY s.created_at",
    )?;
    let rows = st.query_map(params![f.project_id, f.session_id], |r| {
        let id: SessionId = r.get(0)?;
        let tool = tool_from_sql(&r.get::<_, String>(1)?);
        let conv: Option<String> = r.get(2)?;
        let stored: Option<String> = r.get(3)?;
        let state = match (tool, &conv, stored) {
            (Tool::Shell, _, _) => CaptureState::NotApplicable,
            (_, None, _) => CaptureState::WaitingForId,
            (_, _, Some(s)) => state_from_sql(&s),
            // Not read yet: the collector picks it up within seconds.
            (_, _, None) => CaptureState::Collecting,
        };
        let state =
            if state == CaptureState::UpToDate && running.contains(&id) { CaptureState::Collecting } else { state };
        Ok(CaptureStatus {
            session_id: id,
            state,
            last_error: r.get(4)?,
            last_collected_at: opt_ts(r.get(5)?),
            last_checked_at: opt_ts(r.get(6)?),
            message_count: r.get::<_, i64>(7)?.max(0) as u64,
        })
    })?;
    rows.collect()
}

fn message_by_id(c: &Connection, id: MessageId) -> rusqlite::Result<Option<Message>> {
    c.query_row(&format!("SELECT {MSG_COLS} FROM messages m WHERE m.id = ?1"), [id], |r| message_from_row(r, 0))
        .optional()
}

fn messages_where(c: &Connection, sql_tail: &str, p: impl rusqlite::Params) -> rusqlite::Result<Vec<Message>> {
    let mut st = c.prepare(&format!("SELECT {MSG_COLS} FROM messages m {sql_tail}"))?;
    let rows = st.query_map(p, |r| message_from_row(r, 0))?;
    rows.collect()
}

/// Upper bound for context reads in either direction.
pub const MAX_CONTEXT: u32 = 500;

/// A message with the messages before and after it in the same session. None when the anchor
/// does not exist.
pub fn context(c: &Connection, q: &ContextQuery) -> rusqlite::Result<Option<Context>> {
    let before = q.before.min(MAX_CONTEXT) as i64;
    let after = q.after.min(MAX_CONTEXT) as i64;
    let (session_id, anchor) = match q.anchor {
        Anchor::Message { id } => match message_by_id(c, id)? {
            Some(m) => (m.session_id, Some(m)),
            None => return Ok(None),
        },
        Anchor::SessionEnd { id } => {
            let last = messages_where(c, "WHERE m.session_id = ?1 ORDER BY m.seq DESC LIMIT 1", [id])?;
            (id, last.into_iter().next())
        }
    };
    let Some(head) = c
        .query_row("SELECT title, tool, project_id, workspace_id FROM sessions WHERE id = ?1", [session_id], |r| {
            Ok((r.get::<_, String>(0)?, tool_from_sql(&r.get::<_, String>(1)?), r.get(2)?, r.get(3)?))
        })
        .optional()?
    else {
        return Ok(None);
    };
    let mut messages = Vec::new();
    let (mut has_before, mut has_after) = (false, false);
    if let Some(a) = &anchor {
        let mut prev = messages_where(
            c,
            "WHERE m.session_id = ?1 AND m.seq < ?2 ORDER BY m.seq DESC LIMIT ?3",
            params![session_id, a.seq, before + 1],
        )?;
        has_before = prev.len() as i64 > before;
        prev.truncate(before as usize);
        prev.reverse();
        let mut next = messages_where(
            c,
            "WHERE m.session_id = ?1 AND m.seq > ?2 ORDER BY m.seq LIMIT ?3",
            params![session_id, a.seq, after + 1],
        )?;
        has_after = next.len() as i64 > after;
        next.truncate(after as usize);
        messages.extend(prev);
        messages.push(a.clone());
        messages.extend(next);
    }
    let pins = pins_on(c, &messages)?;
    Ok(Some(Context {
        session_id,
        session_title: crate::sessions::visible(&head.0),
        tool: head.1,
        project_id: head.2,
        workspace_id: head.3,
        anchor: anchor.map(|m| m.id),
        messages,
        has_before,
        has_after,
        pins,
    }))
}

fn pins_on(c: &Connection, messages: &[Message]) -> rusqlite::Result<Vec<(MessageId, Uuid)>> {
    let mut st = c.prepare_cached("SELECT id FROM pins WHERE message_id = ?1 ORDER BY created_at")?;
    let mut out = Vec::new();
    for m in messages {
        let ids: Vec<Uuid> = st.query_map([m.id], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
        out.extend(ids.into_iter().map(|p| (m.id, p)));
    }
    Ok(out)
}

/// The newest `limit` messages of a session, in transcript order.
pub fn session_tail(c: &Connection, session: SessionId, limit: usize) -> rusqlite::Result<Vec<Message>> {
    let mut v =
        messages_where(c, "WHERE m.session_id = ?1 ORDER BY m.seq DESC LIMIT ?2", params![session, limit as i64])?;
    v.reverse();
    Ok(v)
}

pub fn latest_answer(c: &Connection, session: SessionId) -> rusqlite::Result<Option<Message>> {
    Ok(messages_where(
        c,
        "WHERE m.session_id = ?1 AND m.role = 'assistant' AND m.sidechain = 0 ORDER BY m.seq DESC LIMIT 1",
        [session],
    )?
    .into_iter()
    .next())
}

pub fn turn_info(c: &Connection, session: SessionId) -> rusqlite::Result<TurnInfo> {
    let ended: Option<String> = c
        .query_row("SELECT turn_ended_at FROM record_sources WHERE session_id = ?1", [session], |r| r.get(0))
        .optional()?
        .flatten();
    Ok(TurnInfo { session_id: session, last_answer: latest_answer(c, session)?, turn_ended_at: opt_ts(ended) })
}

/// Messages of a project written after `since`, oldest first, at most `limit`.
pub fn since(c: &Connection, project: ProjectId, since: &Ts, limit: usize) -> rusqlite::Result<Vec<Message>> {
    messages_where(
        c,
        "WHERE m.project_id = ?1 AND m.at > ?2 ORDER BY m.at, m.rid LIMIT ?3",
        params![project, ts_to_sql(since), limit as i64],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_use_the_first_line() {
        assert_eq!(title_from("  \n로그인 오류 고쳐줘\n자세히").as_deref(), Some("로그인 오류 고쳐줘"));
        assert_eq!(title_from("[이미지]\n이 화면").as_deref(), Some("이 화면"));
        let long = "가".repeat(60);
        let t = title_from(&long).unwrap();
        assert_eq!(t.chars().count(), 40);
        assert!(t.ends_with('…'));
        assert_eq!(title_from("   "), None);
    }
}
