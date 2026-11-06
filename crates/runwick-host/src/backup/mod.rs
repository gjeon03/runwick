//! Local backups of this host's data (S14 기기별 백업): make, list, verify, preview and restore.
//!
//! - `archive`: the snapshot archive (made while runs continue) and its verification
//! - `restore`: replacing the live data with a verified backup, or leaving it untouched
//!
//! Each host backs up its own disk; windows ask every connected host separately.

pub mod archive;
pub mod restore;
pub mod sign;

#[cfg(test)]
mod tests;

use crate::host::{Ctx, Host};
use crate::meta::{kv_get, kv_put};
use crate::router::Router;
use crate::store::db_err;
use archive::{Manifest, Source};
use runwick_core::api::backup::*;
use runwick_core::paths;
use runwick_core::rpc::{RpcError, codes, topic};
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use uuid::Uuid;

const K_DIRS: &str = "backup.dirs";
const K_VERIFIED: &str = "backup.verified";

/// A verification result, valid while the file keeps its size and modification time.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Checked {
    bytes: u64,
    mtime: i64,
    verify: Verify,
}

struct Preview {
    dir: PathBuf,
    item: BackupItem,
    /// This computer's signature could not vouch for it: activation needs the person's word.
    unsigned: bool,
}

#[derive(Default)]
struct Service {
    /// One backup or restore at a time.
    busy: Mutex<bool>,
    previews: Mutex<HashMap<Uuid, Preview>>,
}

static SERVICE: OnceLock<Service> = OnceLock::new();

fn service() -> &'static Service {
    SERVICE.get_or_init(Service::default)
}

struct Busy;

impl Drop for Busy {
    fn drop(&mut self) {
        if let Ok(mut b) = service().busy.lock() {
            *b = false;
        }
    }
}

fn begin() -> Result<Busy, RpcError> {
    let mut b = service().busy.lock().map_err(|_| RpcError::internal("백업 상태를 읽지 못했어요"))?;
    if *b {
        return Err(RpcError::new(codes::BUSY, "다른 백업이나 복구가 진행 중이에요"));
    }
    *b = true;
    Ok(Busy)
}

fn mtime(p: &Path) -> i64 {
    std::fs::metadata(p)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0)
}

fn size_of(p: &Path) -> u64 {
    let Ok(meta) = std::fs::symlink_metadata(p) else {
        return 0;
    };
    if meta.is_file() {
        return meta.len();
    }
    if !meta.is_dir() {
        return 0;
    }
    std::fs::read_dir(p).map(|rd| rd.flatten().map(|e| size_of(&e.path())).sum()).unwrap_or(0)
}

pub fn info(host: &Host) -> StorageInfo {
    let home = paths::home();
    let db = ["runwick.db", "runwick.db-wal", "runwick.db-shm"].iter().map(|f| size_of(&home.join(f))).sum();
    StorageInfo {
        host_id: host.host_id(),
        device_name: host.info.device_name.clone(),
        home: paths::display_path(&home),
        db_bytes: db,
        wiki_bytes: size_of(&paths::wiki_dir()),
        screens_bytes: size_of(&home.join("screens")),
        sync_bytes: size_of(&paths::sync_dir()),
        backups_bytes: dirs(host).iter().map(|d| size_of(d)).sum(),
        backups_dir: paths::display_path(&paths::backups_dir()),
    }
}

/// The default backup folder first, then folders backups were made in before.
fn dirs(host: &Host) -> Vec<PathBuf> {
    let mut out = vec![paths::backups_dir()];
    let extra: Vec<String> =
        kv_get(&host.store, K_DIRS).ok().flatten().and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default();
    for d in extra {
        let p = PathBuf::from(d);
        if p.is_dir() && !out.contains(&p) {
            out.push(p);
        }
    }
    out
}

fn verified(host: &Host) -> BTreeMap<String, Checked> {
    kv_get(&host.store, K_VERIFIED).ok().flatten().and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default()
}

fn remember(host: &Host, path: &Path, verify: &Verify) {
    let mut all = verified(host);
    all.retain(|p, _| Path::new(p).exists());
    all.insert(
        path.to_string_lossy().to_string(),
        Checked {
            bytes: std::fs::metadata(path).map(|m| m.len()).unwrap_or(0),
            mtime: mtime(path),
            verify: verify.clone(),
        },
    );
    let _ = kv_put(&host.store, K_VERIFIED, &serde_json::to_value(all).unwrap_or_default());
}

