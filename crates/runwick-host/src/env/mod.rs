//! Global environment variables (S13 공통 환경변수): one set for every CLI this host starts.
//!
//! Each change of a value creates a new *version*; `env_versions` keeps, per version, which
//! value of each variable was current (`{key: changed_in}`), never the values. A run records the
//! version it started with, so a refresh can list added/changed/removed keys without values.
//! Secret values live in the [`secrets::SecretStore`] and are cached in memory for new runs.
//! Variable values are never logged; only names appear in logs.

pub mod secrets;

use crate::host::{Ctx, Host};
use crate::router::Router;
use crate::sessions::env::EnvOverlay;
use crate::store::{Migration, Store, db_err, ts_from_sql, ts_to_sql};
use runwick_core::api::env::*;
use runwick_core::api::refresh;
use runwick_core::rpc::{RpcError, codes, topic};
use rusqlite::{OptionalExtension, params};
use secrets::SecretStore;
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, OnceLock};

pub const MIGRATIONS: &[Migration] = &[
    (
        "0201_env",
        "CREATE TABLE env_vars (
        key TEXT PRIMARY KEY,
        description TEXT NOT NULL DEFAULT '',
        secret INTEGER NOT NULL DEFAULT 0,
        -- NULL for secrets: they are kept in the secret store.
        value TEXT,
        changed_in INTEGER NOT NULL,
        created_at TEXT NOT NULL,
        updated_at TEXT NOT NULL
    );
    -- Which value of each variable a version had: JSON {key: changed_in}. Never values.
    CREATE TABLE env_versions (
        version INTEGER PRIMARY KEY,
        vars TEXT NOT NULL,
        created_at TEXT NOT NULL
    );",
    ),
    (
        "0204_env_secret_from_start",
        "-- 1 when the current value was entered while the variable was secret (never plain).
    ALTER TABLE env_vars ADD COLUMN secret_from_start INTEGER NOT NULL DEFAULT 0;
    -- When a variable became secret keeping its plain value (NULL otherwise).
    ALTER TABLE env_vars ADD COLUMN secret_at TEXT;",
    ),
];

/// Largest accepted value.
const MAX_VALUE: usize = 32 * 1024;

/// Secret store plus what this host has read from it.
pub struct EnvService {
    secrets: Box<dyn SecretStore>,
    cache: Mutex<HashMap<String, String>>,
    /// Secrets that could not be read when the last overlay was built (names only).
    unreadable: Mutex<Vec<String>>,
}

impl EnvService {
    pub fn new(secrets: Box<dyn SecretStore>) -> Self {
        Self { secrets, cache: Mutex::new(HashMap::new()), unreadable: Mutex::new(Vec::new()) }
    }

    fn secret(&self, key: &str) -> Result<Option<String>, String> {
        if let Some(v) = self.cache.lock().ok().and_then(|c| c.get(key).cloned()) {
            return Ok(Some(v));
        }
        let v = self.secrets.get(key)?;
        if let (Some(v), Ok(mut c)) = (&v, self.cache.lock()) {
            c.insert(key.to_string(), v.clone());
        }
        Ok(v)
    }

    fn put_secret(&self, key: &str, value: &str) -> Result<(), String> {
        self.secrets.set(key, value)?;
        if let Ok(mut c) = self.cache.lock() {
            c.insert(key.to_string(), value.to_string());
        }
        Ok(())
    }

    fn drop_secret(&self, key: &str) -> Result<(), String> {
        if let Ok(mut c) = self.cache.lock() {
            c.remove(key);
        }
        self.secrets.delete(key)
    }

    /// Secrets missing from the most recent run's environment.
    pub fn last_unreadable(&self) -> Vec<String> {
        self.unreadable.lock().map(|u| u.clone()).unwrap_or_default()
    }
}

static SERVICE: OnceLock<EnvService> = OnceLock::new();

