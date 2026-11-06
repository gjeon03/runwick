//! Starting a program in a new pseudo-terminal.
//!
//! The child becomes a session leader with the PTY as its controlling terminal, so signals for
//! the whole run go to its process group (`kill(-pid, ...)`). Signal dispositions ignored by the
//! host daemon (SIGHUP, SIGPIPE) are reset so the program behaves as in a normal terminal.

use std::ffi::OsString;
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;

pub struct Spec {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub env: Vec<(String, String)>,
    pub cwd: PathBuf,
}

pub struct Spawned {
    pub child: Child,
    pub master: OwnedFd,
    /// Blocking reader of program output.
    pub reader: File,
    /// Blocking writer of program input.
    pub writer: File,
}

/// Serializes PTY creation with spawning so a concurrently started run never inherits another
/// run's PTY descriptors before they are marked close-on-exec.
static SPAWN_LOCK: Mutex<()> = Mutex::new(());

pub fn spawn(spec: &Spec, cols: u16, rows: u16) -> io::Result<Spawned> {
    let _guard = SPAWN_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (master, slave) = open_pty(cols, rows)?;
    let mut cmd = Command::new(&spec.program);
    cmd.args(&spec.args)
        .env_clear()
        .envs(spec.env.iter().map(|(k, v)| (k, v)))
        .current_dir(&spec.cwd)
        .stdin(Stdio::from(slave.try_clone()?))
        .stdout(Stdio::from(slave.try_clone()?))
        .stderr(Stdio::from(slave));
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(io::Error::last_os_error());
            }
            if libc::ioctl(0, libc::TIOCSCTTY as _, 0) == -1 {
                return Err(io::Error::last_os_error());
            }
            for sig in [
                libc::SIGCHLD,
                libc::SIGHUP,
                libc::SIGINT,
                libc::SIGQUIT,
                libc::SIGTERM,
                libc::SIGALRM,
                libc::SIGPIPE,
                libc::SIGTSTP,
                libc::SIGTTIN,
                libc::SIGTTOU,
            ] {
                libc::signal(sig, libc::SIG_DFL);
            }
            let mut set: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut set);
            libc::sigprocmask(libc::SIG_SETMASK, &set, std::ptr::null_mut());
            Ok(())
        });
    }
    let child = cmd.spawn()?;
    // `cmd` still holds the slave descriptors; dropping it closes them in this process so the
    // reader sees end-of-file once the program and its children are gone.
    drop(cmd);
    let reader = File::from(master.try_clone()?);
    let writer = File::from(master.try_clone()?);
    Ok(Spawned { child, master, reader, writer })
}

