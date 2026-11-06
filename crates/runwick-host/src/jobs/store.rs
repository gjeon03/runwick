//! Job rows, per-project automation switches and small cursors ("summarized up to").

use crate::store::{opt_ts, ts_from_sql, ts_to_sql};
use runwick_core::Ts;
use runwick_core::api::jobs::*;
use rusqlite::{Connection, OptionalExtension, Row, params};
use uuid::Uuid;

pub fn kind_sql(k: JobKind) -> &'static str {
    match k {
        JobKind::WikiProposals => "wiki_proposals",
        JobKind::WikiDraft => "wiki_draft",
        JobKind::TimelineSummary => "timeline_summary",
    }
}

fn kind_from(s: &str) -> JobKind {
    match s {
        "wiki_draft" => JobKind::WikiDraft,
        "timeline_summary" => JobKind::TimelineSummary,
        _ => JobKind::WikiProposals,
    }
}

fn status_sql(s: JobStatus) -> &'static str {
    match s {
        JobStatus::Queued => "queued",
        JobStatus::Running => "running",
        JobStatus::Done => "done",
        JobStatus::Failed => "failed",
        JobStatus::Held => "held",
        JobStatus::Stopped => "stopped",
    }
}

fn status_from(s: &str) -> JobStatus {
    match s {
        "queued" => JobStatus::Queued,
        "running" => JobStatus::Running,
        "done" => JobStatus::Done,
        "held" => JobStatus::Held,
        "stopped" => JobStatus::Stopped,
        _ => JobStatus::Failed,
    }
}

const SELECT: &str = "SELECT j.*, p.name AS project_name FROM jobs j LEFT JOIN projects p ON p.id = j.project_id";

fn from_row(r: &Row) -> rusqlite::Result<Job> {
    Ok(Job {
        id: r.get("id")?,
        kind: kind_from(&r.get::<_, String>("kind")?),
        project_id: r.get("project_id")?,
        project_name: r.get("project_name")?,
        trigger: if r.get::<_, String>("origin")? == "auto" { Trigger::Auto } else { Trigger::Requested },
        reason: r.get("reason")?,
        target: serde_json::from_str(&r.get::<_, String>("target")?).unwrap_or_default(),
        status: status_from(&r.get::<_, String>("status")?),
        held_reason: r.get("held_reason")?,
        attempts: r.get::<_, i64>("attempts")? as u32,
        retry_at: opt_ts(r.get("retry_at")?),
        created_at: ts_from_sql(&r.get::<_, String>("created_at")?),
        started_at: opt_ts(r.get("started_at")?),
        finished_at: opt_ts(r.get("finished_at")?),
        result: r.get("result")?,
        error: r.get("error")?,
        provider: r.get("provider")?,
        model: r.get("model")?,
        duration_ms: r.get::<_, Option<i64>>("duration_ms")?.map(|v| v as u64),
    })
}

pub fn get(c: &Connection, id: Uuid) -> rusqlite::Result<Option<Job>> {
    c.query_row(&format!("{SELECT} WHERE j.id = ?1"), [id], from_row).optional()
}

pub fn list(c: &Connection, q: &JobQuery) -> rusqlite::Result<Vec<Job>> {
    let limit = if q.limit == 0 { 50 } else { q.limit.min(500) };
    let mut st = c.prepare(&format!(
        "{SELECT} WHERE (?1 IS NULL OR j.project_id = ?1)
           AND (NOT ?2 OR j.status IN ('queued', 'running', 'held'))
         ORDER BY j.created_at DESC LIMIT ?3"
    ))?;
    let rows = st.query_map(params![q.project_id, q.active_only, limit as i64], from_row)?;
    rows.collect()
}

pub fn with_status(c: &Connection, statuses: &[JobStatus]) -> rusqlite::Result<Vec<Job>> {
    let list: Vec<String> = statuses.iter().map(|s| format!("'{}'", status_sql(*s))).collect();
    let mut st = c.prepare(&format!("{SELECT} WHERE j.status IN ({}) ORDER BY j.created_at", list.join(",")))?;
    let rows = st.query_map([], from_row)?;
    rows.collect()
}

