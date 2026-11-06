//! Timeline (no AI): per project and day, what was observed — sessions started and ended,
//! conversation activity, pins, git commits — plus events other features record
//! ([`record_event`]: wiki, collaboration, file changes) and stored AI day summaries
//! ([`store_summary`], always shown as AI with time and basis). Also the resume card's memo and
//! remaining items.
//!
//! Days are cut in the viewer's time zone (`utc_offset_min`). Git history is read with the
//! person's `git` and cached for a few minutes per project; a missing project folder only
//! leaves commits out.

use crate::host::{Ctx, Host};
use crate::router::Router;
use crate::store::{Migration, Store, db_err, opt_ts, ts_from_sql, ts_to_sql};
use chrono::{Duration as ChronoDuration, NaiveDate, TimeZone, Utc};
use runwick_core::Ts;
use runwick_core::api::projects::{ById, Project, ProjectId};
use runwick_core::api::sessions::{Session, Tool};
use runwick_core::api::timeline::*;
use runwick_core::rpc::{RpcError, topic};
use rusqlite::{Connection, OptionalExtension, Row, params};
use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use uuid::Uuid;

pub const MIGRATIONS: &[Migration] = &[(
    "timeline_1",
    "CREATE TABLE timeline_events (
        id BLOB PRIMARY KEY,
        project_id BLOB NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
        kind TEXT NOT NULL,
        title TEXT NOT NULL,
        at TEXT NOT NULL,
        session_id BLOB,
        link TEXT,
        dedupe_key TEXT UNIQUE,
        created_at TEXT NOT NULL
    );
    CREATE INDEX timeline_events_project_at ON timeline_events(project_id, at);
    CREATE INDEX timeline_events_at ON timeline_events(at);
    CREATE TABLE timeline_summaries (
        project_id BLOB NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
        day TEXT NOT NULL,
        text TEXT NOT NULL,
        generated_at TEXT NOT NULL,
        basis_count INTEGER NOT NULL,
        provider TEXT,
        PRIMARY KEY (project_id, day)
    );
    CREATE TABLE project_notes (
        project_id BLOB PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
        memo TEXT NOT NULL DEFAULT '',
        items TEXT NOT NULL DEFAULT '[]',
        updated_at TEXT NOT NULL
    );",
)];

/// Longest range one request may cover.
pub const MAX_RANGE_DAYS: i64 = 62;
const GIT_TTL: Duration = Duration::from_secs(5 * 60);
const MAX_COMMITS: usize = 2000;
const MAX_MEMO: usize = 20_000;
const MAX_ITEMS: usize = 100;
const MAX_ITEM: usize = 500;

pub fn register(r: &mut Router) {
    r.add::<TimelineRange, _, _>(|ctx: Ctx, q: RangeQuery| async move { range(&ctx.host, &q).await });
    r.add::<TimelineResume, _, _>(|ctx: Ctx, p: ById| async move { resume(&ctx.host, p.id).await });
    r.add::<TimelineSaveNote, _, _>(|ctx: Ctx, n: SaveNote| async move {
        let h = ctx.host.clone();
        let note = tokio::task::spawn_blocking(move || save_note(&h.store, &n)).await.map_err(RpcError::internal)??;
        ctx.host.bus.changed(&[topic::TIMELINE]);
        Ok(note)
    });
}

fn kind_to_sql(k: EventKind) -> &'static str {
    match k {
        EventKind::Session => "session",
        EventKind::Messages => "messages",
        EventKind::Pin => "pin",
        EventKind::Commit => "commit",
        EventKind::File => "file",
        EventKind::Wiki => "wiki",
        EventKind::Collab => "collab",
    }
}

fn kind_from_sql(s: &str) -> EventKind {
    EventKind::ALL.into_iter().find(|k| kind_to_sql(*k) == s).unwrap_or(EventKind::File)
}

