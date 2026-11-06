//! Restoring a verified backup over the live data.
//!
//! The archive is unpacked into a separate folder first (preview). Activation then, in one
//! transaction on the live store, replaces every table's rows with the backup's (after bringing
//! the backup's schema up to this version), rebuilds the search index and swaps the wiki
//! folder. If anything fails, the transaction rolls back and the wiki folder is put back: the
//! current data stays in use. This computer's identity and sync connection are kept, and the
//! next sync starts as a union so nothing newer elsewhere is lost.

use super::archive::{DB_FILE, WIKI_DIR};
use crate::store::Store;
use rusqlite::{Connection, OptionalExtension};
use std::collections::HashSet;
use std::path::Path;
use uuid::Uuid;

/// Tables never copied: SQLite internals, the migration log, and the search index (rebuilt).
fn skipped(name: &str) -> bool {
    name.starts_with("sqlite_")
        || name == "schema_migrations"
        || name.starts_with("messages_fts")
        || KEPT_TABLES.contains(&name)
}

/// This computer's configuration a restore never replaces: the global variables (and what
/// sync knows about them) and the registered devices. They are not records, and an older
/// archive must not silently bring back a value or an SSH target.
pub const KEPT_TABLES: &[&str] =
    &["env_vars", "env_versions", "devices", "sync_env_staged", "sync_env_accepted", "sync_env_exposed"];

/// Keys of this computer that a restore keeps: identity, sync connection, what is known about
/// the backup files on disk (they did not change), and the external editor command (a command
/// to run is never taken from an archive).
const KEPT_KEYS: &[&str] =
    &["host.id", "sync.repo", "sync.auto", super::K_DIRS, super::K_VERIFIED, runwick_core::api::files::EDITOR_SETTING];
/// Sync state that would be wrong for older records: the next sync starts as a union in which
/// the restored records win (see `sync::state::K_RESTORED`).
const DROPPED_KEYS: &[&str] = &["sync.base", "sync.last", runwick_core::api::files::EDITOR_SETTING];

/// Migrations a backup has that this version does not know (it was made by a newer version).
pub fn unknown_migrations(backup_db: &Path) -> Result<Vec<String>, String> {
    let c = Connection::open_with_flags(backup_db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| e.to_string())?;
    let mut st = c.prepare("SELECT name FROM schema_migrations").map_err(|e| e.to_string())?;
    let names: Vec<String> = st
        .query_map([], |r| r.get(0))
        .map_err(|e| e.to_string())?
        .collect::<Result<_, _>>()
        .map_err(|e| e.to_string())?;
    let known: HashSet<&str> = crate::store::all_migrations().iter().map(|(n, _)| *n).collect();
    Ok(names.into_iter().filter(|n| !known.contains(n.as_str())).collect())
}

/// Replace the live data with the unpacked backup in `dir`.
pub fn activate(store: &Store, wiki_root: &Path, dir: &Path) -> Result<(), String> {
    let backup_db = dir.join(DB_FILE);
    let newer = unknown_migrations(&backup_db)?;
    if !newer.is_empty() {
        return Err("더 새로운 Runwick에서 만든 백업이라 이 버전으로는 복구할 수 없어요".into());
    }
    // Bring the backup's schema to this version (same tables, same column order as live), and
    // write its pages from its own index: file names exactly as the wiki makes them.
    let new_wiki = dir.join(format!("{WIKI_DIR}-restored"));
    {
        let restored = Store::open(&backup_db).map_err(|e| format!("백업 기록 저장소를 열지 못했어요: {e}"))?;
        restored.with(|c| Ok(super::archive::write_wiki(c, &new_wiki))).map_err(|e| e.to_string())??;
    }

    let old_wiki = wiki_root.with_file_name(format!(".wiki-before-restore-{}", Uuid::new_v4()));
    let had_wiki = wiki_root.exists();
    if had_wiki {
        std::fs::rename(wiki_root, &old_wiki).map_err(|e| format!("위키 폴더를 옮기지 못했어요: {e}"))?;
    }
    let moved =
        if new_wiki.exists() { std::fs::rename(&new_wiki, wiki_root) } else { std::fs::create_dir_all(wiki_root) };
    let undo_wiki = |old_wiki: &Path| {
        let _ = std::fs::remove_dir_all(wiki_root);
        if had_wiki {
            let _ = std::fs::rename(old_wiki, wiki_root);
        }
    };
    if let Err(e) = moved {
        undo_wiki(&old_wiki);
        return Err(format!("백업의 위키 폴더를 옮기지 못했어요: {e}"));
    }
    match store.with(|c| copy_all(c, &backup_db)) {
        Ok(()) => {
            // The previous pages are in the dated backup made before activation.
            let _ = std::fs::remove_dir_all(&old_wiki);
            Ok(())
        }
        Err(e) => {
            undo_wiki(&old_wiki);
            Err(format!("복구하지 못해서 지금 데이터를 그대로 써요: {e}"))
        }
    }
}

