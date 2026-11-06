//! Environment of managed terminals.
//!
//! 1. **Base: the user's login environment.** An app opened from Finder has a minimal `PATH`,
//!    so `claude`/`codex` would not be found. Once per host start the login shell is run as
//!    `$SHELL -l -i -c '<print env>'` starting from a minimal environment — what a fresh
//!    Terminal window gets — and the result is cached. If that fails or takes longer than
//!    5 seconds, this process's environment is used instead.
//! 2. **Stripped:** variables that describe another terminal or a running agent session
//!    ([`STRIPPED`], [`STRIPPED_PREFIXES`]). This host may itself have been started from inside
//!    Claude Code or Codex; their session markers must not leak into nested CLIs.
//! 3. **Runwick's own:** `TERM`, `COLORTERM`, `TERM_PROGRAM`, a UTF-8 locale when none is set,
//!    `RUNWICK_SESSION_ID`, `RUNWICK_SOCKET`, `RUNWICK_HOME`, and `PATH` with
//!    `<RUNWICK_HOME>/bin` first (the `runwick` command for agents).
//! 4. **Global environment variables** (a separate feature) come last through the single hook
//!    [`Manager::set_env_hook`](super::Manager::set_env_hook), which returns an [`EnvOverlay`].

use runwick_core::paths;
use std::collections::BTreeMap;
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};
use uuid::Uuid;

/// Exact names removed from every managed terminal.
pub const STRIPPED: &[&str] = &[
    // Claude Code session markers (this host may run inside a Claude Code session).
    "CLAUDECODE",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_SSE_PORT",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_SESSION_ATTENDED",
    "CLAUDE_CODE_SESSION_KIND",
    "CLAUDE_CODE_SESSION_NAME",
    "CLAUDE_CODE_SESSION_ORIGIN",
    "CLAUDE_CODE_SESSION_ACCESS_TOKEN",
    "CLAUDE_CODE_EXECPATH",
    "CLAUDE_CODE_MESSAGING_SOCKET",
    "CLAUDE_CODE_MESSAGING_TOKEN",
    "CLAUDE_CODE_BRIDGE_SESSION_ID",
    "CLAUDE_CODE_REMOTE_SESSION_ID",
    "CLAUDE_CODE_HOST_SESSION_ID",
    "CLAUDE_CODE_TASK_LIST_ID",
    "CLAUDE_CODE_TMUX_SESSION",
    "CLAUDE_PID",
    "CLAUDE_EFFORT",
    "CLAUDE_PLUGIN_DATA",
    "CLAUDE_PLUGIN_ROOT",
    "CLAUDE_PROJECT_DIR",
    "CLAUDE_ENV_FILE",
    "AI_AGENT",
    // Codex session markers (not configuration such as CODEX_HOME).
    "CODEX_THREAD_ID",
    "CODEX_SESSION_ID",
    "CODEX_TOOL_CALL_ID",
    "CODEX_SANDBOX",
    "CODEX_SANDBOX_NETWORK_DISABLED",
    "CODEX_COMPANION_SESSION_ID",
    "CODEX_CI",
    // Another terminal or multiplexer around the host.
    "TMUX",
    "TMUX_PANE",
    "STY",
    "TERM_PROGRAM",
    "TERM_PROGRAM_VERSION",
    "TERM_SESSION_ID",
    "TERMINFO",
    "WINDOWID",
    "ALACRITTY_WINDOW_ID",
    "ALACRITTY_SOCKET",
    "ALACRITTY_LOG",
    "VTE_VERSION",
    "TERMINAL_EMULATOR",
    "WT_SESSION",
    "WT_PROFILE_ID",
    "INSIDE_EMACS",
    "NVIM",
    "NVIM_LISTEN_ADDRESS",
    "LC_TERMINAL",
    "LC_TERMINAL_VERSION",
    "COLORFGBG",
    "__CFBundleIdentifier",
    // Per-shell state that must be recomputed by the new shell.
    "COLUMNS",
    "LINES",
    "PWD",
    "OLDPWD",
    "SHLVL",
    "_",
];

/// Name prefixes removed from every managed terminal.
pub const STRIPPED_PREFIXES: &[&str] = &[
    "CODEX_SNAPSHOT_",
    "CODEX_NETWORK_PROXY_",
    "CODEX_SHELL_SNAPSHOT_",
    "ITERM_",
    "KITTY_",
    "WEZTERM_",
    "GHOSTTY_",
    "VSCODE_",
    "CMUX_",
    "_P9K_",
    "P9K_",
    "RUNWICK_SESSION_",
];

/// Variables added by the global-environment feature for new runs.
#[derive(Debug, Clone, Default)]
pub struct EnvOverlay {
    /// Version recorded on the session (`Session::env_version`) so refresh can tell what changed.
    pub version: u64,
    pub set: Vec<(String, String)>,
    pub unset: Vec<String>,
}

