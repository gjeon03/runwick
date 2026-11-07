//! One run of a session: the process in its PTY and the threads that serve it.
//!
//! - reader: PTY output → host terminal model → attached clients (in that order, under the
//!   session lock, so an attach snapshot is never ahead of or behind the live stream)
//! - writer: client input and terminal replies → PTY
//! - waiter: process exit → record and live state
//! - (Codex) watcher: discovers the thread id for resume

use super::env::{self, EnvOverlay};
use super::rt::{Rt, RtState};
use super::{codex, pty, record, screens};
use crate::host::Host;
use runwick_core::api::sessions::{Phase, Session, StartErrorKind, Tool};
use runwick_core::rpc::topic;
use runwick_core::wire::Frame;
use std::ffi::OsString;
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::OwnedFd;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime};

pub struct Run {
    pub seq: u64,
    pub pid: u32,
    pub input: mpsc::Sender<Vec<u8>>,
    pub master: OwnedFd,
}

/// What to start.
pub struct Launch<'a> {
    pub session: &'a Session,
    /// Resume the saved provider conversation instead of starting a new one.
    pub resume: bool,
    pub initial_prompt: Option<&'a str>,
}

pub type StartError = (StartErrorKind, String);

/// Start a run. Blocking (waits for the login environment and spawns a process).
pub fn start(host: &Arc<Host>, rt: &Arc<Rt>, launch: Launch<'_>, overlay: &EnvOverlay) -> Result<(), StartError> {
    let s = launch.session;
    let cwd = PathBuf::from(&s.cwd);
    if !cwd.is_dir() {
        return Err((StartErrorKind::FolderMissing, format!("작업 폴더를 찾을 수 없어요: {}", s.cwd)));
    }
    let login = env::login_env();
    let vars = env::session_env(&login, s.id, &s.cwd, overlay);
    let path = vars.iter().find(|(k, _)| k == "PATH").map(|(_, v)| v.as_str()).unwrap_or("");
    let guidance = host.sessions.guidance(host, s);
    let (program, args) = command(s, &launch, &login.shell(), path, guidance.as_deref(), &vars)?;
    let mut spec = pty::Spec { program, args, env: vars, cwd: cwd.clone() };

    let mut st = rt.lock();
    // Proof of being this session for the `runwick` command (collab::identity). Issued under the
    // lock, so a late end of the previous run (which revokes under the same lock) cannot take it.
    spec.env.push((runwick_core::paths::ENV_SESSION_TOKEN.into(), host.sessions.issue_token(s.id)));
    let size = st.term.size();
    let spawned = pty::spawn(&spec, size.cols, size.rows)
        .map_err(|e| (StartErrorKind::SpawnFailed, format!("{} 실행에 실패했어요: {e}", s.tool.label())))?;
    st.run_seq += 1;
    let seq = st.run_seq;
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    let pid = spawned.child.id();
    st.run = Some(Run { seq, pid, input: tx, master: spawned.master });
    st.phase = Phase::Starting;
    st.ever_ran = true;
    st.exit_code = None;
    st.error = None;
    st.attention = None;
    st.scanner = Default::default();
    drop(st);

    let now = runwick_core::now();
    if let Err(e) = record::mark_started(&host.store, s.id, &now, overlay.version) {
        tracing::warn!(session = %s.id, "record start failed: {e}");
    }
    host.sessions.started(host, s, overlay);
    let (done_tx, done_rx) = mpsc::channel::<()>();
    spawn_thread("pty-read", {
        let (host, rt) = (host.clone(), rt.clone());
        move || read_loop(&host, &rt, seq, spawned.reader, done_tx)
    });
    spawn_thread("pty-write", move || write_loop(rx, spawned.writer));
    spawn_thread("pty-wait", {
        let (host, rt) = (host.clone(), rt.clone());
        let mut child = spawned.child;
        move || {
            let status = child.wait();
            // Let the reader drain what the program wrote last.
            let _ = done_rx.recv_timeout(Duration::from_millis(500));
            let code = status.ok().map(|s| s.code().unwrap_or_else(|| 128 + s.signal().unwrap_or(0)));
            finish(&host, &rt, seq, code);
        }
    });
    if s.tool == Tool::Codex && s.provider_conv_id.is_none() {
        let started = SystemTime::now();
        let root = codex::sessions_dir(login.get("CODEX_HOME"));
        spawn_thread("codex-thread-id", {
            let (host, rt) = (host.clone(), rt.clone());
            move || watch_codex_thread(&host, &rt, seq, &root, &cwd, started)
        });
    }
    host.sessions.wake();
    tracing::info!(session = %s.id, tool = ?s.tool, pid, "run started");
    Ok(())
}