/// Record an event observed by another feature. With a `dedupe_key`, recording again replaces
/// the earlier row. Emit `topic::TIMELINE` afterwards. Blocking.
pub fn record_event(store: &Store, ev: &NewEvent) -> Result<(), RpcError> {
    let title = ev.title.trim();
    if title.is_empty() {
        return Err(RpcError::invalid("타임라인 기록의 제목이 비어 있어요"));
    }
    let title: String = title.chars().take(300).collect();
    let link = ev.link.as_ref().map(|l| serde_json::to_string(l).unwrap_or_default());
    store
        .with(|c| {
            c.execute(
                "INSERT INTO timeline_events (id, project_id, kind, title, at, session_id, link, dedupe_key, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                 ON CONFLICT(dedupe_key) DO UPDATE SET kind = excluded.kind, title = excluded.title, at = excluded.at,
                     session_id = excluded.session_id, link = excluded.link",
                params![
                    Uuid::now_v7(),
                    ev.project_id,
                    kind_to_sql(ev.kind),
                    title,
                    ts_to_sql(&ev.at),
                    ev.session_id,
                    link,
                    ev.dedupe_key,
                    ts_to_sql(&runwick_core::now()),
                ],
            )
        })
        .map_err(db_err)?;
    Ok(())
}

/// Store (or replace) the AI summary of a project day. Emit `topic::TIMELINE` afterwards.
/// Blocking.
pub fn store_summary(store: &Store, s: &Summary) -> Result<(), RpcError> {
    if s.text.trim().is_empty() {
        return Err(RpcError::invalid("요약 내용이 비어 있어요"));
    }
    store
        .with(|c| {
            c.execute(
                "INSERT INTO timeline_summaries (project_id, day, text, generated_at, basis_count, provider)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(project_id, day) DO UPDATE SET text = excluded.text, generated_at = excluded.generated_at,
                     basis_count = excluded.basis_count, provider = excluded.provider",
                params![
                    s.project_id,
                    s.day.to_string(),
                    s.text.trim(),
                    ts_to_sql(&s.generated_at),
                    s.basis_count,
                    s.provider
                ],
            )
        })
        .map_err(db_err)?;
    Ok(())
}

/// The observed records of one project day without git commits (what a day summary is based
/// on). Blocking.
pub fn observed_day(
    store: &Store,
    project_id: ProjectId,
    day: NaiveDate,
    utc_offset_min: i32,
) -> Result<ProjectDay, RpcError> {
    let (from, to) = (day_start(day, utc_offset_min), day_start(day + ChronoDuration::days(1), utc_offset_min));
    let events = store.with(|c| observed(c, &from, &to, Some(project_id), utc_offset_min)).map_err(db_err)?;
    let summaries = store.with(|c| summaries(c, day, day, Some(project_id))).map_err(db_err)?;
    let mut days = group(events, summaries, utc_offset_min);
    Ok(days.pop().unwrap_or(ProjectDay { project_id, day, count: 0, summary: None, events: Vec::new() }))
}

fn day_start(day: NaiveDate, off: i32) -> Ts {
    let local = day.and_hms_opt(0, 0, 0).unwrap_or_default();
    Utc.from_utc_datetime(&(local - ChronoDuration::minutes(off as i64)))
}

fn local_day(ts: &Ts, off: i32) -> NaiveDate {
    (ts.naive_utc() + ChronoDuration::minutes(off as i64)).date()
}

fn event(key: String, kind: EventKind, title: String, at: Ts, project_id: ProjectId) -> Event {
    Event { key, kind, title, at, project_id, session_id: None, link: None, count: 1 }
}

