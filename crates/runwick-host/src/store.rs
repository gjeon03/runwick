//! SQLite store. Each feature owns its tables and lists its migrations in `MIGRATIONS`;
//! [`all_migrations`] collects them in a fixed order. A migration runs once, by name.

use anyhow::Context;
use rusqlite::{Connection, OptionalExtension};
use std::path::Path;
use std::sync::Mutex;

pub type Migration = (&'static str, &'static str);

/// Every feature's migrations. Append new features at the end; never reorder or edit a
/// migration that has shipped — add a new one instead.
pub fn all_migrations() -> Vec<Migration> {
    let mut v: Vec<Migration> = Vec::new();
    v.extend_from_slice(crate::meta::MIGRATIONS);
    v.extend_from_slice(crate::projects::MIGRATIONS);
    v.extend_from_slice(crate::sessions::MIGRATIONS);
    v.extend_from_slice(crate::records::MIGRATIONS);
    v.extend_from_slice(crate::timeline::MIGRATIONS);
    v.extend_from_slice(crate::wiki::MIGRATIONS);
    v.extend_from_slice(crate::jobs::MIGRATIONS);
    v.extend_from_slice(crate::collab::MIGRATIONS);
    v.extend_from_slice(crate::env::MIGRATIONS);
    v.extend_from_slice(crate::refresh::MIGRATIONS);
    v.extend_from_slice(crate::roles::MIGRATIONS);
    v.extend_from_slice(crate::sync::MIGRATIONS);
    v.extend_from_slice(crate::devices::MIGRATIONS);
    v
}

pub struct Store {
    conn: Mutex<Connection>,
}

impl Store {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).ok();
        }
        let conn = Connection::open(path).with_context(|| format!("open {}", path.display()))?;
        if conn.is_readonly(rusqlite::MAIN_DB).unwrap_or(false) {
            anyhow::bail!("데이터 파일에 쓸 수 없어요: {} (권한이나 디스크를 확인해 주세요)", path.display());
        }
        let store = Self::init(conn)?;
        restrict_db_files(path);
        Ok(store)
    }

    pub fn open_in_memory() -> anyhow::Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> anyhow::Result<Self> {
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA foreign_keys = ON;
             PRAGMA busy_timeout = 1500;
             PRAGMA secure_delete = ON;
             CREATE TABLE IF NOT EXISTS schema_migrations (
                 name TEXT PRIMARY KEY,
                 applied_at TEXT NOT NULL
             );",
        )?;
        let store = Self { conn: Mutex::new(conn) };
        store.migrate(&all_migrations())?;
        Ok(store)
    }

    pub fn migrate(&self, migrations: &[Migration]) -> anyhow::Result<()> {
        let mut conn = self.conn.lock().unwrap();
        for (name, sql) in migrations {
            let done: Option<String> = conn
                .query_row("SELECT name FROM schema_migrations WHERE name = ?1", [name], |r| r.get(0))
                .optional()?;
            if done.is_some() {
                continue;
            }
            let tx = conn.transaction()?;
            tx.execute_batch(sql).with_context(|| format!("migration {name}"))?;
            tx.execute(
                "INSERT INTO schema_migrations (name, applied_at) VALUES (?1, ?2)",
                rusqlite::params![name, runwick_core::now().to_rfc3339()],
            )?;
            tx.commit()?;
        }
        Ok(())
    }

    /// Run `f` with the connection. Keep work short: the lock is shared by all handlers.
    pub fn with<T>(&self, f: impl FnOnce(&Connection) -> rusqlite::Result<T>) -> rusqlite::Result<T> {
        let conn = self.conn.lock().unwrap();
        f(&conn)
    }

    /// Run `f` inside a transaction.
    pub fn tx<T>(&self, f: impl FnOnce(&rusqlite::Transaction) -> rusqlite::Result<T>) -> rusqlite::Result<T> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let out = f(&tx)?;
        tx.commit()?;
        Ok(out)
    }
}

/// The database and its WAL/SHM files hold conversations and drafts: owner-only.
fn restrict_db_files(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let base = path.as_os_str().to_owned();
    for suffix in ["", "-wal", "-shm"] {
        let mut p = base.clone();
        p.push(suffix);
        let _ = std::fs::set_permissions(Path::new(&p), std::fs::Permissions::from_mode(0o600));
    }
}

/// Timestamps are stored as RFC 3339 text.
pub fn ts_to_sql(ts: &runwick_core::Ts) -> String {
    ts.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

pub fn ts_from_sql(s: &str) -> runwick_core::Ts {
    chrono::DateTime::parse_from_rfc3339(s)
        .map(|d| d.with_timezone(&chrono::Utc))
        .unwrap_or_else(|_| chrono::DateTime::<chrono::Utc>::UNIX_EPOCH)
}

pub fn opt_ts(s: Option<String>) -> Option<runwick_core::Ts> {
    s.as_deref().map(ts_from_sql)
}

pub fn db_err(e: rusqlite::Error) -> runwick_core::RpcError {
    use rusqlite::ErrorCode;
    match &e {
        rusqlite::Error::SqliteFailure(f, _)
            if matches!(f.code, ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked) =>
        {
            runwick_core::RpcError::new(
                runwick_core::rpc::codes::BUSY,
                "다른 프로그램이 기록 저장소를 쓰고 있어요. 잠시 후 다시 시도해 주세요",
            )
        }
        rusqlite::Error::SqliteFailure(f, _) if f.code == ErrorCode::ReadOnly => {
            runwick_core::RpcError::internal("기록 저장소에 쓸 수 없어요 (권한이나 디스크를 확인해 주세요)")
        }
        rusqlite::Error::SqliteFailure(f, _) if f.code == ErrorCode::DiskFull => {
            runwick_core::RpcError::internal("디스크가 가득 차서 저장하지 못했어요")
        }
        _ => runwick_core::RpcError::internal(format!("저장소 오류: {e}")),
    }
}
