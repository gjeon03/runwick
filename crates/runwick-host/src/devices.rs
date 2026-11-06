//! Registered devices and remote workspace links (stored by this computer's host), and folder
//! browsing under the home folder (answered for the add-device flow of another computer).

use crate::git;
use crate::host::Ctx;
use crate::projects::{get_project, get_workspace};
use crate::router::Router;
use crate::store::{Migration, Store, db_err, opt_ts, ts_from_sql, ts_to_sql};
use runwick_core::api::devices::*;
use runwick_core::api::projects::{ById, Workspace};
use runwick_core::josa;
use runwick_core::rpc::{RpcError, codes, topic};
use rusqlite::{OptionalExtension, Row, params};
use std::path::{Path, PathBuf};
use uuid::Uuid;

pub const MIGRATIONS: &[Migration] = &[(
    "0030_devices",
    "CREATE TABLE devices (
        id BLOB PRIMARY KEY,
        name TEXT NOT NULL,
        target TEXT NOT NULL,
        port INTEGER,
        os TEXT,
        arch TEXT,
        remote_host_id BLOB,
        remote_version TEXT,
        remote_exe TEXT,
        fingerprint TEXT,
        via TEXT,
        added_at TEXT NOT NULL,
        last_seen_at TEXT
    );",
)];

/// Root recorded for a project that only holds workspaces of one device. Never a real local
/// folder, so it cannot collide with a project opened on this computer.
pub fn remote_root(device: DeviceId, path: &str) -> String {
    format!("runwick-device://{device}{}", if path.starts_with('/') { "" } else { "/" }) + path
}

pub fn is_remote_root(root: &str) -> bool {
    root.starts_with("runwick-device://")
}

pub fn register(r: &mut Router) {
    r.add::<DevicesList, _, _>(|ctx: Ctx, _: ()| async move { list(&ctx.host.store).map_err(db_err) });
    r.add::<DevicesAdd, _, _>(|ctx: Ctx, p: AddDevice| async move {
        let d = add(&ctx.host.store, ctx.host.host_id(), &p)?;
        ctx.host.bus.changed(&[topic::DEVICES]);
        Ok(d)
    });
    r.add::<DevicesUpdate, _, _>(|ctx: Ctx, p: DeviceUpdate| async move {
        let d = update(&ctx.host.store, &p)?;
        ctx.host.bus.changed(&[topic::DEVICES]);
        Ok(d)
    });
    r.add::<DevicesSeen, _, _>(|ctx: Ctx, p: DeviceSeen| async move {
        let d = seen(&ctx.host.store, &p)?;
        ctx.host.bus.changed(&[topic::DEVICES]);
        Ok(d)
    });
    r.add::<DevicesRemove, _, _>(|ctx: Ctx, p: ById| async move {
        let out = remove(&ctx.host.store, p.id)?;
        ctx.host.bus.changed(&[topic::DEVICES, topic::WORKSPACES, topic::PROJECTS]);
        Ok(out)
    });
    r.add::<WorkspacesLinkRemote, _, _>(|ctx: Ctx, p: LinkRemote| async move {
        let w = link(&ctx.host.store, &p)?;
        ctx.host.bus.changed(&[topic::WORKSPACES, topic::PROJECTS]);
        Ok(w)
    });
    r.add::<WorkspacesUnlinkRemote, _, _>(|ctx: Ctx, p: ById| async move {
        unlink(&ctx.host.store, p.id)?;
        ctx.host.bus.changed(&[topic::WORKSPACES, topic::PROJECTS]);
        Ok(())
    });
    r.add::<DevicesBrowseDirs, _, _>(|_ctx: Ctx, p: BrowseDirs| async move {
        let home = runwick_core::paths::user_home();
        tokio::task::spawn_blocking(move || browse(&home, &p))
            .await
            .map_err(|e| RpcError::internal(format!("폴더 목록을 읽지 못했어요: {e}")))?
    });
}