/// Every observed event in `[from, to)` from the records (not git).
fn observed(c: &Connection, from: &Ts, to: &Ts, project: Option<ProjectId>, off: i32) -> rusqlite::Result<Vec<Event>> {
    let (f, t) = (ts_to_sql(from), ts_to_sql(to));
    let mut out = Vec::new();
    for (col, verb, suffix) in [("created_at", "시작", "start"), ("ended_at", "종료", "end")] {
        let mut st = c.prepare(&format!(
            "SELECT id, project_id, title, {col} FROM sessions
             WHERE {col} >= ?1 AND {col} < ?2 AND (?3 IS NULL OR project_id = ?3)"
        ))?;
        let rows = st.query_map(params![f, t, project], |r| {
            let id: Uuid = r.get(0)?;
            let title = crate::sessions::visible(&r.get::<_, String>(2)?);
            let mut e = event(
                format!("s:{id}:{suffix}"),
                EventKind::Session,
                format!("{title} {verb}"),
                ts_from_sql(&r.get::<_, String>(3)?),
                r.get(1)?,
            );
            e.session_id = Some(id);
            e.link = Some(EventLink::Session { id });
            Ok(e)
        })?;
        out.extend(rows.collect::<rusqlite::Result<Vec<_>>>()?);
    }
    let shift = format!("{off:+} minutes");
    let mut st = c.prepare(
        "SELECT m.session_id, m.project_id, s.title, date(m.at, ?4) AS d, COUNT(*), MAX(m.at)
         FROM messages m JOIN sessions s ON s.id = m.session_id
         WHERE m.at >= ?1 AND m.at < ?2 AND (?3 IS NULL OR m.project_id = ?3)
         GROUP BY m.session_id, d",
    )?;
    let rows = st.query_map(params![f, t, project, shift], |r| {
        let id: Uuid = r.get(0)?;
        let title = crate::sessions::visible(&r.get::<_, String>(2)?);
        let day: String = r.get(3)?;
        let n: i64 = r.get(4)?;
        let mut e = event(
            format!("m:{id}:{day}"),
            EventKind::Messages,
            format!("{title} · 메시지 {n}개"),
            ts_from_sql(&r.get::<_, String>(5)?),
            r.get(1)?,
        );
        e.session_id = Some(id);
        e.link = Some(EventLink::Session { id });
        e.count = n.max(0) as u32;
        Ok(e)
    })?;
    out.extend(rows.collect::<rusqlite::Result<Vec<_>>>()?);
    let mut st = c.prepare(
        "SELECT id, project_id, title, created_at, session_id FROM pins
         WHERE project_id IS NOT NULL AND created_at >= ?1 AND created_at < ?2 AND (?3 IS NULL OR project_id = ?3)",
    )?;
    let rows = st.query_map(params![f, t, project], |r| {
        let id: Uuid = r.get(0)?;
        let title: String = r.get(2)?;
        let mut e = event(
            format!("p:{id}"),
            EventKind::Pin,
            format!("“{title}” 핀 고정"),
            ts_from_sql(&r.get::<_, String>(3)?),
            r.get(1)?,
        );
        e.session_id = r.get(4)?;
        e.link = Some(EventLink::Pin { id });
        Ok(e)
    })?;
    out.extend(rows.collect::<rusqlite::Result<Vec<_>>>()?);
    let mut st = c.prepare(
        "SELECT id, project_id, kind, title, at, session_id, link FROM timeline_events
         WHERE at >= ?1 AND at < ?2 AND (?3 IS NULL OR project_id = ?3)",
    )?;
    let rows = st.query_map(params![f, t, project], |r| {
        let id: Uuid = r.get(0)?;
        let mut e = event(
            format!("e:{id}"),
            kind_from_sql(&r.get::<_, String>(2)?),
            r.get(3)?,
            ts_from_sql(&r.get::<_, String>(4)?),
            r.get(1)?,
        );
        e.session_id = r.get(5)?;
        e.link = r.get::<_, Option<String>>(6)?.and_then(|l| serde_json::from_str(&l).ok());
        Ok(e)
    })?;
    out.extend(rows.collect::<rusqlite::Result<Vec<_>>>()?);
    Ok(out)
}

fn summary_from_row(r: &Row) -> rusqlite::Result<Option<Summary>> {
    let day: String = r.get(1)?;
    let Ok(day) = NaiveDate::parse_from_str(&day, "%Y-%m-%d") else {
        return Ok(None);
    };
    Ok(Some(Summary {
        project_id: r.get(0)?,
        day,
        text: r.get(2)?,
        generated_at: ts_from_sql(&r.get::<_, String>(3)?),
        basis_count: r.get::<_, i64>(4)?.max(0) as u32,
        provider: r.get(5)?,
    }))
}

fn summaries(
    c: &Connection,
    from: NaiveDate,
    to: NaiveDate,
    project: Option<ProjectId>,
) -> rusqlite::Result<Vec<Summary>> {
    let mut st = c.prepare(
        "SELECT project_id, day, text, generated_at, basis_count, provider FROM timeline_summaries
         WHERE day >= ?1 AND day <= ?2 AND (?3 IS NULL OR project_id = ?3)",
    )?;
    let rows = st.query_map(params![from.to_string(), to.to_string(), project], summary_from_row)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?.into_iter().flatten().collect())
}

