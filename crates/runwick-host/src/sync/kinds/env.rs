//! `settings/env.json`: the global variables' names, descriptions, secret flags and the values
//! of variables that are not secret. Secret values never leave the secret store: a secret that
//! arrives from another computer exists here without a value until the person enters one.
//!
//! A value never takes effect on this computer just because another computer set it: every
//! new or different value, and every removal, waits for review until the person accepts it (a
//! key=value accepted once is applied directly next time; only its SHA-256 fingerprint is
//! kept). While a change waits, this computer writes the other computer's version back
//! unchanged, so it never overwrites it there; any edit of that variable here (a new value, a
//! removal) replaces what waits, through triggers on `env_vars`. A variable that is secret here
//! is never turned into a plain value by another computer, and a value put back from "겹친
//! 변경" waits for review the same way (on this computer only).
//!
//! Accepted changes go through the environment's own functions (`crate::env::set` / `remove`),
//! so every value change creates a new environment version and sessions see "적용 대기".

use super::conflicts::Author;
use super::{Cx, Device, Names, Tally};
use crate::env;
use crate::store::{Store, db_err, ts_from_sql, ts_to_sql};
use runwick_core::api::env::{SetVar, key_problem};
use runwick_core::api::sync::{StagedVar, SyncKind, runs_programs};
use runwick_core::rpc::{RpcError, topic};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

pub const PATH: &str = "settings/env.json";

/// Only the fingerprint of an accepted value is kept.
fn fingerprint(key: &str, value: &str) -> String {
    let mut h = Sha256::new();
    h.update(key.as_bytes());
    h.update([0]);
    h.update(value.as_bytes());
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// The variables as this computer would write them: its own, with every change waiting for
/// review shown as the other computer has it (so it is never written over there).
pub fn render(cx: &Cx) -> Result<Value, RpcError> {
    let vars = cx
        .store
        .with(|c| {
            let mut st = c.prepare("SELECT key, description, secret, value, updated_at FROM env_vars ORDER BY key")?;
            let rows = st.query_map([], |r| {
                let secret: bool = r.get(2)?;
                let mut v = Map::new();
                v.insert("description".into(), json!(r.get::<_, String>(1)?));
                v.insert("secret".into(), json!(secret));
                if !secret {
                    v.insert("value".into(), json!(r.get::<_, Option<String>>(3)?.unwrap_or_default()));
                }
                v.insert("updated_at".into(), json!(r.get::<_, String>(4)?));
                Ok((r.get::<_, String>(0)?, Value::Object(v)))
            })?;
            let mut vars = rows.collect::<rusqlite::Result<Map<String, Value>>>()?;
            // Another computer's version, kept as it is there while it waits here.
            let mut st = c.prepare("SELECT key, description, value, updated_at, remove, rejected FROM sync_env_staged WHERE restore = 0")?;
            let staged = st.query_map([], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?, r.get::<_, bool>(4)?, r.get::<_, bool>(5)?))
            })?;
            for row in staged {
                let (key, description, value, updated_at, remove, rejected) = row?;
                match (remove, rejected) {
                    (true, false) => {
                        vars.remove(&key);
                    }
                    (true, true) => {}
                    (false, _) => {
                        vars.insert(key, json!({ "description": description, "secret": false, "value": value, "updated_at": updated_at }));
                    }
                }
            }
            Ok(vars)
        })
        .map_err(db_err)?;
    Ok(json!({ "vars": vars }))
}

fn is_secret(v: Option<&Value>) -> Option<bool> {
    v.map(|v| v.get("secret").and_then(Value::as_bool).unwrap_or(false))
}

fn text(v: &Value, k: &str) -> String {
    v.get(k).and_then(Value::as_str).unwrap_or("").to_string()
}

/// A change time from another computer, never later than now (a time in the future would win
/// every later comparison).
fn clamped(v: &Value) -> String {
    let now = runwick_core::now();
    super::super::format::ts_field(v, "updated_at")
        .filter(|t| *t <= now)
        .map(|t| ts_to_sql(&t))
        .unwrap_or_else(|| ts_to_sql(&now))
}

