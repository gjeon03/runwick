//! GitHub backup and sync between computers (S14).
//!
//! - `format`: the data folder (canonical JSON, manifest, three-way merge of records)
//! - `kinds`: one exporter/importer per kind of record, each self-contained
//! - `engine`: one sync (fetch → import → export → commit → merge ours → push)
//! - `repo`: previewing and connecting a repository; `gh`: the GitHub CLI; `git`: running git
//! - `state`: what this host remembers (repository, merge base, auto sync, last run)
//!
//! Syncs run one at a time on a blocking thread. Auto sync is opt-in: after local changes at
//! most every N minutes, now and then to receive other computers' changes, and once more when
//! the host shuts down with changes pending.

pub mod engine;
mod export;
pub mod format;
pub mod gh;
pub mod git;
mod import;
pub mod kinds;
pub mod repo;
pub mod state;
pub mod tombstones;
pub mod wc;

#[cfg(test)]
mod tests;

use crate::host::{Ctx, Host};
use crate::router::Router;
use crate::store::{Store, db_err};
use engine::{Engine, WorkingCopy};
use git::{Git, GitEnv, GitError};
use kinds::{Cx, Device};
use runwick_core::api::sync::*;
use runwick_core::paths;
use runwick_core::rpc::{RpcError, codes, topic};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use uuid::Uuid;

pub use state::MIGRATIONS;

/// One sync at a time, the step it is on, and clones waiting to be connected.
#[derive(Default)]
pub struct Service {
    running: Mutex<Option<SyncStep>>,
    previews: Mutex<HashMap<Uuid, repo::Preview>>,
}

static SERVICE: OnceLock<Service> = OnceLock::new();

pub fn service() -> &'static Service {
    SERVICE.get_or_init(Service::default)
}

/// Held while a sync runs; clears the running step when dropped.
struct Running<'a>(&'a Service);

impl Drop for Running<'_> {
    fn drop(&mut self) {
        if let Ok(mut r) = self.0.running.lock() {
            *r = None;
        }
    }
}

impl Service {
    fn begin(&self) -> Result<Running<'_>, RpcError> {
        let mut r = self.running.lock().map_err(|_| RpcError::internal("동기화 상태를 읽지 못했어요"))?;
        if r.is_some() {
            return Err(RpcError::new(codes::BUSY, "이미 동기화하고 있어요"));
        }
        *r = Some(SyncStep::Fetch);
        Ok(Running(self))
    }

    fn set_step(&self, s: SyncStep) {
        if let Ok(mut r) = self.running.lock() {
            *r = Some(s);
        }
    }

    pub fn step(&self) -> Option<SyncStep> {
        self.running.lock().ok().and_then(|r| *r)
    }

    pub fn is_running(&self) -> bool {
        self.step().is_some()
    }
}

fn git_err(e: GitError) -> RpcError {
    let code = match e.kind {
        FailKind::Auth => codes::DENIED,
        FailKind::NotFound => codes::NOT_FOUND,
        FailKind::Network => codes::DISCONNECTED,
        FailKind::Busy => codes::BUSY,
        FailKind::Format => codes::UNSUPPORTED,
        FailKind::Other => codes::INTERNAL,
    };
    RpcError::new(code, e.message)
}

fn device(host: &Host) -> Device {
    Device { host_id: host.host_id(), name: host.info.device_name.clone() }
}

/// Every topic a sync can change (used when a failed sync may have imported part of the data).
const DATA_TOPICS: &[&str] = &[
    topic::PROJECTS,
    topic::WORKSPACES,
    topic::SESSIONS,
    topic::MESSAGES,
    topic::PINS,
    topic::WIKI,
    topic::TIMELINE,
    topic::ENV,
    topic::SETTINGS,
    topic::COLLAB,
    runwick_core::api::roles::TOPIC,
    topic::SYNC,
];

