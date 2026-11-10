//! Projects and workspaces.

mod organization;
pub use organization::*;

use crate::git;
use crate::host::Ctx;
use crate::router::Router;
use crate::store::{Migration, Store, db_err, opt_ts, ts_from_sql, ts_to_sql};
use runwick_core::api::projects::*;
use runwick_core::rpc::{RpcError, topic};
use rusqlite::{OptionalExtension, Row, params};
use std::path::{Path, PathBuf};
use uuid::Uuid;

pub const MIGRATIONS: &[Migration] = &[
    (
        "0002_projects",
        "CREATE TABLE projects (
        id BLOB PRIMARY KEY,
        name TEXT NOT NULL,
        root TEXT NOT NULL,
        is_git INTEGER NOT NULL DEFAULT 0,
        favorite INTEGER NOT NULL DEFAULT 0,
        archived INTEGER NOT NULL DEFAULT 0,
        created_at TEXT NOT NULL,
        last_opened_at TEXT,
        remote_url TEXT
    );
    CREATE UNIQUE INDEX projects_root ON projects(root);
    CREATE TABLE workspaces (
        id BLOB PRIMARY KEY,
        project_id BLOB NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
        name TEXT NOT NULL,
        path TEXT NOT NULL,
        kind TEXT NOT NULL,
        base_ref TEXT,
        archived INTEGER NOT NULL DEFAULT 0,
        created_at TEXT NOT NULL,
        last_used_at TEXT,
        remote_device_id BLOB,
        remote_workspace_id BLOB
    );
    CREATE INDEX workspaces_project ON workspaces(project_id);",
    ),
    (
        "0021_project_organization",
        "CREATE TABLE project_groups (
        id BLOB PRIMARY KEY,
        name TEXT NOT NULL,
        order_index INTEGER NOT NULL,
        collapsed INTEGER NOT NULL DEFAULT 0,
        deleted INTEGER NOT NULL DEFAULT 0,
        updated_at TEXT NOT NULL
    );
    ALTER TABLE projects ADD COLUMN order_index INTEGER NOT NULL DEFAULT 0;
    ALTER TABLE projects ADD COLUMN group_id BLOB REFERENCES project_groups(id) ON DELETE SET NULL;
    ALTER TABLE projects ADD COLUMN placement_updated_at TEXT NOT NULL DEFAULT '1970-01-01T00:00:00.000Z';
    WITH ranked AS (
        SELECT id, ROW_NUMBER() OVER (ORDER BY COALESCE(last_opened_at, created_at) DESC, name, id) - 1 AS position
        FROM projects
    )
    UPDATE projects SET order_index = (SELECT position FROM ranked WHERE ranked.id = projects.id);
    CREATE INDEX projects_group_order ON projects(group_id, order_index);",
    ),
];

pub const DEFAULT_WORKSPACE_NAME: &str = "기본 공간";

pub fn register(r: &mut Router) {
    organization::register(r);
    r.add::<ProjectsList, _, _>(|ctx: Ctx, _: ()| async move { list_projects(&ctx.host.store).map_err(db_err) });
    r.add::<ProjectsOpen, _, _>(|ctx: Ctx, p: OpenFolder| async move {
        let out = open_folder(&ctx.host.store, &p.path)?;
        ctx.host.bus.changed(&[topic::PROJECTS, topic::WORKSPACES]);
        Ok(out)
    });
    r.add::<ProjectsUpdate, _, _>(|ctx: Ctx, u: ProjectUpdate| async move {
        let p = update_project(&ctx.host.store, &u)?;
        ctx.host.bus.changed(&[topic::PROJECTS]);
        Ok(p)
    });
    r.add::<WorkspacesList, _, _>(|ctx: Ctx, q: WorkspaceQuery| async move {
        list_workspaces(&ctx.host.store, &q).map_err(db_err)
    });
    r.add::<WorkspacesUpdate, _, _>(|ctx: Ctx, u: WorkspaceUpdate| async move {
        let w = update_workspace(&ctx.host.store, &u)?;
        ctx.host.bus.changed(if u.touch { &[topic::WORKSPACES, topic::PROJECTS] } else { &[topic::WORKSPACES] });
        Ok(w)
    });
}

