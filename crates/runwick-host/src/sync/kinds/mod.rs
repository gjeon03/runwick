//! One module per kind of record: how it is written into the data folder and read back.
//!
//! - Shared kinds (projects, pins, timeline, roles, settings) are JSON records any computer may
//!   change; the engine merges them three ways and hands the result to `apply`.
//! - Origin-owned kinds (sessions, messages, finished delegation records) are written only by
//!   the computer that made them; other computers import them as read-only records.
//! - Wiki pages are Markdown with their own conflict rule (see `wiki`).

pub mod collab;
pub mod conflicts;
pub mod env;
pub mod messages;
pub mod pins;
pub mod prefs;
pub mod projects;
pub mod roles;
pub mod sessions;
pub mod timeline;
pub mod wiki;

use crate::env::EnvService;
use crate::store::Store;
use runwick_core::api::sync::{KindCount, SyncConflict, SyncKind};
use rusqlite::Connection;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use uuid::Uuid;

/// This computer as seen in the data folder.
#[derive(Debug, Clone)]
pub struct Device {
    pub host_id: Uuid,
    pub name: String,
}

/// Everything an exporter or importer needs. Paths are explicit so two data folders can sync in
/// one process (tests).
pub struct Cx<'a> {
    pub store: &'a Store,
    /// `RUNWICK_HOME` (placeholder folders of projects that are not linked here live under it).
    pub home: &'a Path,
    pub wiki_root: &'a Path,
    pub env: &'a EnvService,
    pub device: &'a Device,
}

/// What a sync did, collected while it runs.
#[derive(Default, Debug)]
pub struct Tally {
    pub imported: BTreeMap<SyncKind, u64>,
    pub exported: BTreeMap<SyncKind, u64>,
    pub conflicts: Vec<SyncConflict>,
    pub skipped: Vec<String>,
    /// Repository files left untouched because they could not be read, with the reason; they
    /// are read again by later syncs.
    pub skip_paths: BTreeMap<String, String>,
    /// Content that was not kept, recorded as conflicts once the import is done.
    pub found: Vec<conflicts::Found>,
    pub topics: BTreeSet<&'static str>,
}

impl Tally {
    pub fn imported(&mut self, kind: SyncKind, topics: &[&'static str]) {
        *self.imported.entry(kind).or_default() += 1;
        self.topics.extend(topics.iter().copied());
    }

    pub fn exported(&mut self, kind: SyncKind) {
        *self.exported.entry(kind).or_default() += 1;
    }

    pub fn skip(&mut self, path: &str, why: impl Into<String>) {
        let why = why.into();
        self.skipped.push(format!("{path}: {why}"));
        self.skip_paths.insert(path.to_string(), why);
    }

    pub fn conflict(&mut self, kind: SyncKind, title: impl Into<String>, resolution: impl Into<String>) {
        self.conflicts.push(SyncConflict { kind, title: title.into(), resolution: resolution.into() });
    }

    pub fn counts(map: &BTreeMap<SyncKind, u64>) -> Vec<KindCount> {
        map.iter().filter(|(_, n)| **n > 0).map(|(k, n)| KindCount { kind: *k, count: *n }).collect()
    }
}

/// Kind of a data-folder path (None for the manifest, README, tombstones and unknown files).
/// Computers as the person reads them: "이 컴퓨터" for this one, and another computer by its
/// name, with a short part of its id when this computer or another known one has the same name
/// (two Macs are often both called after their owner).
pub struct Names {
    me: Uuid,
    my_name: String,
    hosts: std::collections::HashMap<String, std::collections::BTreeSet<Uuid>>,
}

impl Names {
    pub fn load(c: &Connection, me: &Device) -> rusqlite::Result<Names> {
        let mut hosts: std::collections::HashMap<String, std::collections::BTreeSet<Uuid>> =
            std::collections::HashMap::new();
        hosts.entry(me.name.clone()).or_default().insert(me.host_id);
        let mut st = c.prepare(
            "SELECT host_id, device FROM sync_conflicts
             UNION SELECT kept_host, kept_device FROM sync_conflicts WHERE kept_host IS NOT NULL
             UNION SELECT lost_host, lost_device FROM sync_conflicts WHERE lost_host IS NOT NULL
             UNION SELECT host_id, device FROM sync_conflict_resolutions
             UNION SELECT host_id, device FROM sync_roots
             UNION SELECT host, device FROM sync_env_staged WHERE host IS NOT NULL
             UNION SELECT origin_host, origin_device FROM sessions
                 WHERE origin_host IS NOT NULL AND origin_device IS NOT NULL",
        )?;
        let rows = st.query_map([], |r| Ok((r.get::<_, Uuid>(0)?, r.get::<_, String>(1)?)))?;
        for row in rows {
            let (id, name) = row?;
            hosts.entry(name).or_default().insert(id);
        }
        Ok(Names { me: me.host_id, my_name: me.name.clone(), hosts })
    }

