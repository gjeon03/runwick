//! `conflicts/<id>.json`: a record changed on two computers where one side's content was not
//! kept (an older field value, the other wiki text). The computer that found it writes the file;
//! every computer imports it, shows it under "겹친 변경" with the content that was not kept, and
//! can put that content back as a new edit. Nothing is lost silently.
//!
//! Records come from the repository, so they are checked before they are kept (the path is a
//! record of the stated kind; a variable's title, path and content name the same variable) and
//! only a record this computer found itself may say the content was this computer's. Putting a
//! variable back never applies it: it waits for review like any value from elsewhere. Putting
//! other content back is a new edit, and what it replaces is recorded the same way.
//!
//! `conflicts/resolved/<id>/<host>.json`: a conflict put back or closed on some computer, so no
//! computer keeps offering it.

use super::{Cx, Device, Names, opt_str, opt_uuid, str_field, ts_text};
use crate::store::{db_err, ts_to_sql};
use runwick_core::api::env::key_problem;
use runwick_core::api::sync::{SyncConflictItem, SyncKind};
use runwick_core::rpc::RpcError;
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde_json::{Value, json};
use uuid::Uuid;

pub const DIR: &str = "conflicts";
pub const RESOLVED: &str = "conflicts/resolved/";

pub fn path_of(id: Uuid) -> String {
    format!("{DIR}/{id}.json")
}

pub fn resolution_path(conflict: Uuid, host: Uuid) -> String {
    format!("{RESOLVED}{conflict}/{host}.json")
}

/// Whose content was not kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Local,
    Remote,
}

/// A conflict found during an import, recorded once the import is done.
#[derive(Debug, Clone)]
pub struct Found {
    pub kind: SyncKind,
    pub path: String,
    pub title: String,
    pub lost: Side,
    /// The content that was not kept (record JSON, or page text).
    pub content: String,
}

/// Who wrote a remote version: `(host id, device name)` when known.
pub type Author = (Option<Uuid>, String);

fn kind_sql(k: SyncKind) -> String {
    serde_json::to_value(k).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default()
}

fn kind_from(s: &str) -> Option<SyncKind> {
    serde_json::from_value(Value::String(s.to_string())).ok()
}

/// The variable a conflict of the environment is about (`settings/env.json#KEY`).
fn env_key(path: &str) -> Option<&str> {
    path.strip_prefix(super::env::PATH)?.strip_prefix('#')
}

/// A conflict record must describe one record of its kind: the path is a file of that kind, and
/// for a variable the path, the title and the content name the same variable.
pub fn check(kind: SyncKind, path: &str, title: &str, lost: &str) -> Result<(), String> {
    let bad = |why: &str| Err(format!("겹친 변경 기록이 맞지 않아 읽지 않았어요 ({why})"));
    match kind {
        SyncKind::Env => {
            let Some(key) = env_key(path) else {
                return bad("변수 경로가 아니에요");
            };
            let named = serde_json::from_str::<Value>(lost)
                .ok()
                .and_then(|v| v.get("key").and_then(Value::as_str).map(str::to_string));
            if key != title || named.as_deref() != Some(key) || key_problem(key).is_some() {
                return bad("변수 이름이 서로 달라요");
            }
        }
        SyncKind::Wiki => {
            if super::wiki::parse_path(path).is_none() {
                return bad("위키 문서 경로가 아니에요");
            }
        }
        k if super::is_shared_json(k) => {
            if !super::super::wc::safe_path(path) || super::kind_of(path) != Some(k) {
                return bad("기록 종류와 경로가 달라요");
            }
            if !serde_json::from_str::<Value>(lost).is_ok_and(|v| v.is_object()) {
                return bad("내용을 읽지 못했어요");
            }
        }
        _ => return bad("겹친 변경이 생길 수 없는 종류예요"),
    }
    Ok(())
}

/// Record a conflict this computer found (once per path and content).
pub fn record(cx: &Cx, c: &Connection, f: &Found, remote: &Author) -> rusqlite::Result<()> {
    let seen = c
        .query_row("SELECT 1 FROM sync_conflicts WHERE path = ?1 AND lost = ?2", params![f.path, f.content], |_| Ok(()))
        .optional()?
        .is_some();
    if seen {
        return Ok(());
    }
    let me = (Some(cx.device.host_id), cx.device.name.clone());
    let (lost, kept) = match f.lost {
        Side::Local => (&me, remote),
        Side::Remote => (remote, &me),
    };
    c.execute(
        "INSERT INTO sync_conflicts (id, host_id, device, kind, path, title, kept_device, kept_host, lost_host, lost_device, lost, at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            Uuid::now_v7(),
            cx.device.host_id,
            cx.device.name,
            kind_sql(f.kind),
            f.path,
            f.title,
            kept.1,
            kept.0,
            lost.0,
            lost.1,
            f.content,
            ts_to_sql(&runwick_core::now()),
        ],
    )?;
    Ok(())
}