fn project_from_row(r: &Row) -> rusqlite::Result<Project> {
    let root: String = r.get("root")?;
    Ok(Project {
        id: r.get("id")?,
        name: r.get("name")?,
        root_exists: Path::new(&root).is_dir(),
        root,
        is_git: r.get("is_git")?,
        favorite: r.get("favorite")?,
        archived: r.get("archived")?,
        created_at: ts_from_sql(&r.get::<_, String>("created_at")?),
        last_opened_at: opt_ts(r.get("last_opened_at")?),
        remote_url: r.get("remote_url")?,
        order_index: r.get("order_index")?,
        group_id: r.get("group_id")?,
        remote_only: r.get("remote_only")?,
    })
}

fn workspace_from_row(r: &Row) -> rusqlite::Result<Workspace> {
    let path: String = r.get("path")?;
    let kind: String = r.get("kind")?;
    let remote_device: Option<Uuid> = r.get("remote_device_id")?;
    let remote_ws: Option<Uuid> = r.get("remote_workspace_id")?;
    let is_remote = remote_device.is_some();
    Ok(Workspace {
        id: r.get("id")?,
        project_id: r.get("project_id")?,
        name: r.get("name")?,
        branch: if is_remote { None } else { git::read_branch(Path::new(&path)) },
        path_exists: !is_remote && Path::new(&path).is_dir(),
        path,
        kind: if kind == "worktree" { WorkspaceKind::Worktree } else { WorkspaceKind::Default },
        base_ref: r.get("base_ref")?,
        archived: r.get("archived")?,
        created_at: ts_from_sql(&r.get::<_, String>("created_at")?),
        last_used_at: opt_ts(r.get("last_used_at")?),
        remote: match (remote_device, remote_ws) {
            (Some(device_id), Some(remote_workspace_id)) => Some(RemoteLink { device_id, remote_workspace_id }),
            _ => None,
        },
    })
}

pub fn list_projects(store: &Store) -> rusqlite::Result<Vec<Project>> {
    store.with(|c| {
        let mut st = c.prepare(&format!(
            "{PROJECT_SELECT} ORDER BY remote_only,
             CASE WHEN NOT ({LOCAL_PROJECT}) THEN COALESCE(last_opened_at, created_at) END DESC,
             CASE WHEN NOT ({LOCAL_PROJECT}) THEN name END,
             order_index, created_at DESC, id"
        ))?;
        let rows = st.query_map([], project_from_row)?;
        rows.collect()
    })
}

pub fn get_project(store: &Store, id: Uuid) -> Result<Project, RpcError> {
    store
        .with(|c| c.query_row(&format!("{PROJECT_SELECT} WHERE p.id = ?1"), [id], project_from_row).optional())
        .map_err(db_err)?
        .ok_or_else(|| RpcError::not_found("프로젝트를 찾을 수 없어요"))
}

pub fn get_workspace(store: &Store, id: Uuid) -> Result<Workspace, RpcError> {
    store
        .with(|c| c.query_row("SELECT * FROM workspaces WHERE id = ?1", [id], workspace_from_row).optional())
        .map_err(db_err)?
        .ok_or_else(|| RpcError::not_found("작업 공간을 찾을 수 없어요"))
}

pub fn list_workspaces(store: &Store, q: &WorkspaceQuery) -> rusqlite::Result<Vec<Workspace>> {
    store.with(|c| {
        let mut st = c.prepare(
            "SELECT * FROM workspaces
             WHERE (?1 IS NULL OR project_id = ?1) AND (?2 OR archived = 0)
             ORDER BY CASE kind WHEN 'default' THEN 0 ELSE 1 END, created_at",
        )?;
        let rows = st.query_map(params![q.project_id, q.include_archived], workspace_from_row)?;
        rows.collect()
    })
}

fn canonical_dir(path: &str) -> Result<PathBuf, RpcError> {
    let expanded = if let Some(rest) = path.strip_prefix("~/") {
        runwick_core::paths::user_home().join(rest)
    } else {
        PathBuf::from(path)
    };
    let p = expanded.canonicalize().map_err(|_| RpcError::invalid(format!("폴더를 찾을 수 없어요: {path}")))?;
    if !p.is_dir() {
        return Err(RpcError::invalid(format!("폴더가 아니에요: {path}")));
    }
    std::fs::read_dir(&p).map_err(|_| RpcError::invalid(format!("폴더를 읽을 권한이 없어요: {path}")))?;
    Ok(p)
}

fn folder_name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| p.display().to_string())
}