/// Where the base environment came from, for diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    LoginShell,
    /// The login shell could not be read; this process's environment was used.
    HostProcess,
}

#[derive(Debug, Clone)]
pub struct LoginEnv {
    pub vars: BTreeMap<String, String>,
    pub source: Source,
}

impl LoginEnv {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.vars.get(key).map(|s| s.as_str())
    }

    /// The user's shell: login `SHELL`, then the password database, then `/bin/sh`.
    pub fn shell(&self) -> String {
        self.get("SHELL").filter(|s| !s.is_empty()).map(str::to_string).unwrap_or_else(passwd_shell)
    }

    pub fn path(&self) -> &str {
        self.get("PATH").unwrap_or("/usr/bin:/bin:/usr/sbin:/sbin")
    }
}

struct Pending {
    value: Mutex<Option<Arc<LoginEnv>>>,
    ready: Condvar,
}

static LOGIN: OnceLock<Pending> = OnceLock::new();

const CAPTURE_TIMEOUT: Duration = Duration::from_secs(5);
const MARK_BEGIN: &str = "__RUNWICK_ENV_BEGIN_7f3a__";
const MARK_END: &str = "__RUNWICK_ENV_END_7f3a__";

/// Start reading the login environment in the background (called at host boot).
pub fn start_capture() {
    let pending = LOGIN.get_or_init(|| Pending { value: Mutex::new(None), ready: Condvar::new() });
    if pending.value.lock().map(|v| v.is_some()).unwrap_or(true) {
        return;
    }
    let spawned = std::thread::Builder::new().name("runwick-login-env".into()).spawn(|| {
        let env = Arc::new(capture());
        if let Some(p) = LOGIN.get() {
            if let Ok(mut v) = p.value.lock() {
                *v = Some(env);
            }
            p.ready.notify_all();
        }
    });
    if spawned.is_err() {
        set(Arc::new(fallback()));
    }
}

fn set(env: Arc<LoginEnv>) {
    let pending = LOGIN.get_or_init(|| Pending { value: Mutex::new(None), ready: Condvar::new() });
    if let Ok(mut v) = pending.value.lock() {
        *v = Some(env);
    }
    pending.ready.notify_all();
}

/// The cached login environment, waiting for the capture if it is still running.
pub fn login_env() -> Arc<LoginEnv> {
    start_capture();
    let Some(pending) = LOGIN.get() else { return Arc::new(fallback()) };
    let deadline = Instant::now() + CAPTURE_TIMEOUT + Duration::from_secs(1);
    let Ok(mut guard) = pending.value.lock() else { return Arc::new(fallback()) };
    loop {
        if let Some(env) = guard.as_ref() {
            return env.clone();
        }
        let now = Instant::now();
        if now >= deadline {
            return Arc::new(fallback());
        }
        match pending.ready.wait_timeout(guard, deadline - now) {
            Ok((g, _)) => guard = g,
            Err(_) => return Arc::new(fallback()),
        }
    }
}

fn fallback() -> LoginEnv {
    LoginEnv { vars: std::env::vars().collect(), source: Source::HostProcess }
}

/// Run the login shell from a minimal environment and read its variables.
fn capture() -> LoginEnv {
    let shell = std::env::var("SHELL").ok().filter(|s| !s.is_empty()).unwrap_or_else(passwd_shell);
    match run_login_shell(&shell, CAPTURE_TIMEOUT) {
        Some(vars) if vars.contains_key("PATH") => LoginEnv { vars, source: Source::LoginShell },
        _ => {
            tracing::warn!(%shell, "login environment unavailable; using the host environment");
            fallback()
        }
    }
}

fn run_login_shell(shell: &str, timeout: Duration) -> Option<BTreeMap<String, String>> {
    let script = format!("printf '\\n%s\\n' {MARK_BEGIN}; env -0; printf '\\0%s' {MARK_END}");
    let mut cmd = Command::new(shell);
    cmd.args(["-l", "-i", "-c", &script])
        .env_clear()
        .envs(minimal_env(shell))
        .current_dir(paths::user_home())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0);
    let mut child = cmd.spawn().ok()?;
    let mut stdout = child.stdout.take()?;
    let (tx, rx) = std::sync::mpsc::channel();
    // Read until the end marker: rc files may start background helpers that keep the pipe open.
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 16 * 1024];
        loop {
            match stdout.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    buf.extend_from_slice(&chunk[..n]);
                    if find(&buf, MARK_END.as_bytes()).is_some() {
                        break;
                    }
                }
            }
        }
        let _ = tx.send(buf);
    });
    let out = rx.recv_timeout(timeout).ok();
    // Stop the shell and anything it started in its process group.
    let pid = child.id() as i32;
    unsafe {
        libc::kill(-pid, libc::SIGKILL);
    }
    let _ = child.wait();
    parse_env_output(&out?)
}

