//! The job runner: decides which automatic work is due, starts queued jobs within the limits
//! and stops them on request. It sleeps until something is due or it is woken (a request,
//! a stop, a settings change); with no project opted in it does not wake on its own.

use super::conditions::{self, DayFacts, Decision, WikiFacts};
use super::{ai, power, records as rec, store, work};
use crate::host::Host;
use runwick_core::Ts;
use runwick_core::api::jobs::*;
use runwick_core::rpc::topic;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use uuid::Uuid;

/// Jobs running at once, and how many of those may be automatic.
pub const MAX_RUNNING: u32 = 2;
pub const MAX_AUTO: u32 = 1;
/// How often conditions are re-checked while some project has automation on.
const CHECK_EVERY: Duration = Duration::from_secs(5 * 60);
const POWER_TTL: chrono::Duration = chrono::Duration::seconds(60);
/// Finished days older than this are never summarized automatically.
const TIMELINE_LOOKBACK_DAYS: i64 = 7;

#[derive(Default)]
struct Running {
    pid: Option<u32>,
    auto: bool,
    stop: bool,
}

#[derive(Default)]
pub struct Runner {
    wake: tokio::sync::Notify,
    running: Mutex<HashMap<Uuid, Running>>,
    power: Mutex<Option<(PowerSource, Ts)>>,
}

static RUNNERS: Mutex<Vec<(usize, Arc<Runner>)>> = Mutex::new(Vec::new());

/// The runner of this host (one per host instance).
pub fn of(host: &Arc<Host>) -> Arc<Runner> {
    let key = Arc::as_ptr(host) as usize;
    let mut all = RUNNERS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((_, r)) = all.iter().find(|(k, _)| *k == key) {
        return r.clone();
    }
    let r = Arc::new(Runner::default());
    all.push((key, r.clone()));
    r
}