/// Conflicts this computer found, and how it settled any conflict, as files.
pub fn render_own(cx: &Cx, c: &Connection) -> rusqlite::Result<Vec<(String, Value)>> {
    let mut st = c.prepare(
        "SELECT id, device, kind, path, title, kept_device, kept_host, lost_host, lost_device, lost, at
         FROM sync_conflicts WHERE host_id = ?1",
    )?;
    let rows = st.query_map([cx.device.host_id], |r| {
        let id: Uuid = r.get(0)?;
        Ok((
            path_of(id),
            json!({
                "id": id.to_string(),
                "host_id": cx.device.host_id.to_string(),
                "device": r.get::<_, String>(1)?,
                "kind": r.get::<_, String>(2)?,
                "path": r.get::<_, String>(3)?,
                "title": r.get::<_, String>(4)?,
                "kept_device": r.get::<_, String>(5)?,
                "kept_host": r.get::<_, Option<Uuid>>(6)?.map(|u| u.to_string()),
                "lost_host": r.get::<_, Option<Uuid>>(7)?.map(|u| u.to_string()),
                "lost_device": r.get::<_, String>(8)?,
                "lost": r.get::<_, String>(9)?,
                "at": r.get::<_, String>(10)?,
            }),
        ))
    })?;
    let mut out = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    let mut st = c.prepare("SELECT conflict_id, device, how, at FROM sync_conflict_resolutions WHERE host_id = ?1")?;
    let rows = st.query_map([cx.device.host_id], |r| {
        let id: Uuid = r.get(0)?;
        Ok((
            resolution_path(id, cx.device.host_id),
            json!({
                "conflict": id.to_string(),
                "host_id": cx.device.host_id.to_string(),
                "device": r.get::<_, String>(1)?,
                "how": r.get::<_, String>(2)?,
                "at": r.get::<_, String>(3)?,
            }),
        ))
    })?;
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// Import another computer's conflict. Returns `(kind, title, lost here)` when it is new here.
/// It may say the content not kept was this computer's only when that content is what this
/// computer had for the record at its last sync (`base`); a variable never is (nothing from
/// another computer changes one here without review).
pub fn apply(
    cx: &Cx,
    c: &Connection,
    v: &Value,
    base: impl Fn(&str) -> Option<Vec<u8>>,
) -> Result<Option<(SyncKind, String, bool)>, String> {
    let host = super::uuid_field(v, "host_id")?;
    if host == cx.device.host_id {
        return Ok(None);
    }
    let kind = kind_from(&str_field(v, "kind")?).ok_or("알 수 없는 기록 종류예요")?;
    let id = super::uuid_field(v, "id")?;
    let title = str_field(v, "title")?;
    let path = str_field(v, "path")?;
    let lost = str_field(v, "lost")?;
    check(kind, &path, &title, &lost)?;
    let lost_host = opt_uuid(v, "lost_host");
    let verified = lost_host == Some(cx.device.host_id)
        && kind != SyncKind::Env
        && base(&path).is_some_and(|had| same_content(kind, &had, &lost));
    let n = c
        .execute(
            "INSERT OR IGNORE INTO sync_conflicts (id, host_id, device, kind, path, title, kept_device, kept_host, lost_host, lost_device, lost, at, lost_verified)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                id,
                host,
                opt_str(v, "device").unwrap_or_default(),
                kind_sql(kind),
                path,
                title,
                opt_str(v, "kept_device").unwrap_or_default(),
                opt_uuid(v, "kept_host"),
                lost_host,
                opt_str(v, "lost_device").unwrap_or_default(),
                lost,
                ts_text(v, "at")?,
                verified,
            ],
        )
        .map_err(|e| e.to_string())?;
    Ok((n > 0).then_some((kind, title, verified)))
}

/// A file's content and a conflict's recorded content are the same record.
fn same_content(kind: SyncKind, file: &[u8], lost: &str) -> bool {
    if kind == SyncKind::Wiki {
        return file == lost.as_bytes();
    }
    match (super::super::format::parse(file), serde_json::from_str::<Value>(lost).ok()) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

/// Import how another computer settled a conflict. Returns true when it is new here.
pub fn apply_resolution(c: &Connection, v: &Value) -> Result<bool, String> {
    let how = str_field(v, "how")?;
    if !matches!(how.as_str(), "restored" | "dismissed") {
        return Err("알 수 없는 정리 방식이에요".into());
    }
    let n = c
        .execute(
            "INSERT OR IGNORE INTO sync_conflict_resolutions (conflict_id, host_id, device, how, at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                super::uuid_field(v, "conflict")?,
                super::uuid_field(v, "host_id")?,
                opt_str(v, "device").unwrap_or_default(),
                how,
                ts_text(v, "at")?,
            ],
        )
        .map_err(|e| e.to_string())?;
    Ok(n > 0)
}