/// The active (queued/running/held) job of a kind for a project, if any.
pub fn active(c: &Connection, project: Uuid, kind: JobKind, day: Option<&str>) -> rusqlite::Result<Option<Job>> {
    let jobs = list(c, &JobQuery { project_id: Some(project), active_only: true, limit: 50 })?;
    Ok(jobs.into_iter().find(|j| j.kind == kind && (day.is_none() || j.target.day.as_deref() == day)))
}

pub struct NewJob<'a> {
    pub kind: JobKind,
    pub project_id: Uuid,
    pub trigger: Trigger,
    pub reason: &'a str,
    pub target: &'a JobTarget,
    pub status: JobStatus,
    pub held_reason: Option<&'a str>,
}

pub fn insert(c: &Connection, n: &NewJob) -> rusqlite::Result<Uuid> {
    let id = Uuid::now_v7();
    c.execute(
        "INSERT INTO jobs (id, kind, project_id, origin, reason, target, status, held_reason, attempts, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 0, ?9)",
        params![
            id,
            kind_sql(n.kind),
            n.project_id,
            if n.trigger == Trigger::Auto { "auto" } else { "requested" },
            n.reason,
            serde_json::to_string(n.target).unwrap_or_else(|_| "{}".into()),
            status_sql(n.status),
            n.held_reason,
            ts_to_sql(&runwick_core::now()),
        ],
    )?;
    Ok(id)
}

pub fn set_waiting(
    c: &Connection,
    id: Uuid,
    status: JobStatus,
    held_reason: Option<&str>,
    reason: Option<&str>,
) -> rusqlite::Result<()> {
    c.execute(
        "UPDATE jobs SET status = ?2, held_reason = ?3, reason = COALESCE(?4, reason) WHERE id = ?1",
        params![id, status_sql(status), held_reason, reason],
    )?;
    Ok(())
}

pub fn mark_started(c: &Connection, id: Uuid, now: &Ts) -> rusqlite::Result<()> {
    c.execute(
        "UPDATE jobs SET status = 'running', held_reason = NULL, retry_at = NULL, started_at = ?2,
             finished_at = NULL, attempts = attempts + 1 WHERE id = ?1",
        params![id, ts_to_sql(now)],
    )?;
    Ok(())
}

pub struct Finish<'a> {
    pub status: JobStatus,
    pub result: Option<&'a str>,
    pub error: Option<&'a str>,
    pub provider: Option<&'a str>,
    pub model: Option<&'a str>,
    pub duration_ms: Option<u64>,
    /// Queue again at this time (automatic retry after a failure).
    pub retry_at: Option<Ts>,
}

pub fn finish(c: &Connection, id: Uuid, f: &Finish, now: &Ts) -> rusqlite::Result<()> {
    let status = if f.retry_at.is_some() { JobStatus::Queued } else { f.status };
    c.execute(
        "UPDATE jobs SET status = ?2, result = ?3, error = ?4, provider = COALESCE(?5, provider),
             model = COALESCE(?6, model), duration_ms = COALESCE(?7, duration_ms), retry_at = ?8, finished_at = ?9,
             held_reason = NULL
         WHERE id = ?1",
        params![
            id,
            status_sql(status),
            f.result,
            f.error,
            f.provider,
            f.model,
            f.duration_ms.map(|d| d as i64),
            f.retry_at.as_ref().map(ts_to_sql),
            ts_to_sql(now),
        ],
    )?;
    Ok(())
}

/// Requeue a finished job as a request (manual retry); resets attempts.
pub fn requeue(c: &Connection, id: Uuid) -> rusqlite::Result<()> {
    c.execute(
        "UPDATE jobs SET status = 'queued', origin = 'requested', attempts = 0, retry_at = NULL, error = NULL,
             held_reason = NULL, result = NULL, finished_at = NULL, started_at = NULL WHERE id = ?1",
        [id],
    )?;
    Ok(())
}