fn copy_all(c: &Connection, backup_db: &Path) -> rusqlite::Result<()> {
    c.execute("ATTACH DATABASE ?1 AS restored", [backup_db.to_string_lossy().as_ref()])?;
    let res = (|| {
        let tx = c.unchecked_transaction()?;
        tx.execute_batch("PRAGMA defer_foreign_keys = ON;")?;
        // Triggers would index and log every copied row; set them aside and put them back.
        let triggers: Vec<(String, String)> = {
            let mut st =
                tx.prepare("SELECT name, sql FROM main.sqlite_master WHERE type = 'trigger' AND sql IS NOT NULL")?;
            st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?
        };
        for (name, _) in &triggers {
            tx.execute_batch(&format!("DROP TRIGGER main.\"{name}\";"))?;
        }
        let kept: Vec<(String, String, String)> = {
            let mut st = tx.prepare(&format!(
                "SELECT key, value, updated_at FROM main.kv WHERE key IN ({})",
                KEPT_KEYS.iter().map(|k| format!("'{k}'")).collect::<Vec<_>>().join(", ")
            ))?;
            st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<Result<_, _>>()?
        };
        // Where each project's folder is on this computer now: kept (an archive's folder may be
        // gone or belong to something else).
        let roots: Vec<(Vec<u8>, String)> = {
            let mut st = tx.prepare("SELECT id, root FROM main.projects")?;
            st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?
        };
        let tables: Vec<String> = {
            let mut st = tx.prepare("SELECT name FROM main.sqlite_master WHERE type = 'table'")?;
            st.query_map([], |r| r.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .filter(|n| !skipped(n))
                .collect()
        };
        for t in &tables {
            tx.execute_batch(&format!("DELETE FROM main.\"{t}\";"))?;
        }
        for t in &tables {
            let present: Option<String> = tx
                .query_row("SELECT name FROM restored.sqlite_master WHERE type = 'table' AND name = ?1", [t], |r| {
                    r.get(0)
                })
                .optional()?;
            if present.is_none() {
                continue;
            }
            let cols: Vec<String> = {
                let mut st = tx.prepare(&format!("PRAGMA main.table_info(\"{t}\")"))?;
                st.query_map([], |r| r.get::<_, String>(1))?.collect::<Result<_, _>>()?
            };
            let list = cols.iter().map(|c| format!("\"{c}\"")).collect::<Vec<_>>().join(", ");
            tx.execute_batch(&format!("INSERT INTO main.\"{t}\" ({list}) SELECT {list} FROM restored.\"{t}\";"))?;
        }
        for k in DROPPED_KEYS {
            tx.execute("DELETE FROM main.kv WHERE key = ?1", [k])?;
        }
        tx.execute("DELETE FROM main.sync_marks", [])?;
        tx.execute(
            "INSERT INTO main.kv (key, value, updated_at) VALUES (?1, 'true', ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
            rusqlite::params![crate::sync::state::K_RESTORED, crate::store::ts_to_sql(&runwick_core::now())],
        )?;
        for (k, v, at) in &kept {
            tx.execute(
                "INSERT INTO main.kv (key, value, updated_at) VALUES (?1, ?2, ?3)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
                rusqlite::params![k, v, at],
            )?;
        }
        for (id, root) in &roots {
            let old: Option<String> =
                tx.query_row("SELECT root FROM main.projects WHERE id = ?1", [id], |r| r.get(0)).optional()?;
            if let Some(old) = old.filter(|o| o != root) {
                tx.execute("UPDATE main.projects SET root = ?2 WHERE id = ?1", rusqlite::params![id, root])?;
                tx.execute(
                    "UPDATE main.workspaces SET path = ?3 WHERE project_id = ?1 AND path = ?2 AND remote_device_id IS NULL",
                    rusqlite::params![id, old, root],
                )?;
            }
        }
        for (_, sql) in &triggers {
            tx.execute_batch(&format!("{sql};"))?;
        }
        tx.execute_batch("INSERT INTO messages_fts(messages_fts) VALUES('rebuild');")?;
        tx.commit()
    })();
    let _ = c.execute("DETACH DATABASE restored", []);
    res
}
