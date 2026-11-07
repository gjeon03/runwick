//! Stopping a run so that nothing of it is left behind. An interactive shell puts each job in
//! its own process group, some programs ignore hang-up, and agent CLIs start their tool
//! commands in sessions of their own (Node's `detached: true`), so neither the shell's group
//! nor its session holds everything:
//!
//! 1. before anything is signalled, note the run's family: every descendant by parent pid,
//!    whatever its session (the family is followed again at every check, so children started
//!    during the stop are included);
//! 2. hang up the run's process group, the terminal's foreground job and that family (SIGHUP,
//!    then SIGCONT so a suspended job sees it);
//! 3. after the grace period, SIGKILL the run's session and its family;
//! 4. look again: whatever is still alive is reported by name, never as stopped.
//!
//! Codex is launched with `--no-daemon`, keeping its tool commands and MCP servers in this
//! family. Only the run's own family is touched. A process already handed to launchd before
//! the stop (a daemon that forked twice, or an older run's shared server) is no longer a
//! descendant and is never killed; when it still carries the run's
//! `RUNWICK_SESSION_ID` it is reported as detached, so a stop never claims everything ended.

use super::pty::{self, Member, Proc};
use super::rt::Rt;
use super::run;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How long killed processes get to disappear before they count as left over.
const KILL_SETTLE: Duration = Duration::from_secs(2);
const POLL: Duration = Duration::from_millis(50);

/// What stopping a run did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Outcome {
    /// Something did not end within the grace period and was killed.
    pub killed: bool,
    /// Processes of the run still alive afterwards ("name (pid N)").
    pub leftovers: Vec<String>,
}

/// The run being stopped: its process (which leads its session) and every descendant seen.
struct Target {
    session: runwick_core::api::sessions::SessionId,
    sid: u32,
    run: Option<Proc>,
    family: Vec<Proc>,
}

impl Target {
    /// Add children that appeared under the run or its family since the last look.
    fn follow(&mut self) {
        // Only processes that are still themselves: a reused pid has another family.
        let roots: Vec<u32> = self.run.iter().chain(&self.family).filter(|p| pty::alive(p)).map(|p| p.pid).collect();
        for p in pty::descendants(&roots) {
            if !self.family.contains(&p) {
                self.family.push(p);
            }
        }
    }

    fn living_family(&self) -> impl Iterator<Item = &Proc> {
        self.family.iter().filter(|p| pty::alive(p))
    }

    /// Everything of the run that is still alive, once each ("name (pid N)").
    fn survivors(&self) -> Vec<String> {
        let mut out: Vec<Member> = pty::session_members(self.sid);
        for p in self.living_family() {
            if !out.iter().any(|m| m.pid == p.pid) {
                out.push(Member { pid: p.pid, name: pty::name(p.pid) });
            }
        }
        let mut lines: Vec<String> = out.iter().map(describe).collect();
        let known: Vec<u32> = out.iter().map(|m| m.pid).chain(self.family.iter().map(|p| p.pid)).collect();
        let marker = runwick_core::paths::ENV_SESSION_ID;
        for m in pty::carrying_env(marker, &self.session.to_string(), &known) {
            lines.push(format!("{} · 세션에서 떨어져 계속 실행 중", describe(&m)));
        }
        lines
    }
}

/// Stop the run: hang up, wait up to `grace` (zero kills right away), then kill what is left
/// of it. `None` when nothing was running.
pub async fn stop(rt: &Arc<Rt>, grace: Duration) -> Option<Outcome> {
    let mut target = hang_up(rt)?;
    let deadline = Instant::now() + grace;
    while !ended(rt, &mut target) && Instant::now() < deadline {
        tokio::time::sleep(POLL).await;
    }
    let mut out = Outcome::default();
    if !ended(rt, &mut target) {
        out.killed = true;
        pty::kill_session(target.sid);
        for p in &target.family {
            pty::signal(p, libc::SIGKILL);
        }
        let deadline = Instant::now() + KILL_SETTLE;
        while !ended(rt, &mut target) && Instant::now() < deadline {
            tokio::time::sleep(POLL).await;
        }
    }
    out.leftovers = target.survivors();
    Some(out)
}

/// Note the run's family, then SIGHUP the run, its foreground job and the family.
fn hang_up(rt: &Rt) -> Option<Target> {
    let st = rt.lock();
    let run = st.run.as_ref()?;
    let mut target = Target { session: rt.id, sid: run.pid, run: pty::proc(run.pid), family: Vec::new() };
    target.follow();
    pty::signal_group(run.pid, libc::SIGHUP);
    if let Some(job) = pty::foreground_group(&run.master).filter(|g| *g != run.pid) {
        pty::signal_group(job, libc::SIGHUP);
        pty::signal_group(job, libc::SIGCONT);
    }
    for p in &target.family {
        pty::signal(p, libc::SIGHUP);
        pty::signal(p, libc::SIGCONT);
    }
    Some(target)
}

/// The run's process has exited and nothing else of its session or family is alive.
fn ended(rt: &Rt, target: &mut Target) -> bool {
    target.follow();
    !run::is_running(rt) && pty::session_members(target.sid).is_empty() && target.living_family().next().is_none()
}

pub fn describe(m: &Member) -> String {
    format!("{} (pid {})", m.name, m.pid)
}