pub fn status(host: &Host) -> Result<SyncStatus, RpcError> {
    let store = &host.store;
    let cfg = state::repo(store);
    let last = state::last(store);
    let pending = match &cfg {
        Some(_) => Some(state::fingerprint(store).map_err(db_err)? != last.fingerprint.clone().unwrap_or_default()),
        None => None,
    };
    Ok(SyncStatus {
        host_id: host.host_id(),
        device_name: host.info.device_name.clone(),
        connected_at: cfg.as_ref().map(|c| c.connected_at),
        repo: cfg.map(|c| c.repo),
        running: service().step(),
        last_success: last.last_success,
        last_attempt: last.last_attempt,
        last_error: last.last_error,
        pending,
        ahead: last.ahead,
        behind: last.behind,
        auto: state::auto(store),
        last_report: last.last_report,
    })
}

/// Run one sync now (blocking). Records the outcome for the status.
pub fn run_blocking(host: &Host) -> Result<SyncReport, RpcError> {
    run_until(host, None)
}

/// One sync that stops at `deadline` (see [`Engine::deadline`]).
fn run_until(host: &Host, deadline: Option<std::time::Instant>) -> Result<SyncReport, RpcError> {
    let svc = service();
    let _running = svc.begin()?;
    host.bus.changed(&[topic::SYNC]);
    let out = run_engine(host, svc, deadline);
    host.bus.changed(&[topic::SYNC]);
    out
}

fn run_engine(host: &Host, svc: &Service, deadline: Option<std::time::Instant>) -> Result<SyncReport, RpcError> {
    let store = &host.store;
    let cfg = state::repo(store).ok_or_else(|| RpcError::invalid("연결된 동기화 저장소가 없어요"))?;
    let mut last = state::last(store);
    let now = runwick_core::now();
    last.last_attempt = Some(now);
    state::set_last(store, &last).map_err(db_err)?;

    let fail = |last: &mut state::LastRun, step: Option<SyncStep>, e: GitError| -> RpcError {
        last.last_error = Some(SyncFailure { kind: e.kind, step, message: e.message.clone(), at: runwick_core::now() });
        let _ = state::set_last(store, last);
        git_err(e)
    };
    let git = match Git::new(GitEnv::login()) {
        Ok(g) => g,
        Err(e) => return Err(fail(&mut last, None, e)),
    };
    let home = paths::home();
    let wiki_root = paths::wiki_dir();
    let device = device(host);
    let cx = Cx { store, home: &home, wiki_root: &wiki_root, env: crate::env::service(), device: &device };
    let wc = WorkingCopy { dir: paths::sync_dir(), branch: cfg.repo.branch.clone() };
    if !wc.dir.join(".git").exists() {
        let e = GitError::other("동기화용 복사본이 없어요. 연결을 끊고 저장소를 다시 연결해 주세요");
        return Err(fail(&mut last, None, e));
    }
    let step = |s: SyncStep| {
        svc.set_step(s);
        host.bus.changed(&[topic::SYNC]);
    };
    let engine = Engine { cx: &cx, git: &git, wc: &wc, step: &step, deadline };
    match engine.run() {
        Ok(out) => {
            last.last_success = Some(out.report.finished_at);
            last.last_error = None;
            last.last_report = Some(out.report.clone());
            (last.ahead, last.behind) = engine.ahead_behind().map(|(a, b)| (Some(a), Some(b))).unwrap_or((None, None));
            last.fingerprint = out.fingerprint.clone();
            state::set_last(store, &last).map_err(db_err)?;
            let topics: Vec<&str> = out.topics.into_iter().collect();
            host.bus.changed(&topics);
            tracing::info!(
                imported = out.report.imported.iter().map(|k| k.count).sum::<u64>(),
                exported = out.report.exported.iter().map(|k| k.count).sum::<u64>(),
                conflicts = out.report.conflicts.len(),
                "sync finished"
            );
            Ok(out.report)
        }
        Err(f) => {
            (last.ahead, last.behind) = engine.ahead_behind().map(|(a, b)| (Some(a), Some(b))).unwrap_or((None, None));
            host.bus.changed(DATA_TOPICS);
            tracing::warn!(step = ?f.step, kind = ?f.error.kind, "sync failed");
            Err(fail(&mut last, Some(f.step), f.error))
        }
    }
}

