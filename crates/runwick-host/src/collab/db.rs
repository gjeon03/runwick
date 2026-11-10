//! Delegation records (tables from migration `collab_1`): requests, per-parent holds and
//! workers a person is talking to directly.

use crate::store::{Store, db_err, opt_ts, ts_from_sql, ts_to_sql};
use runwick_core::api::collab::{Note, ReqStatus, Request, RequestId, RequestQuery, Source, Via, WorkResult};
use runwick_core::api::sessions::{SessionId, Tool};
use runwick_core::rpc::RpcError;
use rusqlite::{OptionalExtension, Row, params};
use uuid::Uuid;

pub const ACTIVITY_MIGRATION: &str = "CREATE INDEX collab_requests_recent ON collab_requests(created_at DESC, id DESC);
    CREATE INDEX collab_requests_unread_person ON collab_requests(created_at DESC, id DESC)
    WHERE parent_id IS NULL AND acknowledged = 0 AND status IN ('done', 'failed');";

pub const TREE_MIGRATION: &str = "ALTER TABLE collab_requests ADD COLUMN start_tree TEXT;";

/// Persist revision and working-tree state together before the task can reach the worker.
pub fn save_start(store: &Store, r: &Request, tree: Option<&super::changes::TreeState>) -> Result<(), RpcError> {
    let tree = tree.map(serde_json::to_string).transpose().map_err(RpcError::internal)?;
    store
        .with(|c| {
            c.execute(
                "UPDATE collab_requests SET start_rev = ?2, start_tree = ?3 WHERE id = ?1",
                params![r.id, r.start_rev, tree],
            )
        })
        .map_err(db_err)?;
    Ok(())
}

/// Legacy requests have no baseline; do not guess that their worktree was clean.
pub fn start_tree(store: &Store, id: RequestId) -> Result<Option<super::changes::TreeState>, RpcError> {
    let json: Option<String> = store
        .with(|c| c.query_row("SELECT start_tree FROM collab_requests WHERE id = ?1", [id], |r| r.get(0)))
        .map_err(db_err)?;
    json.map(|j| serde_json::from_str(&j)).transpose().map_err(RpcError::internal)
}

pub const MIGRATION: &str = "CREATE TABLE collab_requests (
        id BLOB PRIMARY KEY,
        parent_id BLOB,
        worker_id BLOB NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
        previous_id BLOB,
        attempt INTEGER NOT NULL DEFAULT 1,
        title TEXT NOT NULL,
        role TEXT,
        tool TEXT NOT NULL,
        prompt TEXT NOT NULL,
        source TEXT NOT NULL,
        via TEXT NOT NULL,
        interrupt INTEGER NOT NULL DEFAULT 0,
        idem_key TEXT,
        status TEXT NOT NULL,
        reason TEXT,
        start_rev TEXT,
        created_at TEXT NOT NULL,
        delivered_at TEXT,
        finished_at TEXT,
        updated_at TEXT NOT NULL,
        result TEXT,
        acknowledged INTEGER NOT NULL DEFAULT 0,
        notes TEXT NOT NULL DEFAULT '[]'
    );
    CREATE INDEX collab_requests_parent ON collab_requests(parent_id, created_at);
    CREATE INDEX collab_requests_worker ON collab_requests(worker_id, created_at);
    CREATE INDEX collab_requests_status ON collab_requests(status);
    CREATE UNIQUE INDEX collab_requests_key ON collab_requests(parent_id, idem_key) WHERE idem_key IS NOT NULL;
    CREATE TABLE collab_holds (parent_id BLOB PRIMARY KEY, since TEXT NOT NULL);
    CREATE TABLE collab_direct (worker_id BLOB PRIMARY KEY, since TEXT NOT NULL);";

fn status_sql(s: ReqStatus) -> &'static str {
    match s {
        ReqStatus::Queued => "queued",
        ReqStatus::Delivered => "delivered",
        ReqStatus::Running => "running",
        ReqStatus::NeedsInput => "needs_input",
        ReqStatus::Done => "done",
        ReqStatus::Failed => "failed",
        ReqStatus::Cancelled => "cancelled",
        ReqStatus::Uncertain => "uncertain",
    }
}

fn status_from(s: &str) -> ReqStatus {
    match s {
        "queued" => ReqStatus::Queued,
        "delivered" => ReqStatus::Delivered,
        "running" => ReqStatus::Running,
        "needs_input" => ReqStatus::NeedsInput,
        "done" => ReqStatus::Done,
        "cancelled" => ReqStatus::Cancelled,
        "uncertain" => ReqStatus::Uncertain,
        _ => ReqStatus::Failed,
    }
}