/// The host's environment service (platform secret store).
pub fn service() -> &'static EnvService {
    SERVICE.get_or_init(|| EnvService::new(secrets::platform_store()))
}

/// Give new runs the global variables (installed once at host start).
pub fn boot(host: &Arc<Host>) {
    host.sessions.set_env_hook(Arc::new(|host: &Host| match overlay(&host.store, service()) {
        Ok(o) => o,
        Err(e) => {
            tracing::warn!("global environment unavailable for a new run: {}", e.message);
            EnvOverlay::default()
        }
    }));
}

fn interactive_only(ctx: &Ctx) -> Result<(), RpcError> {
    if ctx.client.is_interactive() {
        Ok(())
    } else {
        Err(RpcError::new(codes::DENIED, "공통 환경변수는 Runwick 창에서만 보고 바꿀 수 있어요"))
    }
}

async fn blocking<T: Send + 'static>(
    ctx: &Ctx,
    f: impl FnOnce(&Store) -> Result<T, RpcError> + Send + 'static,
) -> Result<T, RpcError> {
    let host = ctx.host.clone();
    tokio::task::spawn_blocking(move || f(&host.store)).await.map_err(RpcError::internal)?
}

pub fn register(r: &mut Router) {
    r.add::<EnvList, _, _>(|ctx: Ctx, _: ()| async move { blocking(&ctx, |s| list(s, service())).await });
    r.add::<EnvSet, _, _>(|ctx: Ctx, p: SetVar| async move {
        interactive_only(&ctx)?;
        let var = blocking(&ctx, move |s| set(s, service(), p)).await?;
        ctx.host.bus.changed(&[topic::ENV, refresh::TOPIC]);
        Ok(var)
    });
    r.add::<EnvRemove, _, _>(|ctx: Ctx, p: VarKey| async move {
        interactive_only(&ctx)?;
        blocking(&ctx, move |s| remove(s, service(), &p.key)).await?;
        ctx.host.bus.changed(&[topic::ENV, refresh::TOPIC]);
        Ok(())
    });
    r.add::<EnvPlainHistory, _, _>(|ctx: Ctx, _: ()| async move { blocking(&ctx, plain_history).await });
    r.add::<EnvReveal, _, _>(|ctx: Ctx, p: VarKey| async move {
        interactive_only(&ctx)?;
        blocking(&ctx, move |s| reveal(s, service(), &p.key)).await
    });
}

fn var_from_row(r: &rusqlite::Row) -> rusqlite::Result<EnvVar> {
    let secret: bool = r.get("secret")?;
    Ok(EnvVar {
        key: r.get("key")?,
        description: r.get("description")?,
        secret,
        value: if secret { None } else { r.get("value")? },
        changed_in: r.get::<_, i64>("changed_in")? as u64,
        updated_at: ts_from_sql(&r.get::<_, String>("updated_at")?),
    })
}

fn all_vars(store: &Store) -> Result<Vec<EnvVar>, RpcError> {
    store
        .with(|c| {
            let mut st = c.prepare("SELECT * FROM env_vars ORDER BY key")?;
            let rows = st.query_map([], var_from_row)?;
            rows.collect()
        })
        .map_err(db_err)
}

fn get_var(store: &Store, key: &str) -> Result<Option<EnvVar>, RpcError> {
    store.with(|c| c.query_row("SELECT * FROM env_vars WHERE key = ?1", [key], var_from_row).optional()).map_err(db_err)
}

/// The current version (0 before the first change).
pub fn current_version(store: &Store) -> Result<u64, RpcError> {
    store
        .with(|c| c.query_row("SELECT COALESCE(MAX(version), 0) FROM env_versions", [], |r| r.get::<_, i64>(0)))
        .map(|v| v as u64)
        .map_err(db_err)
}