/// Events and summaries grouped by project and day, newest event first.
fn group(events: Vec<Event>, summaries: Vec<Summary>, off: i32) -> Vec<ProjectDay> {
    let mut days: BTreeMap<(NaiveDate, ProjectId), ProjectDay> = BTreeMap::new();
    for e in events {
        let day = match e.kind {
            // Conversation rows are already grouped by the viewer's day.
            EventKind::Messages => e.key.rsplit(':').next().and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok()),
            _ => None,
        }
        .unwrap_or_else(|| local_day(&e.at, off));
        let d = days.entry((day, e.project_id)).or_insert_with(|| ProjectDay {
            project_id: e.project_id,
            day,
            count: 0,
            summary: None,
            events: Vec::new(),
        });
        d.count += e.count;
        d.events.push(e);
    }
    for s in summaries {
        let d = days.entry((s.day, s.project_id)).or_insert_with(|| ProjectDay {
            project_id: s.project_id,
            day: s.day,
            count: 0,
            summary: None,
            events: Vec::new(),
        });
        d.summary = Some(s);
    }
    let mut out: Vec<ProjectDay> = days.into_values().collect();
    for d in &mut out {
        d.events.sort_by(|a, b| b.at.cmp(&a.at).then_with(|| a.key.cmp(&b.key)));
    }
    out
}

#[derive(Debug, Clone)]
struct Commit {
    sha: String,
    at: Ts,
    subject: String,
}

struct GitEntry {
    fetched: Instant,
    root: String,
    since: Ts,
    commits: Arc<Vec<Commit>>,
}

fn git_cache() -> &'static Mutex<HashMap<ProjectId, GitEntry>> {
    static CACHE: OnceLock<Mutex<HashMap<ProjectId, GitEntry>>> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// Commits on local branches since `since` (cached). Err is a note for the person.
async fn commits(p: &Project, since: Ts) -> Result<Arc<Vec<Commit>>, String> {
    if !p.root_exists {
        return Err("폴더를 찾을 수 없어 커밋은 확인하지 못했어요".into());
    }
    if !p.is_git {
        return Ok(Arc::default());
    }
    if let Ok(cache) = git_cache().lock()
        && let Some(e) = cache.get(&p.id)
        && e.root == p.root
        && e.since <= since
        && e.fetched.elapsed() < GIT_TTL
    {
        return Ok(e.commits.clone());
    }
    let since_arg = format!("--since={}", since.format("%Y-%m-%d %H:%M:%S +0000"));
    let limit = format!("-n{MAX_COMMITS}");
    let out =
        crate::git::git(Path::new(&p.root), &["log", "--branches", &since_arg, &limit, "--format=%H%x1f%cI%x1f%s"])
            .await;
    let text = match out {
        Ok(t) => t,
        Err(e) if e.contains("does not have any commits") => String::new(),
        Err(e) => {
            return Err(format!("커밋 기록을 읽지 못했어요: {}", e.lines().next().unwrap_or("")));
        }
    };
    let commits: Vec<Commit> = text
        .lines()
        .filter_map(|l| {
            let mut it = l.split('\u{1f}');
            let sha = it.next()?.to_string();
            let at = chrono::DateTime::parse_from_rfc3339(it.next()?).ok()?.with_timezone(&Utc);
            Some(Commit { sha, at, subject: it.next().unwrap_or("").to_string() })
        })
        .collect();
    let commits = Arc::new(commits);
    if let Ok(mut cache) = git_cache().lock() {
        cache.insert(p.id, GitEntry { fetched: Instant::now(), root: p.root.clone(), since, commits: commits.clone() });
    }
    Ok(commits)
}