impl Runner {
    pub fn wake(&self) {
        self.wake.notify_one();
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<Uuid, Running>> {
        self.running.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn counts(&self) -> (u32, u32) {
        let r = self.lock();
        (r.len() as u32, r.values().filter(|j| j.auto).count() as u32)
    }

    /// Ask a running job to stop and kill its CLI. Returns false when it is not running here.
    pub fn stop(&self, id: Uuid) -> bool {
        let mut r = self.lock();
        let Some(j) = r.get_mut(&id) else {
            return false;
        };
        j.stop = true;
        ai::kill_group(j.pid);
        true
    }

    /// Power source, read at most once a minute.
    pub async fn power(&self, now: Ts) -> (PowerSource, Option<Ts>) {
        if let Some((p, at)) = *self.power.lock().unwrap_or_else(|e| e.into_inner())
            && now - at < POWER_TTL
        {
            return (p, Some(at));
        }
        let p = tokio::task::spawn_blocking(power::read).await.unwrap_or(PowerSource::Unknown);
        *self.power.lock().unwrap_or_else(|e| e.into_inner()) = Some((p, now));
        (p, Some(now))
    }
}

pub async fn run(host: Arc<Host>) {
    let runner = of(&host);
    loop {
        let next = tick(&host, &runner).await;
        match next {
            Some(d) => {
                tokio::select! {
                    _ = tokio::time::sleep(d) => {}
                    _ = runner.wake.notified() => {}
                }
            }
            None => runner.wake.notified().await,
        }
    }
}

fn db<T>(host: &Host, f: impl FnOnce(&rusqlite::Connection) -> rusqlite::Result<T>) -> Option<T> {
    match host.store.with(f) {
        Ok(v) => Some(v),
        Err(e) => {
            tracing::warn!("jobs: {e}");
            None
        }
    }
}

/// One scheduling pass. Returns when to look again.
async fn tick(host: &Arc<Host>, runner: &Arc<Runner>) -> Option<Duration> {
    let now = runwick_core::now();
    let opted = db(host, store::opted_in).unwrap_or_default();
    let waiting = db(host, |c| store::with_status(c, &[JobStatus::Queued, JobStatus::Held])).unwrap_or_default();
    let mut changed = false;
    if !opted.is_empty() || waiting.iter().any(|j| j.trigger == Trigger::Auto) {
        let (power, _) = runner.power(now).await;
        for project in &opted {
            changed |= plan_project(host, *project, power, now);
        }
        changed |= release_held(host, power);
    }
    changed |= start_due(host, runner, now).await;
    if changed {
        host.bus.changed(&[topic::JOBS]);
    }
    let next_retry = db(host, |c| store::with_status(c, &[JobStatus::Queued]))
        .unwrap_or_default()
        .iter()
        .filter_map(|j| j.retry_at)
        .min()
        .map(|t| (t - now).to_std().unwrap_or(Duration::from_secs(1)).max(Duration::from_secs(1)));
    let periodic = (!opted.is_empty()).then_some(CHECK_EVERY);
    match (next_retry, periodic) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

/// Create, hold or release this project's automatic jobs. Returns true when rows changed.
fn plan_project(host: &Host, project: Uuid, power: PowerSource, now: Ts) -> bool {
    let Some(sw) = db(host, |c| store::switches(c, project)) else {
        return false;
    };
    let last_activity = db(host, |c| store::last_activity(c, project)).flatten();
    let mut changed = false;
    if sw.wiki {
        let since = work::wiki_cursor(host, project).unwrap_or(now - chrono::Duration::days(14));
        let new_messages = match rec::messages_since(host, project, &since, conditions::MIN_NEW_MESSAGES as usize * 5) {
            Ok(m) => m.len() as u64,
            Err(_) => 0,
        };
        let facts = WikiFacts {
            new_messages,
            last_job_at: db(host, |c| store::last_finished(c, project, JobKind::WikiProposals)).flatten(),
            open_proposals: crate::wiki::ops::open_proposals(&host.store, project).unwrap_or(0),
            last_activity_at: last_activity,
        };
        changed |= settle(host, project, JobKind::WikiProposals, None, conditions::wiki(&facts, power, now));
    }
    if sw.timeline {
        let today = chrono::Local::now().date_naive();
        let start = sw
            .timeline_since
            .map(|t| t.with_timezone(&chrono::Local).date_naive() - chrono::Duration::days(1))
            .unwrap_or(today)
            .max(today - chrono::Duration::days(TIMELINE_LOOKBACK_DAYS));
        let mut day = start;
        while day < today {
            let key = day.to_string();
            if let Ok(d) = rec::day(host, project, day) {
                let facts = DayFacts {
                    day: key.clone(),
                    records: d.count,
                    fingerprint: d.fingerprint,
                    summarized: db(host, |c| store::cursor(c, project, JobKind::TimelineSummary, &key)).flatten(),
                    last_activity_at: last_activity,
                };
                let decision = conditions::timeline(&facts, power, now);
                if decision != Decision::Skip {
                    changed |= settle(host, project, JobKind::TimelineSummary, Some(&key), decision);
                    break; // one day at a time
                }
            }
            day += chrono::Duration::days(1);
        }
    }
    changed
}

/// Make the stored automatic job match a decision. Requested jobs are never touched.
pub(super) fn settle(host: &Host, project: Uuid, kind: JobKind, day: Option<&str>, d: Decision) -> bool {
    let existing =
        db(host, |c| store::active(c, project, kind, if kind == JobKind::TimelineSummary { day } else { None }))
            .flatten();
    if existing.as_ref().is_some_and(|j| j.trigger == Trigger::Requested || j.status == JobStatus::Running) {
        return false;
    }
    if kind == JobKind::TimelineSummary
        && existing.is_none()
        && db(host, |c| store::active(c, project, kind, None)).flatten().is_some()
    {
        return false; // another day is in progress for this project
    }
    let target = JobTarget { day: day.map(str::to_string), sessions: Vec::new() };
    let res = match (d, existing) {
        (Decision::Skip, Some(j)) => host.store.with(|c| {
            let f = store::Finish {
                status: JobStatus::Stopped,
                result: Some("조건이 바뀌어 이번에는 건너뛰었어요"),
                error: None,
                provider: None,
                model: None,
                duration_ms: None,
                retry_at: None,
            };
            store::finish(c, j.id, &f, &runwick_core::now())
        }),
        (Decision::Skip, None) => return false,
        (Decision::Hold { reason, held }, Some(j)) => {
            if j.status == JobStatus::Held && j.held_reason.as_deref() == Some(held) && j.reason == reason {
                return false;
            }
            host.store.with(|c| store::set_waiting(c, j.id, JobStatus::Held, Some(held), Some(&reason)))
        }
        (Decision::Hold { reason, held }, None) => host
            .store
            .with(|c| {
                store::insert(
                    c,
                    &store::NewJob {
                        kind,
                        project_id: project,
                        trigger: Trigger::Auto,
                        reason: &reason,
                        target: &target,
                        status: JobStatus::Held,
                        held_reason: Some(held),
                    },
                )
            })
            .map(|_| ()),
        (Decision::Run { reason }, Some(j)) => {
            if j.status == JobStatus::Queued {
                return false;
            }
            host.store.with(|c| store::set_waiting(c, j.id, JobStatus::Queued, None, Some(&reason)))
        }
        (Decision::Run { reason }, None) => host
            .store
            .with(|c| {
                store::insert(
                    c,
                    &store::NewJob {
                        kind,
                        project_id: project,
                        trigger: Trigger::Auto,
                        reason: &reason,
                        target: &target,
                        status: JobStatus::Queued,
                        held_reason: None,
                    },
                )
            })
            .map(|_| ()),
    };
    if let Err(e) = res {
        tracing::warn!("jobs: {e}");
    }
    true
}

/// Battery holds apply to queued automatic jobs too; they come back when power returns.
pub(super) fn release_held(host: &Host, power: PowerSource) -> bool {
    let Some(jobs) = db(host, |c| store::with_status(c, &[JobStatus::Queued, JobStatus::Held])) else {
        return false;
    };
    let mut changed = false;
    for j in jobs.iter().filter(|j| j.trigger == Trigger::Auto) {
        let on_battery = power == PowerSource::Battery;
        let res = match (j.status, j.held_reason.as_deref()) {
            (JobStatus::Queued, _) if on_battery => {
                host.store.with(|c| store::set_waiting(c, j.id, JobStatus::Held, Some(conditions::HELD_BATTERY), None))
            }
            (JobStatus::Held, Some(conditions::HELD_BATTERY)) if !on_battery => {
                host.store.with(|c| store::set_waiting(c, j.id, JobStatus::Queued, None, None))
            }
            _ => continue,
        };
        changed |= res.is_ok();
    }
    changed
}

async fn start_due(host: &Arc<Host>, runner: &Arc<Runner>, now: Ts) -> bool {
    let Some(queued) = db(host, |c| store::with_status(c, &[JobStatus::Queued])) else {
        return false;
    };
    let mut started = false;
    for job in queued {
        if job.retry_at.is_some_and(|t| t > now) {
            continue;
        }
        let (all, auto) = runner.counts();
        let is_auto = job.trigger == Trigger::Auto;
        if all >= MAX_RUNNING || (is_auto && auto >= MAX_AUTO) {
            continue;
        }
        runner.lock().insert(job.id, Running { pid: None, auto: is_auto, stop: false });
        if db(host, |c| store::mark_started(c, job.id, &now)).is_none() {
            runner.lock().remove(&job.id);
            continue;
        }
        started = true;
        tokio::spawn(execute(host.clone(), runner.clone(), job));
    }
    started
}

struct Failure {
    message: String,
    retry: bool,
}

fn fail(message: impl Into<String>, retry: bool) -> Failure {
    Failure { message: message.into(), retry }
}

async fn execute(host: Arc<Host>, runner: Arc<Runner>, job: Job) {
    host.bus.changed(&[topic::JOBS]);
    let mut provider_label: Option<&'static str> = None;
    let mut model = None;
    let mut duration = None;
    let outcome: Result<String, Failure> = async {
        let h = host.clone();
        let setup = tokio::task::spawn_blocking(move || ai::setup(&h)).await.map_err(|e| fail(e.to_string(), false))?;
        let provider = setup
            .provider
            .clone()
            .ok_or_else(|| fail("Claude Code나 Codex를 찾지 못했어요. 설정 › CLI 도구에서 확인해 주세요", false))?;
        provider_label = Some(provider.label());
        let h = host.clone();
        let j = job.clone();
        let prepared = tokio::task::spawn_blocking(move || work::prepare(&h, &runwick_core::paths::wiki_dir(), &j))
            .await
            .map_err(|e| fail(e.to_string(), false))?
            .map_err(|e| fail(e, false))?;
        let (prompt, basis) = match prepared {
            work::Prepared::Nothing(result) => {
                provider_label = None;
                return Ok(result);
            }
            work::Prepared::Ask { prompt, basis } => (prompt, basis),
        };
        let workdir = runwick_core::paths::home().join("jobs").join(job.id.to_string());
        let id = job.id;
        let r2 = runner.clone();
        let out = ai::run(&provider, &setup.env, &prompt, &workdir, move |pid| {
            if let Some(j) = r2.lock().get_mut(&id) {
                j.pid = Some(pid);
            }
        })
        .await;
        let _ = std::fs::remove_dir_all(&workdir);
        let out = out.map_err(|e| fail(e, true))?;
        model = out.model.clone();
        duration = Some(out.duration_ms);
        let h = host.clone();
        let j = job.clone();
        let label = provider.label();
        tokio::task::spawn_blocking(move || {
            work::complete(&h, &runwick_core::paths::wiki_dir(), &j, &basis, &out.text, label)
        })
        .await
        .map_err(|e| fail(e.to_string(), false))?
        .map_err(|e| fail(e, true))
    }
    .await;
    let stopped = runner.lock().remove(&job.id).is_some_and(|r| r.stop);
    let now = runwick_core::now();
    let attempts = job.attempts + 1;
    let (status, result, error, retry_at) = match outcome {
        _ if stopped => (JobStatus::Stopped, None, Some("멈췄어요".to_string()), None),
        Ok(r) => (JobStatus::Done, Some(r), None, None),
        Err(f) => {
            let retry_at = f.retry.then(|| conditions::retry_delay(attempts)).flatten().map(|d| now + d);
            (JobStatus::Failed, None, Some(f.message), retry_at)
        }
    };
    let finish = store::Finish {
        status,
        result: result.as_deref(),
        error: error.as_deref(),
        provider: provider_label,
        model: model.as_deref(),
        duration_ms: duration,
        retry_at,
    };
    if let Err(e) = host.store.with(|c| store::finish(c, job.id, &finish, &now)) {
        tracing::warn!("jobs: cannot record the end of {}: {e}", job.id);
    }
    let mut topics = vec![topic::JOBS];
    if status == JobStatus::Done {
        topics.extend([topic::WIKI, topic::TIMELINE]);
    }
    host.bus.changed(&topics);
    runner.wake();
}