fn spawn_thread(name: &str, f: impl FnOnce() + Send + 'static) {
    if let Err(e) = std::thread::Builder::new().name(format!("runwick-{name}")).spawn(f) {
        tracing::error!("cannot start {name} thread: {e}");
    }
}

/// Program and arguments for a tool; CLIs are resolved on the run's `PATH`. `guidance` is
/// given to Claude Code as `--append-system-prompt` (2.1.319) and Codex as
/// `developer_instructions` (0.167.0). Codex runs without the shared daemon so its tools keep
/// this run's process ancestry and lifetime. Only public Runwick variables enter its config;
/// the session token must stay out of argv, and known peers are identified by ancestry.
fn command(
    s: &Session,
    launch: &Launch<'_>,
    shell: &str,
    path: &str,
    guidance: Option<&str>,
    vars: &[(String, String)],
) -> Result<(PathBuf, Vec<OsString>), StartError> {
    let mut args: Vec<OsString> = Vec::new();
    let program = match s.tool {
        Tool::Shell => {
            args.push("-l".into());
            PathBuf::from(shell)
        }
        Tool::Claude | Tool::Codex => {
            let name = if s.tool == Tool::Claude { "claude" } else { "codex" };
            let Some(path) = crate::tools::resolve(name, path) else {
                return Err((
                    StartErrorKind::NotInstalled,
                    format!("{} CLI(`{name}`)를 찾지 못했어요. 설치한 뒤 다시 시도해 주세요.", s.tool.label()),
                ));
            };
            let guidance = guidance.filter(|g| !g.trim().is_empty());
            if s.tool == Tool::Codex {
                // Global options precede `resume <id>` as well as a fresh prompt.
                args.push("--no-daemon".into());
                for (key, value) in vars {
                    if matches!(key.as_str(), "RUNWICK_HOME" | "RUNWICK_SOCKET" | "RUNWICK_SESSION_ID") {
                        codex_config(&mut args, &format!("shell_environment_policy.set.{key}"), value);
                    }
                }
                if let Some(g) = guidance {
                    codex_config(&mut args, "developer_instructions", g);
                }
            }
            let conv = s.provider_conv_id.as_deref();
            match (s.tool, launch.resume, conv) {
                (_, true, None) => {
                    return Err((
                        StartErrorKind::NotResumable,
                        "저장된 대화 ID가 없어 같은 대화를 재개할 수 없어요".into(),
                    ));
                }
                (Tool::Claude, true, Some(id)) => args.extend(["--resume".into(), id.into()]),
                (Tool::Claude, false, Some(id)) => args.extend(["--session-id".into(), id.into()]),
                (Tool::Codex, true, Some(id)) => args.extend(["resume".into(), id.into()]),
                _ => {}
            }
            if s.tool == Tool::Claude
                && let Some(g) = guidance
            {
                args.extend(["--append-system-prompt".into(), g.into()]);
            }
            if !launch.resume
                && let Some(p) = launch.initial_prompt.filter(|p| !p.trim().is_empty())
            {
                // The prompt can come from another agent (delegation). End option parsing first so a
                // prompt such as "--flag ..." is always read as text, never as a CLI option.
                // Both CLIs' parsers honour `--` (checked with Claude Code 2.1.319 and codex-cli
                // 0.164.0: a prompt "--version …" arrives as the first message, not as the option).
                args.push("--".into());
                args.push(p.into());
            }
            path
        }
    };
    if s.tool == Tool::Shell && !Path::new(&program).is_file() {
        return Err((StartErrorKind::NotInstalled, format!("셸을 찾지 못했어요: {}", program.display())));
    }
    Ok((program, args))
}

fn codex_config(args: &mut Vec<OsString>, key: &str, value: &str) {
    // JSON escaping plus DEL escaping produces a TOML basic string. Pass one argv entry,
    // never shell text, so quotes and newlines cannot add options or config keys.
    let value = serde_json::to_string(value).expect("a string serializes").replace('\u{7f}', "\\u007f");
    args.extend(["-c".into(), format!("{key}={value}").into()]);
}

