//! Which connections are Runwick windows.
//!
//! On every start the host writes a new random token to `<RUNWICK_HOME>/gui.token` (mode 0600)
//! before it accepts connections. A connection is a window — and may do what only a person
//! should (change or reveal global variables, the editor command, roles, sync, backup, terminals)
//! — only when all of these hold ([`apply_claim`]):
//!
//! - its `host.hello` says it is a window and carries this launch's token;
//! - the socket reports the connecting process, and that process runs this Runwick executable
//!   (a script or other program holding a copied token is not a window);
//! - the process is not inside a managed run (an agent or any command in a Runwick terminal);
//! - if it is a `runwick host --stdio` bridge relaying another computer's window, an `sshd`
//!   process is among its ancestors (a real SSH login): a bridge started from a session or
//!   detached from one is never a window. The bridge adds the token only under `SSH_CONNECTION`
//!   (defence in depth; the host decides).
//!
//! Remaining scope: a program running as the same user can still read Runwick's database and
//! keychain items directly, and one that detaches from its session and starts the Runwick app
//! itself opens a visible window like the person would. The token value is never logged.
//!
//! Verification only — scripted clients as windows: start a host with
//! `RUNWICK_TEST_WINDOW_CLIENTS=1` and `RUNWICK_HOME=<a temporary folder>` (never the default
//! `~/.runwick`, where the flag is ignored). Then any process that presents that host's token
//! (read `<RUNWICK_HOME>/gui.token`) in `host.hello` with `interactive: true` is a window — a
//! Python or shell client, for example — except a process inside a managed run, which is still
//! refused. The host logs a warning at start while this is on.

use crate::host::{Client, Host};
use runwick_core::api::host::WindowRefusal;
use std::io::Write;
use std::path::Path;
use std::sync::RwLock;
use std::sync::atomic::{AtomicBool, Ordering};

static TOKEN: RwLock<Option<String>> = RwLock::new(None);
/// Scripted clients with the token count as windows (verification hosts only, see above).
static TEST_CLIENTS: AtomicBool = AtomicBool::new(false);

/// Environment variable that lets scripted clients be windows on a verification host.
pub const TEST_CLIENTS_ENV: &str = "RUNWICK_TEST_WINDOW_CLIENTS";

/// Create a new token, keep it in memory and write it to `path` (in the host's data folder)
/// readable only by this user; decide whether scripted clients may be windows on this host.
pub fn issue(path: &Path) -> std::io::Result<()> {
    binary::remember();
    let home = path.parent().unwrap_or(path);
    // The account's real home (not `$HOME`, which a program can set to anything).
    let default_home = passwd_home().map(|h| h.join(".runwick"));
    // Verification builds only: a release build never reads the flag.
    #[cfg(any(debug_assertions, feature = "verify-hooks"))]
    let flag = std::env::var(TEST_CLIENTS_ENV).ok();
    #[cfg(not(any(debug_assertions, feature = "verify-hooks")))]
    let flag: Option<String> = None;
    let test = test_clients(flag.as_deref(), home, default_home.as_deref());
    TEST_CLIENTS.store(test, Ordering::Relaxed);
    if test {
        tracing::warn!(
            home = %home.display(),
            "{TEST_CLIENTS_ENV}=1: any process with the window token counts as a window (verification only)"
        );
    }
    let token = format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple());
    write_private(path, &token)?;
    if let Ok(mut t) = TOKEN.write() {
        *t = Some(token);
    }
    Ok(())
}

fn write_private(path: &Path, token: &str) -> std::io::Result<()> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let tmp = path.with_extension("token.tmp");
    let _ = std::fs::remove_file(&tmp);
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp)?;
    f.write_all(token.as_bytes())?;
    f.sync_all()?;
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    std::fs::rename(&tmp, path)
}

