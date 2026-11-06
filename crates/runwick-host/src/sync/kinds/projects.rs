//! `projects/<id>.json`: name, favorite, archived, origin URL and where the project's folder is
//! on each computer (`roots`, keyed by host id). Each computer writes only its own root.
//!
//! A project that arrives without a folder on this computer gets a placeholder folder under
//! `RUNWICK_HOME/unlinked/` (which never exists), so its records stay readable until the person
//! links a folder.

use super::{Cx, bool_field, opt_str, str_field, ts_text};
use crate::projects::DEFAULT_WORKSPACE_NAME;
use crate::store::ts_to_sql;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use uuid::Uuid;

pub const DIR: &str = "projects";

pub fn path_of(id: Uuid) -> String {
    format!("{DIR}/{id}.json")
}

/// Placeholder folder of a project that has no folder on this computer.
pub fn unlinked_root(home: &Path, id: Uuid) -> PathBuf {
    home.join("unlinked").join(id.to_string())
}

pub fn is_unlinked(home: &Path, root: &str) -> bool {
    Path::new(root).starts_with(home.join("unlinked"))
}

/// Projects that live on this computer's records (not view-only links to another device).
pub const SYNCED: &str = "EXISTS (SELECT 1 FROM workspaces w WHERE w.project_id = p.id AND w.remote_device_id IS NULL)";

pub fn render(cx: &Cx, c: &Connection, out: &mut BTreeMap<String, Value>) -> rusqlite::Result<()> {
    let mut others: BTreeMap<Uuid, Map<String, Value>> = BTreeMap::new();
    {
        let mut st = c.prepare("SELECT project_id, host_id, device, path FROM sync_roots")?;
        let rows = st.query_map([], |r| {
            Ok((r.get::<_, Uuid>(0)?, r.get::<_, Uuid>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?))
        })?;
        for row in rows {
            let (pid, host, device, path) = row?;
            others.entry(pid).or_default().insert(host.to_string(), json!({ "device": device, "path": path }));
        }
    }
    let mut st = c.prepare(&format!(
        "SELECT id, name, root, favorite, archived, created_at, remote_url FROM projects p WHERE {SYNCED}"
    ))?;
    let rows = st.query_map([], |r| {
        Ok((
            r.get::<_, Uuid>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, bool>(3)?,
            r.get::<_, bool>(4)?,
            r.get::<_, String>(5)?,
            r.get::<_, Option<String>>(6)?,
        ))
    })?;
    for row in rows {
        let (id, name, root, favorite, archived, created_at, remote_url) = row?;
        let mut roots = others.remove(&id).unwrap_or_default();
        if !is_unlinked(cx.home, &root) {
            roots.insert(cx.device.host_id.to_string(), json!({ "device": cx.device.name, "path": root }));
        }
        out.insert(
            path_of(id),
            json!({
                "id": id.to_string(),
                "name": name,
                "favorite": favorite,
                "archived": archived,
                "created_at": created_at,
                "remote_url": remote_url.as_deref().map(runwick_core::api::sync::without_credentials),
                "roots": Value::Object(roots),
            }),
        );
    }
    Ok(())
}

pub fn apply(cx: &Cx, c: &Connection, path: &str, merged: Option<&Value>) -> Result<(), String> {
    // Projects are never deleted (only archived); a missing file changes nothing here.
    let Some(v) = merged else { return Ok(()) };
    let id = super::uuid_at(path, 1).ok_or("파일 이름이 프로젝트 id가 아니에요")?;
    if super::uuid_field(v, "id")? != id {
        return Err("파일 이름과 id가 달라요".into());
    }
    let name = str_field(v, "name")?.trim().to_string();
    if name.is_empty() {
        return Err("프로젝트 이름이 비어 있어요".into());
    }
    let created_at = ts_text(v, "created_at")?;
    let remote_url = opt_str(v, "remote_url").map(|u| runwick_core::api::sync::without_credentials(&u));
    let (favorite, archived) = (bool_field(v, "favorite"), bool_field(v, "archived"));
    let roots = v.get("roots").and_then(Value::as_object).cloned().unwrap_or_default();
    let db = |e: rusqlite::Error| e.to_string();

    let exists = super::project_exists(c, id).map_err(db)?;
    if exists {
        c.execute(
            "UPDATE projects SET name = ?2, favorite = ?3, archived = ?4, remote_url = COALESCE(?5, remote_url) WHERE id = ?1",
            params![id, name, favorite, archived, remote_url],
        )
        .map_err(db)?;
    } else {
        let root = own_root(c, &roots, cx).map_err(db)?.unwrap_or_else(|| unlinked_root(cx.home, id));
        let root_s = root.to_string_lossy().to_string();
        let is_git = root.is_dir() && crate::git::find_repo_root(&root).is_some();
        let now = ts_to_sql(&runwick_core::now());
        c.execute(
            "INSERT INTO projects (id, name, root, is_git, favorite, archived, created_at, remote_url)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![id, name, root_s, is_git, favorite, archived, created_at, remote_url],
        )
        .map_err(db)?;
        c.execute(
            "INSERT INTO workspaces (id, project_id, name, path, kind, created_at)
             VALUES (?1, ?2, ?3, ?4, 'default', ?5)",
            params![Uuid::now_v7(), id, DEFAULT_WORKSPACE_NAME, root_s, now],
        )
        .map_err(db)?;
    }
    c.execute("DELETE FROM sync_roots WHERE project_id = ?1", [id]).map_err(db)?;
    for (host, entry) in &roots {
        let Ok(host) = host.parse::<Uuid>() else {
            continue;
        };
        if host == cx.device.host_id {
            continue;
        }
        let (Some(device), Some(p)) = (opt_str(entry, "device"), opt_str(entry, "path")) else {
            continue;
        };
        c.execute(
            "INSERT INTO sync_roots (project_id, host_id, device, path) VALUES (?1, ?2, ?3, ?4)",
            params![id, host, device, p],
        )
        .map_err(db)?;
    }
    Ok(())
}