fn device_from_row(r: &Row) -> rusqlite::Result<Device> {
    Ok(Device {
        id: r.get("id")?,
        name: r.get("name")?,
        target: r.get("target")?,
        port: r.get::<_, Option<i64>>("port")?.and_then(|p| u16::try_from(p).ok()),
        os: r.get("os")?,
        arch: r.get("arch")?,
        remote_host_id: r.get("remote_host_id")?,
        remote_version: r.get("remote_version")?,
        remote_exe: r.get("remote_exe")?,
        fingerprint: r.get("fingerprint")?,
        via: r.get("via")?,
        added_at: ts_from_sql(&r.get::<_, String>("added_at")?),
        last_seen_at: opt_ts(r.get("last_seen_at")?),
    })
}

pub fn list(store: &Store) -> rusqlite::Result<Vec<Device>> {
    store.with(|c| {
        let mut st = c.prepare("SELECT * FROM devices ORDER BY added_at, name")?;
        let rows = st.query_map([], device_from_row)?;
        rows.collect()
    })
}

pub fn get(store: &Store, id: DeviceId) -> Result<Device, RpcError> {
    store
        .with(|c| c.query_row("SELECT * FROM devices WHERE id = ?1", [id], device_from_row).optional())
        .map_err(db_err)?
        .ok_or_else(|| RpcError::not_found("등록된 기기를 찾을 수 없어요"))
}

/// Two devices with the same name could not be told apart in the sidebar.
fn name_taken(c: &rusqlite::Connection, name: &str, except: DeviceId) -> rusqlite::Result<Option<String>> {
    c.query_row(
        "SELECT name FROM devices WHERE lower(trim(name)) = lower(?1) AND id <> ?2",
        params![name, except],
        |r| r.get(0),
    )
    .optional()
}

fn clean_name(name: &str) -> Result<String, RpcError> {
    let n = name.trim();
    if n.is_empty() {
        return Err(RpcError::invalid("기기 이름을 입력하세요"));
    }
    if n.chars().count() > 60 {
        return Err(RpcError::invalid("기기 이름은 60자까지 쓸 수 있어요"));
    }
    Ok(n.to_string())
}

pub fn add(store: &Store, local_host: Uuid, p: &AddDevice) -> Result<Device, RpcError> {
    let name = clean_name(&p.name)?;
    let target = p.target.trim();
    if !target.starts_with(SIMULATED_PREFIX) {
        let (dest, _) = parse_target(target).map_err(RpcError::invalid)?;
        if dest != target {
            return Err(RpcError::invalid("SSH 주소와 포트를 나눠서 보내야 해요"));
        }
    }
    if p.remote_host_id == Some(local_host) {
        return Err(RpcError::invalid("지금 쓰는 컴퓨터예요. 등록하지 않아도 이미 쓰고 있어요"));
    }
    let id = p.id.unwrap_or_else(Uuid::now_v7);
    let now = ts_to_sql(&runwick_core::now());
    store
        .tx(|tx| {
            if let Some(hid) = p.remote_host_id {
                let other: Option<String> = tx
                    .query_row(
                        "SELECT name FROM devices WHERE remote_host_id = ?1 AND id <> ?2",
                        params![hid, id],
                        |r| r.get(0),
                    )
                    .optional()?;
                if let Some(other) = other {
                    return Ok(Err(RpcError::new(
                        codes::CONFLICT,
                        format!("이미 {} 등록된 기기예요", josa::euro_ro(&format!("‘{other}’"))),
                    )));
                }
            }
            if tx.query_row("SELECT 1 FROM devices WHERE id = ?1", [id], |_| Ok(())).optional()?.is_some() {
                return Ok(Err(RpcError::new(codes::CONFLICT, "이미 등록된 기기예요")));
            }
            if let Some(other) = name_taken(tx, &name, id)? {
                return Ok(Err(RpcError::new(
                    codes::CONFLICT,
                    format!("‘{other}’ 이름의 기기가 이미 있어요. 다른 이름을 쓰세요"),
                )));
            }
            tx.execute(
                "INSERT INTO devices (id, name, target, port, os, arch, remote_host_id, remote_version,
                     remote_exe, fingerprint, via, added_at, last_seen_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                params![
                    id,
                    name,
                    target,
                    p.port.map(i64::from),
                    p.os,
                    p.arch,
                    p.remote_host_id,
                    p.remote_version,
                    p.remote_exe,
                    p.fingerprint,
                    p.via,
                    now,
                    p.remote_host_id.map(|_| now.clone()),
                ],
            )?;
            Ok(Ok(()))
        })
        .map_err(db_err)??;
    get(store, id)
}