pub struct Stored {
    pub kind: SyncKind,
    pub path: String,
    pub title: String,
    pub lost: String,
    pub kept: Author,
    pub lost_device: String,
    /// The computer that settled it already, when one did.
    pub settled_by: Option<String>,
}

pub fn get(c: &Connection, id: Uuid) -> rusqlite::Result<Option<Stored>> {
    c.query_row(
        "SELECT kind, path, title, lost, kept_host, kept_device, lost_device,
                (SELECT device FROM sync_conflict_resolutions r WHERE r.conflict_id = sync_conflicts.id LIMIT 1)
         FROM sync_conflicts WHERE id = ?1",
        [id],
        |r| {
            let Some(kind) = kind_from(&r.get::<_, String>(0)?) else {
                return Ok(None);
            };
            Ok(Some(Stored {
                kind,
                path: r.get(1)?,
                title: r.get(2)?,
                lost: r.get(3)?,
                kept: (r.get(4)?, r.get(5)?),
                lost_device: r.get(6)?,
                settled_by: r.get(7)?,
            }))
        },
    )
    .optional()
    .map(Option::flatten)
}

/// Settle a conflict on this computer (`restored` or `dismissed`); every computer stops showing it.
pub fn settle(cx: &Cx, c: &Connection, id: Uuid, how: &str) -> rusqlite::Result<()> {
    c.execute(
        "INSERT OR REPLACE INTO sync_conflict_resolutions (conflict_id, host_id, device, how, at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![id, cx.device.host_id, cx.device.name, how, ts_to_sql(&runwick_core::now())],
    )?;
    c.execute("UPDATE sync_conflicts SET dismissed = 1 WHERE id = ?1", [id]).map(|_| ())
}

fn item(r: &Row, names: &Names, me: Uuid) -> rusqlite::Result<Option<SyncConflictItem>> {
    let Some(kind) = kind_from(&r.get::<_, String>("kind")?) else {
        return Ok(None);
    };
    let path: String = r.get("path")?;
    let lost: String = r.get("lost")?;
    let finder: Uuid = r.get("host_id")?;
    let lost_host: Option<Uuid> = r.get("lost_host")?;
    let verified: bool = r.get("lost_verified")?;
    let kept_host: Option<Uuid> = r.get("kept_host")?;
    // Older records of this computer had no kept host: the side kept was this computer's
    // when the side lost was not.
    let kept_host = kept_host.or_else(|| (finder == me && lost_host != Some(me)).then_some(me));
    // Only this computer's own records, or another computer's record checked against what this
    // computer had, may say the content was this computer's: a record from the repository
    // could claim anything.
    let lost_here = lost_host == Some(me) && (finder == me || verified);
    let lost_device: String = r.get("lost_device")?;
    let lost_device = if lost_host == Some(me) && !lost_here {
        names.label(None, &lost_device)
    } else {
        names.label(lost_host, &lost_device)
    };
    let (project_id, slug) = match super::wiki::parse_path(&path) {
        Some((p, s)) if kind == SyncKind::Wiki => (Some(p), Some(s)),
        _ => (super::uuid_at(&path, 1).filter(|_| kind == SyncKind::Timeline), None),
    };
    Ok(Some(SyncConflictItem {
        id: r.get("id")?,
        kind,
        title: r.get("title")?,
        at: crate::store::ts_from_sql(&r.get::<_, String>("at")?),
        found_on: names.label(Some(finder), &r.get::<_, String>("device")?),
        kept_device: names.label(kept_host, &r.get::<_, String>("kept_device")?),
        lost_device,
        lost_here,
        found_here: finder == me,
        lost_text: describe(kind, &lost),
        can_restore: kind != SyncKind::Prefs,
        project_id,
        slug,
    }))
}

/// Conflicts nobody settled, newest first, with computers named for the person (see `Names`).
pub fn list(c: &Connection, me: &Device) -> rusqlite::Result<Vec<SyncConflictItem>> {
    let names = Names::load(c, me)?;
    let mut st = c.prepare(
        "SELECT * FROM sync_conflicts s WHERE dismissed = 0
         AND NOT EXISTS (SELECT 1 FROM sync_conflict_resolutions r WHERE r.conflict_id = s.id)
         ORDER BY at DESC LIMIT 200",
    )?;
    let rows = st.query_map([], |r| item(r, &names, me.host_id))?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?.into_iter().flatten().collect())
}