/// Scripted window clients are allowed only when asked for and never on the default data folder
/// (`default_home`; unknown means refuse). The folders are compared by file identity (device and
/// inode), so another spelling of the same folder — a link, `/System/Volumes/Data/…` — is
/// still the default one.
fn test_clients(flag: Option<&str>, home: &Path, default_home: Option<&Path>) -> bool {
    use std::os::unix::fs::MetadataExt;
    let Some(default_home) = default_home else {
        return false;
    };
    let id = |p: &Path| std::fs::metadata(p).ok().map(|m| (m.dev(), m.ino()));
    let same = match (id(home), id(default_home)) {
        (Some(a), Some(b)) => a == b,
        (None, _) => true,
        // No default folder on disk: only its own path can be it.
        (Some(_), None) => {
            let canonical = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
            canonical(home) == canonical(default_home)
        }
    };
    flag == Some("1") && !same
}

/// Home folder of this user from the user database.
fn passwd_home() -> Option<std::path::PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut buf = vec![0 as libc::c_char; 16 * 1024];
    let mut found: *mut libc::passwd = std::ptr::null_mut();
    // SAFETY: every pointer refers to a live, writable value of the right type and size.
    let r = unsafe { libc::getpwuid_r(libc::getuid(), &mut pwd, buf.as_mut_ptr(), buf.len(), &mut found) };
    if r != 0 || found.is_null() || pwd.pw_dir.is_null() {
        return None;
    }
    // SAFETY: `pw_dir` points into `buf`, NUL-terminated by getpwuid_r.
    let dir = unsafe { std::ffi::CStr::from_ptr(pwd.pw_dir) };
    Some(std::path::PathBuf::from(std::ffi::OsStr::from_bytes(dir.to_bytes())))
}

/// Decide whether a connection that sent `host.hello` is a Runwick window (see the module
/// notes) and record it on the connection, with the reason when a window claim is refused.
/// `report` logs a refusal (once per hello: the socket path reports, the handler not).
pub fn apply_claim(host: &Host, client: &Client, interactive: bool, token: Option<&str>, report: bool) {
    let outcome = claim(host, client, interactive, token);
    client.interactive.store(outcome.is_ok(), Ordering::Relaxed);
    let refusal = if interactive { outcome.err() } else { None };
    if let (Some((reason, _)), true) = (refusal, report) {
        tracing::info!(client = client.id, ?reason, "window claim refused");
    }
    client.set_window_refusal(refusal);
}

/// `Err((reason, may_restart))` when the connection is not a window.
fn claim(host: &Host, client: &Client, interactive: bool, token: Option<&str>) -> Result<(), (WindowRefusal, bool)> {
    if !interactive || !matches(token) {
        return Err((WindowRefusal::NoToken, false));
    }
    let Some(pid) = client.peer_pid() else {
        return Err((WindowRefusal::UnknownProcess, false));
    };
    let facts = PeerFacts::of(host, client, pid);
    if TEST_CLIENTS.load(Ordering::Relaxed) {
        // Verification host: any program with the token (verification hooks included), but never
        // one inside a managed run.
        return if facts.in_run { Err((WindowRefusal::InsideRun, false)) } else { Ok(()) };
    }
    match facts.refusal() {
        None => Ok(()),
        Some(r) => Err((r, facts.may_restart())),
    }
}

/// What the host knows about the process on the other end of a connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeerFacts {
    /// It runs this Runwick executable.
    pub same_binary: bool,
    /// It runs inside a managed session's terminal.
    pub in_run: bool,
    /// It is a `runwick host --stdio` bridge.
    pub bridge: bool,
    /// An `sshd` process is among its ancestors.
    pub via_ssh: bool,
    /// It was started with a verification hook (scripted input, a given UI state, snapshots…)
    /// in its environment, or its environment could not be read.
    pub hooks: bool,
}

impl PeerFacts {
    pub fn allows_window(self) -> bool {
        self.refusal().is_none()
    }

    /// The first reason this process is not a window, in the order a person can act on.
    pub fn refusal(self) -> Option<WindowRefusal> {
        if self.in_run {
            Some(WindowRefusal::InsideRun)
        } else if !self.same_binary {
            Some(WindowRefusal::DifferentBinary)
        } else if self.hooks {
            Some(WindowRefusal::Hooks)
        } else if self.bridge && !self.via_ssh {
            Some(WindowRefusal::NoSsh)
        } else {
            None
        }
    }