fn tool_sql(t: Tool) -> &'static str {
    match t {
        Tool::Claude => "claude",
        Tool::Codex => "codex",
        Tool::Shell => "shell",
    }
}

fn tool_from(s: &str) -> Tool {
    match s {
        "claude" => Tool::Claude,
        "codex" => Tool::Codex,
        _ => Tool::Shell,
    }
}

/// Statuses that are still going or waiting.
const OPEN: &str = "('queued', 'delivered', 'running', 'needs_input', 'uncertain')";

fn from_row(r: &Row) -> rusqlite::Result<Request> {
    let result: Option<String> = r.get("result")?;
    let notes: String = r.get("notes")?;
    Ok(Request {
        id: r.get("id")?,
        parent_id: r.get("parent_id")?,
        worker_id: r.get("worker_id")?,
        previous_id: r.get("previous_id")?,
        attempt: r.get::<_, i64>("attempt")?.max(1) as u32,
        title: r.get("title")?,
        role: r.get("role")?,
        tool: tool_from(&r.get::<_, String>("tool")?),
        prompt: r.get("prompt")?,
        source: if r.get::<_, String>("source")? == "person" { Source::Person } else { Source::Agent },
        via: if r.get::<_, String>("via")? == "launch" { Via::Launch } else { Via::Typed },
        interrupt: r.get("interrupt")?,
        status: status_from(&r.get::<_, String>("status")?),
        reason: r.get("reason")?,
        start_rev: r.get("start_rev")?,
        created_at: ts_from_sql(&r.get::<_, String>("created_at")?),
        delivered_at: opt_ts(r.get("delivered_at")?),
        finished_at: opt_ts(r.get("finished_at")?),
        updated_at: ts_from_sql(&r.get::<_, String>("updated_at")?),
        result: result.and_then(|j| serde_json::from_str::<WorkResult>(&j).ok()),
        acknowledged: r.get("acknowledged")?,
        notes: serde_json::from_str::<Vec<Note>>(&notes).unwrap_or_default(),
    })
}

pub fn insert(store: &Store, r: &Request, key: Option<&str>) -> Result<(), RpcError> {
    store
        .with(|c| {
            c.execute(
                "INSERT INTO collab_requests (id, parent_id, worker_id, previous_id, attempt, title, role, tool, prompt,
                    source, via, interrupt, idem_key, status, reason, start_rev, created_at, delivered_at,
                    finished_at, updated_at, result, acknowledged, notes)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20,
                    ?21, ?22, ?23)",
                params![
                    r.id,
                    r.parent_id,
                    r.worker_id,
                    r.previous_id,
                    r.attempt as i64,
                    r.title,
                    r.role,
                    tool_sql(r.tool),
                    r.prompt,
                    if r.source == Source::Person { "person" } else { "agent" },
                    if r.via == Via::Launch { "launch" } else { "typed" },
                    r.interrupt,
                    key,
                    status_sql(r.status),
                    r.reason,
                    r.start_rev,
                    ts_to_sql(&r.created_at),
                    r.delivered_at.as_ref().map(ts_to_sql),
                    r.finished_at.as_ref().map(ts_to_sql),
                    ts_to_sql(&r.updated_at),
                    r.result.as_ref().and_then(|x| serde_json::to_string(x).ok()),
                    r.acknowledged,
                    serde_json::to_string(&r.notes).unwrap_or_else(|_| "[]".into()),
                ],
            )
        })
        .map_err(db_err)?;
    Ok(())
}

/// Write every field that can change after creation.
pub fn save(store: &Store, r: &Request) -> Result<(), RpcError> {
    store
        .with(|c| {
            c.execute(
                "UPDATE collab_requests SET status = ?2, reason = ?3, start_rev = ?4, via = ?5, delivered_at = ?6,
                    finished_at = ?7, updated_at = ?8, result = ?9, acknowledged = ?10, notes = ?11
                 WHERE id = ?1",
                params![
                    r.id,
                    status_sql(r.status),
                    r.reason,
                    r.start_rev,
                    if r.via == Via::Launch { "launch" } else { "typed" },
                    r.delivered_at.as_ref().map(ts_to_sql),
                    r.finished_at.as_ref().map(ts_to_sql),
                    ts_to_sql(&r.updated_at),
                    r.result.as_ref().and_then(|x| serde_json::to_string(x).ok()),
                    r.acknowledged,
                    serde_json::to_string(&r.notes).unwrap_or_else(|_| "[]".into()),
                ],
            )
        })
        .map_err(db_err)?;
    Ok(())
}