fn minimal_env(shell: &str) -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = std::env::vars()
        .filter(|(k, _)| {
            matches!(k.as_str(), "HOME" | "USER" | "LOGNAME" | "TMPDIR" | "SSH_AUTH_SOCK" | "__CF_USER_TEXT_ENCODING")
                || k == "LANG"
                || k.starts_with("LC_") && k != "LC_TERMINAL" && k != "LC_TERMINAL_VERSION"
        })
        .collect();
    if !v.iter().any(|(k, _)| k == "HOME") {
        v.push(("HOME".into(), paths::user_home().to_string_lossy().into_owned()));
    }
    v.push(("SHELL".into(), shell.to_string()));
    v.push(("PATH".into(), "/usr/bin:/bin:/usr/sbin:/sbin".into()));
    v.push(("TERM".into(), "xterm-256color".into()));
    // Lets rc files skip slow interactive-only work while Runwick reads the environment.
    v.push(("RUNWICK_RESOLVING_ENVIRONMENT".into(), "1".into()));
    v
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn parse_env_output(out: &[u8]) -> Option<BTreeMap<String, String>> {
    let begin = format!("{MARK_BEGIN}\n");
    let start = find(out, begin.as_bytes())? + begin.len();
    let rest = &out[start..];
    let end = find(rest, MARK_END.as_bytes())?;
    let mut vars = BTreeMap::new();
    for entry in rest[..end].split(|b| *b == 0) {
        let entry = String::from_utf8_lossy(entry);
        if let Some((k, v)) = entry.split_once('=')
            && !k.is_empty()
            && k != "RUNWICK_RESOLVING_ENVIRONMENT"
        {
            vars.insert(k.to_string(), v.to_string());
        }
    }
    Some(vars)
}

fn passwd_shell() -> String {
    unsafe {
        let pw = libc::getpwuid(libc::getuid());
        if !pw.is_null() && !(*pw).pw_shell.is_null() {
            let s = std::ffi::CStr::from_ptr((*pw).pw_shell).to_string_lossy().into_owned();
            if !s.is_empty() {
                return s;
            }
        }
    }
    "/bin/sh".to_string()
}

pub fn is_stripped(name: &str) -> bool {
    STRIPPED.contains(&name) || STRIPPED_PREFIXES.iter().any(|p| name.starts_with(p))
}

/// Full environment for a new run.
pub fn session_env(base: &LoginEnv, session: Uuid, cwd: &str, overlay: &EnvOverlay) -> Vec<(String, String)> {
    let mut vars: BTreeMap<String, String> =
        base.vars.iter().filter(|(k, _)| !is_stripped(k)).map(|(k, v)| (k.clone(), v.clone())).collect();
    vars.insert("TERM".into(), "xterm-256color".into());
    vars.insert("COLORTERM".into(), "truecolor".into());
    vars.insert("TERM_PROGRAM".into(), "Runwick".into());
    vars.insert("TERM_PROGRAM_VERSION".into(), runwick_core::VERSION.into());
    // Light text on a dark ground (matches the terminal palette).
    vars.insert("COLORFGBG".into(), "15;0".into());
    if !has_utf8_locale(&vars) {
        vars.insert("LANG".into(), default_utf8_locale(vars.get("LANG").map(String::as_str)));
    }
    vars.insert(paths::ENV_SESSION_ID.into(), session.to_string());
    vars.insert(paths::ENV_SOCKET.into(), paths::socket_path().to_string_lossy().into_owned());
    vars.insert(paths::ENV_HOME.into(), paths::home().to_string_lossy().into_owned());
    vars.insert("PWD".into(), cwd.to_string());
    let bin = paths::bin_dir().to_string_lossy().into_owned();
    let path = base.path().split(':').filter(|p| !p.is_empty() && *p != bin).collect::<Vec<_>>().join(":");
    vars.insert("PATH".into(), if path.is_empty() { bin } else { format!("{bin}:{path}") });
    for k in &overlay.unset {
        vars.remove(k);
    }
    for (k, v) in &overlay.set {
        vars.insert(k.clone(), v.clone());
    }
    vars.into_iter().collect()
}

fn has_utf8_locale(vars: &BTreeMap<String, String>) -> bool {
    ["LC_ALL", "LC_CTYPE", "LANG"].iter().any(|k| {
        vars.get(*k).is_some_and(|v| {
            let v = v.to_ascii_lowercase();
            v.contains("utf-8") || v.contains("utf8")
        })
    })
}

