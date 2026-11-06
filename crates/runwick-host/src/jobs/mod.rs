//! Automatic and requested AI work (S13 자동 작업): wiki proposals, wiki drafts and timeline
//! day summaries, run through the person's own CLI.
//!
//! - `conditions`: when automatic work may start (opt-in, change volume, idle, power)
//! - `runner`: scheduling, limits, stop, retry with backoff; `ai`: the CLI call
//! - `work`/`prompts`: the records sent and how answers become proposals or summaries
//! - `records`: the single adapter to the RECORDS track (messages, timeline)

mod ai;
pub mod conditions;
mod power;
mod prompts;
pub mod records;
mod runner;
mod store;
mod work;

use crate::host::{Ctx, Host};
use crate::router::Router;
use crate::store::{Migration, db_err};
use runwick_core::api::jobs::*;
use runwick_core::rpc::{RpcError, codes, topic};
use std::sync::Arc;

pub const MIGRATIONS: &[Migration] = &[(
    "0102_jobs",
    "CREATE TABLE jobs (
        id BLOB PRIMARY KEY,
        kind TEXT NOT NULL,
        project_id BLOB NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
        -- auto | requested
        origin TEXT NOT NULL,
        reason TEXT NOT NULL,
        target TEXT NOT NULL DEFAULT '{}',
        status TEXT NOT NULL,
        held_reason TEXT,
        attempts INTEGER NOT NULL DEFAULT 0,
        retry_at TEXT,
        created_at TEXT NOT NULL,
        started_at TEXT,
        finished_at TEXT,
        result TEXT,
        error TEXT,
        provider TEXT,
        model TEXT,
        duration_ms INTEGER
    );
    CREATE INDEX jobs_project ON jobs(project_id, created_at);
    CREATE INDEX jobs_status ON jobs(status);
    CREATE TABLE automation (
        project_id BLOB PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
        wiki INTEGER NOT NULL DEFAULT 0,
        timeline INTEGER NOT NULL DEFAULT 0,
        timeline_since TEXT,
        updated_at TEXT NOT NULL
    );
    CREATE TABLE job_cursors (
        project_id BLOB NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
        kind TEXT NOT NULL,
        key TEXT NOT NULL DEFAULT '',
        value TEXT NOT NULL,
        updated_at TEXT NOT NULL,
        PRIMARY KEY (project_id, kind, key)
    );",
)];

/// Called once after the host starts: nothing left over from before runs by itself.
pub fn boot(host: &Arc<Host>) {
    match host.store.with(|c| store::stop_unfinished(c, &runwick_core::now())) {
        Ok(n) if n > 0 => {
            tracing::info!(count = n, "jobs left from before the restart were stopped")
        }
        Ok(_) => {}
        Err(e) => tracing::warn!("jobs: {e}"),
    }
    tokio::spawn(runner::run(host.clone()));
}

fn get(host: &Host, id: JobId) -> Result<Job, RpcError> {
    host.store.with(|c| store::get(c, id)).map_err(db_err)?.ok_or_else(|| RpcError::not_found("작업을 찾을 수 없어요"))
}

fn changed(host: &Arc<Host>) {
    host.bus.changed(&[topic::JOBS]);
    runner::of(host).wake();
}

fn stop_job(host: &Arc<Host>, job: &Job, why: &str) -> Result<(), RpcError> {
    if job.status == JobStatus::Running && runner::of(host).stop(job.id) {
        return Ok(()); // the runner records the end
    }
    let f = store::Finish {
        status: JobStatus::Stopped,
        result: None,
        error: Some(why),
        provider: None,
        model: None,
        duration_ms: None,
        retry_at: None,
    };
    host.store.with(|c| store::finish(c, job.id, &f, &runwick_core::now())).map_err(db_err)
}

fn automation_row(host: &Host, p: &runwick_core::api::projects::Project) -> Result<ProjectAutomation, RpcError> {
    host.store
        .with(|c| {
            let sw = store::switches(c, p.id)?;
            Ok(ProjectAutomation {
                project_id: p.id,
                project_name: p.name.clone(),
                wiki: sw.wiki,
                timeline: sw.timeline,
                last_wiki_at: store::last_finished(c, p.id, JobKind::WikiProposals)?,
                last_timeline_at: store::last_finished(c, p.id, JobKind::TimelineSummary)?,
            })
        })
        .map_err(db_err)
}