fn open_pty(cols: u16, rows: u16) -> io::Result<(OwnedFd, OwnedFd)> {
    let mut master: libc::c_int = -1;
    let mut slave: libc::c_int = -1;
    let mut ws = winsize(cols, rows);
    let rc = unsafe {
        libc::openpty(&mut master, &mut slave, std::ptr::null_mut(), std::ptr::null_mut(), &mut ws as *mut _ as _)
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    let (master, slave) = unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
    for fd in [master.as_raw_fd(), slave.as_raw_fd()] {
        unsafe {
            libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
        }
    }
    // Let the line discipline treat input as UTF-8 (correct erase of multi-byte characters).
    unsafe {
        let mut t: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(master.as_raw_fd(), &mut t) == 0 {
            t.c_iflag |= libc::IUTF8;
            libc::tcsetattr(master.as_raw_fd(), libc::TCSANOW, &t);
        }
    }
    Ok((master, slave))
}

fn winsize(cols: u16, rows: u16) -> libc::winsize {
    libc::winsize { ws_row: rows, ws_col: cols, ws_xpixel: 0, ws_ypixel: 0 }
}

/// Tell the kernel (and so the foreground program, via SIGWINCH) the new size.
pub fn resize(master: &OwnedFd, cols: u16, rows: u16) -> io::Result<()> {
    let ws = winsize(cols, rows);
    if unsafe { libc::ioctl(master.as_raw_fd(), libc::TIOCSWINSZ, &ws) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Signal the run's whole process group.
pub fn signal_group(pid: u32, sig: libc::c_int) {
    unsafe {
        libc::kill(-(pid as libc::pid_t), sig);
    }
}

/// The terminal's foreground process group (the shell's own, or a job it started).
pub fn foreground_group(master: &OwnedFd) -> Option<u32> {
    let group = unsafe { libc::tcgetpgrp(master.as_raw_fd()) };
    u32::try_from(group).ok().filter(|g| *g > 0)
}

/// A process that belongs to a run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    pub pid: u32,
    pub name: String,
}

/// A process told apart from a later one that gets the same pid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Proc {
    pub pid: u32,
    start: u64,
}

/// What the system says about a process.
struct Info {
    ppid: u32,
    /// Start time (microseconds on macOS, clock ticks on Linux): with the pid, an identity.
    start: u64,
    zombie: bool,
}

/// Living processes of session `sid`. The run's process leads its own session (its pid is the
/// session id), and every job an interactive shell starts stays in it, whatever its process
/// group. Processes that left the session (`setsid`, Node's `detached: true`) are found
/// through [`descendants`] instead.
pub fn session_members(sid: u32) -> Vec<Member> {
    all_pids()
        .into_iter()
        .filter(|pid| unsafe { libc::getsid(*pid as libc::pid_t) } == sid as libc::pid_t)
        .filter(|pid| info(*pid).is_some_and(|i| !i.zombie))
        .map(|pid| Member { pid, name: process_name(pid) })
        .collect()
}

/// SIGKILL every process of session `sid`.
pub fn kill_session(sid: u32) {
    signal_group(sid, libc::SIGKILL);
    for m in session_members(sid) {
        unsafe {
            libc::kill(m.pid as libc::pid_t, libc::SIGKILL);
        }
    }
}

/// The living process `pid`, if any.
pub fn proc(pid: u32) -> Option<Proc> {
    info(pid).filter(|i| !i.zombie).map(|i| Proc { pid, start: i.start })
}

/// Living descendants of `roots` (children, their children, …) by parent pid, roots excluded.
/// Unlike the session, this follows processes that started their own session. Only the
/// processes' own family: anything else of the same user (a shared daemon that was already
/// handed to launchd, for example) is never included.
pub fn descendants(roots: &[u32]) -> Vec<Proc> {
    let all: Vec<(u32, Info)> = all_pids().into_iter().filter_map(|pid| Some((pid, info(pid)?))).collect();
    let mut found: Vec<Proc> = Vec::new();
    let mut parents: Vec<u32> = roots.to_vec();
    while let Some(parent) = parents.pop() {
        for (pid, i) in &all {
            if i.ppid == parent && *pid != parent && !i.zombie && !found.iter().any(|p| p.pid == *pid) {
                found.push(Proc { pid: *pid, start: i.start });
                parents.push(*pid);
            }
        }
    }
    found
}

/// The same process is still alive (not ended, not a zombie, its pid not reused).
pub fn alive(p: &Proc) -> bool {
    info(p.pid).is_some_and(|i| !i.zombie && i.start == p.start)
}

/// Signal `p` if it is still that process.
pub fn signal(p: &Proc, sig: libc::c_int) {
    if alive(p) {
        unsafe {
            libc::kill(p.pid as libc::pid_t, sig);
        }
    }
}

pub fn name(pid: u32) -> String {
    process_name(pid)
}

/// Living processes, other than `skip` and this one, whose environment holds `var=value`: for a
/// run's `RUNWICK_SESSION_ID`, processes it started that left its family (a daemon that forked
/// twice, a shared server). On macOS the environment of another process of this user is visible
/// through `KERN_PROCARGS2` except for Apple platform binaries, which hide it: those (for example
/// a detached `/usr/bin/perl` daemon) are not found there.
#[cfg(not(target_os = "macos"))]
pub fn carrying_env(var: &str, value: &str, skip: &[u32]) -> Vec<Member> {
    let needle = format!("\0{var}={value}\0").into_bytes();
    let me = std::process::id();
    all_pids()
        .into_iter()
        .filter(|pid| *pid != me && !skip.contains(pid) && info(*pid).is_some_and(|i| !i.zombie))
        .filter(|pid| {
            // `/proc/<pid>/environ` is NUL-separated; wrap it so the first and last entries match too.
            let Ok(env) = std::fs::read(format!("/proc/{pid}/environ")) else { return false };
            let wrapped = [&[0u8][..], &env, &[0u8][..]].concat();
            wrapped.windows(needle.len()).any(|w| w == needle.as_slice())
        })
        .map(|pid| Member { pid, name: process_name(pid) })
        .collect()
}

#[cfg(target_os = "macos")]
pub fn carrying_env(var: &str, value: &str, skip: &[u32]) -> Vec<Member> {
    let entry = format!("{var}={value}");
    let me = std::process::id();
    all_pids()
        .into_iter()
        .filter(|pid| *pid != me && *pid > 1 && !skip.contains(pid) && info(*pid).is_some_and(|i| !i.zombie))
        .filter(|pid| crate::gui_token::proc::cmdline(*pid).is_some_and(|(_, env)| env.contains(&entry)))
        .map(|pid| Member { pid, name: process_name(pid) })
        .collect()
}

#[cfg(target_os = "macos")]
fn all_pids() -> Vec<u32> {
    let count = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    if count <= 0 {
        return Vec::new();
    }
    let mut pids = vec![0 as libc::c_int; count as usize + 64];
    let size = (pids.len() * std::mem::size_of::<libc::c_int>()) as libc::c_int;
    let got = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), size) };
    pids.truncate(got.max(0) as usize);
    pids.into_iter().filter_map(|p| u32::try_from(p).ok()).filter(|p| *p > 0).collect()
}