fn item(host: &Host, path: &Path, manifest: Option<&Manifest>, checks: &BTreeMap<String, Checked>) -> BackupItem {
    let bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let key = path.to_string_lossy().to_string();
    let verify = checks
        .get(&key)
        .filter(|c| c.bytes == bytes && c.mtime == mtime(path))
        .map(|c| c.verify.clone())
        .unwrap_or(Verify::NotChecked);
    BackupItem {
        file_name: path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
        path: key,
        bytes,
        created_at: manifest.map(|m| m.created_at),
        device: manifest.map(|m| m.device.clone()),
        host_id: manifest.map(|m| m.host_id),
        copy: manifest.is_some_and(|m| m.host_id != host.host_id()),
        before_restore: manifest.and_then(|m| m.reason.as_deref()) == Some("before_restore"),
        verify,
        counts: manifest.map(|m| m.counts.clone()).unwrap_or_default(),
    }
}

pub fn list(host: &Host) -> BackupList {
    let checks = verified(host);
    let ds = dirs(host);
    let mut items = Vec::new();
    for d in &ds {
        let Ok(rd) = std::fs::read_dir(d) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            let name = e.file_name().to_string_lossy().to_string();
            if !p.is_file() || !archive::is_archive(&name) {
                continue;
            }
            let manifest = archive::read_manifest(&p).map(|(m, _)| m);
            items.push(item(host, &p, manifest.as_ref(), &checks));
        }
    }
    items.sort_by(|a, b| b.created_at.cmp(&a.created_at).then(b.file_name.cmp(&a.file_name)));
    BackupList { dirs: ds.iter().map(|d| paths::display_path(d)).collect(), items }
}

/// A new folder for unpacking a backup (under `restore/`, readable only by this user).
fn work_dir(kind: &str) -> PathBuf {
    let parent = paths::home().join("restore");
    if let Err(e) = archive::private_dir(&parent, true) {
        tracing::warn!("restore folder not made private: {e}");
    }
    parent.join(format!("{kind}-{}", Uuid::new_v4()))
}

/// Make a backup now and verify it.
pub fn create(host: &Host, dir: Option<&str>, reason: Option<&str>) -> Result<BackupItem, RpcError> {
    let dir = match dir.map(str::trim).filter(|d| !d.is_empty()) {
        Some(d) => {
            let p = PathBuf::from(d);
            if !p.is_absolute() {
                return Err(RpcError::invalid("백업 폴더는 전체 경로로 골라 주세요"));
            }
            p
        }
        None => paths::backups_dir(),
    };
    let home = paths::home();
    let wiki_root = paths::wiki_dir();
    // Pages edited outside Runwick become versions first, so the snapshot has them.
    crate::wiki::ops::refresh(&host.store, &wiki_root, None)?;
    // A backup matters more than its signature: without the key it is made unsigned.
    let signer =
        sign::signer(true).inspect_err(|e| tracing::warn!(error = %e, "backup signing key unavailable")).ok().flatten();
    let src = Source { home: &home, host_id: host.host_id(), device: &host.info.device_name, signer: signer.as_ref() };
    let (path, side) = archive::create(&src, &dir, reason).map_err(RpcError::internal)?;
    if dir != paths::backups_dir() {
        let mut extra: Vec<String> =
            kv_get(&host.store, K_DIRS).ok().flatten().and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default();
        let d = dir.to_string_lossy().to_string();
        if !extra.contains(&d) {
            extra.push(d);
            let _ = kv_put(&host.store, K_DIRS, &serde_json::to_value(extra).unwrap_or_default());
        }
    }
    let scratch = work_dir("verify");
    let verify = check(&path, &scratch);
    let _ = std::fs::remove_dir_all(&scratch);
    remember(host, &path, &verify);
    tracing::info!(bytes = side.archive_bytes, ok = matches!(verify, Verify::Ok { .. }), "backup made");
    if let Verify::Failed { problems, .. } = &verify {
        // Never present a backup that failed its own check as a good one.
        return Err(RpcError::internal(format!("백업을 만들었지만 확인에 실패했어요: {}", problems.join(" · "))));
    }
    Ok(item(host, &path, Some(&side.manifest), &verified(host)))
}

fn check(path: &Path, scratch: &Path) -> Verify {
    let problems = match archive::verify(path, scratch) {
        Ok(_) if signature(path) == sign::Check::Bad => vec![sign::TAMPERED.to_string()],
        Ok(_) => {
            return Verify::Ok { at: runwick_core::now() };
        }
        Err(problems) => problems,
    };
    Verify::Failed { at: runwick_core::now(), problems }
}

/// This computer's signature check of an archive.
fn signature(path: &Path) -> sign::Check {
    let signer = sign::signer(false);
    archive::signature(path, signer.as_ref().map(Option::as_ref).map_err(|_| ()))
}

/// A backup file from the person (it must be one this host lists or a Runwick archive).
fn archive_path(path: &str) -> Result<PathBuf, RpcError> {
    let p = PathBuf::from(path);
    let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    if !p.is_absolute() || !archive::is_archive(&name) || !p.is_file() {
        return Err(RpcError::not_found("Runwick 백업 파일을 찾을 수 없어요"));
    }
    Ok(p)
}