async fn blocking<T: Send + 'static>(
    host: &Arc<Host>,
    f: impl FnOnce(&Host) -> Result<T, RpcError> + Send + 'static,
) -> Result<T, RpcError> {
    let host = host.clone();
    tokio::task::spawn_blocking(move || f(&host)).await.map_err(RpcError::internal)?
}

fn preview_dir() -> std::path::PathBuf {
    paths::home().join("sync-preview")
}

pub fn register(r: &mut Router) {
    r.add::<SyncStatusGet, _, _>(|ctx: Ctx, _: ()| async move { blocking(&ctx.host, status).await });
    r.add::<SyncGhStatus, _, _>(|ctx: Ctx, _: ()| async move {
        blocking(&ctx.host, |_| Ok(gh::status(&GitEnv::login()))).await
    });
    r.add::<SyncGhRepos, _, _>(|ctx: Ctx, _: ()| async move {
        blocking(&ctx.host, |_| gh::repos(&GitEnv::login()).map_err(git_err)).await
    });
    r.add::<SyncCreateRepo, _, _>(|ctx: Ctx, p: NewRepo| async move {
        interactive(&ctx)?;
        blocking(&ctx.host, move |_| gh::create(&GitEnv::login(), &p.name).map_err(git_err)).await
    });
    r.add::<SyncPreview, _, _>(|ctx: Ctx, p: RepoUrl| async move {
        interactive(&ctx)?;
        blocking(&ctx.host, move |h| {
            let git = Git::new(GitEnv::login()).map_err(git_err)?;
            let dir = preview_dir().join(Uuid::new_v4().to_string());
            let (out, pre) =
                repo::preview(&git, &h.store, &paths::home(), &device(h), &p.url, &dir).map_err(git_err)?;
            let mut previews =
                service().previews.lock().map_err(|_| RpcError::internal("미리보기를 보관하지 못했어요"))?;
            // Only the newest preview is kept.
            for (_, old) in previews.drain() {
                let _ = std::fs::remove_dir_all(&old.dir);
            }
            previews.insert(out.id, pre);
            Ok(out)
        })
        .await
    });
    r.add::<SyncDiscardPreview, _, _>(|_ctx: Ctx, p: PreviewId| async move {
        if let Some(pre) = service().previews.lock().ok().and_then(|mut m| m.remove(&p.id)) {
            let _ = std::fs::remove_dir_all(&pre.dir);
        }
        Ok(())
    });
    r.add::<SyncConnect, _, _>(|ctx: Ctx, p: PreviewId| async move {
        interactive(&ctx)?;
        blocking(&ctx.host, move |h| connect(h, p.id)).await
    });
    r.add::<SyncRun, _, _>(|ctx: Ctx, _: ()| async move { blocking(&ctx.host, run_blocking).await });
    r.add::<SyncSetAuto, _, _>(|ctx: Ctx, a: AutoSync| async move {
        let a = AutoSync { enabled: a.enabled, interval_min: a.interval_min.clamp(AUTO_INTERVALS[0], 24 * 60) };
        state::set_auto(&ctx.host.store, &a).map_err(db_err)?;
        ctx.host.bus.changed(&[topic::SYNC]);
        blocking(&ctx.host, status).await
    });
    r.add::<SyncHistory, _, _>(|ctx: Ctx, _: ()| async move {
        blocking(&ctx.host, |h| {
            if state::repo(&h.store).is_none() {
                return Ok(Vec::new());
            }
            let git = Git::new(GitEnv::login()).map_err(git_err)?;
            let names = h.store.with(|c| kinds::Names::load(c, &device(h))).map_err(db_err)?;
            repo::history(&git, &paths::sync_dir(), &names).map_err(git_err)
        })
        .await
    });
    r.add::<SyncDisconnect, _, _>(|ctx: Ctx, _: ()| async move {
        interactive(&ctx)?;
        let svc = service();
        let _running = svc.begin()?;
        state::disconnect(&ctx.host.store).map_err(db_err)?;
        let dir = paths::sync_dir();
        if dir.exists()
            && let Err(e) = std::fs::remove_dir_all(&dir)
        {
            tracing::warn!("sync working copy was not removed: {e}");
        }
        ctx.host.bus.changed(&[topic::SYNC, topic::PROJECTS]);
        Ok(())
    });
    r.add::<SyncConflicts, _, _>(|ctx: Ctx, _: ()| async move {
        let me = device(&ctx.host);
        ctx.host.store.with(|c| kinds::conflicts::list(c, &me)).map_err(db_err)
    });
    r.add::<SyncConflictDismiss, _, _>(|ctx: Ctx, p: ConflictRef| async move {
        let home = paths::home();
        let wiki_root = paths::wiki_dir();
        let device = device(&ctx.host);
        let cx = Cx {
            store: &ctx.host.store,
            home: &home,
            wiki_root: &wiki_root,
            env: crate::env::service(),
            device: &device,
        };
        ctx.host.store.with(|c| kinds::conflicts::settle(&cx, c, p.id, "dismissed")).map_err(db_err)?;
        ctx.host.bus.changed(&[topic::SYNC]);
        Ok(())
    });
    r.add::<SyncConflictRestore, _, _>(|ctx: Ctx, p: ConflictRef| async move {
        interactive(&ctx)?;
        let topics = blocking(&ctx.host, move |h| restore_conflict(h, p.id)).await?;
        ctx.host.bus.changed(&topics);
        Ok(())
    });
    r.add::<SyncExposedKeys, _, _>(|ctx: Ctx, _: ()| async move { state::exposed(&ctx.host.store).map_err(db_err) });
    r.add::<SyncStagedVars, _, _>(|ctx: Ctx, _: ()| async move {
        kinds::env::staged(&ctx.host.store, &device(&ctx.host)).map_err(db_err)
    });
    r.add::<SyncDecideStaged, _, _>(|ctx: Ctx, p: StagedDecision| async move {
        interactive(&ctx)?;
        blocking(&ctx.host, move |h| kinds::env::decide(&h.store, crate::env::service(), &p.key, p.accept)).await?;
        ctx.host.bus.changed(&[topic::ENV, topic::SYNC]);
        Ok(())
    });
    r.add::<SyncUnlinked, _, _>(|ctx: Ctx, _: ()| async move {
        let home = paths::home();
        ctx.host.store.with(|c| kinds::projects::unlinked(&home, c, &device(&ctx.host))).map_err(db_err)
    });
}