/// Which value of each variable `version` had (`{key: changed_in}`); empty for version 0.
pub fn snapshot(store: &Store, version: u64) -> Result<Option<BTreeMap<String, u64>>, RpcError> {
    if version == 0 {
        return Ok(Some(BTreeMap::new()));
    }
    let json: Option<String> = store
        .with(|c| {
            c.query_row("SELECT vars FROM env_versions WHERE version = ?1", [version as i64], |r| r.get(0)).optional()
        })
        .map_err(db_err)?;
    Ok(json.and_then(|j| serde_json::from_str(&j).ok()))
}

pub fn list(store: &Store, svc: &EnvService) -> Result<EnvState, RpcError> {
    let saved_at = store
        .with(|c| {
            c.query_row("SELECT created_at FROM env_versions ORDER BY version DESC LIMIT 1", [], |r| {
                r.get::<_, String>(0)
            })
            .optional()
        })
        .map_err(db_err)?
        .map(|s| ts_from_sql(&s));
    Ok(EnvState { version: current_version(store)?, vars: all_vars(store)?, secrets: svc.secrets.info(), saved_at })
}

/// Record a new version from the current table (inside the caller's transaction).
fn bump(tx: &rusqlite::Transaction, now: &str) -> rusqlite::Result<u64> {
    let next: i64 = tx.query_row("SELECT COALESCE(MAX(version), 0) + 1 FROM env_versions", [], |r| r.get(0))?;
    tx.execute("UPDATE env_vars SET changed_in = ?1 WHERE changed_in < 0", [next])?;
    let mut st = tx.prepare("SELECT key, changed_in FROM env_vars")?;
    let map: BTreeMap<String, i64> = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?;
    tx.execute(
        "INSERT INTO env_versions (version, vars, created_at) VALUES (?1, ?2, ?3)",
        params![next, serde_json::to_string(&map).unwrap_or_else(|_| "{}".into()), now],
    )?;
    Ok(next as u64)
}

/// Placeholder `changed_in` for rows whose value changes in the version being created.
const NEW_VALUE: i64 = -1;

