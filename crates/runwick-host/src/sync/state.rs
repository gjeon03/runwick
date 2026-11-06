//! What sync remembers on this host: the connected repository, the last origin commit whose
//! changes are fully in the records (the merge base for the next sync), auto-sync settings, the
//! last run, which message files are already written, and records deleted here.

use crate::meta::{kv_get, kv_put};
use crate::store::{Migration, Store};
use runwick_core::Ts;
use runwick_core::api::sync::{AutoSync, RepoRef, SyncFailure, SyncReport};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const MIGRATIONS: &[Migration] = &[(
    "0301_sync",
    "-- Sessions synced from another computer: read-only here (NULL for this computer's own).
    ALTER TABLE sessions ADD COLUMN origin_host BLOB;
    ALTER TABLE sessions ADD COLUMN origin_device TEXT;
    -- Where a project's folder is on other computers.
    CREATE TABLE sync_roots (
        project_id BLOB NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
        host_id BLOB NOT NULL,
        device TEXT NOT NULL,
        path TEXT NOT NULL,
        PRIMARY KEY (project_id, host_id)
    );
    -- Signature of each message file as last written into the working copy.
    CREATE TABLE sync_marks (path TEXT PRIMARY KEY, sig TEXT NOT NULL);
    -- Records removed on this computer, written out as tombstones by the next sync.
    CREATE TABLE sync_deleted (
        kind TEXT NOT NULL,
        key TEXT NOT NULL,
        deleted_at TEXT NOT NULL,
        PRIMARY KEY (kind, key)
    );
    CREATE TRIGGER sync_pins_deleted AFTER DELETE ON pins BEGIN
        INSERT OR REPLACE INTO sync_deleted (kind, key, deleted_at)
        VALUES ('pins', lower(hex(old.id)), strftime('%Y-%m-%dT%H:%M:%fZ', 'now'));
    END;
    CREATE TRIGGER sync_roles_deleted AFTER DELETE ON roles BEGIN
        INSERT OR REPLACE INTO sync_deleted (kind, key, deleted_at)
        VALUES ('roles', lower(hex(old.id)), strftime('%Y-%m-%dT%H:%M:%fZ', 'now'));
    END;
    CREATE TRIGGER sync_wiki_deleted AFTER UPDATE OF deleted ON wiki_pages
    WHEN new.deleted = 1 AND old.deleted = 0 BEGIN
        INSERT OR REPLACE INTO sync_deleted (kind, key, deleted_at)
        VALUES ('wiki', lower(hex(new.project_id)) || '/' || new.slug, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'));
    END;",
),
(
    "0302_sync_safety",
    "-- Counts every change to a synced record: \"올리지 않은 변경\" compares it, not sizes.
    CREATE TABLE sync_changes (id INTEGER PRIMARY KEY CHECK (id = 1), n INTEGER NOT NULL);
    INSERT INTO sync_changes (id, n) VALUES (1, 0);
    CREATE TRIGGER sync_n_projects_insert AFTER INSERT ON projects BEGIN
        UPDATE sync_changes SET n = n + 1 WHERE id = 1;
    END;
    CREATE TRIGGER sync_n_projects_update AFTER UPDATE ON projects BEGIN
        UPDATE sync_changes SET n = n + 1 WHERE id = 1;
    END;
    CREATE TRIGGER sync_n_projects_delete AFTER DELETE ON projects BEGIN
        UPDATE sync_changes SET n = n + 1 WHERE id = 1;
    END;
    CREATE TRIGGER sync_n_pins_insert AFTER INSERT ON pins BEGIN
        UPDATE sync_changes SET n = n + 1 WHERE id = 1;
    END;
    CREATE TRIGGER sync_n_pins_update AFTER UPDATE ON pins BEGIN
        UPDATE sync_changes SET n = n + 1 WHERE id = 1;
    END;
    CREATE TRIGGER sync_n_pins_delete AFTER DELETE ON pins BEGIN
        UPDATE sync_changes SET n = n + 1 WHERE id = 1;
    END;
    CREATE TRIGGER sync_n_wiki_pages_insert AFTER INSERT ON wiki_pages BEGIN
        UPDATE sync_changes SET n = n + 1 WHERE id = 1;
    END;
    CREATE TRIGGER sync_n_wiki_pages_update AFTER UPDATE ON wiki_pages BEGIN
        UPDATE sync_changes SET n = n + 1 WHERE id = 1;
    END;
    CREATE TRIGGER sync_n_wiki_pages_delete AFTER DELETE ON wiki_pages BEGIN
        UPDATE sync_changes SET n = n + 1 WHERE id = 1;
    END;
    CREATE TRIGGER sync_n_roles_insert AFTER INSERT ON roles BEGIN
        UPDATE sync_changes SET n = n + 1 WHERE id = 1;
    END;
    CREATE TRIGGER sync_n_roles_update AFTER UPDATE ON roles BEGIN
        UPDATE sync_changes SET n = n + 1 WHERE id = 1;
    END;
    CREATE TRIGGER sync_n_roles_delete AFTER DELETE ON roles BEGIN
        UPDATE sync_changes SET n = n + 1 WHERE id = 1;
    END;
    CREATE TRIGGER sync_n_env_vars_insert AFTER INSERT ON env_vars BEGIN
        UPDATE sync_changes SET n = n + 1 WHERE id = 1;
    END;
    CREATE TRIGGER sync_n_env_vars_update AFTER UPDATE ON env_vars BEGIN
        UPDATE sync_changes SET n = n + 1 WHERE id = 1;
    END;
    CREATE TRIGGER sync_n_env_vars_delete AFTER DELETE ON env_vars BEGIN
        UPDATE sync_changes SET n = n + 1 WHERE id = 1;
    END;
    CREATE TRIGGER sync_n_project_notes_insert AFTER INSERT ON project_notes BEGIN
        UPDATE sync_changes SET n = n + 1 WHERE id = 1;
    END;
    CREATE TRIGGER sync_n_project_notes_update AFTER UPDATE ON project_notes BEGIN
        UPDATE sync_changes SET n = n + 1 WHERE id = 1;
    END;
    CREATE TRIGGER sync_n_project_notes_delete AFTER DELETE ON project_notes BEGIN
        UPDATE sync_changes SET n = n + 1 WHERE id = 1;
    END;
    CREATE TRIGGER sync_n_timeline_summaries_insert AFTER INSERT ON timeline_summaries BEGIN
        UPDATE sync_changes SET n = n + 1 WHERE id = 1;
    END;
    CREATE TRIGGER sync_n_timeline_summaries_update AFTER UPDATE ON timeline_summaries BEGIN
        UPDATE sync_changes SET n = n + 1 WHERE id = 1;
    END;
    CREATE TRIGGER sync_n_timeline_summaries_delete AFTER DELETE ON timeline_summaries BEGIN
        UPDATE sync_changes SET n = n + 1 WHERE id = 1;
    END;
    CREATE TRIGGER sync_n_collab_requests_insert AFTER INSERT ON collab_requests BEGIN
        UPDATE sync_changes SET n = n + 1 WHERE id = 1;
    END;
    CREATE TRIGGER sync_n_collab_requests_update AFTER UPDATE ON collab_requests BEGIN
        UPDATE sync_changes SET n = n + 1 WHERE id = 1;
    END;
    CREATE TRIGGER sync_n_collab_requests_delete AFTER DELETE ON collab_requests BEGIN
        UPDATE sync_changes SET n = n + 1 WHERE id = 1;
    END;
    CREATE TRIGGER sync_n_sessions_insert AFTER INSERT ON sessions BEGIN
        UPDATE sync_changes SET n = n + 1 WHERE id = 1;
    END;
    CREATE TRIGGER sync_n_sessions_delete AFTER DELETE ON sessions BEGIN
        UPDATE sync_changes SET n = n + 1 WHERE id = 1;
    END;
    CREATE TRIGGER sync_n_sessions_update AFTER UPDATE OF title, title_user_set, archived, last_active_at, ended_at, exit_code, parent_id, role ON sessions BEGIN
        UPDATE sync_changes SET n = n + 1 WHERE id = 1;
    END;
    CREATE TRIGGER sync_n_messages_insert AFTER INSERT ON messages BEGIN
        UPDATE sync_changes SET n = n + 1 WHERE id = 1;
    END;
    CREATE TRIGGER sync_n_messages_delete AFTER DELETE ON messages BEGIN
        UPDATE sync_changes SET n = n + 1 WHERE id = 1;
    END;
    CREATE TRIGGER sync_n_messages_update AFTER UPDATE OF text ON messages BEGIN
        UPDATE sync_changes SET n = n + 1 WHERE id = 1;
    END;
    CREATE TRIGGER sync_n_prefs_insert AFTER INSERT ON kv WHEN new.key = 'prefs' BEGIN
        UPDATE sync_changes SET n = n + 1 WHERE id = 1;
    END;
    CREATE TRIGGER sync_n_prefs_update AFTER UPDATE ON kv WHEN new.key = 'prefs' BEGIN
        UPDATE sync_changes SET n = n + 1 WHERE id = 1;
    END;
    -- Repository files that could not be imported; read again by every sync until they are.
    CREATE TABLE sync_skips (path TEXT PRIMARY KEY, reason TEXT NOT NULL, at TEXT NOT NULL);
    -- Records changed on two computers where one side's content was not kept. Written as a
    -- file by the computer that found it, so every computer can show and restore it.
    CREATE TABLE sync_conflicts (
        id BLOB PRIMARY KEY,
        host_id BLOB NOT NULL,
        device TEXT NOT NULL,
        kind TEXT NOT NULL,
        path TEXT NOT NULL,
        title TEXT NOT NULL,
        kept_device TEXT NOT NULL,
        lost_host BLOB,
        lost_device TEXT NOT NULL,
        lost TEXT NOT NULL,
        at TEXT NOT NULL,
        dismissed INTEGER NOT NULL DEFAULT 0
    );
    -- Variables whose plain value went into the repository (its history keeps it).
    CREATE TABLE sync_env_exposed (key TEXT PRIMARY KEY, at TEXT NOT NULL);
    -- Values from another computer for variables that can run programs: shown for review,
    -- applied only when the person accepts them.
    CREATE TABLE sync_env_staged (
        key TEXT PRIMARY KEY,
        value TEXT NOT NULL,
        description TEXT NOT NULL DEFAULT '',
        updated_at TEXT NOT NULL,
        device TEXT NOT NULL,
        at TEXT NOT NULL,
        rejected INTEGER NOT NULL DEFAULT 0
    );",
),
    (
        "0303_sync_env_review",
        "-- A staged change can also be a removal made on the other computer.
    ALTER TABLE sync_env_staged ADD COLUMN remove INTEGER NOT NULL DEFAULT 0;
    -- key=value pairs the person accepted from another computer: SHA-256 fingerprints only,
    -- so the same value arriving again is applied without asking twice.
    CREATE TABLE sync_env_accepted (
        key TEXT NOT NULL,
        fingerprint TEXT NOT NULL,
        at TEXT NOT NULL,
        PRIMARY KEY (key, fingerprint)
    );",
    ),
    (
        "0304_sync_precise_changes",
        "-- Only changes to what is written into the repository count as changes not synced yet
    -- (opening a project or a page file's time alone writes nothing).
    DROP TRIGGER sync_n_projects_update;
    CREATE TRIGGER sync_n_projects_update AFTER UPDATE ON projects
    WHEN old.name IS NOT new.name OR old.root IS NOT new.root OR old.favorite IS NOT new.favorite
        OR old.archived IS NOT new.archived OR old.remote_url IS NOT new.remote_url
        OR old.created_at IS NOT new.created_at
    BEGIN
        UPDATE sync_changes SET n = n + 1 WHERE id = 1;
    END;
    DROP TRIGGER sync_n_wiki_pages_update;
    CREATE TRIGGER sync_n_wiki_pages_update AFTER UPDATE ON wiki_pages
    WHEN old.slug IS NOT new.slug OR old.title IS NOT new.title OR old.status IS NOT new.status
        OR old.sources IS NOT new.sources OR old.tags IS NOT new.tags OR old.version IS NOT new.version
        OR old.deleted IS NOT new.deleted OR old.project_id IS NOT new.project_id
    BEGIN
        UPDATE sync_changes SET n = n + 1 WHERE id = 1;
    END;
    DROP TRIGGER sync_n_env_vars_update;
    CREATE TRIGGER sync_n_env_vars_update AFTER UPDATE ON env_vars
    WHEN old.description IS NOT new.description OR old.secret IS NOT new.secret
        OR old.value IS NOT new.value OR old.updated_at IS NOT new.updated_at
    BEGIN
        UPDATE sync_changes SET n = n + 1 WHERE id = 1;
    END;
    DROP TRIGGER sync_n_prefs_update;
    CREATE TRIGGER sync_n_prefs_update AFTER UPDATE ON kv
    WHEN new.key = 'prefs' AND old.value IS NOT new.value
    BEGIN
        UPDATE sync_changes SET n = n + 1 WHERE id = 1;
    END;",
    ),
    (
        "0305_sync_env_local_edits",
        "-- A variable added, removed or given another value on this computer replaces whatever
    -- waits for review for it: the person's own edit is the newest change.
    CREATE TRIGGER sync_env_staged_local_insert AFTER INSERT ON env_vars BEGIN
        DELETE FROM sync_env_staged WHERE key = new.key;
    END;
    CREATE TRIGGER sync_env_staged_local_delete AFTER DELETE ON env_vars BEGIN
        DELETE FROM sync_env_staged WHERE key = old.key;
    END;
    CREATE TRIGGER sync_env_staged_local_update AFTER UPDATE ON env_vars
    WHEN old.value IS NOT new.value OR old.secret IS NOT new.secret OR old.changed_in IS NOT new.changed_in
    BEGIN
        DELETE FROM sync_env_staged WHERE key = new.key;
    END;
    -- Changes an edit here had already replaced (they were only hidden before).
    DELETE FROM sync_env_staged WHERE key IN (
        SELECT s.key FROM sync_env_staged s JOIN env_vars v ON v.key = s.key
        WHERE NOT ((s.remove = 0 AND v.updated_at < s.updated_at) OR (s.remove = 1 AND v.updated_at < s.at)));
    -- A value put back from '겹친 변경': it waits for review on this computer only and is never
    -- written into the repository as another computer's value.
    ALTER TABLE sync_env_staged ADD COLUMN restore INTEGER NOT NULL DEFAULT 0;",
    ),
    (
        "0306_sync_conflict_resolutions",
        "-- Who wrote the side that was kept, to name it when putting the other side back.
    ALTER TABLE sync_conflicts ADD COLUMN kept_host BLOB;
    -- Another computer's record says the content not kept was this computer's, and it matched
    -- what this computer had at its last sync (a record from the repository is not trusted alone).
    ALTER TABLE sync_conflicts ADD COLUMN lost_verified INTEGER NOT NULL DEFAULT 0;
    -- How a conflict was settled (put back or closed), by whichever computer did it. Written
    -- as files too, so no computer keeps offering to put back content that was settled.
    CREATE TABLE sync_conflict_resolutions (
        conflict_id BLOB NOT NULL,
        host_id BLOB NOT NULL,
        device TEXT NOT NULL,
        how TEXT NOT NULL,
        at TEXT NOT NULL,
        PRIMARY KEY (conflict_id, host_id)
    );",
    ),
    (
        "0307_sync_staged_host",
        "-- Which computer wrote a value waiting for review (to tell computers with the same name apart).
    ALTER TABLE sync_env_staged ADD COLUMN host BLOB;",
    ),
];

const K_REPO: &str = "sync.repo";
const K_BASE: &str = "sync.base";
const K_AUTO: &str = "sync.auto";
const K_LAST: &str = "sync.last";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoConfig {
    pub repo: RepoRef,
    pub connected_at: Ts,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LastRun {
    #[serde(default)]
    pub last_success: Option<Ts>,
    #[serde(default)]
    pub last_attempt: Option<Ts>,
    #[serde(default)]
    pub last_error: Option<SyncFailure>,
    #[serde(default)]
    pub last_report: Option<SyncReport>,
    #[serde(default)]
    pub ahead: Option<u32>,
    #[serde(default)]
    pub behind: Option<u32>,
    /// Records fingerprint when the last sync finished (see [`fingerprint`]).
    #[serde(default)]
    pub fingerprint: Option<String>,
}

fn get<T: serde::de::DeserializeOwned>(store: &Store, key: &str) -> Option<T> {
    kv_get(store, key).ok().flatten().and_then(|v| serde_json::from_value(v).ok())
}

fn put<T: Serialize>(store: &Store, key: &str, v: &T) -> rusqlite::Result<()> {
    kv_put(store, key, &serde_json::to_value(v).unwrap_or(serde_json::Value::Null))
}

fn delete(store: &Store, key: &str) -> rusqlite::Result<()> {
    store.with(|c| c.execute("DELETE FROM kv WHERE key = ?1", [key]).map(|_| ()))
}

pub fn repo(store: &Store) -> Option<RepoConfig> {
    get(store, K_REPO)
}

pub fn set_repo(store: &Store, cfg: &RepoConfig) -> rusqlite::Result<()> {
    put(store, K_REPO, cfg)
}

pub fn base(store: &Store) -> Option<String> {
    get(store, K_BASE)
}

pub fn set_base(store: &Store, commit: Option<&str>) -> rusqlite::Result<()> {
    match commit {
        Some(c) => put(store, K_BASE, &c),
        None => delete(store, K_BASE),
    }
}

pub fn auto(store: &Store) -> AutoSync {
    get(store, K_AUTO).unwrap_or_default()
}

pub fn set_auto(store: &Store, a: &AutoSync) -> rusqlite::Result<()> {
    put(store, K_AUTO, a)
}

pub fn last(store: &Store) -> LastRun {
    get(store, K_LAST).unwrap_or_default()
}

pub fn set_last(store: &Store, l: &LastRun) -> rusqlite::Result<()> {
    put(store, K_LAST, l)
}

/// Forget the repository (records stay as they are).
pub fn disconnect(store: &Store) -> rusqlite::Result<()> {
    for k in [K_REPO, K_BASE, K_LAST, K_RESTORED] {
        delete(store, k)?;
    }
    store.with(|c| c.execute("DELETE FROM sync_skips", []).map(|_| ()))?;
    clear_marks(store)
}

pub fn marks(store: &Store) -> rusqlite::Result<BTreeMap<String, String>> {
    store.with(|c| {
        let mut st = c.prepare("SELECT path, sig FROM sync_marks")?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.collect()
    })
}

pub fn set_marks(store: &Store, marks: &[(String, String)]) -> rusqlite::Result<()> {
    store.tx(|c| {
        for (p, s) in marks {
            c.execute(
                "INSERT INTO sync_marks (path, sig) VALUES (?1, ?2) ON CONFLICT(path) DO UPDATE SET sig = excluded.sig",
                params![p, s],
            )?;
        }
        Ok(())
    })
}

pub fn drop_mark(store: &Store, path: &str) -> rusqlite::Result<()> {
    store.with(|c| c.execute("DELETE FROM sync_marks WHERE path = ?1", [path]).map(|_| ()))
}

pub fn clear_marks(store: &Store) -> rusqlite::Result<()> {
    store.with(|c| c.execute("DELETE FROM sync_marks", []).map(|_| ()))
}

/// Records removed here since the last sync: `(kind, key, deleted_at)`.
pub fn deleted(store: &Store) -> rusqlite::Result<Vec<(String, String, String)>> {
    store.with(|c| {
        let mut st = c.prepare("SELECT kind, key, deleted_at FROM sync_deleted ORDER BY kind, key")?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        rows.collect()
    })
}

pub fn clear_deleted(store: &Store, done: &[(String, String, String)]) -> rusqlite::Result<()> {
    store.tx(|c| {
        for (kind, key, at) in done {
            // A record removed again after this sync read the list keeps its newer entry.
            c.execute(
                "DELETE FROM sync_deleted WHERE kind = ?1 AND key = ?2 AND deleted_at = ?3",
                params![kind, key, at],
            )?;
        }
        Ok(())
    })
}

/// Changes to synced records so far (bumped by triggers); differs whenever something synced
/// changed, including renames that keep the same length.
pub fn fingerprint(store: &Store) -> rusqlite::Result<String> {
    store.with(|c| {
        c.query_row("SELECT n FROM sync_changes WHERE id = 1", [], |r| r.get::<_, i64>(0)).map(|n| n.to_string())
    })
}

/// Set by a backup restore: the next sync keeps this computer's (restored) records where both
/// sides differ, and records the other side's content as a conflict.
pub const K_RESTORED: &str = "sync.restored";

pub fn restored(store: &Store) -> bool {
    get::<bool>(store, K_RESTORED).unwrap_or(false)
}

pub fn clear_restored(store: &Store) -> rusqlite::Result<()> {
    delete(store, K_RESTORED)
}

/// Repository files that could not be imported yet.
pub fn skips(store: &Store) -> rusqlite::Result<Vec<String>> {
    store.with(|c| {
        let mut st = c.prepare("SELECT path FROM sync_skips ORDER BY path")?;
        let rows = st.query_map([], |r| r.get(0))?;
        rows.collect()
    })
}

/// After a sync committed: files it processed are done, files it skipped are kept for retry.
pub fn update_skips(store: &Store, processed: &[String], skipped: &BTreeMap<String, String>) -> rusqlite::Result<()> {
    let now = crate::store::ts_to_sql(&runwick_core::now());
    store.tx(|c| {
        for p in processed {
            c.execute("DELETE FROM sync_skips WHERE path = ?1", [p])?;
        }
        for (p, why) in skipped {
            c.execute(
                "INSERT INTO sync_skips (path, reason, at) VALUES (?1, ?2, ?3)
                 ON CONFLICT(path) DO UPDATE SET reason = excluded.reason, at = excluded.at",
                params![p, why, now],
            )?;
        }
        Ok(())
    })
}

/// Remember variables whose plain value went into the repository.
pub fn expose(store: &Store, keys: &[String]) -> rusqlite::Result<()> {
    let now = crate::store::ts_to_sql(&runwick_core::now());
    store.tx(|c| {
        for k in keys {
            c.execute("INSERT OR IGNORE INTO sync_env_exposed (key, at) VALUES (?1, ?2)", params![k, now])?;
        }
        Ok(())
    })
}

/// Variables whose plain value is in the repository's history.
pub fn exposed(store: &Store) -> rusqlite::Result<Vec<String>> {
    store.with(|c| {
        let mut st = c.prepare("SELECT key FROM sync_env_exposed ORDER BY key")?;
        let rows = st.query_map([], |r| r.get(0))?;
        rows.collect()
    })
}