/// After a host restart nothing continues by itself: unfinished jobs are marked stopped.
pub fn stop_unfinished(c: &Connection, now: &Ts) -> rusqlite::Result<usize> {
    c.execute(
        "UPDATE jobs SET status = 'stopped', held_reason = NULL, retry_at = NULL, finished_at = ?1,
             error = COALESCE(error, '호스트가 다시 시작되어 멈췄어요. 다시 실행할 수 있어요')
         WHERE status IN ('queued', 'running', 'held')",
        [ts_to_sql(now)],
    )
}

/// Last finished job of a kind (done or failed) for a project.
pub fn last_finished(c: &Connection, project: Uuid, kind: JobKind) -> rusqlite::Result<Option<Ts>> {
    c.query_row(
        "SELECT MAX(COALESCE(finished_at, created_at)) FROM jobs
         WHERE project_id = ?1 AND kind = ?2 AND status IN ('done', 'failed')",
        params![project, kind_sql(kind)],
        |r| r.get::<_, Option<String>>(0),
    )
    .map(opt_ts)
}

// ---------------------------------------------------------------------------------------------
// Automation switches

pub struct Switches {
    pub wiki: bool,
    pub timeline: bool,
    /// When the timeline switch was turned on (days before it are not summarized automatically).
    pub timeline_since: Option<Ts>,
}

pub fn switches(c: &Connection, project: Uuid) -> rusqlite::Result<Switches> {
    Ok(c.query_row("SELECT wiki, timeline, timeline_since FROM automation WHERE project_id = ?1", [project], |r| {
        Ok(Switches { wiki: r.get(0)?, timeline: r.get(1)?, timeline_since: opt_ts(r.get(2)?) })
    })
    .optional()?
    .unwrap_or(Switches { wiki: false, timeline: false, timeline_since: None }))
}

pub fn set_switches(
    c: &Connection,
    project: Uuid,
    wiki: Option<bool>,
    timeline: Option<bool>,
    now: &Ts,
) -> rusqlite::Result<()> {
    let cur = switches(c, project)?;
    let timeline_on = timeline.unwrap_or(cur.timeline);
    let since = match (cur.timeline, timeline_on) {
        (false, true) => Some(*now),
        (_, true) => cur.timeline_since,
        (_, false) => None,
    };
    c.execute(
        "INSERT INTO automation (project_id, wiki, timeline, timeline_since, updated_at) VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(project_id) DO UPDATE SET wiki = excluded.wiki, timeline = excluded.timeline,
             timeline_since = excluded.timeline_since, updated_at = excluded.updated_at",
        params![project, wiki.unwrap_or(cur.wiki), timeline_on, since.as_ref().map(ts_to_sql), ts_to_sql(now)],
    )?;
    Ok(())
}

/// Projects with at least one switch on.
pub fn opted_in(c: &Connection) -> rusqlite::Result<Vec<Uuid>> {
    let mut st = c.prepare(
        "SELECT a.project_id FROM automation a JOIN projects p ON p.id = a.project_id
         WHERE (a.wiki = 1 OR a.timeline = 1) AND p.archived = 0",
    )?;
    let rows = st.query_map([], |r| r.get(0))?;
    rows.collect()
}

// ---------------------------------------------------------------------------------------------
// Cursors

pub fn cursor(c: &Connection, project: Uuid, kind: JobKind, key: &str) -> rusqlite::Result<Option<String>> {
    c.query_row(
        "SELECT value FROM job_cursors WHERE project_id = ?1 AND kind = ?2 AND key = ?3",
        params![project, kind_sql(kind), key],
        |r| r.get(0),
    )
    .optional()
}

pub fn set_cursor(c: &Connection, project: Uuid, kind: JobKind, key: &str, value: &str) -> rusqlite::Result<()> {
    c.execute(
        "INSERT INTO job_cursors (project_id, kind, key, value, updated_at) VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(project_id, kind, key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        params![project, kind_sql(kind), key, value, ts_to_sql(&runwick_core::now())],
    )?;
    Ok(())
}

/// Newest activity of any session in the project (touched while output flows).
pub fn last_activity(c: &Connection, project: Uuid) -> rusqlite::Result<Option<Ts>> {
    c.query_row("SELECT MAX(last_active_at) FROM sessions WHERE project_id = ?1", [project], |r| {
        r.get::<_, Option<String>>(0)
    })
    .map(opt_ts)
}