pub fn update(store: &Store, p: &DeviceUpdate) -> Result<Device, RpcError> {
    get(store, p.id)?;
    if let Some(name) = &p.name {
        let name = clean_name(name)?;
        if let Some(other) = store.with(|c| name_taken(c, &name, p.id)).map_err(db_err)? {
            return Err(RpcError::new(
                codes::CONFLICT,
                format!("‘{other}’ 이름의 기기가 이미 있어요. 다른 이름을 쓰세요"),
            ));
        }
        store.with(|c| c.execute("UPDATE devices SET name = ?2 WHERE id = ?1", params![p.id, name])).map_err(db_err)?;
    }
    if let Some(exe) = &p.remote_exe {
        let exe = exe.trim();
        if exe.is_empty() {
            return Err(RpcError::invalid("Runwick 실행 파일 경로를 입력하세요"));
        }
        store
            .with(|c| c.execute("UPDATE devices SET remote_exe = ?2 WHERE id = ?1", params![p.id, exe]))
            .map_err(db_err)?;
    }
    get(store, p.id)
}

/// Record a successful connection. A different host id at the same address is refused: it is
/// another installation (or another computer) and must be added again on purpose.
pub fn seen(store: &Store, p: &DeviceSeen) -> Result<Device, RpcError> {
    let d = get(store, p.id)?;
    if let Some(known) = d.remote_host_id
        && known != p.remote_host_id
    {
        return Err(RpcError::new(
            codes::CONFLICT,
            "등록할 때와 다른 Runwick이 응답했어요. 같은 주소의 다른 기기이거나 다시 설치된 것 같아요",
        ));
    }
    store
        .with(|c| {
            c.execute(
                "UPDATE devices SET remote_host_id = ?2, remote_version = ?3, os = ?4, arch = ?5, last_seen_at = ?6
                 WHERE id = ?1",
                params![p.id, p.remote_host_id, p.remote_version, p.os, p.arch, ts_to_sql(&runwick_core::now())],
            )
        })
        .map_err(db_err)?;
    get(store, p.id)
}