pub fn verify(host: &Host, path: &str) -> Result<BackupItem, RpcError> {
    let p = archive_path(path)?;
    let scratch = work_dir("verify");
    let v = check(&p, &scratch);
    let _ = std::fs::remove_dir_all(&scratch);
    remember(host, &p, &v);
    let manifest = archive::read_manifest(&p).map(|(m, _)| m);
    Ok(item(host, &p, manifest.as_ref(), &verified(host)))
}

fn blockers(host: &Host, item: &BackupItem, unsaved: u32) -> Vec<String> {
    let mut out = Vec::new();
    if item.copy {
        out.push(format!(
            "{}에서 만든 백업이에요. 백업은 만든 기기에서 복구해요 (다른 컴퓨터로 옮길 때는 GitHub 동기화를 써요)",
            item.device.as_deref().unwrap_or("다른 기기")
        ));
    }
    let running = host.sessions.running_ids().len();
    if running > 0 {
        out.push(format!("실행 중인 작업 {running}개가 있어요. 멈춘 뒤에 바꿀 수 있어요"));
    }
    if unsaved > 0 {
        out.push(format!("저장하지 않은 문서가 {unsaved}개 있어요. 저장하거나 닫은 뒤에 바꿀 수 있어요"));
    }
    if crate::sync::service().is_running() {
        out.push("동기화가 끝난 뒤에 바꿀 수 있어요".into());
    }
    out
}

pub fn preview(host: &Host, path: &str) -> Result<RestorePreview, RpcError> {
    let p = archive_path(path)?;
    let dir = work_dir("preview");
    let manifest = match archive::verify(&p, &dir) {
        Ok(_) if signature(&p) == sign::Check::Bad => Err(vec![sign::TAMPERED.to_string()]),
        other => other,
    };
    let manifest = match manifest {
        Ok(m) => m,
        Err(problems) => {
            let _ = std::fs::remove_dir_all(&dir);
            remember(host, &p, &Verify::Failed { at: runwick_core::now(), problems: problems.clone() });
            return Err(RpcError::invalid(format!("백업 확인에 실패해서 열지 않았어요: {}", problems.join(" · "))));
        }
    };
    remember(host, &p, &Verify::Ok { at: runwick_core::now() });
    type Roots = Vec<(Vec<u8>, String, String)>;
    let read = || -> rusqlite::Result<(Vec<runwick_core::api::sync::KindCount>, Vec<String>, Roots)> {
        let c = rusqlite::Connection::open_with_flags(
            dir.join(archive::DB_FILE),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        let counts = archive::counts(&c)?;
        let mut st = c.prepare("SELECT name FROM projects ORDER BY name")?;
        let names = st.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
        let mut st = c.prepare("SELECT id, name, root FROM projects")?;
        let roots = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<Result<_, _>>()?;
        Ok((counts, names, roots))
    };
    let (in_backup, projects, backup_roots) =
        read().map_err(|e| RpcError::internal(format!("백업 기록을 읽지 못했어요: {e}")))?;
    let kept = kept_here(host, &backup_roots).map_err(db_err)?;
    let now = host.store.with(archive::counts).map_err(db_err)?;
    let item = item(host, &p, Some(&manifest), &verified(host));
    // Only this computer's own backups can be restored; those carry its signature.
    let unsigned = if item.copy { None } else { signature(&p).unconfirmed() };
    let id = Uuid::new_v4();
    let out = RestorePreview {
        unsigned: unsigned.clone(),
        id,
        blockers: blockers(host, &item, 0),
        item: item.clone(),
        in_backup,
        now,
        projects,
        kept,
    };
    let mut previews = service().previews.lock().map_err(|_| RpcError::internal("미리보기를 보관하지 못했어요"))?;
    for (_, old) in previews.drain() {
        let _ = std::fs::remove_dir_all(&old.dir);
    }
    previews.insert(id, Preview { dir, item, unsigned: unsigned.is_some() });
    Ok(out)
}

/// What the restore keeps as it is on this computer (see `restore::KEPT_TABLES`), in words.
fn kept_here(host: &Host, backup_roots: &[(Vec<u8>, String, String)]) -> rusqlite::Result<Vec<String>> {
    host.store.with(|c| {
        let n = |sql: &str| c.query_row(sql, [], |r| r.get::<_, i64>(0));
        let mut out = vec![
            format!("공통 환경변수 {}개는 지금 것을 그대로 둬요", n("SELECT COUNT(*) FROM env_vars")?),
            format!("등록한 기기 {}대는 지금 것을 그대로 둬요", n("SELECT COUNT(*) FROM devices")?),
            "외부 편집기 명령은 지금 것을 그대로 둬요".to_string(),
        ];
        let mut moved = Vec::new();
        for (id, name, root) in backup_roots {
            let now: Option<String> =
                c.query_row("SELECT root FROM projects WHERE id = ?1", [id], |r| r.get(0)).optional()?;
            if now.as_deref().is_some_and(|r| r != root) {
                moved.push(name.clone());
            }
        }
        if !moved.is_empty() {
            out.push(format!("폴더 위치는 지금 것을 써요: {}", moved.join(", ")));
        }
        Ok(out)
    })
}

fn discard(id: Uuid) {
    if let Some(p) = service().previews.lock().ok().and_then(|mut m| m.remove(&id)) {
        let _ = std::fs::remove_dir_all(&p.dir);
    }
}

pub fn activate(host: &Arc<Host>, a: &ActivateRestore) -> Result<RestoreDone, RpcError> {
    let _busy = begin()?;
    let (dir, item, unsigned) = {
        let previews = service().previews.lock().map_err(|_| RpcError::internal("미리보기를 읽지 못했어요"))?;
        let p = previews
            .get(&a.id)
            .ok_or_else(|| RpcError::not_found("복구 미리보기가 끝났어요. 백업을 다시 열어 주세요"))?;
        (p.dir.clone(), p.item.clone(), p.unsigned)
    };
    if unsigned && !a.accept_unsigned {
        return Err(RpcError::invalid(
            "이 컴퓨터의 서명을 확인할 수 없는 백업이에요. 직접 만든 백업이 맞다고 확인한 뒤에 바꿀 수 있어요",
        ));
    }
    let blocked = blockers(host, &item, a.unsaved_documents);
    if !blocked.is_empty() {
        return Err(RpcError::new(codes::BUSY, blocked.join(" · ")));
    }
    // Keep the current data as a dated backup first; without it nothing is replaced.
    let kept = create(host, None, Some("before_restore"))
        .map_err(|e| RpcError::internal(format!("지금 데이터를 보관하지 못해서 복구하지 않았어요: {}", e.message)))?;
    restore::activate(&host.store, &paths::wiki_dir(), &dir).map_err(RpcError::internal)?;
    discard(a.id);
    tracing::info!("backup restored; restarting the host to load it");
    // Restart so every feature reloads from the restored records (no run is alive).
    let h = host.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        h.shutdown.notify_waiters();
    });
    Ok(RestoreDone { kept: kept.path, restarting: true })
}

