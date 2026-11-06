//! Who is calling a collaboration method, decided by the host — never by what the request
//! says (`parent_id`, `worker_id`, `source` and `RUNWICK_SESSION_ID` are only claims).
//!
//! 1. The socket's peer process, when the system reports it: walking up its parents to a run's
//!    process names the managed session it runs in. That decides, whatever else is presented —
//!    a program inside a managed terminal is that session's agent, never a window.
//! 2. A Runwick window proved itself with this launch's window token (`gui_token`).
//! 3. A run's session token (`paths::ENV_SESSION_TOKEN`) counts only when the peer process is
//!    unknown (a bridge, an in-process client): the token is in the run's environment, which
//!    other programs of the same user can read (`ps eww`), so it never outweighs the process.
//! 4. Anything else is unverified and may only read.

use crate::host::{Client, Host};
use runwick_core::api::sessions::SessionId;
use std::collections::HashMap;

/// Who is calling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Caller {
    /// A Runwick window (the person).
    Window,
    /// A program inside this managed session's terminal (its agent).
    Session(SessionId),
    /// Neither could be shown.
    Unverified,
}

/// Parent processes looked at, at most (a shell, a CLI, its tool runner, `runwick`, …).
const MAX_DEPTH: usize = 32;

pub fn caller(host: &Host, client: &Client) -> Caller {
    let peer = client.peer_pid().map(|_| peer_session(host, client));
    decide(peer, client.session(), client.is_interactive())
}

/// `peer`: `None` when the peer process is unknown, else the session it runs in (if any).
fn decide(peer: Option<Option<SessionId>>, token: Option<SessionId>, window: bool) -> Caller {
    match peer {
        Some(Some(s)) => Caller::Session(s),
        Some(None) if window => Caller::Window,
        Some(None) => Caller::Unverified,
        None if window => Caller::Window,
        None => token.map_or(Caller::Unverified, Caller::Session),
    }
}

/// The managed session whose terminal the connecting process runs in (its run's process or one
/// of its ancestors), when the socket reported the process. Used to keep a program inside a
/// managed run from counting as a window, and to check a presented session token.
pub fn peer_session(host: &Host, client: &Client) -> Option<SessionId> {
    let pid = client.peer_pid()?;
    let runs: HashMap<u32, SessionId> = host
        .sessions
        .running_ids()
        .into_iter()
        .filter_map(|id| host.sessions.live(host, id).and_then(|l| l.pid).map(|p| (p, id)))
        .collect();
    session_of(pid, &runs, parent_pid)
}

/// Whether a presented session token may stand for `token_session` on this connection: only
/// when the peer process is unknown, or runs inside that same session.
pub fn token_fits(host: &Host, client: &Client, token_session: SessionId) -> bool {
    client.peer_pid().is_none() || peer_session(host, client) == Some(token_session)
}

/// The session whose run process is `pid` or one of its ancestors.
fn session_of(pid: u32, runs: &HashMap<u32, SessionId>, parent: impl Fn(u32) -> Option<u32>) -> Option<SessionId> {
    let mut cur = pid;
    for _ in 0..MAX_DEPTH {
        if let Some(id) = runs.get(&cur) {
            return Some(*id);
        }
        match parent(cur) {
            Some(p) if p > 1 && p != cur => cur = p,
            _ => return None,
        }
    }
    None
}

#[cfg(target_os = "macos")]
pub(crate) fn parent_pid(pid: u32) -> Option<u32> {
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    // SAFETY: `info` is a valid, writable `proc_bsdinfo` of `size` bytes.
    let n = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDTBSDINFO,
            0,
            (&mut info as *mut libc::proc_bsdinfo).cast(),
            size,
        )
    };
    (n == size).then_some(info.pbi_ppid)
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn parent_pid(pid: u32) -> Option<u32> {
    // /proc/<pid>/stat: "pid (comm) state ppid …"; comm may contain spaces and parentheses.
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after = &stat[stat.rfind(')')? + 1..];
    after.split_whitespace().nth(1)?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_command_belongs_to_the_session_above_it() {
        let worker = uuid::Uuid::now_v7();
        let parent = uuid::Uuid::now_v7();
        let runs: HashMap<u32, SessionId> = [(100, parent), (200, worker)].into_iter().collect();
        // worker run 200 → bash 210 → runwick 220; parent run 100 → claude 110.
        let tree: HashMap<u32, u32> = [(220, 210), (210, 200), (200, 1), (110, 100), (100, 1), (900, 1)].into();
        let up = |p: u32| tree.get(&p).copied();
        assert_eq!(session_of(220, &runs, up), Some(worker), "the env var does not matter");
        assert_eq!(session_of(110, &runs, up), Some(parent));
        assert_eq!(session_of(900, &runs, up), None, "outside every terminal");
        assert_eq!(session_of(5, &runs, |_| None), None);
        let looped = |p: u32| Some(p + 1);
        assert_eq!(session_of(1000, &runs, looped), None, "the walk is bounded");
    }

    #[test]
    fn the_process_outweighs_tokens_and_window_claims() {
        let (a, b) = (uuid::Uuid::now_v7(), uuid::Uuid::now_v7());
        // Inside run `a`: that session, even with `b`'s token or a window claim.
        assert_eq!(decide(Some(Some(a)), Some(b), true), Caller::Session(a));
        // Known process outside every run: a token proves nothing; a window stays a window.
        assert_eq!(decide(Some(None), Some(b), false), Caller::Unverified);
        assert_eq!(decide(Some(None), None, true), Caller::Window);
        // Unknown process (bridge, in-process): the token, then the window claim.
        assert_eq!(decide(None, Some(b), false), Caller::Session(b));
        assert_eq!(decide(None, None, true), Caller::Window);
        assert_eq!(decide(None, None, false), Caller::Unverified);
    }

    #[test]
    fn this_process_has_a_parent() {
        let me = std::process::id();
        assert!(parent_pid(me).is_some_and(|p| p > 0 && p != me));
    }
}