/// Put back the content a conflict did not keep. A variable goes to review (never applied
/// here directly); other content becomes a new edit, and what it replaces is recorded as a
/// conflict of its own so it can be put back in turn. Returns the kind that changed.
pub fn restore(cx: &Cx, id: Uuid) -> Result<SyncKind, RpcError> {
    let stored = cx
        .store
        .with(|c| get(c, id))
        .map_err(db_err)?
        .ok_or_else(|| RpcError::not_found("겹친 변경을 찾을 수 없어요"))?;
    if let Some(by) = &stored.settled_by {
        return Err(RpcError::invalid(format!("{by}에서 이미 정리한 겹친 변경이에요")));
    }
    check(stored.kind, &stored.path, &stored.title, &stored.lost).map_err(RpcError::invalid)?;
    let parse =
        || serde_json::from_str::<Value>(&stored.lost).map_err(|_| RpcError::invalid("되돌릴 내용을 읽지 못했어요"));
    let replaced = |content: String| Found {
        kind: stored.kind,
        path: stored.path.clone(),
        title: stored.title.clone(),
        lost: Side::Remote,
        content,
    };
    match stored.kind {
        SyncKind::Env => {
            let v = parse()?;
            if v.get("secret").and_then(Value::as_bool).unwrap_or(false) {
                return Err(RpcError::invalid("비밀값은 되돌릴 수 없어요. 직접 입력해 주세요"));
            }
            let text = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
            super::env::stage_restored(cx, &stored.title, &text("value"), &text("description"), &stored.lost_device)?;
        }
        SyncKind::Wiki => {
            let now = super::wiki::current(cx, &stored.path)?;
            super::wiki::restore(cx, &stored.path, &stored.lost)?;
            if let Some(text) = now.filter(|t| *t != stored.lost) {
                cx.store.tx(|c| record(cx, c, &replaced(text), &stored.kept)).map_err(db_err)?;
            }
        }
        kind if super::is_shared_json(kind) && kind != SyncKind::Prefs => {
            let mut v = parse()?;
            // A new edit: it must win over what is there now.
            let now = ts_to_sql(&runwick_core::now());
            if kind == SyncKind::Projects {
                super::projects::stamp_restored(&mut v, &now);
            }
            if let Some(o) = v.as_object_mut() {
                for k in ["updated_at", "generated_at"] {
                    if o.contains_key(k) {
                        o.insert(k.into(), Value::String(now.clone()));
                    }
                }
            }
            cx.store
                .tx(|c| {
                    let current = super::render_shared(cx, c)?.remove(&stored.path);
                    if let Err(e) = super::apply_shared(cx, c, kind, &stored.path, Some(&v)) {
                        return Ok(Err(e));
                    }
                    if let Some(cur) = current.filter(|cur| !same_record(cur, &v)) {
                        let text = super::super::format::canonical(&cur);
                        record(cx, c, &replaced(text), &stored.kept)?;
                    }
                    Ok(Ok(()))
                })
                .map_err(db_err)?
                .map_err(RpcError::invalid)?;
        }
        _ => return Err(RpcError::invalid("이 기록은 되돌릴 수 없어요")),
    }
    cx.store.with(|c| settle(cx, c, id, "restored")).map_err(db_err)?;
    Ok(stored.kind)
}

/// The same record apart from its change times.
fn same_record(a: &Value, b: &Value) -> bool {
    let strip = |v: &Value| {
        let mut v = v.clone();
        if let Some(o) = v.as_object_mut() {
            o.remove("updated_at");
            o.remove("generated_at");
        }
        v
    };
    strip(a) == strip(b)
}

/// The content that was not kept, readable.
pub fn describe(kind: SyncKind, lost: &str) -> String {
    let Some(v) = serde_json::from_str::<Value>(lost).ok().filter(Value::is_object) else {
        return lost.to_string();
    };
    let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let lines: Vec<String> = match kind {
        SyncKind::Pins => vec![format!("제목: {}", s("title")), format!("메모: {}", s("note")), s("text")],
        SyncKind::Roles => {
            let steps: Vec<String> = v
                .get("procedure")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
                .unwrap_or_default();
            vec![format!("이름: {}", s("name")), s("instructions"), format!("절차: {}", steps.join(" → "))]
        }
        SyncKind::Timeline if v.get("memo").is_some() => {
            let items: Vec<String> = v
                .get("items")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter().filter_map(|i| i.get("text").and_then(Value::as_str)).map(|t| format!("- {t}")).collect()
                })
                .unwrap_or_default();
            std::iter::once(s("memo")).chain(items).collect()
        }
        SyncKind::Timeline => vec![s("text")],
        SyncKind::Env => vec![format!("{} = {}", s("key"), s("value")), s("description")],
        SyncKind::Projects => vec![format!("이름: {}", s("name"))],
        _ => vec![super::super::format::canonical(&v)],
    };
    lines.into_iter().filter(|l| !l.trim().is_empty()).collect::<Vec<_>>().join("\n")
}