/// Bring the variables towards `merged` (from `local`, this computer's render). `device` wrote
/// the remote version.
pub fn apply(cx: &Cx, local: &Value, merged: &Value, device: &Author, tally: &mut Tally) {
    let empty = Map::new();
    let have = local.get("vars").and_then(Value::as_object).unwrap_or(&empty);
    let want = merged.get("vars").and_then(Value::as_object).unwrap_or(&empty);
    // What this computer really uses (`local` shows waiting changes as the other side has them).
    let mine = match own(cx) {
        Ok(m) => m,
        Err(e) => return tally.skipped.push(format!("{PATH}: {e}")),
    };
    let mut keys: Vec<&String> = have.keys().chain(want.keys()).collect();
    keys.sort();
    keys.dedup();
    for key in keys {
        let (cur, next) = (have.get(key), want.get(key));
        if cur == next {
            continue;
        }
        let here = mine.get(key);
        if here.is_none() && next.is_none() {
            // Gone on both sides: only what waited for it is left to drop.
            if let Err(e) = clear_staged(cx.store, key) {
                tally.skipped.push(format!("{PATH} · {key}: {e}"));
            }
            continue;
        }
        let res = match (here, next) {
            (None, None) => Ok(""),
            (Some(_), None) => stage(cx, key, None, device).map(|_| "다른 컴퓨터에서 지워서 검토를 기다려요"),
            (_, Some(n)) if is_secret(Some(n)) == Some(true) => apply_secret(cx, key, here, n).map(|_| ""),
            // Secret here, plain there: this computer keeps its secret; the plain value waits for
            // review like any other, and accepting it keeps the variable secret here.
            (Some(h), Some(n)) if is_secret(Some(h)) == Some(true) => stage(cx, key, Some(n), device)
                .map(|_| "이 컴퓨터에서는 비밀값이라 그대로 뒀어요 · 쓰기를 골라도 비밀값으로 저장해요"),
            (Some(h), Some(n)) if h.get("value") == n.get("value") => set_description(cx, key, n).map(|_| ""),
            (_, Some(n)) => {
                let value = text(n, "value");
                if accepted(cx.store, key, &value) {
                    set_plain(cx, key, n).map(|_| "")
                } else {
                    stage(cx, key, Some(n), device).map(|_| "다른 컴퓨터에서 다른 값이 와서 검토를 기다려요")
                }
            }
        };
        // Applied (or already so here): nothing about this variable waits any more.
        let res = match res {
            Ok("") => clear_staged(cx.store, key).map(|_| ""),
            other => other,
        };
        match res {
            Ok("") => tally.imported(SyncKind::Env, &[topic::ENV]),
            Ok(note) => {
                tally.topics.insert(topic::ENV);
                tally.conflict(SyncKind::Env, key, note);
            }
            Err(e) => tally.skipped.push(format!("{PATH} · {key}: {e}")),
        }
    }
}

fn accepted(store: &Store, key: &str, value: &str) -> bool {
    store
        .with(|c| {
            c.query_row(
                "SELECT 1 FROM sync_env_accepted WHERE key = ?1 AND fingerprint = ?2",
                params![key, fingerprint(key, value)],
                |_| Ok(()),
            )
            .optional()
        })
        .ok()
        .flatten()
        .is_some()
}

/// Secrets carry no value: a new one exists here empty, a plain one here becomes secret (its value
/// moves to the secret store), and otherwise only the description can change.
fn apply_secret(cx: &Cx, key: &str, cur: Option<&Value>, next: &Value) -> Result<(), String> {
    if let Some(problem) = key_problem(key) {
        return Err(problem);
    }
    let description = text(next, "description");
    let at = clamped(next);
    match is_secret(cur) {
        None => {
            let version = env::current_version(cx.store).map_err(|e| e.message)?;
            cx.store
                .with(|c| {
                    c.execute(
                        "INSERT INTO env_vars (key, description, secret, value, changed_in, created_at, updated_at)
                         VALUES (?1, ?2, 1, NULL, ?3, ?4, ?4)",
                        params![key, description, version as i64, at],
                    )
                })
                .map_err(|e| e.to_string())?;
        }
        Some(true) => {
            cx.store
                .with(|c| {
                    c.execute(
                        "UPDATE env_vars SET description = ?2, updated_at = ?3 WHERE key = ?1",
                        params![key, description, at],
                    )
                })
                .map_err(|e| e.to_string())?;
        }
        Some(false) => {
            env::set(cx.store, cx.env, SetVar { key: key.into(), value: None, description, secret: true })
                .map_err(|e| e.message)?;
            keep_time(cx.store, key, &at)?;
        }
    }
    Ok(())
}