fn read_loop(host: &Arc<Host>, rt: &Arc<Rt>, seq: u64, mut reader: File, done: mpsc::Sender<()>) {
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break, // EIO once the last process holding the terminal is gone
        };
        let data = &buf[..n];
        {
            let mut st = rt.lock();
            if st.run_seq != seq {
                continue; // a previous run's straggler; its terminal state was reset
            }
            on_output(rt, &mut st, data);
        }
        host.sessions.wake();
    }
    let _ = done.send(());
}

fn on_output(rt: &Rt, st: &mut RtState, data: &[u8]) {
    let fx = st.term.advance(data);
    let content = st.scanner.scan(data);
    st.content_seq += 1;
    // Clients get what passed the output guard, the same bytes the host's terminal saw.
    st.attached.retain(|a| Frame::output_chunks(rt.id, &fx.forward).all(|f| a.client.send(f)));
    if !fx.reply.is_empty()
        && let Some(run) = &st.run
    {
        let _ = run.input.send(fx.reply);
    }
    // Agent CLIs retitle the window constantly; their session title comes from the person or
    // the first user message (records), never from the program. See `live::shell_title`.
    if let Some(title) = fx.title
        && !rt.tool.is_agent()
    {
        st.pending_title = Some((title, Instant::now()));
    }
    if fx.bell && !st.anyone_viewing() {
        st.bell_at = Some(runwick_core::now());
    }
    if content {
        st.last_output = Some(Instant::now());
        st.last_output_at = Some(runwick_core::now());
        st.screen_dirty = true;
        if !st.anyone_viewing() {
            st.unread = true;
        }
        if st.phase == Phase::Starting {
            st.phase = Phase::Running;
        }
    }
}

fn write_loop(rx: mpsc::Receiver<Vec<u8>>, mut writer: File) {
    for data in rx {
        if writer.write_all(&data).is_err() {
            break;
        }
    }
}

fn finish(host: &Arc<Host>, rt: &Arc<Rt>, seq: u64, code: Option<i32>) {
    let screen = {
        let mut st = rt.lock();
        if st.run_seq != seq {
            return;
        }
        st.run = None;
        host.sessions.revoke_token(rt.id);
        st.saved_seq = st.content_seq;
        (st.content_seq, st.term.size(), st.term.snapshot_with_history(screens::HISTORY))
    };
    if let Err(e) = screens::save(rt.id, screen.0, screen.1, &screen.2) {
        tracing::warn!(session = %rt.id, "cannot save the last screen: {e}");
    }
    // Recorded before it is announced: whoever sees the run as ended also finds the record
    // ended (never a stopped run still marked as interrupted).
    if let Err(e) = record::mark_ended(&host.store, rt.id, &runwick_core::now(), code) {
        tracing::warn!(session = %rt.id, "record end failed: {e}");
    }
    {
        let mut st = rt.lock();
        if st.run_seq == seq {
            st.phase = Phase::Exited;
            st.exit_code = code;
            st.attention = None;
        }
    }
    tracing::info!(session = %rt.id, ?code, "run ended");
    host.bus.changed(&[topic::SESSIONS]);
    host.sessions.wake();
}

pub fn is_running(rt: &Rt) -> bool {
    rt.lock().run.is_some()
}