/// Open a folder: reuse the project if this folder (or the repository it is a worktree of)
/// is known, otherwise create a project with its default workspace.
pub fn open_folder(store: &Store, path: &str) -> Result<Opened, RpcError> {
    let dir = canonical_dir(path)?;
    let dir_s = dir.to_string_lossy().to_string();
    let now = runwick_core::now();

    // 1. A workspace already points at this folder.
    let existing: Option<Workspace> = store
        .with(|c| {
            c.query_row(
                "SELECT * FROM workspaces WHERE path = ?1 AND remote_device_id IS NULL ORDER BY archived LIMIT 1",
                [&dir_s],
                workspace_from_row,
            )
            .optional()
        })
        .map_err(db_err)?;
    if let Some(ws) = existing {
        store
            .with(|c| {
                c.execute(
                    "UPDATE workspaces SET archived = 0, last_used_at = ?2 WHERE id = ?1",
                    params![ws.id, ts_to_sql(&now)],
                )?;
                c.execute(
                    "UPDATE projects SET archived = 0, last_opened_at = ?2 WHERE id = ?1",
                    params![ws.project_id, ts_to_sql(&now)],
                )
            })
            .map_err(db_err)?;
        return Ok(Opened {
            project: get_project(store, ws.project_id)?,
            workspace: get_workspace(store, ws.id)?,
            created: false,
        });
    }

    let repo_root = git::find_repo_root(&dir);
    let is_git = repo_root.is_some();

    // 2. A linked worktree of a known project's repository: attach as a workspace.
    if let Some(main) = repo_root.as_deref().and_then(git::main_worktree_root) {
        let main_s = main.to_string_lossy().to_string();
        if main != dir {
            let owner: Option<Uuid> = store
                .with(|c| c.query_row("SELECT id FROM projects WHERE root = ?1", [&main_s], |r| r.get(0)).optional())
                .map_err(db_err)?;
            if let Some(project_id) = owner {
                let ws_id = Uuid::now_v7();
                let name = git::read_branch(&dir).unwrap_or_else(|| folder_name(&dir));
                store
                    .with(|c| {
                        c.execute(
                            "INSERT INTO workspaces (id, project_id, name, path, kind, created_at, last_used_at)
                             VALUES (?1, ?2, ?3, ?4, 'worktree', ?5, ?5)",
                            params![ws_id, project_id, name, dir_s, ts_to_sql(&now)],
                        )
                    })
                    .map_err(db_err)?;
                return Ok(Opened {
                    project: get_project(store, project_id)?,
                    workspace: get_workspace(store, ws_id)?,
                    created: false,
                });
            }
        }
    }

    // 3. New project with its default workspace.
    let pid = Uuid::now_v7();
    let wid = Uuid::now_v7();
    let remote_url = repo_root.as_deref().and_then(git::origin_url);
    // 3a. A project synced from another computer for the same repository: link this folder.
    if let Some(url) = remote_url.as_deref()
        && let Some((pid, wid)) =
            crate::sync::kinds::projects::link_folder(store, &runwick_core::paths::home(), &dir_s, is_git, url)
                .map_err(db_err)?
    {
        return Ok(Opened { project: get_project(store, pid)?, workspace: get_workspace(store, wid)?, created: false });
    }
    store
        .tx(|tx| {
            tx.execute(
                "INSERT INTO projects (id, name, root, is_git, created_at, last_opened_at, remote_url, order_index, placement_updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?5, ?6,
                    (SELECT COALESCE(MIN(order_index), 0) - 1 FROM projects WHERE group_id IS NULL), ?5)",
                params![pid, folder_name(&dir), dir_s, is_git, ts_to_sql(&now), remote_url],
            )?;
            tx.execute(
                "INSERT INTO workspaces (id, project_id, name, path, kind, created_at, last_used_at)
                 VALUES (?1, ?2, ?3, ?4, 'default', ?5, ?5)",
                params![wid, pid, DEFAULT_WORKSPACE_NAME, dir_s, ts_to_sql(&now)],
            )?;
            Ok(())
        })
        .map_err(db_err)?;
    Ok(Opened { project: get_project(store, pid)?, workspace: get_workspace(store, wid)?, created: true })
}