    /// Only another Runwick build (and nothing else wrong) may ask the host to restart for it:
    /// no more than any program of this user outside a run can already do by ending the host.
    pub fn may_restart(self) -> bool {
        !self.same_binary && !self.in_run && !self.hooks && (!self.bridge || self.via_ssh)
    }

    fn of(host: &Host, client: &Client, pid: u32) -> Self {
        let (args, env) = proc::cmdline(pid).unwrap_or_default();
        let bridge = is_bridge(&args);
        PeerFacts {
            same_binary: proc::exe(pid).is_some_and(|e| binary::same_as_host(&e)),
            in_run: crate::collab::peer_session(host, client).is_some(),
            bridge,
            via_ssh: bridge && has_sshd_ancestor(pid),
            hooks: args.is_empty() || has_verification_hooks(&env),
        }
    }
}

/// Environment variables that make the app take scripted input or a prepared state (the
/// verification aids). A window started with any of them is not trusted outside test hosts.
fn has_verification_hooks(env: &[String]) -> bool {
    const PREFIXES: &[&str] = &["RUNWICK_UI_", "RUNWICK_VERIFY", "RUNWICK_SNAPSHOT", "RUNWICK_DEV_"];
    const EXACT: &[&str] = &["RUNWICK_DOC_VIEW", "RUNWICK_HISTORY", "RUNWICK_TRANSCRIPTS", "RUNWICK_WINDOW"];
    env.iter()
        .filter_map(|e| e.split_once('=').map(|(k, _)| k))
        .any(|k| PREFIXES.iter().any(|p| k.starts_with(p)) || EXACT.contains(&k))
}

/// `runwick host --stdio` (the GUI of another computer, relayed over SSH).
fn is_bridge(args: &[String]) -> bool {
    args.get(1).is_some_and(|a| a == "host") && args.iter().skip(2).any(|a| a == "--stdio")
}

/// Processes looked at above the connecting one, at most.
const MAX_DEPTH: usize = 32;

fn has_sshd_ancestor(pid: u32) -> bool {
    let mut cur = pid;
    for _ in 0..MAX_DEPTH {
        match crate::collab::parent_pid(cur) {
            Some(p) if p > 1 && p != cur => cur = p,
            _ => return false,
        }
        if proc::name(cur).is_some_and(|n| n.starts_with("sshd")) {
            return true;
        }
    }
    false
}

/// Is a file the same program as the host's own executable? The host opens its executable when
/// it starts and keeps it open, so the answer stays right when the app is moved or replaced by
/// an update while the host runs: the same path, the same file (moved within a volume), or the
/// same bytes (copied elsewhere; SHA-256, cached per path, modification time and size).
pub mod binary {
    use sha2::{Digest, Sha256};
    use std::collections::HashMap;
    use std::fs::File;
    use std::os::unix::fs::{FileExt, MetadataExt};
    use std::path::{Path, PathBuf};
    use std::sync::{Mutex, OnceLock};
    use std::time::SystemTime;

    struct HostExe {
        path: PathBuf,
        file: File,
        id: (u64, u64),
        size: u64,
        hash: OnceLock<Option<[u8; 32]>>,
    }