/// Put back content a conflict did not keep (see `kinds::conflicts::restore`).
fn restore_conflict(host: &Host, id: Uuid) -> Result<Vec<&'static str>, RpcError> {
    let home = paths::home();
    let wiki_root = paths::wiki_dir();
    let device = device(host);
    let cx = Cx { store: &host.store, home: &home, wiki_root: &wiki_root, env: crate::env::service(), device: &device };
    let kind = kinds::conflicts::restore(&cx, id)?;
    let mut topics = vec![topic::SYNC];
    topics.extend_from_slice(import::topics_of(kind));
    if kind == SyncKind::Env {
        topics.push(topic::ENV);
    }
    Ok(topics)
}

/// Only Runwick windows choose repositories (agent CLIs may read the status).
fn interactive(ctx: &Ctx) -> Result<(), RpcError> {
    if ctx.client.is_interactive() {
        Ok(())
    } else {
        Err(RpcError::new(codes::DENIED, "동기화 저장소는 Runwick 창에서만 바꿀 수 있어요"))
    }
}

fn connect(host: &Host, id: Uuid) -> Result<SyncReport, RpcError> {
    if state::repo(&host.store).is_some() {
        return Err(RpcError::new(codes::CONFLICT, "이미 연결된 저장소가 있어요. 먼저 연결을 끊어 주세요"));
    }
    let pre = service()
        .previews
        .lock()
        .ok()
        .and_then(|mut m| m.remove(&id))
        .ok_or_else(|| RpcError::not_found("저장소 확인이 끝났거나 오래됐어요. 다시 확인해 주세요"))?;
    {
        let _running = service().begin()?;
        let installed = repo::install(&pre, &paths::sync_dir());
        if installed.is_err() {
            let _ = std::fs::remove_dir_all(&pre.dir);
        }
        installed.map_err(git_err)?;
        state::set_base(&host.store, None).map_err(db_err)?;
        state::clear_marks(&host.store).map_err(db_err)?;
        state::set_last(&host.store, &state::LastRun::default()).map_err(db_err)?;
        state::set_repo(&host.store, &state::RepoConfig { repo: pre.repo.clone(), connected_at: runwick_core::now() })
            .map_err(db_err)?;
    }
    host.bus.changed(&[topic::SYNC]);
    run_blocking(host)
        .map_err(|e| RpcError::new(e.code, format!("저장소는 연결했지만 첫 동기화를 마치지 못했어요: {}", e.message)))
}