/// Forget a device: its workspace links go away from this computer; projects that only held
/// those links are archived (hidden), not deleted. The device keeps its runs and records.
pub fn remove(store: &Store, id: DeviceId) -> Result<DeviceRemoved, RpcError> {
    get(store, id)?;
    let unlinked = store
        .tx(|tx| {
            let projects: Vec<Uuid> = {
                let mut st = tx.prepare("SELECT DISTINCT project_id FROM workspaces WHERE remote_device_id = ?1")?;
                st.query_map([id], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?
            };
            let n = tx.execute("DELETE FROM workspaces WHERE remote_device_id = ?1", [id])?;
            for p in projects {
                archive_if_empty_remote_project(tx, p)?;
            }
            tx.execute("DELETE FROM devices WHERE id = ?1", [id])?;
            Ok(n)
        })
        .map_err(db_err)?;
    Ok(DeviceRemoved { unlinked })
}

fn archive_if_empty_remote_project(tx: &rusqlite::Transaction, project: Uuid) -> rusqlite::Result<()> {
    let root: Option<String> =
        tx.query_row("SELECT root FROM projects WHERE id = ?1", [project], |r| r.get(0)).optional()?;
    let left: i64 = tx.query_row("SELECT COUNT(*) FROM workspaces WHERE project_id = ?1", [project], |r| r.get(0))?;
    if left == 0 && root.as_deref().is_some_and(is_remote_root) {
        tx.execute("UPDATE projects SET archived = 1 WHERE id = ?1", [project])?;
    }
    Ok(())
}

/// Show a device's workspace in this computer's sidebar. Linking the same remote workspace
/// again returns the existing link.
pub fn link(store: &Store, p: &LinkRemote) -> Result<Workspace, RpcError> {
    get(store, p.device_id)?;
    let name = p.name.trim();
    if name.is_empty() {
        return Err(RpcError::invalid("공간 이름이 비어 있어요"));
    }
    let path = p.path.trim();
    if !path.starts_with('/') {
        return Err(RpcError::invalid("기기의 폴더는 절대 경로여야 해요"));
    }
    let existing: Option<Uuid> = store
        .with(|c| {
            c.query_row(
                "SELECT id FROM workspaces WHERE remote_device_id = ?1 AND remote_workspace_id = ?2",
                params![p.device_id, p.remote_workspace_id],
                |r| r.get(0),
            )
            .optional()
        })
        .map_err(db_err)?;
    if let Some(id) = existing {
        store.with(|c| c.execute("UPDATE workspaces SET archived = 0 WHERE id = ?1", [id])).map_err(db_err)?;
        let w = get_workspace(store, id)?;
        store.with(|c| c.execute("UPDATE projects SET archived = 0 WHERE id = ?1", [w.project_id])).map_err(db_err)?;
        return Ok(w);
    }
    let now = ts_to_sql(&runwick_core::now());
    let ws_id = Uuid::now_v7();
    let project_id = match &p.project {
        LinkProject::Existing { project_id } => {
            get_project(store, *project_id)?;
            *project_id
        }
        LinkProject::New { name } => {
            let pname = name.trim();
            if pname.is_empty() {
                return Err(RpcError::invalid("프로젝트 이름이 비어 있어요"));
            }
            let pid = Uuid::now_v7();
            let root = remote_root(p.device_id, path);
            let reused: Option<Uuid> = store
                .with(|c| c.query_row("SELECT id FROM projects WHERE root = ?1", [&root], |r| r.get(0)).optional())
                .map_err(db_err)?;
            match reused {
                Some(id) => {
                    store
                        .with(|c| c.execute("UPDATE projects SET archived = 0 WHERE id = ?1", [id]))
                        .map_err(db_err)?;
                    id
                }
                None => {
                    store
                        .with(|c| {
                            c.execute(
                                "INSERT INTO projects (id, name, root, is_git, created_at, last_opened_at, remote_url)
                                 VALUES (?1, ?2, ?3, ?4, ?5, ?5, ?6)",
                                params![pid, pname, root, p.remote_url.is_some(), now, p.remote_url],
                            )
                        })
                        .map_err(db_err)?;
                    pid
                }
            }
        }
    };
    store
        .with(|c| {
            c.execute(
                "INSERT INTO workspaces (id, project_id, name, path, kind, created_at, last_used_at,
                     remote_device_id, remote_workspace_id)
                 VALUES (?1, ?2, ?3, ?4, 'default', ?5, ?5, ?6, ?7)",
                params![ws_id, project_id, name, path, now, p.device_id, p.remote_workspace_id],
            )?;
            c.execute("UPDATE projects SET archived = 0, last_opened_at = ?2 WHERE id = ?1", params![project_id, now])
        })
        .map_err(db_err)?;
    get_workspace(store, ws_id)
}

/// Remove one remote workspace link (the device keeps the workspace and its records).
pub fn unlink(store: &Store, id: Uuid) -> Result<(), RpcError> {
    let w = get_workspace(store, id)?;
    if w.remote.is_none() {
        return Err(RpcError::invalid("이 컴퓨터의 공간은 연결 해제할 수 없어요"));
    }
    store
        .tx(|tx| {
            tx.execute("DELETE FROM workspaces WHERE id = ?1", [id])?;
            archive_if_empty_remote_project(tx, w.project_id)
        })
        .map_err(db_err)
}