    fn host_exe() -> Option<&'static HostExe> {
        static EXE: OnceLock<Option<HostExe>> = OnceLock::new();
        EXE.get_or_init(|| {
            let path = std::env::current_exe().ok()?;
            let path = std::fs::canonicalize(&path).unwrap_or(path);
            let file = File::open(&path).ok()?;
            let m = file.metadata().ok()?;
            Some(HostExe { path, file, id: (m.dev(), m.ino()), size: m.len(), hash: OnceLock::new() })
        })
        .as_ref()
    }

    /// Open the host's executable now, before anything can move or replace it.
    pub fn remember() {
        let _ = host_exe();
    }

    /// The host's executable as it was at start (display path).
    pub fn host_path() -> Option<PathBuf> {
        host_exe().map(|h| h.path.clone())
    }

    pub fn same_as_host(peer: &Path) -> bool {
        let Some(h) = host_exe() else { return false };
        if peer == h.path {
            return true;
        }
        let Ok(m) = std::fs::metadata(peer) else {
            return false;
        };
        if (m.dev(), m.ino()) == h.id {
            return true;
        }
        if m.len() != h.size {
            return false;
        }
        let mine = h.hash.get_or_init(|| hash_reader(|buf, at| h.file.read_at(buf, at)));
        mine.is_some() && *mine == peer_hash(peer, m.modified().ok(), m.len())
    }

    fn peer_hash(path: &Path, modified: Option<SystemTime>, size: u64) -> Option<[u8; 32]> {
        type Key = (PathBuf, Option<SystemTime>, u64);
        static CACHE: Mutex<Option<HashMap<Key, [u8; 32]>>> = Mutex::new(None);
        let key = (path.to_path_buf(), modified, size);
        if let Some(h) = CACHE.lock().ok()?.as_ref().and_then(|c| c.get(&key)) {
            return Some(*h);
        }
        let file = File::open(path).ok()?;
        let h = hash_reader(|buf, at| file.read_at(buf, at))?;
        if let Ok(mut c) = CACHE.lock() {
            c.get_or_insert_with(HashMap::new).insert(key, h);
        }
        Some(h)
    }

    fn hash_reader(read_at: impl Fn(&mut [u8], u64) -> std::io::Result<usize>) -> Option<[u8; 32]> {
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 1 << 20];
        let mut at = 0u64;
        loop {
            let n = read_at(&mut buf, at).ok()?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            at += n as u64;
        }
        Some(hasher.finalize().into())
    }
}

/// Window status and the restart a window of another Runwick build may ask for.
pub fn register(r: &mut crate::router::Router) {
    use runwick_core::api::host::{HostRestartForUpdate, HostWindowStatus, RunningRun, WindowStatus};
    use runwick_core::rpc::{RpcError, codes};
    r.add::<HostWindowStatus, _, _>(|ctx: crate::host::Ctx, _: ()| async move {
        let refusal = ctx.client.window_refusal();
        let (reason, may_restart) = match refusal {
            Some((r, m)) => (Some(r), m),
            None => (None, false),
        };
        let running = if may_restart {
            let host = ctx.host.clone();
            tokio::task::spawn_blocking(move || {
                host.sessions
                    .running_ids()
                    .into_iter()
                    .filter_map(|id| crate::sessions::session_info(&host, id).ok())
                    .map(|i| RunningRun {
                        session: i.session.id,
                        title: i.session.title,
                        tool: i.session.tool.label().to_string(),
                    })
                    .collect()
            })
            .await
            .map_err(RpcError::internal)?
        } else {
            Vec::new()
        };
        let host_exe = (reason == Some(WindowRefusal::DifferentBinary))
            .then(binary::host_path)
            .flatten()
            .map(|p| runwick_core::paths::display_path(&p));
        Ok(WindowStatus { window: ctx.client.is_interactive(), refusal: reason, host_exe, may_restart, running })
    });
    r.add::<HostRestartForUpdate, _, _>(|ctx: crate::host::Ctx, _: ()| async move {
        if !matches!(ctx.client.window_refusal(), Some((WindowRefusal::DifferentBinary, true))) {
            return Err(RpcError::new(codes::DENIED, "다른 Runwick의 창에서만 다시 시작을 요청할 수 있어요"));
        }
        tracing::warn!(client = ctx.client.id, "a window of another Runwick build restarts the host; stopping runs");
        crate::meta::shutdown(&ctx.host, true).await
    });
}

/// Facts about other processes of this user, as the OS reports them.
pub(crate) mod proc {
    use std::path::PathBuf;