async fn range(host: &Arc<Host>, q: &RangeQuery) -> Result<Range, RpcError> {
    if q.to < q.from || (q.to - q.from).num_days() > MAX_RANGE_DAYS {
        return Err(RpcError::invalid(format!("기간은 {MAX_RANGE_DAYS}일 이내로 골라 주세요")));
    }
    if q.utc_offset_min.abs() > 18 * 60 {
        return Err(RpcError::invalid("시간대가 올바르지 않아요"));
    }
    let off = q.utc_offset_min;
    let (from, to) = (day_start(q.from, off), day_start(q.to + ChronoDuration::days(1), off));
    let (h, qq) = (host.clone(), q.clone());
    let (mut events, sums, projects) = tokio::task::spawn_blocking(move || -> Result<_, RpcError> {
        let ev = h.store.with(|c| observed(c, &from, &to, qq.project_id, off)).map_err(db_err)?;
        let sums = h.store.with(|c| summaries(c, qq.from, qq.to, qq.project_id)).map_err(db_err)?;
        let projects = crate::projects::list_projects(&h.store).map_err(db_err)?;
        Ok((ev, sums, projects))
    })
    .await
    .map_err(RpcError::internal)??;
    let projects: Vec<Project> = projects.into_iter().filter(|p| q.project_id.is_none_or(|id| id == p.id)).collect();
    if let Some(id) = q.project_id
        && projects.is_empty()
    {
        return Err(RpcError::not_found(format!("프로젝트를 찾을 수 없어요 ({id})")));
    }
    let mut notes: HashMap<ProjectId, String> = HashMap::new();
    for p in projects.iter().filter(|p| !p.archived || q.project_id.is_some()) {
        match commits(p, from).await {
            Ok(list) => {
                events.extend(list.iter().filter(|c| c.at >= from && c.at < to).map(|c| {
                    let subject: String = c.subject.chars().take(120).collect();
                    let mut e =
                        event(format!("c:{}", c.sha), EventKind::Commit, format!("커밋: {subject}"), c.at, p.id);
                    e.link = Some(EventLink::Commit { sha: c.sha.clone() });
                    e
                }));
            }
            Err(note) => {
                notes.insert(p.id, note);
            }
        }
    }
    let days = group(events, sums, off);
    let rows = projects
        .iter()
        .filter(|p| q.project_id == Some(p.id) || days.iter().any(|d| d.project_id == p.id))
        .map(|p| ProjectRow {
            project_id: p.id,
            name: p.name.clone(),
            root_exists: p.root_exists,
            commits_note: notes.get(&p.id).cloned(),
        })
        .collect();
    Ok(Range { from: q.from, to: q.to, projects: rows, days })
}

fn tool_from_sql(s: &str) -> Tool {
    match s {
        "claude" => Tool::Claude,
        "codex" => Tool::Codex,
        _ => Tool::Shell,
    }
}

fn session_from_row(r: &Row) -> rusqlite::Result<(Session, bool)> {
    Ok((
        Session {
            id: r.get("id")?,
            project_id: r.get("project_id")?,
            workspace_id: r.get("workspace_id")?,
            title: r.get("title")?,
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
            env_version: r.get::<_, i64>("env_version")?.max(0) as u64,
            interrupted: false,
            origin_device: r.get("origin_device").unwrap_or(None),
        },
        r.get("was_running")?,
    ))
}

/// The project's most recent session, agent sessions before shells.
fn last_session(c: &Connection, project: ProjectId) -> rusqlite::Result<Option<(Session, bool)>> {
    c.query_row(
        "SELECT * FROM sessions WHERE project_id = ?1 AND archived = 0
         ORDER BY (tool = 'shell'), COALESCE(last_active_at, created_at) DESC LIMIT 1",
        [project],
        session_from_row,
    )
    .optional()
}

fn load_note(c: &Connection, project: ProjectId) -> rusqlite::Result<Note> {
    Ok(c.query_row("SELECT memo, items, updated_at FROM project_notes WHERE project_id = ?1", [project], |r| {
        Ok(Note {
            memo: r.get(0)?,
            items: serde_json::from_str(&r.get::<_, String>(1)?).unwrap_or_default(),
            updated_at: opt_ts(r.get(2)?),
        })
    })
    .optional()?
    .unwrap_or_default())
}

fn latest_summary(c: &Connection, project: ProjectId) -> rusqlite::Result<Option<Summary>> {
    Ok(c.query_row(
        "SELECT project_id, day, text, generated_at, basis_count, provider FROM timeline_summaries
             WHERE project_id = ?1 ORDER BY day DESC LIMIT 1",
        [project],
        summary_from_row,
    )
    .optional()?
    .flatten())
}