pub fn get(store: &Store, id: RequestId) -> Result<Request, RpcError> {
    find(store, id)?.ok_or_else(|| RpcError::not_found("맡긴 일을 찾을 수 없어요"))
}

pub fn find(store: &Store, id: RequestId) -> Result<Option<Request>, RpcError> {
    store
        .with(|c| c.query_row("SELECT * FROM collab_requests WHERE id = ?1", [id], from_row).optional())
        .map_err(db_err)
}

pub fn by_key(store: &Store, parent: Option<SessionId>, key: &str) -> Result<Option<Request>, RpcError> {
    store
        .with(|c| {
            c.query_row(
                "SELECT * FROM collab_requests WHERE parent_id IS ?1 AND idem_key = ?2",
                params![parent, key],
                from_row,
            )
            .optional()
        })
        .map_err(db_err)
}

pub fn list(store: &Store, q: &RequestQuery) -> Result<Vec<Request>, RpcError> {
    let limit = if q.limit == 0 { 200 } else { q.limit.min(1000) } as i64;
    let filter =
        format!("(?1 IS NULL OR parent_id = ?1) AND (?2 IS NULL OR worker_id = ?2) AND (NOT ?3 OR status IN {OPEN})");
    let sql = if q.activity {
        format!(
            "SELECT * FROM collab_requests WHERE {filter} AND id IN (
                SELECT id FROM collab_requests WHERE status IN {OPEN}
                UNION SELECT id FROM collab_requests
                    WHERE parent_id IS NULL AND acknowledged = 0 AND status IN ('done', 'failed')
                UNION SELECT id FROM (SELECT id FROM collab_requests WHERE {filter}
                    ORDER BY created_at DESC, id DESC LIMIT ?4)
             ) ORDER BY created_at DESC, id DESC"
        )
    } else {
        format!("SELECT * FROM collab_requests WHERE {filter} ORDER BY created_at DESC, id DESC LIMIT ?4")
    };
    store
        .with(|c| {
            let mut st = c.prepare(&sql)?;
            let rows = st.query_map(params![q.parent_id, q.worker_id, q.open_only, limit], from_row)?;
            rows.collect()
        })
        .map_err(db_err)
}

/// Open requests, oldest first (delivery order).
pub fn open(store: &Store) -> Result<Vec<Request>, RpcError> {
    let sql = format!("SELECT * FROM collab_requests WHERE status IN {OPEN} ORDER BY created_at, id");
    store
        .with(|c| {
            let mut st = c.prepare(&sql)?;
            let rows = st.query_map([], from_row)?;
            rows.collect()
        })
        .map_err(db_err)
}

/// The worker's newest request, or its newest one in one of `statuses`.
pub fn latest_for_worker(
    store: &Store,
    worker: SessionId,
    statuses: &[ReqStatus],
) -> Result<Option<Request>, RpcError> {
    let all = list(store, &RequestQuery { worker_id: Some(worker), limit: 50, ..Default::default() })?;
    Ok(all.into_iter().find(|r| statuses.is_empty() || statuses.contains(&r.status)))
}

pub fn set_hold(store: &Store, parent: SessionId, held: bool) -> Result<(), RpcError> {
    store
        .with(|c| {
            if held {
                c.execute(
                    "INSERT OR IGNORE INTO collab_holds (parent_id, since) VALUES (?1, ?2)",
                    params![parent, ts_to_sql(&runwick_core::now())],
                )
            } else {
                c.execute("DELETE FROM collab_holds WHERE parent_id = ?1", [parent])
            }
        })
        .map_err(db_err)?;
    Ok(())
}

pub fn set_direct(store: &Store, worker: SessionId, on: bool) -> Result<bool, RpcError> {
    let n = store
        .with(|c| {
            if on {
                c.execute(
                    "INSERT OR IGNORE INTO collab_direct (worker_id, since) VALUES (?1, ?2)",
                    params![worker, ts_to_sql(&runwick_core::now())],
                )
            } else {
                c.execute("DELETE FROM collab_direct WHERE worker_id = ?1", [worker])
            }
        })
        .map_err(db_err)?;
    Ok(n > 0)
}