/// `ko_KR.UTF-8` for a Korean setup, otherwise `en_US.UTF-8`.
fn default_utf8_locale(lang: Option<&str>) -> String {
    let korean = lang.is_some_and(|l| l.starts_with("ko")) || system_language_is_korean();
    if korean { "ko_KR.UTF-8".into() } else { "en_US.UTF-8".into() }
}

fn system_language_is_korean() -> bool {
    #[cfg(target_os = "macos")]
    {
        static KOREAN: OnceLock<bool> = OnceLock::new();
        *KOREAN.get_or_init(|| {
            Command::new("/usr/bin/defaults")
                .args(["read", "-g", "AppleLocale"])
                .stderr(Stdio::null())
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().starts_with("ko"))
                .unwrap_or(false)
        })
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base(vars: &[(&str, &str)]) -> LoginEnv {
        LoginEnv {
            vars: vars.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            source: Source::LoginShell,
        }
    }

    #[test]
    fn strips_agent_and_terminal_markers_but_keeps_configuration() {
        let b = base(&[
            ("CLAUDECODE", "1"),
            ("CLAUDE_CODE_ENTRYPOINT", "cli"),
            ("CLAUDE_CODE_MESSAGING_TOKEN", "secret"),
            ("CLAUDE_CODE_USE_BEDROCK", "1"),
            ("CODEX_THREAD_ID", "x"),
            ("CODEX_HOME", "/h/.codex"),
            ("TMUX", "/tmp/t"),
            ("TERM_PROGRAM", "iTerm.app"),
            ("_P9K_TTY", "/dev/ttys001"),
            ("PATH", "/opt/homebrew/bin:/usr/bin"),
            ("LANG", "en_US.UTF-8"),
        ]);
        let id = Uuid::new_v4();
        let env: BTreeMap<String, String> = session_env(&b, id, "/w", &EnvOverlay::default()).into_iter().collect();
        for gone in [
            "CLAUDECODE",
            "CLAUDE_CODE_ENTRYPOINT",
            "CLAUDE_CODE_MESSAGING_TOKEN",
            "CODEX_THREAD_ID",
            "TMUX",
            "_P9K_TTY",
        ] {
            assert!(!env.contains_key(gone), "{gone} leaked");
        }
        assert_eq!(env["CLAUDE_CODE_USE_BEDROCK"], "1");
        assert_eq!(env["CODEX_HOME"], "/h/.codex");
        assert_eq!(env["TERM"], "xterm-256color");
        assert_eq!(env["TERM_PROGRAM"], "Runwick");
        assert_eq!(env["COLORTERM"], "truecolor");
        assert_eq!(env["RUNWICK_SESSION_ID"], id.to_string());
        assert_eq!(env["PWD"], "/w");
        let bin = paths::bin_dir().to_string_lossy().into_owned();
        assert_eq!(env["PATH"], format!("{bin}:/opt/homebrew/bin:/usr/bin"));
        assert_eq!(env["LANG"], "en_US.UTF-8");
    }

    #[test]
    fn adds_a_utf8_locale_and_applies_the_overlay_last() {
        let b = base(&[("PATH", "/usr/bin"), ("LANG", "C")]);
        let overlay = EnvOverlay { version: 3, set: vec![("FOO".into(), "bar".into())], unset: vec!["PATH".into()] };
        let env: BTreeMap<String, String> = session_env(&b, Uuid::new_v4(), "/", &overlay).into_iter().collect();
        assert!(env["LANG"].ends_with(".UTF-8"));
        assert_eq!(env["FOO"], "bar");
        assert!(!env.contains_key("PATH"));
    }

    #[test]
    fn parses_marked_env_output_with_noise_around_it() {
        let out = format!("motd noise\n{MARK_BEGIN}\nA=1\0B=x=y\0EMPTY=\0\0{MARK_END}trailing");
        let vars = parse_env_output(out.as_bytes()).unwrap();
        assert_eq!(vars["A"], "1");
        assert_eq!(vars["B"], "x=y");
        assert_eq!(vars["EMPTY"], "");
        assert!(parse_env_output(b"no markers").is_none());
    }

    #[test]
    fn login_shell_capture_reads_a_real_shell() {
        // Generous here: on a busy machine (parallel builds) a login shell can take longer than
        // the host's 5 s, which then falls back to the host environment by design.
        let vars = run_login_shell("/bin/sh", Duration::from_secs(60)).expect("sh -l -i -c works");
        assert!(vars.contains_key("PATH"));
        assert!(!vars.contains_key("RUNWICK_RESOLVING_ENVIRONMENT"));
        // Started from a minimal environment: the host's agent markers are not inherited.
        assert!(!vars.contains_key("CLAUDECODE"));
    }
}