    /// Executable of `pid` (canonical path).
    #[cfg(target_os = "macos")]
    pub fn exe(pid: u32) -> Option<PathBuf> {
        let mut buf = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
        // SAFETY: `buf` is writable and as long as the size passed.
        let n = unsafe { libc::proc_pidpath(pid as libc::c_int, buf.as_mut_ptr().cast(), buf.len() as u32) };
        if n <= 0 {
            return None;
        }
        buf.truncate(n as usize);
        let p = PathBuf::from(String::from_utf8(buf).ok()?);
        Some(std::fs::canonicalize(&p).unwrap_or(p))
    }

    #[cfg(not(target_os = "macos"))]
    pub fn exe(pid: u32) -> Option<PathBuf> {
        let p = std::fs::read_link(format!("/proc/{pid}/exe")).ok()?;
        Some(std::fs::canonicalize(&p).unwrap_or(p))
    }

    /// File name of the executable of `pid`.
    pub fn name(pid: u32) -> Option<String> {
        exe(pid).and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
    }

    /// Command line and starting environment of `pid`.
    #[cfg(target_os = "macos")]
    pub fn cmdline(pid: u32) -> Option<(Vec<String>, Vec<String>)> {
        let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid as libc::c_int];
        let mut size: libc::size_t = 0;
        // SAFETY: a size query (null buffer) for a valid three-element MIB.
        let r = unsafe { libc::sysctl(mib.as_mut_ptr(), 3, std::ptr::null_mut(), &mut size, std::ptr::null_mut(), 0) };
        if r != 0 || size == 0 {
            return None;
        }
        let mut buf = vec![0u8; size];
        // SAFETY: `buf` is writable for `size` bytes.
        let r =
            unsafe { libc::sysctl(mib.as_mut_ptr(), 3, buf.as_mut_ptr().cast(), &mut size, std::ptr::null_mut(), 0) };
        if r != 0 {
            return None;
        }
        buf.truncate(size);
        parse_procargs(&buf)
    }

    #[cfg(not(target_os = "macos"))]
    pub fn cmdline(pid: u32) -> Option<(Vec<String>, Vec<String>)> {
        let split = |raw: Vec<u8>| -> Vec<String> {
            raw.split(|b| *b == 0).filter(|a| !a.is_empty()).map(|a| String::from_utf8_lossy(a).into_owned()).collect()
        };
        let args = split(std::fs::read(format!("/proc/{pid}/cmdline")).ok()?);
        let env = split(std::fs::read(format!("/proc/{pid}/environ")).ok()?);
        Some((args, env))
    }

    /// `KERN_PROCARGS2`: argc (i32), the executable path, NUL padding, then argc arguments and
    /// the environment strings, each NUL-terminated.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub fn parse_procargs(buf: &[u8]) -> Option<(Vec<String>, Vec<String>)> {
        let argc = usize::try_from(i32::from_ne_bytes(buf.get(..4)?.try_into().ok()?)).ok()?;
        let rest = buf.get(4..)?;
        let rest = &rest[rest.iter().position(|b| *b == 0)?..];
        let rest = &rest[rest.iter().position(|b| *b != 0)?..];
        let mut words = rest.split(|b| *b == 0).map(|a| String::from_utf8_lossy(a).into_owned());
        let args: Vec<String> = words.by_ref().take(argc).collect();
        let env = words.take_while(|w| !w.is_empty()).collect();
        Some((args, env))
    }
}