pub fn update_project(store: &Store, u: &ProjectUpdate) -> Result<Project, RpcError> {
    let before = get_project(store, u.id)?;
    let now = runwick_core::now();
    if let Some(name) = &u.name {
        let name = name.trim();
        if name.is_empty() {
            return Err(RpcError::invalid("이름을 비워 둘 수 없어요"));
        }
        store
            .with(|c| c.execute("UPDATE projects SET name = ?2 WHERE id = ?1", params![u.id, name]))
            .map_err(db_err)?;
    }
    if let Some(f) = u.favorite {
        store
            .with(|c| c.execute("UPDATE projects SET favorite = ?2 WHERE id = ?1", params![u.id, f]))
            .map_err(db_err)?;
    }
    if let Some(a) = u.archived {
        store
            .with(|c| c.execute("UPDATE projects SET archived = ?2 WHERE id = ?1", params![u.id, a]))
            .map_err(db_err)?;
    }
    if let Some(root) = &u.root {
        let dir = canonical_dir(root)?;
        let dir_s = dir.to_string_lossy().to_string();
        let old_root = before.root.clone();
        store
            .tx(|tx| {
                tx.execute(
                    "UPDATE projects SET root = ?2, is_git = ?3, remote_url = COALESCE(?4, remote_url) WHERE id = ?1",
                    params![u.id, dir_s, git::find_repo_root(&dir).is_some(), git::origin_url(&dir)],
                )?;
                tx.execute(
                    "UPDATE workspaces SET path = ?3 WHERE project_id = ?1 AND path = ?2 AND remote_device_id IS NULL",
                    params![u.id, old_root, dir_s],
                )?;
                Ok(())
            })
            .map_err(|e| match e {
                rusqlite::Error::SqliteFailure(f, _) if f.code == rusqlite::ErrorCode::ConstraintViolation => {
                    RpcError::invalid("이 폴더는 이미 다른 프로젝트에 연결되어 있어요")
                }
                other => db_err(other),
            })?;
    }
    if u.touch {
        store
            .with(|c| {
                c.execute("UPDATE projects SET last_opened_at = ?2 WHERE id = ?1", params![u.id, ts_to_sql(&now)])
            })
            .map_err(db_err)?;
    }
    get_project(store, u.id)
}

pub fn update_workspace(store: &Store, u: &WorkspaceUpdate) -> Result<Workspace, RpcError> {
    get_workspace(store, u.id)?;
    if let Some(name) = &u.name {
        let name = name.trim();
        if name.is_empty() {
            return Err(RpcError::invalid("이름을 비워 둘 수 없어요"));
        }
        store
            .with(|c| c.execute("UPDATE workspaces SET name = ?2 WHERE id = ?1", params![u.id, name]))
            .map_err(db_err)?;
    }
    if let Some(a) = u.archived {
        store
            .with(|c| c.execute("UPDATE workspaces SET archived = ?2 WHERE id = ?1", params![u.id, a]))
            .map_err(db_err)?;
    }
    if u.touch {
        let now = ts_to_sql(&runwick_core::now());
        store
            .with(|c| {
                c.execute("UPDATE workspaces SET last_used_at = ?2 WHERE id = ?1", params![u.id, now])?;
                c.execute(
                    "UPDATE projects SET last_opened_at = ?2 WHERE id = (SELECT project_id FROM workspaces WHERE id = ?1)",
                    params![u.id, now],
                )
            })
            .map_err(db_err)?;
    }
    get_workspace(store, u.id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_same_folder_twice_reuses_project() {
        let store = Store::open_in_memory().unwrap();
        let d = tempfile::tempdir().unwrap();
        let a = open_folder(&store, d.path().to_str().unwrap()).unwrap();
        assert!(a.created);
        assert_eq!(a.workspace.kind, WorkspaceKind::Default);
        assert_eq!(a.workspace.name, DEFAULT_WORKSPACE_NAME);
        let b = open_folder(&store, d.path().to_str().unwrap()).unwrap();
        assert!(!b.created);
        assert_eq!(a.project.id, b.project.id);
        assert_eq!(list_projects(&store).unwrap().len(), 1);
    }

    #[test]
    fn missing_folder_is_a_clear_error() {
        let store = Store::open_in_memory().unwrap();
        let e = open_folder(&store, "/definitely/not/here").unwrap_err();
        assert!(e.message.contains("폴더를 찾을 수 없어요"), "{e}");
    }

    #[test]
    fn rename_rejects_empty_and_favorite_toggles() {
        let store = Store::open_in_memory().unwrap();
        let d = tempfile::tempdir().unwrap();
        let a = open_folder(&store, d.path().to_str().unwrap()).unwrap();
        assert!(
            update_project(&store, &ProjectUpdate { id: a.project.id, name: Some("  ".into()), ..Default::default() })
                .is_err()
        );
        let p = update_project(&store, &ProjectUpdate { id: a.project.id, favorite: Some(true), ..Default::default() })
            .unwrap();
        assert!(p.favorite);
    }
}