fn watch_codex_thread(host: &Arc<Host>, rt: &Arc<Rt>, seq: u64, root: &Path, cwd: &Path, started: SystemTime) {
    let begun = Instant::now();
    loop {
        std::thread::sleep(codex::next_poll(begun.elapsed()));
        if rt.lock().run.as_ref().is_none_or(|r| r.seq != seq) {
            return;
        }
        for c in codex::candidates(root, cwd, started) {
            match record::conv_id_taken(&host.store, &c.id) {
                Ok(false) => {}
                _ => continue,
            }
            if let Ok(true) = record::claim_conv_id(&host.store, rt.id, &c.id) {
                tracing::info!(session = %rt.id, thread = %c.id, "codex thread found");
                host.bus.changed(&[topic::SESSIONS]);
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(tool: Tool, conv: Option<&str>) -> Session {
        Session {
            id: uuid::Uuid::new_v4(),
            project_id: uuid::Uuid::new_v4(),
            workspace_id: uuid::Uuid::new_v4(),
            title: "t".into(),
            title_user_set: false,
            tool,
            provider_conv_id: conv.map(str::to_string),
            parent_id: None,
            role: None,
            cwd: "/".into(),
            created_at: runwick_core::now(),
            last_active_at: None,
            ended_at: None,
            exit_code: None,
            hidden: false,
            archived: false,
            env_version: 0,
            interrupted: false,
            origin_device: None,
        }
    }

    fn fake_bin(dir: &Path, name: &str) {
        use std::os::unix::fs::PermissionsExt;
        let p = dir.join(name);
        std::fs::write(&p, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn args(r: Result<(PathBuf, Vec<OsString>), StartError>) -> Vec<String> {
        r.unwrap().1.into_iter().map(|a| a.to_string_lossy().into_owned()).collect()
    }

    #[test]
    fn only_shells_take_titles_from_the_program() {
        let osc = b"\x1b]2;\xe2\x9c\xb3 Fix login bug\x07";
        for tool in Tool::ALL {
            let rt = Rt::new(uuid::Uuid::new_v4(), tool, super::super::term::Size::clamped(80, 24));
            let mut st = rt.lock();
            on_output(&rt, &mut st, osc);
            assert_eq!(st.pending_title.is_some(), !tool.is_agent(), "{tool:?}");
        }
    }

    #[test]
    fn missing_cli_is_reported_as_not_installed() {
        let s = session(Tool::Claude, Some("abc"));
        let launch = Launch { session: &s, resume: false, initial_prompt: None };
        let err = command(&s, &launch, "/bin/sh", "/nonexistent", None, &[]).unwrap_err();
        assert_eq!(err.0, StartErrorKind::NotInstalled);
        assert!(err.1.contains("claude"), "{}", err.1);
    }

    #[test]
    fn agent_arguments_for_new_resume_and_prompt() {
        let d = tempfile::tempdir().unwrap();
        fake_bin(d.path(), "claude");
        fake_bin(d.path(), "codex");
        let path = d.path().to_string_lossy().into_owned();
        let c = session(Tool::Claude, Some("uuid-1"));
        let new = Launch { session: &c, resume: false, initial_prompt: Some("고쳐줘") };
        assert_eq!(args(command(&c, &new, "/bin/sh", &path, None, &[])), ["--session-id", "uuid-1", "--", "고쳐줘"]);
        // A flag-shaped prompt stays a positional argument after the end-of-options marker.
        let flaggy = Launch { session: &c, resume: false, initial_prompt: Some("--dangerous-flag do it") };
        assert_eq!(
            args(command(&c, &flaggy, "/bin/sh", &path, None, &[])),
            ["--session-id", "uuid-1", "--", "--dangerous-flag do it"]
        );
        let resume = Launch { session: &c, resume: true, initial_prompt: Some("ignored") };
        assert_eq!(args(command(&c, &resume, "/bin/sh", &path, None, &[])), ["--resume", "uuid-1"]);
        let x = session(Tool::Codex, None);
        let new = Launch { session: &x, resume: false, initial_prompt: None };
        assert_eq!(args(command(&x, &new, "/bin/sh", &path, None, &[])), ["--no-daemon"]);
        let flaggy = Launch { session: &x, resume: false, initial_prompt: Some("-c sandbox=none 정리") };
        assert_eq!(
            args(command(&x, &flaggy, "/bin/sh", &path, None, &[])),
            ["--no-daemon", "--", "-c sandbox=none 정리"]
        );
        let resume = Launch { session: &x, resume: true, initial_prompt: None };
        assert_eq!(command(&x, &resume, "/bin/sh", &path, None, &[]).unwrap_err().0, StartErrorKind::NotResumable);
        let x = session(Tool::Codex, Some("thread-9"));
        let resume = Launch { session: &x, resume: true, initial_prompt: None };
        assert_eq!(args(command(&x, &resume, "/bin/sh", &path, None, &[])), ["--no-daemon", "resume", "thread-9"]);
        let sh = session(Tool::Shell, None);
        let new = Launch { session: &sh, resume: false, initial_prompt: None };
        assert_eq!(args(command(&sh, &new, "/bin/sh", "", None, &[])), ["-l"]);
    }

    #[test]
    fn guidance_goes_before_the_prompt_marker() {
        let d = tempfile::tempdir().unwrap();
        fake_bin(d.path(), "claude");
        fake_bin(d.path(), "codex");
        let path = d.path().to_string_lossy().into_owned();
        let g = Some("Use \"runwick\".\nLine 2");
        let c = session(Tool::Claude, Some("uuid-1"));
        let new = Launch { session: &c, resume: false, initial_prompt: Some("고쳐줘") };
        assert_eq!(
            args(command(&c, &new, "/bin/sh", &path, g, &[])),
            ["--session-id", "uuid-1", "--append-system-prompt", "Use \"runwick\".\nLine 2", "--", "고쳐줘"]
        );
        let resume = Launch { session: &c, resume: true, initial_prompt: None };
        assert_eq!(
            args(command(&c, &resume, "/bin/sh", &path, g, &[])),
            ["--resume", "uuid-1", "--append-system-prompt", "Use \"runwick\".\nLine 2"]
        );
        let x = session(Tool::Codex, Some("thread-9"));
        let resume = Launch { session: &x, resume: true, initial_prompt: None };
        assert_eq!(
            args(command(&x, &resume, "/bin/sh", &path, g, &[])),
            ["--no-daemon", "-c", "developer_instructions=\"Use \\\"runwick\\\".\\nLine 2\"", "resume", "thread-9"]
        );
        let x = session(Tool::Codex, None);
        let new = Launch { session: &x, resume: false, initial_prompt: Some("검토") };
        assert_eq!(
            args(command(&x, &new, "/bin/sh", &path, g, &[])),
            ["--no-daemon", "-c", "developer_instructions=\"Use \\\"runwick\\\".\\nLine 2\"", "--", "검토"]
        );
        let sh = session(Tool::Shell, None);
        let new = Launch { session: &sh, resume: false, initial_prompt: None };
        assert_eq!(args(command(&sh, &new, "/bin/sh", "", g, &[])), ["-l"]);
    }

    #[test]
    fn codex_sets_only_public_routing_variables_even_without_guidance() {
        let d = tempfile::tempdir().unwrap();
        fake_bin(d.path(), "codex");
        let path = d.path().to_string_lossy().into_owned();
        let home = "/tmp/한글 \"home\"\\path\nnext\u{7f}";
        let vars: Vec<(String, String)> = [
            ("RUNWICK_HOME", home),
            ("RUNWICK_SOCKET", "/tmp/runwick.sock"),
            ("RUNWICK_SESSION_ID", "session-id"),
            ("RUNWICK_SESSION_TOKEN", "must-not-appear"),
            ("UNRELATED_KEY", "also-private"),
            ("PATH", "/preserve/the/users/path"),
        ]
        .into_iter()
        .map(|(k, v)| (k.into(), v.into()))
        .collect();
        for resume in [false, true] {
            let s = session(Tool::Codex, resume.then_some("thread-9"));
            let launch = Launch { session: &s, resume, initial_prompt: Some("--flag-shaped task") };
            for guidance in [None, Some(""), Some(" \n"), Some("Delegate when useful")] {
                let actual = args(command(&s, &launch, "/bin/sh", &path, guidance, &vars));
                assert_eq!(actual[0], "--no-daemon");
                let configs: Vec<_> = actual.windows(2).filter(|a| a[0] == "-c").map(|a| a[1].as_str()).collect();
                let enabled = guidance.is_some_and(|g| !g.trim().is_empty());
                assert_eq!(configs.len(), if enabled { 4 } else { 3 });
                for (key, expected) in vars.iter().take(3) {
                    let prefix = format!("shell_environment_policy.set.{key}=");
                    let value = configs.iter().find_map(|c| c.strip_prefix(&prefix)).unwrap();
                    assert!(!value.contains('\u{7f}'), "TOML requires DEL to be escaped");
                    assert_eq!(serde_json::from_str::<String>(value).unwrap(), *expected);
                }
                assert_eq!(configs.iter().any(|c| c.starts_with("developer_instructions=")), enabled);
                for forbidden in
                    ["TOKEN", "must-not-appear", "UNRELATED_KEY", "also-private", "PATH", "sandbox", "approval_policy"]
                {
                    assert!(!actual.iter().any(|a| a.contains(forbidden)), "{forbidden}: {actual:?}");
                }
                let tail = if resume { ["resume", "thread-9"] } else { ["--", "--flag-shaped task"] };
                assert_eq!(&actual[actual.len() - 2..], tail);
            }
        }
    }
}