pub fn register(r: &mut Router) {
    r.add::<JobsList, _, _>(|ctx: Ctx, q: JobQuery| async move {
        ctx.host.store.with(|c| store::list(c, &q)).map_err(db_err)
    });
    r.add::<JobsOverview, _, _>(|ctx: Ctx, _: ()| async move {
        let runner = runner::of(&ctx.host);
        let (power, checked) = runner.power(runwick_core::now()).await;
        let waiting =
            ctx.host.store.with(|c| store::with_status(c, &[JobStatus::Queued, JobStatus::Held])).map_err(db_err)?;
        let h = ctx.host.clone();
        let provider = tokio::task::spawn_blocking(move || ai::setup(&h).provider).await.ok().flatten();
        Ok(Overview {
            power,
            power_checked_at: checked,
            running: runner.counts().0,
            queued: waiting.iter().filter(|j| j.status == JobStatus::Queued).count() as u32,
            held: waiting.iter().filter(|j| j.status == JobStatus::Held).count() as u32,
            max_running: runner::MAX_RUNNING,
            max_auto: runner::MAX_AUTO,
            provider: provider.map(|p| p.label().to_string()),
        })
    });
    r.add::<JobsRequest, _, _>(|ctx: Ctx, req: RequestJob| async move {
        let host = &ctx.host;
        let project = crate::projects::get_project(&host.store, req.project_id)?;
        let mut target = req.target.clone();
        match req.kind {
            JobKind::WikiDraft if target.sessions.is_empty() => {
                return Err(RpcError::invalid("초안을 만들 세션을 하나 이상 골라 주세요"));
            }
            JobKind::TimelineSummary => {
                let day = target.day.clone().unwrap_or_default();
                if chrono::NaiveDate::parse_from_str(&day, "%Y-%m-%d").is_err() {
                    return Err(RpcError::invalid("요약할 날짜가 올바르지 않아요"));
                }
                target.sessions.clear();
            }
            _ => {}
        }
        let day = (req.kind == JobKind::TimelineSummary).then(|| target.day.clone()).flatten();
        let existing = host.store.with(|c| store::active(c, project.id, req.kind, day.as_deref())).map_err(db_err)?;
        let id = match existing {
            Some(j) if j.status == JobStatus::Running => j.id,
            // "지금 정리" on waiting automatic work: run it now as a request.
            Some(j) if req.kind != JobKind::WikiDraft => {
                host.store
                    .with(|c| {
                        store::set_waiting(c, j.id, JobStatus::Queued, None, Some("직접 요청"))?;
                        store::requeue(c, j.id)
                    })
                    .map_err(db_err)?;
                j.id
            }
            _ => host
                .store
                .with(|c| {
                    store::insert(
                        c,
                        &store::NewJob {
                            kind: req.kind,
                            project_id: project.id,
                            trigger: Trigger::Requested,
                            reason: "직접 요청",
                            target: &target,
                            status: JobStatus::Queued,
                            held_reason: None,
                        },
                    )
                })
                .map_err(db_err)?,
        };
        changed(host);
        get(host, id)
    });
    r.add::<JobsStop, _, _>(|ctx: Ctx, p: JobRef| async move {
        let job = get(&ctx.host, p.id)?;
        if !job.status.is_active() {
            return Err(RpcError::invalid("이미 끝난 작업이에요"));
        }
        stop_job(&ctx.host, &job, "멈췄어요")?;
        changed(&ctx.host);
        get(&ctx.host, p.id)
    });
    r.add::<JobsRetry, _, _>(|ctx: Ctx, p: JobRef| async move {
        let job = get(&ctx.host, p.id)?;
        if !matches!(job.status, JobStatus::Failed | JobStatus::Stopped) {
            return Err(RpcError::new(codes::INVALID, "실패하거나 멈춘 작업만 다시 실행할 수 있어요"));
        }
        ctx.host.store.with(|c| store::requeue(c, p.id)).map_err(db_err)?;
        changed(&ctx.host);
        get(&ctx.host, p.id)
    });
    r.add::<AutomationList, _, _>(|ctx: Ctx, _: ()| async move {
        let projects = crate::projects::list_projects(&ctx.host.store).map_err(db_err)?;
        projects.iter().filter(|p| !p.archived).map(|p| automation_row(&ctx.host, p)).collect()
    });
    r.add::<AutomationSet, _, _>(|ctx: Ctx, s: SetAutomation| async move {
        let host = &ctx.host;
        let project = crate::projects::get_project(&host.store, s.project_id)?;
        host.store
            .with(|c| store::set_switches(c, project.id, s.wiki, s.timeline, &runwick_core::now()))
            .map_err(db_err)?;
        let turned_off: Vec<JobKind> = [(s.wiki, JobKind::WikiProposals), (s.timeline, JobKind::TimelineSummary)]
            .into_iter()
            .filter(|(v, _)| *v == Some(false))
            .map(|(_, k)| k)
            .collect();
        if !turned_off.is_empty() {
            let active = host
                .store
                .with(|c| store::list(c, &JobQuery { project_id: Some(project.id), active_only: true, limit: 100 }))
                .map_err(db_err)?;
            for j in active.iter().filter(|j| j.trigger == Trigger::Auto && turned_off.contains(&j.kind)) {
                stop_job(host, j, "자동 정리를 꺼서 멈췄어요")?;
            }
        }
        changed(host);
        automation_row(host, &project)
    });
}

#[cfg(test)]
mod tests;