/// Start auto sync and clear clones left by a previous run.
pub fn boot(host: &Arc<Host>) {
    let _ = std::fs::remove_dir_all(preview_dir());
    let host = host.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(60)).await;
            let h = host.clone();
            let due = tokio::task::spawn_blocking(move || auto_due(&h.store)).await.unwrap_or(false);
            if due {
                let h = host.clone();
                let res = tokio::task::spawn_blocking(move || run_blocking(&h)).await;
                if let Ok(Err(e)) = res {
                    tracing::info!(code = %e.code, "auto sync did not finish");
                }
            }
        }
    });
}

/// Auto sync is on and either local records changed or it has been a while.
fn auto_due(store: &Store) -> bool {
    let auto = state::auto(store);
    if !auto.enabled || state::repo(store).is_none() || service().is_running() {
        return false;
    }
    let last = state::last(store);
    let now = runwick_core::now();
    let interval = chrono::Duration::minutes(auto.interval_min.max(1) as i64);
    if last.last_attempt.is_some_and(|t| now - t < interval) {
        return false;
    }
    let pending = state::fingerprint(store).ok() != last.fingerprint;
    let quiet = chrono::Duration::minutes((auto.interval_min as i64).max(30));
    let stale = last.last_success.is_none_or(|t| now - t >= quiet);
    pending || stale
}

/// Longest a shutdown waits for pending changes to be pushed.
const SHUTDOWN_SYNC: Duration = Duration::from_secs(8);

/// After runs are stopped, before the host exits: push pending changes when auto sync is on,
/// within [`SHUTDOWN_SYNC`].
pub async fn on_shutdown(host: &Arc<Host>) {
    let h = host.clone();
    let pending = tokio::task::spawn_blocking(move || {
        let store = &h.store;
        state::auto(store).enabled
            && state::repo(store).is_some()
            && !service().is_running()
            && state::fingerprint(store).ok() != state::last(store).fingerprint
    })
    .await
    .unwrap_or(false);
    if !pending {
        return;
    }
    // The engine stops itself at the deadline (network commands are cut, nothing is committed
    // after it); the outer limit only guards against a stuck thread.
    let h = host.clone();
    let deadline = std::time::Instant::now() + SHUTDOWN_SYNC;
    let job = tokio::task::spawn_blocking(move || run_until(&h, Some(deadline)));
    match tokio::time::timeout(SHUTDOWN_SYNC + Duration::from_secs(1), job).await {
        Ok(Ok(Ok(_))) => tracing::info!("pending changes synced before shutdown"),
        Ok(Ok(Err(e))) => {
            tracing::warn!(code = %e.code, "sync before shutdown stopped; changes stay pending for the next start")
        }
        _ => tracing::warn!("sync before shutdown did not finish in time; changes stay pending for the next start"),
    }
}