    /// `host` wrote as `name`; `None` when it is not known which computer that was.
    pub fn label(&self, host: Option<Uuid>, name: &str) -> String {
        let name = if name.trim().is_empty() { "다른 컴퓨터" } else { name };
        match host {
            Some(h) if h == self.me => "이 컴퓨터".into(),
            Some(h) => {
                let shared = name == self.my_name || self.hosts.get(name).is_some_and(|ids| ids.len() > 1);
                if shared { told_apart(name, h) } else { name.to_string() }
            }
            None if name == self.my_name => format!("{name} (어느 컴퓨터인지 확인 안 됨)"),
            None => name.to_string(),
        }
    }
}

/// Another computer's name with the end of its id ("gyeongyeon_jeon (다른 컴퓨터 · 3f2a)").
pub fn told_apart(name: &str, host: Uuid) -> String {
    let id = host.simple().to_string();
    format!("{name} (다른 컴퓨터 · {})", &id[id.len() - 4..])
}

/// Let requests waiting for the store in between two transactions of a long import (the
/// lock is not fair: without a pause the importer takes it back before they can).
pub fn breathe() {
    std::thread::sleep(std::time::Duration::from_millis(2));
}

pub fn kind_of(path: &str) -> Option<SyncKind> {
    let first = path.split('/').next()?;
    Some(match first {
        "projects" => SyncKind::Projects,
        "sessions" => SyncKind::Sessions,
        "messages" => SyncKind::Messages,
        "pins" => SyncKind::Pins,
        "wiki" => SyncKind::Wiki,
        "timeline" => SyncKind::Timeline,
        "roles" => SyncKind::Roles,
        "collab" => SyncKind::Collab,
        "settings" if path == env::PATH => SyncKind::Env,
        "settings" if path == prefs::PATH => SyncKind::Prefs,
        _ => return None,
    })
}

/// Shared JSON kinds, merged by the engine and applied inside the import transaction.
pub fn is_shared_json(kind: SyncKind) -> bool {
    matches!(kind, SyncKind::Projects | SyncKind::Pins | SyncKind::Timeline | SyncKind::Roles | SyncKind::Prefs)
}

/// Kinds whose records can be deleted here; their deletions travel as tombstones.
pub fn deletable(kind: SyncKind) -> bool {
    matches!(kind, SyncKind::Pins | SyncKind::Roles | SyncKind::Wiki)
}

/// Every shared JSON record of this computer, by path (inside one read).
pub fn render_shared(cx: &Cx, c: &Connection) -> rusqlite::Result<BTreeMap<String, Value>> {
    let mut out = BTreeMap::new();
    projects::render(cx, c, &mut out)?;
    pins::render(c, &mut out)?;
    timeline::render(c, &mut out)?;
    roles::render(c, &mut out)?;
    prefs::render(c, &mut out)?;
    Ok(out)
}

/// Write one merged shared record (None deletes it). Errors are about the record's data.
pub fn apply_shared(cx: &Cx, c: &Connection, kind: SyncKind, path: &str, merged: Option<&Value>) -> Result<(), String> {
    match kind {
        SyncKind::Projects => projects::apply(cx, c, path, merged),
        SyncKind::Pins => pins::apply(c, path, merged),
        SyncKind::Timeline => timeline::apply(c, path, merged),
        SyncKind::Roles => roles::apply(c, path, merged),
        SyncKind::Prefs => prefs::apply(c, merged),
        _ => Err("공유 기록이 아니에요".into()),
    }
}

/// Short human title of a shared record, for conflict notes.
pub fn title_of(kind: SyncKind, v: &Value) -> String {
    let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
    match kind {
        SyncKind::Projects | SyncKind::Roles => s("name"),
        SyncKind::Pins => s("title"),
        SyncKind::Timeline => s("day").map(|d| format!("{d} 요약")).or_else(|| Some("프로젝트 메모".into())),
        SyncKind::Prefs => Some("일반 설정".into()),
        _ => None,
    }
    .unwrap_or_else(|| kind.label().to_string())
}

// Helpers shared by the kinds.

pub fn uuid_at(path: &str, index: usize) -> Option<Uuid> {
    let seg = path.split('/').nth(index)?;
    seg.strip_suffix(".json").or(seg.strip_suffix(".jsonl")).unwrap_or(seg).parse().ok()
}

pub fn str_field(v: &Value, k: &str) -> Result<String, String> {
    v.get(k).and_then(Value::as_str).map(str::to_string).ok_or_else(|| format!("{k} 값이 없어요"))
}

pub fn opt_str(v: &Value, k: &str) -> Option<String> {
    v.get(k).and_then(Value::as_str).map(str::to_string)
}

pub fn bool_field(v: &Value, k: &str) -> bool {
    v.get(k).and_then(Value::as_bool).unwrap_or(false)
}

pub fn uuid_field(v: &Value, k: &str) -> Result<Uuid, String> {
    str_field(v, k)?.parse().map_err(|_| format!("{k} 값이 올바른 id가 아니에요"))
}

pub fn opt_uuid(v: &Value, k: &str) -> Option<Uuid> {
    opt_str(v, k).and_then(|s| s.parse().ok())
}

/// A timestamp field kept exactly as written (it must parse as RFC 3339).
pub fn ts_text(v: &Value, k: &str) -> Result<String, String> {
    let s = str_field(v, k)?;
    chrono::DateTime::parse_from_rfc3339(&s).map_err(|_| format!("{k} 시각 형식이 아니에요"))?;
    Ok(s)
}

pub fn opt_ts_text(v: &Value, k: &str) -> Option<String> {
    opt_str(v, k).filter(|s| chrono::DateTime::parse_from_rfc3339(s).is_ok())
}

pub fn project_exists(c: &Connection, id: Uuid) -> rusqlite::Result<bool> {
    use rusqlite::OptionalExtension;
    c.query_row("SELECT 1 FROM projects WHERE id = ?1", [id], |_| Ok(())).optional().map(|o| o.is_some())
}