async fn resume(host: &Arc<Host>, project_id: ProjectId) -> Result<Resume, RpcError> {
    let running = host.sessions.running_ids();
    let h = host.clone();
    tokio::task::spawn_blocking(move || {
        let project = crate::projects::get_project(&h.store, project_id)?;
        let s = &h.store;
        let last = match s.with(|c| last_session(c, project_id)).map_err(db_err)? {
            Some((mut session, was_running)) => {
                let alive = running.contains(&session.id);
                session.interrupted = was_running && !alive;
                let ws = crate::projects::get_workspace(s, session.workspace_id).ok();
                let last_at = session.last_active_at.unwrap_or(session.created_at);
                Some(LastWork {
                    workspace_name: ws.as_ref().map(|w| w.name.clone()).unwrap_or_default(),
                    workspace_exists: ws.as_ref().is_some_and(|w| w.path_exists),
                    running: alive,
                    last_at,
                    session,
                })
            }
            None => None,
        };
        Ok(Resume {
            project_id,
            project_name: project.name.clone(),
            root_exists: project.root_exists,
            last,
            note: s.with(|c| load_note(c, project_id)).map_err(db_err)?,
            summary: s.with(|c| latest_summary(c, project_id)).map_err(db_err)?,
            pins: crate::records::project_pins(s, project_id, 30)?,
        })
    })
    .await
    .map_err(RpcError::internal)?
}