pub fn set(store: &Store, svc: &EnvService, p: SetVar) -> Result<EnvVar, RpcError> {
    let key = p.key.trim().to_string();
    if let Some(problem) = key_problem(&key) {
        return Err(RpcError::invalid(problem));
    }
    if let Some(v) = &p.value {
        if v.len() > MAX_VALUE {
            return Err(RpcError::invalid("값은 32KB까지 저장할 수 있어요"));
        }
        if v.contains('\0') {
            return Err(RpcError::invalid("값에 NUL 문자를 넣을 수 없어요"));
        }
    }
    let description: String = p.description.trim().chars().take(200).collect();
    let existing = get_var(store, &key)?;
    let now = ts_to_sql(&runwick_core::now());
    let secret_err = |e: String| RpcError::new(codes::INTERNAL, e);

    let Some(old) = existing else {
        let value = p.value.ok_or_else(|| RpcError::invalid("값을 입력해 주세요"))?;
        if p.secret {
            svc.put_secret(&key, &value).map_err(secret_err)?;
        }
        let stored = (!p.secret).then_some(value);
        let res = store.tx(|tx| {
            tx.execute(
                "INSERT INTO env_vars (key, description, secret, value, changed_in, created_at, updated_at,
                    secret_from_start)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6, ?3)",
                params![key, description, p.secret, stored, NEW_VALUE, now],
            )?;
            bump(tx, &now)
        });
        if let Err(e) = res {
            if p.secret {
                let _ = svc.drop_secret(&key);
            }
            return Err(db_err(e));
        }
        tracing::info!(key = %key, secret = p.secret, "global variable added");
        return get_var(store, &key)?.ok_or_else(|| RpcError::internal("저장한 변수를 찾지 못했어요"));
    };

    // The value as stored now (needed to compare, or to move between stores).
    let current = if old.secret {
        match svc.secret(&key) {
            Ok(v) => v,
            Err(e) if p.value.is_some() => {
                tracing::warn!(key = %key, "cannot read the old secret: {e}");
                None
            }
            Err(e) => return Err(secret_err(e)),
        }
    } else {
        old.value.clone()
    };
    let value_changed = p.value.as_ref().is_some_and(|v| Some(v) != current.as_ref());
    let Some(new_value) = p.value.clone().or(current) else {
        return Err(RpcError::new(codes::INTERNAL, "저장된 값을 읽지 못했어요. 값을 다시 입력해 주세요"));
    };
    if p.secret && (value_changed || !old.secret) {
        svc.put_secret(&key, &new_value).map_err(secret_err)?;
    }
    let stored = (!p.secret).then(|| new_value.clone());
    store
        .tx(|tx| {
            tx.execute(
                "UPDATE env_vars SET description = ?2, secret = ?3, value = ?4, updated_at = ?5 WHERE key = ?1",
                params![key, description, p.secret, stored, now],
            )?;
            // Whether the value now stored was ever plain: a new value entered as a secret was not;
            // turning the flag on keeps a value that was.
            let from_start = match (p.secret, value_changed, old.secret) {
                (false, _, _) => Some(false),
                (true, true, _) => Some(true),
                (true, false, true) => None,
                (true, false, false) => Some(false),
            };
            if let Some(f) = from_start {
                // Turning the flag on while keeping the plain value ends its plain period now.
                let secret_at = (p.secret && !f).then(|| now.clone());
                tx.execute(
                    "UPDATE env_vars SET secret_from_start = ?2, secret_at = ?3 WHERE key = ?1",
                    params![key, f, secret_at],
                )?;
            }
            if value_changed {
                tx.execute("UPDATE env_vars SET changed_in = ?2 WHERE key = ?1", params![key, NEW_VALUE])?;
                bump(tx, &now)?;
            }
            Ok(())
        })
        .map_err(db_err)?;
    if old.secret
        && !p.secret
        && let Err(e) = svc.drop_secret(&key)
    {
        tracing::warn!(key = %key, "old secret was not removed from the secret store: {e}");
    }
    tracing::info!(key = %key, value_changed, secret = p.secret, "global variable saved");
    get_var(store, &key)?.ok_or_else(|| RpcError::internal("저장한 변수를 찾지 못했어요"))
}

pub fn remove(store: &Store, svc: &EnvService, key: &str) -> Result<(), RpcError> {
    let Some(old) = get_var(store, key)? else {
        return Err(RpcError::not_found("변수를 찾을 수 없어요"));
    };
    let now = ts_to_sql(&runwick_core::now());
    store
        .tx(|tx| {
            tx.execute("DELETE FROM env_vars WHERE key = ?1", [key])?;
            bump(tx, &now)
        })
        .map_err(db_err)?;
    if old.secret
        && let Err(e) = svc.drop_secret(key)
    {
        tracing::warn!(key = %key, "secret was not removed from the secret store: {e}");
    }
    tracing::info!(key = %key, "global variable removed");
    Ok(())
}

/// Variables whose current value is or was plain, with that period (see [`PlainPeriod`]).
pub fn plain_history(store: &Store) -> Result<Vec<PlainPeriod>, RpcError> {
    store
        .with(|c| {
            let mut st = c.prepare(
                "SELECT key, created_at, secret, COALESCE(secret_at, updated_at) FROM env_vars
                 WHERE secret = 0 OR secret_from_start = 0 ORDER BY key",
            )?;
            let rows = st.query_map([], |r| {
                let secret: bool = r.get(2)?;
                Ok(PlainPeriod {
                    key: r.get(0)?,
                    since: ts_from_sql(&r.get::<_, String>(1)?),
                    // Rows from before the time was kept: their last change is the latest it can be.
                    until: if secret { Some(ts_from_sql(&r.get::<_, String>(3)?)) } else { None },
                })
            })?;
            rows.collect()
        })
        .map_err(db_err)
}