/// Whether `presented` is this host's current token (compared in constant time).
pub fn matches(presented: Option<&str>) -> bool {
    let Ok(guard) = TOKEN.read() else {
        return false;
    };
    let (Some(expected), Some(given)) = (guard.as_deref(), presented) else {
        return false;
    };
    let (a, b) = (expected.as_bytes(), given.as_bytes());
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn only_this_binary_outside_runs_without_hooks_or_a_bridge_under_ssh_is_a_window() {
        let f = |same_binary, in_run, bridge, via_ssh, hooks| PeerFacts { same_binary, in_run, bridge, via_ssh, hooks };
        assert!(f(true, false, false, false, false).allows_window(), "the window or menu bar");
        assert!(f(true, false, true, true, false).allows_window(), "a bridge in an SSH login");
        assert!(!f(false, false, false, false, false).allows_window(), "another program with the token");
        assert!(!f(true, true, false, false, false).allows_window(), "this binary inside a managed run");
        assert!(!f(true, false, true, false, false).allows_window(), "a bridge without an SSH login");
        assert!(!f(true, true, true, true, false).allows_window(), "a bridge inside a run, even under SSH");
        assert!(!f(true, false, false, false, true).allows_window(), "a window started with verification hooks");
    }

    #[test]
    fn the_first_actionable_reason_is_reported_and_only_another_build_may_restart() {
        let f = |same_binary, in_run, bridge, via_ssh, hooks| PeerFacts { same_binary, in_run, bridge, via_ssh, hooks };
        assert_eq!(f(true, false, false, false, false).refusal(), None);
        assert_eq!(f(false, true, false, false, true).refusal(), Some(WindowRefusal::InsideRun));
        assert_eq!(f(false, false, false, false, true).refusal(), Some(WindowRefusal::DifferentBinary));
        assert_eq!(f(true, false, false, false, true).refusal(), Some(WindowRefusal::Hooks));
        assert_eq!(f(true, false, true, false, false).refusal(), Some(WindowRefusal::NoSsh));
        assert!(f(false, false, false, false, false).may_restart(), "another Runwick build");
        assert!(f(false, false, true, true, false).may_restart(), "another build over SSH");
        assert!(!f(false, true, false, false, false).may_restart(), "inside a run");
        assert!(!f(false, false, false, false, true).may_restart(), "with verification hooks");
        assert!(!f(false, false, true, false, false).may_restart(), "a bridge without SSH");
        assert!(!f(true, false, false, false, false).may_restart(), "already a window");
    }

    #[test]
    fn the_same_program_is_recognised_after_a_move_or_copy_but_not_other_bytes() {
        let me = std::env::current_exe().unwrap();
        binary::remember();
        assert!(binary::same_as_host(&me));
        let d = tempfile::tempdir().unwrap();
        // Moved within the volume (same file) and copied elsewhere (same bytes).
        let linked = d.path().join("moved-runwick");
        std::fs::hard_link(&me, &linked).unwrap();
        assert!(binary::same_as_host(&linked));
        let copied = d.path().join("copied-runwick");
        std::fs::copy(&me, &copied).unwrap();
        assert!(binary::same_as_host(&copied));
        assert!(binary::same_as_host(&copied), "the cached answer");
        // Another build: same size with other bytes, or another size.
        let mut other = std::fs::read(&me).unwrap();
        let last = other.len() - 1;
        other[last] ^= 0xff;
        let changed = d.path().join("changed-runwick");
        std::fs::write(&changed, &other).unwrap();
        assert!(!binary::same_as_host(&changed));
        let shorter = d.path().join("shorter-runwick");
        std::fs::write(&shorter, &other[..last]).unwrap();
        assert!(!binary::same_as_host(&shorter));
        assert!(!binary::same_as_host(&d.path().join("missing")));
    }

    #[test]
    fn verification_hooks_are_recognised_in_an_environment() {
        let e = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(!has_verification_hooks(&e(&["HOME=/u", "RUNWICK_HOME=/r", "RUNWICK_SOCKET=/s", "PATH=/bin"])));
        for hook in [
            "RUNWICK_VERIFY_INPUT=key:enter",
            "RUNWICK_VERIFY_DIR=/t",
            "RUNWICK_UI_STATE={}",
            "RUNWICK_UI_OVERLAY=palette",
            "RUNWICK_UI_ENV_EDIT=new",
            "RUNWICK_UI_REFRESH_CARD=x",
            "RUNWICK_SNAPSHOT=/t/a.png",
            "RUNWICK_SNAPSHOT_DELAY_MS=1",
            "RUNWICK_DOC_VIEW=x",
            "RUNWICK_HISTORY=x",
            "RUNWICK_TRANSCRIPTS=/t",
            "RUNWICK_WINDOW=10x10",
            "RUNWICK_DEV_SIMULATED_DEVICES=1",
        ] {
            assert!(has_verification_hooks(&e(&["HOME=/u", hook])), "{hook}");
        }
        // A value that merely mentions a hook is not one.
        assert!(!has_verification_hooks(&e(&["NOTE=RUNWICK_UI_STATE"])));
    }

    #[test]
    fn bridges_and_command_lines_are_recognised() {
        let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(is_bridge(&a(&["/x/runwick", "host", "--stdio"])));
        assert!(!is_bridge(&a(&["/x/runwick", "host", "--daemon"])));
        assert!(!is_bridge(&a(&["/x/runwick"])));
        assert!(!is_bridge(&a(&["/x/runwick", "menubar", "--stdio"])));
        let mut buf = 3i32.to_ne_bytes().to_vec();
        buf.extend_from_slice(b"/x/runwick\0\0\0/x/runwick\0host\0--stdio\0HOME=/u\0RUNWICK_UI_STATE={}\0\0\0junk");
        let (args, env) = proc::parse_procargs(&buf).unwrap();
        assert_eq!(args, a(&["/x/runwick", "host", "--stdio"]));
        assert_eq!(env, a(&["HOME=/u", "RUNWICK_UI_STATE={}"]));
        // This process: its own executable, command line and environment.
        let me = std::process::id();
        assert!(proc::exe(me).is_some_and(|e| binary::same_as_host(&e)));
        let (args, env) = proc::cmdline(me).unwrap();
        assert!(!args.is_empty() && !is_bridge(&args), "{args:?}");
        assert!(env.iter().any(|v| v.starts_with("PATH=")), "the environment is read too");
        assert!(!has_sshd_ancestor(me) || std::env::var_os("SSH_CONNECTION").is_some());
    }

    #[test]
    fn scripted_window_clients_only_on_a_non_default_data_folder_when_asked() {
        let d = tempfile::tempdir().unwrap();
        let default_home = d.path().join(".runwick");
        let temp_home = d.path().join("verify").join("home");
        std::fs::create_dir_all(&default_home).unwrap();
        std::fs::create_dir_all(&temp_home).unwrap();
        let def = Some(default_home.as_path());
        assert!(test_clients(Some("1"), &temp_home, def));
        // Ignored for the default data folder, however it is reached: the same path, another
        // spelling, or a link to it (the folder's identity decides, not its name).
        assert!(!test_clients(Some("1"), &default_home, def));
        assert!(!test_clients(Some("1"), &d.path().join("verify").join("..").join(".runwick"), def));
        let link = d.path().join("link-to-default");
        std::os::unix::fs::symlink(&default_home, &link).unwrap();
        assert!(!test_clients(Some("1"), &link, def));
        // When the account's home cannot be read, or the data folder cannot be examined: off.
        assert!(!test_clients(Some("1"), &temp_home, None));
        assert!(!test_clients(Some("1"), &d.path().join("missing"), def));
        // Only the exact value turns it on.
        for flag in [None, Some("0"), Some("true"), Some("")] {
            assert!(!test_clients(flag, &temp_home, def), "{flag:?}");
        }
        // The account's home comes from the user database, not from $HOME.
        let home = passwd_home().unwrap();
        assert!(home.is_absolute() && home.is_dir(), "{home:?}");
    }

    #[test]
    fn a_new_private_token_every_launch_and_only_it_matches() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("gui.token");
        issue(&path).unwrap();
        let first = std::fs::read_to_string(&path).unwrap();
        assert_eq!(first.len(), 64);
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        assert!(matches(Some(&first)));
        assert!(!matches(None));
        assert!(!matches(Some("")));
        assert!(!matches(Some(&first[..63])));
        issue(&path).unwrap();
        let second = std::fs::read_to_string(&path).unwrap();
        assert_ne!(first, second);
        assert!(!matches(Some(&first)), "an old launch's token no longer works");
        assert!(matches(Some(&second)));
    }
}