fn dir_info(path: &Path) -> DirInfo {
    let is_git = path.join(".git").exists();
    DirInfo {
        name: path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| path.display().to_string()),
        path: path.to_string_lossy().to_string(),
        is_git,
        branch: if is_git { git::read_branch(path) } else { None },
        remote_url: if is_git { git::origin_url(path) } else { None },
    }
}

fn display(home: &Path, p: &Path) -> String {
    match p.strip_prefix(home) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".into(),
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => p.display().to_string(),
    }
}

/// Sub-folders of `p.path` (default: home). Only folders inside the home folder are listed;
/// symbolic links that lead outside it are refused.
pub fn browse(home: &Path, p: &BrowseDirs) -> Result<DirListing, RpcError> {
    let home = home.canonicalize().map_err(|_| RpcError::internal("이 기기의 홈 폴더를 찾을 수 없어요"))?;
    let asked = match p.path.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        None | Some("~") => home.clone(),
        Some(s) => match s.strip_prefix("~/") {
            Some(rest) => home.join(rest),
            None => PathBuf::from(s),
        },
    };
    let dir =
        asked.canonicalize().map_err(|_| RpcError::not_found(format!("폴더를 찾을 수 없어요: {}", asked.display())))?;
    if !dir.starts_with(&home) {
        return Err(RpcError::new(codes::DENIED, "홈 폴더 안의 폴더만 고를 수 있어요"));
    }
    if !dir.is_dir() {
        return Err(RpcError::invalid(format!("폴더가 아니에요: {}", display(&home, &dir))));
    }
    let rd = std::fs::read_dir(&dir)
        .map_err(|e| RpcError::new(codes::DENIED, format!("폴더를 읽을 수 없어요 ({}): {e}", display(&home, &dir))))?;
    let mut names: Vec<PathBuf> = rd
        .filter_map(|e| e.ok())
        .filter(|e| {
            let name = e.file_name();
            let name = name.to_string_lossy();
            p.show_hidden || !name.starts_with('.')
        })
        .map(|e| e.path())
        // Follow links only when they stay inside the home folder.
        .filter(|path| path.is_dir() && path.canonicalize().is_ok_and(|c| c.starts_with(&home)))
        .collect();
    names.sort_by_key(|a| a.file_name().map(|n| n.to_string_lossy().to_lowercase()));
    let more = names.len().saturating_sub(BROWSE_LIMIT);
    names.truncate(BROWSE_LIMIT);
    let parent = if dir == home { None } else { dir.parent().map(|x| x.to_string_lossy().to_string()) };
    Ok(DirListing {
        display: display(&home, &dir),
        dirs: names.iter().map(|x| dir_info(x)).collect(),
        dir: dir_info(&dir),
        parent,
        home: home.to_string_lossy().to_string(),
        more,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projects::{list_projects, list_workspaces, open_folder};
    use runwick_core::api::projects::WorkspaceQuery;

    fn add_dev(store: &Store, name: &str, host: Option<Uuid>) -> Device {
        let p = AddDevice {
            name: name.into(),
            target: "me@mini.tail3f2a.ts.net".into(),
            port: Some(2222),
            remote_host_id: host,
            remote_version: Some("0.1.0".into()),
            os: Some("macos".into()),
            arch: Some("aarch64".into()),
            ..Default::default()
        };
        add(store, Uuid::now_v7(), &p).unwrap()
    }

    #[test]
    fn device_crud_round_trip() {
        let store = Store::open_in_memory().unwrap();
        let hid = Uuid::now_v7();
        let d = add_dev(&store, "  집 Mac mini ", Some(hid));
        assert_eq!(d.name, "집 Mac mini");
        assert_eq!(d.port, Some(2222));
        assert!(d.last_seen_at.is_some(), "a device checked during setup was seen");
        let d =
            update(&store, &DeviceUpdate { id: d.id, name: Some("Mac mini".into()), ..Default::default() }).unwrap();
        assert_eq!(d.name, "Mac mini");
        assert!(update(&store, &DeviceUpdate { id: d.id, name: Some(" ".into()), ..Default::default() }).is_err());
        assert_eq!(list(&store).unwrap().len(), 1);
        let seen_ok = seen(
            &store,
            &DeviceSeen {
                id: d.id,
                remote_host_id: hid,
                remote_version: "0.2.0".into(),
                os: "macos".into(),
                arch: "aarch64".into(),
            },
        )
        .unwrap();
        assert_eq!(seen_ok.remote_version.as_deref(), Some("0.2.0"));
        // Another installation answering at the same address is refused.
        let e = seen(
            &store,
            &DeviceSeen {
                id: d.id,
                remote_host_id: Uuid::now_v7(),
                remote_version: "0.2.0".into(),
                os: "macos".into(),
                arch: "aarch64".into(),
            },
        )
        .unwrap_err();
        assert_eq!(e.code, codes::CONFLICT);
        assert_eq!(remove(&store, d.id).unwrap(), DeviceRemoved { unlinked: 0 });
        assert!(list(&store).unwrap().is_empty());
    }

    #[test]
    fn same_host_twice_and_this_computer_are_refused() {
        let store = Store::open_in_memory().unwrap();
        let hid = Uuid::now_v7();
        add_dev(&store, "A", Some(hid));
        let e = add(
            &store,
            Uuid::now_v7(),
            &AddDevice { name: "B".into(), target: "b".into(), remote_host_id: Some(hid), ..Default::default() },
        )
        .unwrap_err();
        assert!(e.message.contains("‘A’"), "{e}");
        let local = Uuid::now_v7();
        let e = add(
            &store,
            local,
            &AddDevice { name: "me".into(), target: "me".into(), remote_host_id: Some(local), ..Default::default() },
        )
        .unwrap_err();
        assert!(e.message.contains("지금 쓰는 컴퓨터"), "{e}");
        let e = add(
            &store,
            local,
            &AddDevice { name: "x".into(), target: "-oProxyCommand=sh".into(), ..Default::default() },
        )
        .unwrap_err();
        assert_eq!(e.code, codes::INVALID);
        // Names tell devices apart in the sidebar.
        let e = add(&store, local, &AddDevice { name: " a ".into(), target: "other".into(), ..Default::default() })
            .unwrap_err();
        assert_eq!(e.code, codes::CONFLICT);
        let b = add(&store, local, &AddDevice { name: "B".into(), target: "b2".into(), ..Default::default() }).unwrap();
        let e = update(&store, &DeviceUpdate { id: b.id, name: Some("A".into()), ..Default::default() }).unwrap_err();
        assert_eq!(e.code, codes::CONFLICT);
        assert_eq!(
            update(&store, &DeviceUpdate { id: b.id, name: Some("B".into()), ..Default::default() }).unwrap().name,
            "B"
        );
    }

    #[test]
    fn link_and_unlink_remote_workspaces() {
        let store = Store::open_in_memory().unwrap();
        let d = add_dev(&store, "집 Mac mini", Some(Uuid::now_v7()));
        let folder = tempfile::tempdir().unwrap();
        let local = open_folder(&store, folder.path().to_str().unwrap()).unwrap();
        let rws = Uuid::now_v7();
        // Under an existing project, chosen by the person.
        let w = link(
            &store,
            &LinkRemote {
                device_id: d.id,
                remote_workspace_id: rws,
                name: "기본 공간".into(),
                path: "/Users/me/work/payments-api".into(),
                project: LinkProject::Existing { project_id: local.project.id },
                remote_url: None,
            },
        )
        .unwrap();
        assert_eq!(w.project_id, local.project.id);
        let r = w.remote.clone().unwrap();
        assert_eq!((r.device_id, r.remote_workspace_id), (d.id, rws));
        assert!(!w.path_exists && w.branch.is_none(), "a device folder is never checked on this computer");
        // Linking again returns the same link.
        let again = link(
            &store,
            &LinkRemote {
                device_id: d.id,
                remote_workspace_id: rws,
                name: "기본 공간".into(),
                path: "/Users/me/work/payments-api".into(),
                project: LinkProject::New { name: "dup".into() },
                remote_url: None,
            },
        )
        .unwrap();
        assert_eq!(again.id, w.id);
        // A separate project for another folder.
        let w2 = link(
            &store,
            &LinkRemote {
                device_id: d.id,
                remote_workspace_id: Uuid::now_v7(),
                name: "기본 공간".into(),
                path: "/Users/me/work/data-batch".into(),
                project: LinkProject::New { name: "data-batch (집 Mac mini)".into() },
                remote_url: Some("git@github.com:acme/data-batch.git".into()),
            },
        )
        .unwrap();
        assert_ne!(w2.project_id, local.project.id);
        let p2 = get_project(&store, w2.project_id).unwrap();
        assert!(is_remote_root(&p2.root) && !p2.root_exists);
        assert_eq!(p2.remote_url.as_deref(), Some("git@github.com:acme/data-batch.git"));
        // Relative paths are refused.
        assert!(
            link(
                &store,
                &LinkRemote {
                    device_id: d.id,
                    remote_workspace_id: Uuid::now_v7(),
                    name: "x".into(),
                    path: "work/x".into(),
                    project: LinkProject::Existing { project_id: local.project.id },
                    remote_url: None,
                },
            )
            .is_err()
        );
        // Unlink the separate project's only workspace: the project is archived, not deleted.
        unlink(&store, w2.id).unwrap();
        assert!(get_project(&store, w2.project_id).unwrap().archived);
        assert!(unlink(&store, local.workspace.id).is_err(), "local workspaces are not links");
        // Removing the device drops its remaining links; local data stays.
        assert_eq!(remove(&store, d.id).unwrap().unlinked, 1);
        let all = list_workspaces(&store, &WorkspaceQuery { include_archived: true, ..Default::default() }).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].id, local.workspace.id);
        assert!(!get_project(&store, local.project.id).unwrap().archived);
        assert_eq!(list_projects(&store).unwrap().len(), 2);
    }

    #[test]
    fn browse_stays_inside_home() {
        let home = tempfile::tempdir().unwrap();
        let h = home.path();
        std::fs::create_dir_all(h.join("work/payments-api/.git")).unwrap();
        std::fs::write(h.join("work/payments-api/.git/HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::write(
            h.join("work/payments-api/.git/config"),
            "[remote \"origin\"]\n\turl = git@github.com:acme/payments-api.git\n",
        )
        .unwrap();
        std::fs::create_dir_all(h.join("work/notes")).unwrap();
        std::fs::create_dir_all(h.join(".hidden")).unwrap();
        std::fs::write(h.join("file.txt"), "x").unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), h.join("escape")).unwrap();

        let root = browse(h, &BrowseDirs::default()).unwrap();
        assert_eq!(root.display, "~");
        assert!(root.parent.is_none());
        let names: Vec<_> = root.dirs.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, vec!["work"], "files, hidden folders and links leaving home are left out");

        let work = browse(h, &BrowseDirs { path: Some("~/work".into()), show_hidden: false }).unwrap();
        assert_eq!(work.display, "~/work");
        let repo = work.dirs.iter().find(|d| d.name == "payments-api").unwrap();
        assert!(repo.is_git);
        assert_eq!(repo.branch.as_deref(), Some("main"));
        assert_eq!(repo.remote_url.as_deref(), Some("git@github.com:acme/payments-api.git"));
        assert!(work.parent.is_some());

        let e = browse(h, &BrowseDirs { path: Some(outside.path().to_string_lossy().into()), show_hidden: false })
            .unwrap_err();
        assert_eq!(e.code, codes::DENIED);
        let e = browse(h, &BrowseDirs { path: Some("~/escape".into()), show_hidden: false }).unwrap_err();
        assert_eq!(e.code, codes::DENIED);
        let e = browse(h, &BrowseDirs { path: Some("~/nope".into()), show_hidden: false }).unwrap_err();
        assert_eq!(e.code, codes::NOT_FOUND);
        let hidden = browse(h, &BrowseDirs { path: None, show_hidden: true }).unwrap();
        assert!(hidden.dirs.iter().any(|d| d.name == ".hidden"));
    }
}