pub fn reveal(store: &Store, svc: &EnvService, key: &str) -> Result<String, RpcError> {
    let var = get_var(store, key)?.ok_or_else(|| RpcError::not_found("변수를 찾을 수 없어요"))?;
    if !var.secret {
        return Ok(var.value.unwrap_or_default());
    }
    svc.secret(key)
        .map_err(|e| RpcError::new(codes::INTERNAL, e))?
        .ok_or_else(|| RpcError::not_found("보안 저장소에 값이 없어요. 값을 다시 입력해 주세요"))
}

/// Variables for a new run. Secrets that cannot be read are left out and remembered
/// ([`EnvService::last_unreadable`]) so the refresh view can say the run lacks them.
pub fn overlay(store: &Store, svc: &EnvService) -> Result<EnvOverlay, RpcError> {
    let version = current_version(store)?;
    let vars = all_vars(store)?;
    let mut set = Vec::with_capacity(vars.len());
    let mut unreadable = Vec::new();
    for v in vars {
        if !v.secret {
            set.push((v.key, v.value.unwrap_or_default()));
            continue;
        }
        match svc.secret(&v.key) {
            Ok(Some(value)) => set.push((v.key, value)),
            Ok(None) | Err(_) => {
                tracing::warn!(key = %v.key, "secret unavailable; the run starts without it");
                unreadable.push(v.key);
            }
        }
    }
    if let Ok(mut u) = svc.unreadable.lock() {
        *u = unreadable;
    }
    Ok(EnvOverlay { version, set, unset: Vec::new() })
}

/// Keys added, changed or removed between two snapshots.
pub fn diff(old: &BTreeMap<String, u64>, new: &BTreeMap<String, u64>) -> Vec<EnvChange> {
    let mut out = Vec::new();
    for (k, rev) in new {
        match old.get(k) {
            None => out.push(EnvChange { key: k.clone(), kind: EnvChangeKind::Added }),
            Some(o) if o != rev => out.push(EnvChange { key: k.clone(), kind: EnvChangeKind::Changed }),
            Some(_) => {}
        }
    }
    for k in old.keys().filter(|k| !new.contains_key(*k)) {
        out.push(EnvChange { key: k.clone(), kind: EnvChangeKind::Removed });
    }
    out.sort_by(|a, b| a.key.cmp(&b.key));
    out
}