/// This computer's own folder recorded in the data folder (for example after reinstalling),
/// when it still exists and no other project uses it.
fn own_root(c: &Connection, roots: &Map<String, Value>, cx: &Cx) -> rusqlite::Result<Option<PathBuf>> {
    let Some(p) = roots.get(&cx.device.host_id.to_string()).and_then(|e| opt_str(e, "path")) else {
        return Ok(None);
    };
    let path = PathBuf::from(&p);
    if !path.is_dir() {
        return Ok(None);
    }
    let taken = c.query_row("SELECT 1 FROM projects WHERE root = ?1", [&p], |_| Ok(())).optional()?.is_some();
    Ok((!taken).then_some(path))
}

/// Projects without a folder here, with where they live on other computers.
pub fn unlinked(
    home: &Path,
    c: &Connection,
    me: &super::Device,
) -> rusqlite::Result<Vec<runwick_core::api::sync::UnlinkedProject>> {
    let names = super::Names::load(c, me)?;
    let mut st = c.prepare("SELECT id, name, root, remote_url FROM projects WHERE archived = 0 ORDER BY name")?;
    let rows: Vec<(Uuid, String, String, Option<String>)> =
        st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?.collect::<Result<_, _>>()?;
    let mut out = Vec::new();
    for (id, name, root, remote_url) in rows {
        if !is_unlinked(home, &root) {
            continue;
        }
        let mut st = c.prepare("SELECT host_id, device, path FROM sync_roots WHERE project_id = ?1 ORDER BY device")?;
        let elsewhere = st
            .query_map([id], |r| {
                Ok(format!("{} · {}", names.label(Some(r.get(0)?), &r.get::<_, String>(1)?), r.get::<_, String>(2)?))
            })?
            .collect::<Result<_, _>>()?;
        out.push(runwick_core::api::sync::UnlinkedProject { id, name, remote_url, elsewhere });
    }
    Ok(out)
}

/// An unlinked project whose origin URL matches `remote_url` (for linking a folder the person
/// opened). Only one clear match counts.
pub fn match_unlinked(home: &Path, c: &Connection, remote_url: &str) -> rusqlite::Result<Option<Uuid>> {
    let want = runwick_core::api::sync::normalize_remote(remote_url);
    let mut st = c.prepare("SELECT id, root, remote_url FROM projects WHERE remote_url IS NOT NULL")?;
    let rows: Vec<(Uuid, String, String)> =
        st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<Result<_, _>>()?;
    let hits: Vec<Uuid> = rows
        .into_iter()
        .filter(|(_, root, url)| is_unlinked(home, root) && runwick_core::api::sync::normalize_remote(url) == want)
        .map(|(id, _, _)| id)
        .collect();
    Ok(if hits.len() == 1 { Some(hits[0]) } else { None })
}

/// Link a folder the person opened to the one unlinked synced project with the same origin
/// URL. Returns the project and its default workspace, or None when there is no single match.
pub fn link_folder(
    store: &crate::store::Store,
    home: &Path,
    dir: &str,
    is_git: bool,
    remote_url: &str,
) -> rusqlite::Result<Option<(Uuid, Uuid)>> {
    store.tx(|c| {
        let Some(pid) = match_unlinked(home, c, remote_url)? else { return Ok(None) };
        let old: String = c.query_row("SELECT root FROM projects WHERE id = ?1", [pid], |r| r.get(0))?;
        let now = ts_to_sql(&runwick_core::now());
        c.execute(
            "UPDATE projects SET root = ?2, is_git = ?3, archived = 0, last_opened_at = ?4 WHERE id = ?1",
            params![pid, dir, is_git, now],
        )?;
        c.execute(
            "UPDATE workspaces SET path = ?3, last_used_at = ?4 WHERE project_id = ?1 AND path = ?2 AND remote_device_id IS NULL",
            params![pid, old, dir, now],
        )?;
        let ws = super::sessions::default_workspace(c, pid)?;
        tracing::info!(project = %pid, "linked an opened folder to a project synced from another computer");
        Ok(Some((pid, ws)))
    })
}