fn set_description(cx: &Cx, key: &str, next: &Value) -> Result<(), String> {
    cx.store
        .with(|c| {
            c.execute(
                "UPDATE env_vars SET description = ?2, updated_at = ?3 WHERE key = ?1",
                params![key, text(next, "description"), clamped(next)],
            )
        })
        .map(|_| ())
        .map_err(|e| e.to_string())
}

fn set_plain(cx: &Cx, key: &str, next: &Value) -> Result<(), String> {
    if let Some(problem) = key_problem(key) {
        return Err(problem);
    }
    env::set(
        cx.store,
        cx.env,
        SetVar {
            key: key.into(),
            value: Some(text(next, "value")),
            description: text(next, "description"),
            secret: false,
        },
    )
    .map_err(|e| e.message)?;
    keep_time(cx.store, key, &clamped(next))
}

/// Keep the other computer's change time so both sides write the same record.
fn keep_time(store: &Store, key: &str, at: &str) -> Result<(), String> {
    store
        .with(|c| c.execute("UPDATE env_vars SET updated_at = ?2 WHERE key = ?1", params![key, at]))
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// This computer's variables as `render` writes them, without any waiting change.
fn own(cx: &Cx) -> Result<Map<String, Value>, String> {
    cx.store
        .with(|c| {
            let mut st = c.prepare("SELECT key, description, secret, value FROM env_vars")?;
            let rows = st.query_map([], |r| {
                let secret: bool = r.get(2)?;
                let mut v = json!({ "description": r.get::<_, String>(1)?, "secret": secret });
                if !secret {
                    v["value"] = json!(r.get::<_, Option<String>>(3)?.unwrap_or_default());
                }
                Ok((r.get::<_, String>(0)?, v))
            })?;
            rows.collect()
        })
        .map_err(|e| e.to_string())
}

fn clear_staged(store: &Store, key: &str) -> Result<(), String> {
    store
        .with(|c| c.execute("DELETE FROM sync_env_staged WHERE key = ?1", [key]))
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Wait for review: a value (`Some`) or a removal (`None`).
fn stage(cx: &Cx, key: &str, next: Option<&Value>, device: &Author) -> Result<(), String> {
    let now = ts_to_sql(&runwick_core::now());
    let (value, description, at, remove) = match next {
        Some(n) => (text(n, "value"), text(n, "description"), clamped(n), false),
        None => (String::new(), String::new(), now.clone(), true),
    };
    cx.store
        .with(|c| {
            c.execute(
                "INSERT INTO sync_env_staged (key, value, description, updated_at, device, at, rejected, remove, restore, host)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, ?7, 0, ?8)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value, description = excluded.description,
                     updated_at = excluded.updated_at, device = excluded.device, host = excluded.host,
                     at = excluded.at, remove = excluded.remove,
                     restore = 0,
                     rejected = CASE WHEN sync_env_staged.value = excluded.value AND sync_env_staged.remove = excluded.remove
                                     THEN sync_env_staged.rejected ELSE 0 END",
                params![key, value, description, at, device.1, now, remove, device.0],
            )
        })
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// A value put back from "겹친 변경": it waits for review here like a value from another
/// computer (with the same warning for variables that can run programs) and is never written
/// into the repository as another computer's value. Nothing waits when it is already the value
/// here.
pub fn stage_restored(cx: &Cx, key: &str, value: &str, description: &str, device: &str) -> Result<(), RpcError> {
    if let Some(problem) = key_problem(key) {
        return Err(RpcError::invalid(problem));
    }
    let now = ts_to_sql(&runwick_core::now());
    cx.store
        .tx(|c| {
            let waiting: Option<bool> = c
                .query_row(
                    "SELECT restore FROM sync_env_staged WHERE key = ?1",
                    [key],
                    |r| r.get(0),
                )
                .optional()?;
            if waiting == Some(false) {
                return Ok(Err(RpcError::invalid(
                    "이 변수에는 다른 컴퓨터에서 온 값이 검토를 기다리고 있어요. 그 값부터 정해 주세요",
                )));
            }
            let here: Option<(bool, Option<String>)> = c
                .query_row(
                    "SELECT secret, value FROM env_vars WHERE key = ?1",
                    [key],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            if let Some((false, Some(v))) = &here
                && v == value
            {
                c.execute("DELETE FROM sync_env_staged WHERE key = ?1", [key])?;
                return Ok(Ok(()));
            }
            c.execute(
                "INSERT OR REPLACE INTO sync_env_staged (key, value, description, updated_at, device, at, rejected, remove, restore, host)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?4, 0, 0, 1, NULL)",
                params![key, value, description, now, device],
            )?;
            Ok(Ok(()))
        })
        .map_err(db_err)?
}

/// Changes from other computers waiting for review (or declined here), with the computer that
/// made each named for the person (see `Names`).
pub fn staged(store: &Store, me: &Device) -> rusqlite::Result<Vec<StagedVar>> {
    store.with(|c| {
        let names = Names::load(c, me)?;
        let mut st = c.prepare("SELECT key, value, description, device, at, rejected, remove, host, restore FROM sync_env_staged ORDER BY key")?;
        let rows = st.query_map([], |r| {
            let key: String = r.get(0)?;
            let device = names.label(r.get(7)?, &r.get::<_, String>(3)?);
            let restore: bool = r.get(8)?;
            Ok(StagedVar {
                dangerous: runs_programs(&key),
                key,
                value: r.get(1)?,
                description: r.get(2)?,
                device: if restore {
                    format!("{device} · 겹친 변경에서")
                } else {
                    device
                },
                at: ts_from_sql(&r.get::<_, String>(4)?),
                rejected: r.get(5)?,
                remove: r.get(6)?,
            })
        })?;
        rows.collect()
    })
}

fn staged_row(c: &Connection, key: &str) -> rusqlite::Result<Option<(String, String, String, bool)>> {
    c.query_row("SELECT value, description, updated_at, remove FROM sync_env_staged WHERE key = ?1", [key], |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
    })
    .optional()
}

/// Use a change from another computer here, or keep not using it.
pub fn decide(store: &Store, svc: &env::EnvService, key: &str, accept: bool) -> Result<(), RpcError> {
    let Some((value, description, updated_at, remove)) = store.with(|c| staged_row(c, key)).map_err(db_err)? else {
        return Err(RpcError::not_found("검토할 변경이 없어요"));
    };
    let forget = |store: &Store| {
        store.with(|c| c.execute("DELETE FROM sync_env_staged WHERE key = ?1", [key])).map(|_| ()).map_err(db_err)
    };
    match (accept, remove) {
        // Keep the variable here: this computer writes it back, the other computer reviews it.
        (false, true) => forget(store),
        // Declined: another computer's value stays shown as declined (and is still written back
        // unchanged); a value put back from '겹친 변경' is simply dropped.
        (false, false) => store
            .with(|c| {
                c.execute("DELETE FROM sync_env_staged WHERE key = ?1 AND restore = 1", [key])?;
                c.execute("UPDATE sync_env_staged SET rejected = 1 WHERE key = ?1", [key])
            })
            .map(|_| ())
            .map_err(db_err),
        (true, true) => {
            let exists = store
                .with(|c| c.query_row("SELECT COUNT(*) FROM env_vars WHERE key = ?1", [key], |r| r.get::<_, i64>(0)))
                .map_err(db_err)?;
            if exists > 0 {
                env::remove(store, svc, key)?;
            }
            forget(store)
        }
        (true, false) => {
            // A secret here stays secret: only its value is replaced.
            let secret = store
                .with(|c| {
                    c.query_row("SELECT secret FROM env_vars WHERE key = ?1", [key], |r| r.get::<_, bool>(0)).optional()
                })
                .map_err(db_err)?
                .unwrap_or(false);
            env::set(store, svc, SetVar { key: key.into(), value: Some(value.clone()), description, secret })?;
            let now = ts_to_sql(&runwick_core::now());
            store
                .with(|c| {
                    c.execute("UPDATE env_vars SET updated_at = ?2 WHERE key = ?1", params![key, updated_at])?;
                    c.execute(
                        "INSERT OR IGNORE INTO sync_env_accepted (key, fingerprint, at) VALUES (?1, ?2, ?3)",
                        params![key, fingerprint(key, &value), now],
                    )
                })
                .map_err(db_err)?;
            forget(store)
        }
    }
}