async fn blocking<T: Send + 'static>(
    host: &Arc<Host>,
    f: impl FnOnce(&Arc<Host>) -> Result<T, RpcError> + Send + 'static,
) -> Result<T, RpcError> {
    let host = host.clone();
    tokio::task::spawn_blocking(move || f(&host)).await.map_err(RpcError::internal)?
}

pub fn register(r: &mut Router) {
    r.add::<BackupInfo, _, _>(|ctx: Ctx, _: ()| async move { blocking(&ctx.host, |h| Ok(info(h))).await });
    r.add::<BackupListGet, _, _>(|ctx: Ctx, _: ()| async move { blocking(&ctx.host, |h| Ok(list(h))).await });
    r.add::<BackupCreate, _, _>(|ctx: Ctx, p: CreateBackup| async move {
        let out = blocking(&ctx.host, move |h| {
            let _busy = begin()?;
            create(h, p.dir.as_deref(), None)
        })
        .await;
        ctx.host.bus.changed(&[TOPIC]);
        out
    });
    r.add::<BackupVerify, _, _>(|ctx: Ctx, p: BackupPath| async move {
        let out = blocking(&ctx.host, move |h| verify(h, &p.path)).await;
        ctx.host.bus.changed(&[TOPIC]);
        out
    });
    r.add::<BackupRestorePreview, _, _>(|ctx: Ctx, p: BackupPath| async move {
        interactive(&ctx)?;
        let out = blocking(&ctx.host, move |h| preview(h, &p.path)).await;
        ctx.host.bus.changed(&[TOPIC]);
        out
    });
    r.add::<BackupRestoreDiscard, _, _>(|_ctx: Ctx, p: PreviewRef| async move {
        discard(p.id);
        Ok(())
    });
    r.add::<BackupRestoreActivate, _, _>(|ctx: Ctx, a: ActivateRestore| async move {
        interactive(&ctx)?;
        let out = blocking(&ctx.host, move |h| activate(h, &a)).await;
        ctx.host.bus.changed(&[TOPIC, topic::PROJECTS]);
        out
    });
}

fn interactive(ctx: &Ctx) -> Result<(), RpcError> {
    if ctx.client.is_interactive() {
        Ok(())
    } else {
        Err(RpcError::new(codes::DENIED, "복구는 Runwick 창에서만 할 수 있어요"))
    }
}

/// Clear previews left by a previous run.
pub fn boot() {
    let _ = std::fs::remove_dir_all(paths::home().join("restore"));
}