fn ids(store: &Store, sql: &str) -> Result<Vec<Uuid>, RpcError> {
    store
        .with(|c| {
            let mut st = c.prepare(sql)?;
            let rows = st.query_map([], |r| r.get(0))?;
            rows.collect()
        })
        .map_err(db_err)
}

pub fn held_parents(store: &Store) -> Result<Vec<SessionId>, RpcError> {
    ids(store, "SELECT parent_id FROM collab_holds ORDER BY since")
}

pub fn direct_workers(store: &Store) -> Result<Vec<SessionId>, RpcError> {
    ids(store, "SELECT worker_id FROM collab_direct ORDER BY since")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activity_keeps_old_open_and_unread_person_requests_with_bounded_history() {
        let store = Store::open_in_memory().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let opened = crate::projects::open_folder(&store, dir.path().to_str().unwrap()).unwrap();
        let worker = Uuid::new_v4();
        store
            .with(|c| {
                c.execute(
                    "INSERT INTO sessions (id, project_id, workspace_id, title, tool, cwd, created_at)
             VALUES (?1, ?2, ?3, 'worker', 'claude', ?4, ?5)",
                    params![
                        worker,
                        opened.project.id,
                        opened.workspace.id,
                        dir.path().to_str(),
                        ts_to_sql(&runwick_core::now())
                    ],
                )
            })
            .unwrap();
        let parent = Uuid::new_v4();
        let mut required = Vec::new();
        let mut optional = Vec::new();
        for (n, status) in [
            ReqStatus::Queued,
            ReqStatus::Delivered,
            ReqStatus::Running,
            ReqStatus::NeedsInput,
            ReqStatus::Uncertain,
            ReqStatus::Done,
            ReqStatus::Failed,
            ReqStatus::Done,
            ReqStatus::Failed,
            ReqStatus::Done,
            ReqStatus::Cancelled,
        ]
        .into_iter()
        .enumerate()
        {
            let mut r = super::super::new_request(Some(parent), worker, Tool::Claude, "task");
            r.status = status;
            r.created_at -= chrono::Duration::days(180);
            if (5..=6).contains(&n) || n == 9 {
                r.parent_id = None;
            }
            if n == 9 {
                r.acknowledged = true;
            }
            insert(&store, &r, None).unwrap();
            if n < 7 {
                required.push(r.id);
            } else {
                optional.push(r.id);
            }
        }
        for _ in 0..450 {
            let mut r = super::super::new_request(Some(parent), worker, Tool::Claude, "recent");
            r.status = ReqStatus::Done;
            insert(&store, &r, None).unwrap();
        }
        let recent = list(&store, &RequestQuery { limit: 400, ..Default::default() }).unwrap();
        assert_eq!(recent.len(), 400);
        assert!(recent.iter().all(|r| !required.contains(&r.id)));
        let activity = list(&store, &RequestQuery { activity: true, limit: 25, ..Default::default() }).unwrap();
        assert_eq!(activity.len(), 25 + required.len());
        for id in required {
            assert!(activity.iter().any(|r| r.id == id));
        }
        assert!(activity.iter().all(|r| !optional.contains(&r.id)));
        assert!(activity.windows(2).all(|w| (w[0].created_at, w[0].id) > (w[1].created_at, w[1].id)));
        let filtered =
            list(&store, &RequestQuery { activity: true, worker_id: Some(Uuid::new_v4()), ..Default::default() })
                .unwrap();
        assert!(filtered.is_empty());
        let open =
            list(&store, &RequestQuery { activity: true, open_only: true, limit: 1, ..Default::default() }).unwrap();
        assert_eq!(open.len(), 5);
        assert!(open.iter().all(|r| r.status.is_open()));
        // Named migrations can be reapplied without changing existing records.
        store.migrate(super::super::MIGRATIONS).unwrap();
        assert_eq!(list(&store, &RequestQuery { activity: true, limit: 25, ..Default::default() }).unwrap(), activity);
    }

    #[test]
    fn statuses_round_trip() {
        for s in [
            ReqStatus::Queued,
            ReqStatus::Delivered,
            ReqStatus::Running,
            ReqStatus::NeedsInput,
            ReqStatus::Done,
            ReqStatus::Failed,
            ReqStatus::Cancelled,
            ReqStatus::Uncertain,
        ] {
            assert_eq!(status_from(status_sql(s)), s);
            assert_eq!(OPEN.contains(&format!("'{}'", status_sql(s))), s.is_open(), "{s:?}");
        }
    }
}