#[cfg(target_os = "macos")]
fn info(pid: u32) -> Option<Info> {
    let mut i: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    let got = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDTBSDINFO,
            0,
            (&mut i as *mut libc::proc_bsdinfo).cast(),
            size,
        )
    };
    (got == size).then(|| Info {
        ppid: i.pbi_ppid,
        start: i.pbi_start_tvsec * 1_000_000 + i.pbi_start_tvusec,
        zombie: i.pbi_status == libc::SZOMB,
    })
}

#[cfg(target_os = "macos")]
fn process_name(pid: u32) -> String {
    let mut buf = [0u8; 256];
    let n = unsafe { libc::proc_name(pid as libc::c_int, buf.as_mut_ptr().cast(), buf.len() as u32) };
    if n <= 0 { format!("pid {pid}") } else { String::from_utf8_lossy(&buf[..n as usize]).into_owned() }
}

#[cfg(not(target_os = "macos"))]
fn all_pids() -> Vec<u32> {
    std::fs::read_dir("/proc")
        .map(|d| d.filter_map(|e| e.ok()?.file_name().to_str()?.parse().ok()).collect())
        .unwrap_or_default()
}

#[cfg(not(target_os = "macos"))]
fn info(pid: u32) -> Option<Info> {
    // `/proc/<pid>/stat`: "pid (name) S ppid … starttime …"; fields count from after the ')'.
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let (_, rest) = stat.rsplit_once(')')?;
    let fields: Vec<&str> = rest.split_whitespace().collect();
    Some(Info { ppid: fields.get(1)?.parse().ok()?, start: fields.get(19)?.parse().ok()?, zombie: fields[0] == "Z" })
}

#[cfg(not(target_os = "macos"))]
fn process_name(pid: u32) -> String {
    std::fs::read_to_string(format!("/proc/{pid}/comm"))
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| format!("pid {pid}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    #[test]
    fn spawns_in_a_pty_with_size_and_clean_env() {
        let spec = Spec {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), "stty size; echo \"[$FOO]\"; read x; echo got:$x".into()],
            env: vec![("FOO".into(), "bar".into()), ("PATH".into(), "/usr/bin:/bin".into())],
            cwd: std::env::temp_dir(),
        };
        let mut s = spawn(&spec, 91, 17).unwrap();
        s.writer.write_all(b"hello\r").unwrap();
        let mut out = String::new();
        let mut buf = [0u8; 4096];
        while let Ok(n) = s.reader.read(&mut buf) {
            if n == 0 {
                break;
            }
            out.push_str(&String::from_utf8_lossy(&buf[..n]));
            if out.contains("got:hello") {
                break;
            }
        }
        assert!(out.contains("17 91"), "{out:?}");
        assert!(out.contains("[bar]"), "{out:?}");
        assert!(out.contains("got:hello"), "{out:?}");
        assert!(s.child.wait().unwrap().success());
    }

    /// Helper process for the test below: a non-platform binary (this test executable) that
    /// stays alive for a while.
    #[test]
    #[ignore]
    fn sleeper() {
        std::thread::sleep(std::time::Duration::from_secs(30));
    }

    #[test]
    fn a_process_that_left_the_run_is_found_by_its_session_marker() {
        let exe = std::env::current_exe().unwrap();
        let mut child = std::process::Command::new(exe)
            .args(["--exact", "sessions::pty::tests::sleeper", "--ignored", "--test-threads=1"])
            .env(runwick_core::paths::ENV_SESSION_ID, "marker-for-pty-test")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let found = (0..100).any(|_| {
            std::thread::sleep(std::time::Duration::from_millis(50));
            carrying_env(runwick_core::paths::ENV_SESSION_ID, "marker-for-pty-test", &[])
                .iter()
                .any(|m| m.pid == child.id())
        });
        let skipped = carrying_env(runwick_core::paths::ENV_SESSION_ID, "marker-for-pty-test", &[child.id()]);
        let other = carrying_env(runwick_core::paths::ENV_SESSION_ID, "another-session", &[]);
        let _ = child.kill();
        let _ = child.wait();
        assert!(found, "the marked process was not found");
        assert!(skipped.iter().all(|m| m.pid != child.id()), "skip list ignored");
        assert!(other.iter().all(|m| m.pid != child.id()), "matched another session's marker");
    }
}