/// What a run that started with `from` (missing `unreadable` secrets) lacks compared with now.
/// `None` when the starting version is unknown to this host.
pub fn changes_since(store: &Store, from: u64, unreadable: &[String]) -> Result<Option<Vec<EnvChange>>, RpcError> {
    let to = current_version(store)?;
    let (Some(old), Some(new)) = (snapshot(store, from)?, snapshot(store, to)?) else {
        return Ok(None);
    };
    let mut changes = if from == to { Vec::new() } else { diff(&old, &new) };
    for k in unreadable {
        if new.contains_key(k) && !changes.iter().any(|c| &c.key == k) {
            changes.push(EnvChange { key: k.clone(), kind: EnvChangeKind::Unreadable });
        }
    }
    changes.sort_by(|a, b| a.key.cmp(&b.key));
    Ok(Some(changes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use secrets::FakeStore;

    fn setup() -> (Store, EnvService) {
        (Store::open_in_memory().unwrap(), EnvService::new(Box::new(FakeStore::default())))
    }

    fn put(s: &Store, svc: &EnvService, key: &str, value: Option<&str>, secret: bool) -> EnvVar {
        let p = SetVar { key: key.into(), value: value.map(str::to_string), description: String::new(), secret };
        set(s, svc, p).unwrap()
    }

    #[test]
    fn every_value_change_bumps_the_version_and_metadata_does_not() {
        let (s, svc) = setup();
        assert_eq!(current_version(&s).unwrap(), 0);
        put(&s, &svc, "HTTP_PROXY", Some("http://p:1"), false);
        assert_eq!(current_version(&s).unwrap(), 1);
        put(&s, &svc, "NODE_OPTIONS", Some("--x"), false);
        assert_eq!(current_version(&s).unwrap(), 2);
        // Same value with a new description: no new version.
        let p = SetVar {
            key: "HTTP_PROXY".into(),
            value: Some("http://p:1".into()),
            description: "프록시".into(),
            secret: false,
        };
        let v = set(&s, &svc, p).unwrap();
        assert_eq!(current_version(&s).unwrap(), 2);
        assert_eq!(v.description, "프록시");
        assert_eq!(v.changed_in, 1);
        let v = put(&s, &svc, "HTTP_PROXY", Some("http://p:2"), false);
        assert_eq!((current_version(&s).unwrap(), v.changed_in), (3, 3));
        remove(&s, &svc, "NODE_OPTIONS").unwrap();
        assert_eq!(current_version(&s).unwrap(), 4);
        let snap = snapshot(&s, 4).unwrap().unwrap();
        assert_eq!(snap.into_iter().collect::<Vec<_>>(), vec![("HTTP_PROXY".to_string(), 3)]);
        assert!(set(&s, &svc, SetVar { key: "PATH".into(), value: Some("/x".into()), ..Default::default() }).is_err());
        assert!(set(&s, &svc, SetVar { key: "NEW".into(), value: None, ..Default::default() }).is_err());
    }

    #[test]
    fn secrets_are_masked_kept_out_of_the_table_and_moved_between_stores() {
        let (s, svc) = setup();
        let v = put(&s, &svc, "GITHUB_TOKEN", Some("ghp_secret"), true);
        assert_eq!(v.value, None);
        let listed = list(&s, &svc).unwrap();
        assert!(listed.vars.iter().all(|v| v.value.is_none()));
        let raw: Option<String> =
            s.with(|c| c.query_row("SELECT value FROM env_vars WHERE key = 'GITHUB_TOKEN'", [], |r| r.get(0))).unwrap();
        assert_eq!(raw, None, "secret value must not be stored in the database");
        assert_eq!(reveal(&s, &svc, "GITHUB_TOKEN").unwrap(), "ghp_secret");
        assert!(!serde_json::to_string(&listed).unwrap().contains("ghp_secret"));
        // Editing the description keeps the secret and the version.
        let p = SetVar { key: "GITHUB_TOKEN".into(), value: None, description: "깃허브".into(), secret: true };
        set(&s, &svc, p).unwrap();
        assert_eq!(current_version(&s).unwrap(), 1);
        assert_eq!(reveal(&s, &svc, "GITHUB_TOKEN").unwrap(), "ghp_secret");
        // Turning the flag off moves the value into the table without a new version.
        let p = SetVar { key: "GITHUB_TOKEN".into(), value: None, description: String::new(), secret: false };
        let v = set(&s, &svc, p).unwrap();
        assert_eq!(v.value.as_deref(), Some("ghp_secret"));
        assert_eq!(current_version(&s).unwrap(), 1);
        assert_eq!(svc.secrets.get("GITHUB_TOKEN").unwrap(), None);
    }

    #[test]
    fn plain_periods_end_when_a_kept_value_becomes_secret_and_vanish_when_it_changes() {
        let (s, svc) = setup();
        let keys =
            |s: &Store| plain_history(s).unwrap().into_iter().map(|p| (p.key, p.until.is_some())).collect::<Vec<_>>();
        put(&s, &svc, "BORN_SECRET", Some("s"), true);
        put(&s, &svc, "WAS_PLAIN", Some("p"), false);
        // Still plain: an open period. A secret entered as such has none.
        assert_eq!(keys(&s), [("WAS_PLAIN".to_string(), false)]);
        // Turning the flag on keeps the plain value: the period closes.
        let p = SetVar { key: "WAS_PLAIN".into(), value: None, description: String::new(), secret: true };
        set(&s, &svc, p).unwrap();
        let h = plain_history(&s).unwrap();
        assert_eq!(keys(&s), [("WAS_PLAIN".to_string(), true)]);
        assert!(h[0].until.unwrap() >= h[0].since);
        // Editing only the description keeps it; a new value entered as a secret ends it.
        let p = SetVar { key: "WAS_PLAIN".into(), value: None, description: "설명".into(), secret: true };
        set(&s, &svc, p).unwrap();
        assert_eq!(plain_history(&s).unwrap(), h);
        put(&s, &svc, "WAS_PLAIN", Some("rotated"), true);
        assert!(keys(&s).is_empty());
        // Plain again, then secret with a new value in the same save: never plain.
        put(&s, &svc, "WAS_PLAIN", Some("p2"), false);
        put(&s, &svc, "WAS_PLAIN", Some("s2"), true);
        assert!(keys(&s).is_empty());
    }

    #[test]
    fn overlay_carries_values_and_reports_unreadable_secrets() {
        let (s, svc) = setup();
        put(&s, &svc, "A", Some("1"), false);
        put(&s, &svc, "TOKEN", Some("t"), true);
        put(&s, &svc, "LOCKED", Some("l"), true);
        // A fresh host with an empty cache and a locked item.
        let fresh = FakeStore::default();
        fresh.map.lock().unwrap().insert("TOKEN".into(), "t".into());
        fresh.map.lock().unwrap().insert("LOCKED".into(), "l".into());
        fresh.failing.lock().unwrap().push("LOCKED".into());
        let svc2 = EnvService::new(Box::new(fresh));
        let o = overlay(&s, &svc2).unwrap();
        assert_eq!(o.version, 3);
        let set: BTreeMap<_, _> = o.set.into_iter().collect();
        assert_eq!(set.get("A").map(String::as_str), Some("1"));
        assert_eq!(set.get("TOKEN").map(String::as_str), Some("t"));
        assert!(!set.contains_key("LOCKED"));
        assert_eq!(svc2.last_unreadable(), vec!["LOCKED".to_string()]);
        let changes = changes_since(&s, 3, &svc2.last_unreadable()).unwrap().unwrap();
        assert_eq!(changes, vec![EnvChange { key: "LOCKED".into(), kind: EnvChangeKind::Unreadable }]);
    }

    #[test]
    fn changes_since_lists_keys_never_values() {
        let (s, svc) = setup();
        put(&s, &svc, "KEEP", Some("k"), false);
        put(&s, &svc, "EDIT", Some("v1"), true);
        put(&s, &svc, "GONE", Some("g"), false);
        let from = current_version(&s).unwrap();
        put(&s, &svc, "EDIT", Some("v2"), true);
        put(&s, &svc, "ADDED", Some("a"), false);
        remove(&s, &svc, "GONE").unwrap();
        let c = changes_since(&s, from, &[]).unwrap().unwrap();
        let kinds: Vec<(&str, EnvChangeKind)> = c.iter().map(|c| (c.key.as_str(), c.kind)).collect();
        assert_eq!(
            kinds,
            vec![("ADDED", EnvChangeKind::Added), ("EDIT", EnvChangeKind::Changed), ("GONE", EnvChangeKind::Removed)]
        );
        assert!(!serde_json::to_string(&c).unwrap().contains("v2"));
        assert_eq!(changes_since(&s, current_version(&s).unwrap(), &[]).unwrap().unwrap(), vec![]);
        assert_eq!(changes_since(&s, 99, &[]).unwrap(), None);
        // A run from before any variable existed lacks everything that exists now.
        let all = changes_since(&s, 0, &[]).unwrap().unwrap();
        assert!(all.iter().all(|c| c.kind == EnvChangeKind::Added));
        assert_eq!(all.len(), 3);
    }
}