fn save_note(store: &Store, n: &SaveNote) -> Result<Note, RpcError> {
    crate::projects::get_project(store, n.project_id)?;
    if n.memo.chars().count() > MAX_MEMO {
        return Err(RpcError::invalid(format!("메모는 {MAX_MEMO}자까지 저장할 수 있어요")));
    }
    if n.items.len() > MAX_ITEMS {
        return Err(RpcError::invalid(format!("남은 일은 {MAX_ITEMS}개까지 적을 수 있어요")));
    }
    let items: Vec<NoteItem> = n
        .items
        .iter()
        .map(|i| NoteItem { text: i.text.trim().chars().take(MAX_ITEM).collect(), done: i.done })
        .filter(|i| !i.text.is_empty())
        .collect();
    let now = runwick_core::now();
    let json = serde_json::to_string(&items).map_err(RpcError::internal)?;
    store
        .with(|c| {
            c.execute(
                "INSERT INTO project_notes (project_id, memo, items, updated_at) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(project_id) DO UPDATE SET memo = excluded.memo, items = excluded.items,
                     updated_at = excluded.updated_at",
                params![n.project_id, n.memo, json, ts_to_sql(&now)],
            )
        })
        .map_err(db_err)?;
    Ok(Note { memo: n.memo.clone(), items, updated_at: Some(now) })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store_with_project() -> (Store, ProjectId, Uuid) {
        let store = Store::open_in_memory().unwrap();
        let (p, w, s) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
        store
            .with(|c| {
                c.execute(
                    "INSERT INTO projects (id, name, root, created_at) VALUES (?1, 'payments-api', '/nope', '2026-11-01T00:00:00.000Z')",
                    [p],
                )?;
                c.execute(
                    "INSERT INTO workspaces (id, project_id, name, path, kind, created_at)
                     VALUES (?1, ?2, '기본 공간', '/nope', 'default', '2026-11-01T00:00:00.000Z')",
                    params![w, p],
                )?;
                c.execute(
                    "INSERT INTO sessions (id, project_id, workspace_id, title, tool, cwd, created_at, ended_at)
                     VALUES (?1, ?2, ?3, '로그인 오류 수정', 'claude', '/nope', '2026-11-05T02:01:00.000Z', '2026-11-05T09:30:00.000Z')",
                    params![s, p, w],
                )?;
                for (i, at) in ["2026-11-05T02:01:00.000Z", "2026-11-05T14:59:00.000Z", "2026-11-05T15:01:00.000Z"].iter().enumerate() {
                    c.execute(
                        "INSERT INTO messages (id, session_id, project_id, workspace_id, provider_msg_id, seq, role, text, at, collected_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'user', '질문', ?7, ?7)",
                        params![Uuid::now_v7(), s, p, w, format!("k{i}"), i as i64, at],
                    )?;
                }
                Ok(())
            })
            .unwrap();
        (store, p, s)
    }

    #[test]
    fn days_follow_the_viewer_time_zone() {
        let (store, p, _) = store_with_project();
        let kst = 9 * 60;
        let d5 = observed_day(&store, p, NaiveDate::from_ymd_opt(2026, 11, 5).unwrap(), kst).unwrap();
        // 02:01Z start + message (11:01 KST), 09:30Z end (18:30 KST); 14:59Z is 23:59 KST.
        assert_eq!(d5.count, 1 + 1 + 2);
        assert!(d5.events.iter().any(|e| e.kind == EventKind::Messages && e.title.ends_with("메시지 2개")));
        let d6 = observed_day(&store, p, NaiveDate::from_ymd_opt(2026, 11, 6).unwrap(), kst).unwrap();
        assert_eq!(d6.count, 1, "15:01Z is the next day in Korea");
        let utc = observed_day(&store, p, NaiveDate::from_ymd_opt(2026, 11, 5).unwrap(), 0).unwrap();
        assert_eq!(utc.count, 1 + 1 + 3);
    }

    #[test]
    fn recorded_events_dedupe_and_summaries_attach() {
        let (store, p, _) = store_with_project();
        let day = NaiveDate::from_ymd_opt(2026, 11, 5).unwrap();
        let at = day_start(day, 0) + ChronoDuration::hours(12);
        let ev = NewEvent {
            project_id: p,
            kind: EventKind::Wiki,
            title: "위키 “로그인과 세션” 제안 도착".into(),
            at,
            session_id: None,
            link: Some(EventLink::Wiki { page: "login.md".into() }),
            dedupe_key: Some("wiki:login:1".into()),
        };
        record_event(&store, &ev).unwrap();
        record_event(&store, &NewEvent { title: "위키 “로그인과 세션” 제안 적용".into(), ..ev.clone() }).unwrap();
        record_event(&store, &NewEvent { dedupe_key: None, kind: EventKind::Collab, title: "결과 도착".into(), ..ev })
            .unwrap();
        assert!(
            record_event(
                &store,
                &NewEvent {
                    project_id: p,
                    kind: EventKind::File,
                    title: "  ".into(),
                    at,
                    session_id: None,
                    link: None,
                    dedupe_key: None
                }
            )
            .is_err()
        );
        store_summary(
            &store,
            &Summary {
                project_id: p,
                day,
                text: "만료 오류를 고쳤습니다.".into(),
                generated_at: at,
                basis_count: 7,
                provider: None,
            },
        )
        .unwrap();
        let d = observed_day(&store, p, day, 0).unwrap();
        let wiki: Vec<&Event> = d.events.iter().filter(|e| e.kind == EventKind::Wiki).collect();
        assert_eq!(wiki.len(), 1);
        assert!(wiki[0].title.ends_with("적용"));
        assert_eq!(wiki[0].link, Some(EventLink::Wiki { page: "login.md".into() }));
        assert_eq!(d.events.iter().filter(|e| e.kind == EventKind::Collab).count(), 1);
        assert_eq!(d.summary.as_ref().map(|s| s.basis_count), Some(7));
        // A summary-only day still appears (with zero observed records).
        let empty_day = NaiveDate::from_ymd_opt(2026, 11, 3).unwrap();
        store_summary(
            &store,
            &Summary {
                project_id: p,
                day: empty_day,
                text: "x".into(),
                generated_at: at,
                basis_count: 0,
                provider: None,
            },
        )
        .unwrap();
        let d3 = observed_day(&store, p, empty_day, 0).unwrap();
        assert_eq!((d3.count, d3.summary.is_some()), (0, true));
    }

    #[test]
    fn notes_are_saved_trimmed_and_resume_reads_them() {
        let (store, p, s) = store_with_project();
        let saved = save_note(
            &store,
            &SaveNote {
                project_id: p,
                memo: "스테이징에서 재현 확인".into(),
                items: vec![
                    NoteItem { text: " auth-tests 통합 ".into(), done: false },
                    NoteItem { text: " ".into(), done: true },
                ],
            },
        )
        .unwrap();
        assert_eq!(saved.items, vec![NoteItem { text: "auth-tests 통합".into(), done: false }]);
        let note = store.with(|c| load_note(c, p)).unwrap();
        assert_eq!(note.memo, "스테이징에서 재현 확인");
        let (last, _) = store.with(|c| last_session(c, p)).unwrap().unwrap();
        assert_eq!(last.id, s);
        assert!(save_note(&store, &SaveNote { project_id: Uuid::now_v7(), ..Default::default() }).is_err());
    }
}
