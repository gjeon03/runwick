//! Reaching a device's host: `ssh … '<runwick> host --stdio'` speaks the same framed protocol
//! as the local socket. Failures are classified from ssh's exit status and its stderr so the
//! person sees the cause (network, login, host key, missing Runwick, no answer) instead of a
//! generic error — and never mistakes it for an AI run failing.

use crate::client::Transport;
use runwick_core::api::devices::{Device, SIMULATED_PREFIX};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Env var that allows simulated devices (`sim:<home>` targets; development and tests only).
pub const SIMULATED_ENV: &str = "RUNWICK_DEV_SIMULATED_DEVICES";

/// Development only: never in a release build (see `hook_env!`).
pub fn simulated_allowed() -> bool {
    cfg!(any(debug_assertions, feature = "verify-hooks")) && std::env::var(SIMULATED_ENV).is_ok_and(|v| v == "1")
}

/// How to start a device's host.
#[derive(Debug, Clone, PartialEq)]
pub enum Route {
    Ssh {
        target: String,
        port: Option<u16>,
        exe: String,
    },
    /// Another data folder on this computer, started like a remote host (development only).
    Simulated {
        home: PathBuf,
        exe: PathBuf,
    },
}

impl Route {
    /// The route for a registered device; `local_exe` runs simulated devices.
    pub fn for_device(d: &Device, local_exe: &std::path::Path) -> Result<Route, String> {
        if let Some(home) = d.target.strip_prefix(SIMULATED_PREFIX) {
            if !simulated_allowed() {
                return Err(format!("시뮬레이션 기기는 개발용이에요 ({SIMULATED_ENV}=1 일 때만 연결해요)"));
            }
            return Ok(Route::Simulated { home: PathBuf::from(home), exe: local_exe.to_path_buf() });
        }
        let exe = d.remote_exe.clone().filter(|e| !e.trim().is_empty()).ok_or("Runwick 경로가 아직 없어요")?;
        Ok(Route::Ssh { target: d.target.clone(), port: d.port, exe })
    }

    /// Command that relays the device host's protocol over stdin/stdout.
    pub fn host_command(&self) -> Command {
        match self {
            Route::Ssh { target, port, exe } => {
                let mut c = ssh(target, *port, &SshOpts { keepalive: true, ..Default::default() });
                c.arg(format!("{} host --stdio", sh_quote(exe)));
                c
            }
            Route::Simulated { home, exe } => {
                let mut c = Command::new(exe);
                c.args(["host", "--stdio"]).env("RUNWICK_HOME", home);
                c
            }
        }
    }

    pub fn describe(&self) -> String {
        match self {
            Route::Ssh { target, port: Some(p), .. } => format!("SSH {target}:{p}"),
            Route::Ssh { target, .. } => format!("SSH {target}"),
            Route::Simulated { home, .. } => format!("시뮬레이션 {}", home.display()),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct SshOpts {
    /// Accept and remember a host key seen for the first time (only after the person confirmed
    /// the fingerprint). Otherwise an unknown key fails.
    pub accept_new: bool,
    /// Detect a dead link (sleep, network change) within about 45 seconds.
    pub keepalive: bool,
}

/// `ssh` for `target` without prompts: never asks for a password or a host key.
pub fn ssh(target: &str, port: Option<u16>, o: &SshOpts) -> Command {
    let mut c = Command::new("ssh");
    c.args(["-T", "-o", "BatchMode=yes", "-o", "ConnectTimeout=10"]);
    if o.keepalive {
        c.args(["-o", "ServerAliveInterval=15", "-o", "ServerAliveCountMax=3"]);
    }
    if o.accept_new {
        c.args(["-o", "StrictHostKeyChecking=accept-new"]);
    }
    if let Some(p) = port {
        c.args(["-p", &p.to_string()]);
    }
    c.args(["--", target]);
    c
}

/// Quote for a POSIX shell (also fine for zsh, bash and fish): `'…'` with `'` escaped.
pub fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Which of the five causes a failure belongs to (shown as `원인 · …`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cause {
    Network,
    Auth,
    HostKey,
    Runwick,
    NoAnswer,
}

impl Cause {
    pub fn label(self) -> &'static str {
        match self {
            Cause::Network => "네트워크",
            Cause::Auth => "SSH 로그인",
            Cause::HostKey => "호스트 키",
            Cause::Runwick => "Runwick 없음",
            Cause::NoAnswer => "응답 없음",
        }
    }
}

/// A classified connection failure. `explain` is a full Korean sentence for the person;
/// `detail` is ssh's own last line (shown on hover).
#[derive(Debug, Clone, PartialEq)]
pub struct Failure {
    pub cause: Cause,
    pub kind: Kind,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    NoAddress,
    NoRoute,
    SshOff,
    Dropped,
    Timeout,
    Denied,
    UnknownKey,
    ChangedKey,
    Missing,
    Other,
}

impl Failure {
    pub fn new(kind: Kind, detail: impl Into<String>) -> Self {
        let cause = match kind {
            Kind::NoAddress | Kind::NoRoute | Kind::SshOff | Kind::Dropped | Kind::Other => Cause::Network,
            Kind::Timeout => Cause::NoAnswer,
            Kind::Denied => Cause::Auth,
            Kind::UnknownKey | Kind::ChangedKey => Cause::HostKey,
            Kind::Missing => Cause::Runwick,
        };
        Failure { cause, kind, detail: detail.into() }
    }

    /// Explanation naming the device.
    pub fn explain(&self, name: &str) -> String {
        match self.kind {
            Kind::NoAddress => "주소를 찾을 수 없어요. 기기 주소나 Tailscale 연결을 확인하세요.".into(),
            Kind::NoRoute => format!("{name}에 닿지 않아요. 같은 네트워크나 Tailscale에 연결돼 있는지 확인하세요."),
            Kind::SshOff => format!(
                "{name}에서 SSH 접속이 꺼져 있어요. macOS라면 시스템 설정 › 일반 › 공유에서 ‘원격 로그인’을 켜세요."
            ),
            Kind::Dropped => "연결이 중간에 끊겼어요. 네트워크가 불안정할 수 있어요.".into(),
            Kind::Timeout => {
                format!("{name}에서 응답이 없어요. 꺼져 있거나 잠자기 중일 수 있어요.")
            }
            Kind::Denied => {
                format!("이 컴퓨터의 공개 키가 {name}에 등록되지 않았어요. 네트워크 문제는 아니에요.")
            }
            Kind::UnknownKey => "처음 보는 기기라 연결하지 않았어요. 지문을 확인한 뒤 다시 연결하세요.".into(),
            Kind::ChangedKey => {
                format!("{name}의 호스트 키가 등록할 때와 달라요. 다른 기기가 응답했을 수 있어 연결하지 않았어요.")
            }
            Kind::Missing => {
                format!("{name}에서 Runwick을 찾을 수 없어요. 기기 설정에서 다시 준비하세요.")
            }
            Kind::Other if self.detail.is_empty() => "연결하지 못했어요.".into(),
            Kind::Other => format!("연결하지 못했어요 · {}", self.detail),
        }
    }

    /// Retrying soon helps only for passing network trouble.
    pub fn transient(&self) -> bool {
        matches!(self.kind, Kind::NoRoute | Kind::Dropped | Kind::Timeout | Kind::Other | Kind::NoAddress)
    }
}

/// Classify a finished ssh from its exit code and stderr. `None` for a clean end
/// (exit 0 without an error message).
pub fn classify(stderr: &str, code: Option<i32>) -> Option<Failure> {
    let lower = stderr.to_lowercase();
    let detail = stderr
        .lines()
        .map(str::trim)
        .rfind(|l| !l.is_empty() && !l.starts_with('@') && !l.starts_with("Warning: Permanently added"))
        .unwrap_or("")
        .to_string();
    let has = |s: &str| lower.contains(s);
    let kind = if has("remote host identification has changed") || (has("host key for") && has("has changed")) {
        Kind::ChangedKey
    } else if has("host key verification failed") || has("no matching host key") {
        Kind::UnknownKey
    } else if has("permission denied") || has("too many authentication failures") || has("authentication failed") {
        Kind::Denied
    } else if has("could not resolve hostname") || has("name or service not known") || has("nodename nor servname") {
        Kind::NoAddress
    } else if has("connection refused") {
        Kind::SshOff
    } else if has("timed out") || has("not responding") || has("timeout") {
        Kind::Timeout
    } else if has("no route to host") || has("network is unreachable") || has("host is down") {
        Kind::NoRoute
    } else if code == Some(127)
        || code == Some(126)
        || has("command not found")
        || (has("no such file or directory") && has("runwick"))
    {
        Kind::Missing
    } else if has("connection reset") || has("connection closed") || has("broken pipe") || has("kex_exchange") {
        Kind::Dropped
    } else if code == Some(0) && detail.is_empty() {
        return None;
    } else if code.is_none() {
        // Ended by a signal (killed relay, sleep): the link dropped.
        Kind::Dropped
    } else if code == Some(255) {
        Kind::NoRoute
    } else {
        Kind::Other
    };
    Some(Failure::new(kind, detail))
}

/// One running `ssh` (or simulated host) process.
type Proc = Arc<Mutex<Option<Child>>>;

/// [`Transport`] to a device's host. Each open starts a fresh process; the last failure stays
/// readable for the status lines.
pub struct SshTransport {
    route: Route,
    last_failure: Arc<Mutex<Option<Failure>>>,
    current: Mutex<Option<Proc>>,
}

impl SshTransport {
    pub fn new(route: Route) -> Self {
        Self { route, last_failure: Arc::default(), current: Mutex::new(None) }
    }

    pub fn last_failure(&self) -> Option<Failure> {
        self.last_failure.lock().ok().and_then(|f| f.clone())
    }

    /// Stop the current process now (used when this computer disconnects on purpose; the
    /// device's host and its runs are not affected).
    pub fn close(&self) {
        if let Some(p) = self.current.lock().ok().and_then(|mut c| c.take())
            && let Ok(mut g) = p.lock()
            && let Some(child) = g.as_mut()
        {
            let _ = child.kill();
        }
    }
}

impl Transport for SshTransport {
    fn open(&self) -> std::io::Result<(Box<dyn Read + Send>, Box<dyn Write + Send>)> {
        let mut cmd = self.route.host_command();
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = cmd.spawn().map_err(|e| {
            let msg = if e.kind() == std::io::ErrorKind::NotFound {
                "이 컴퓨터에서 ssh를 찾을 수 없어요".to_string()
            } else {
                format!("ssh를 시작하지 못했어요: {e}")
            };
            *self.last_failure.lock().unwrap_or_else(|p| p.into_inner()) = Some(Failure::new(Kind::Other, &msg));
            std::io::Error::other(msg)
        })?;
        let stdin = child.stdin.take().ok_or_else(|| std::io::Error::other("ssh 입력을 열지 못했어요"))?;
        let stdout = child.stdout.take().ok_or_else(|| std::io::Error::other("ssh 출력을 열지 못했어요"))?;
        let stderr = Arc::new(Mutex::new(String::new()));
        if let Some(mut err) = child.stderr.take() {
            let sink = stderr.clone();
            std::thread::spawn(move || {
                let mut buf = [0u8; 2048];
                while let Ok(n) = err.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    let mut s = sink.lock().unwrap_or_else(|p| p.into_inner());
                    if s.len() < 16 * 1024 {
                        s.push_str(&String::from_utf8_lossy(&buf[..n]));
                    }
                }
            });
        }
        let proc: Proc = Arc::new(Mutex::new(Some(child)));
        *self.current.lock().unwrap_or_else(|p| p.into_inner()) = Some(proc.clone());
        let reader = ProcReader { out: stdout, proc, stderr, last: self.last_failure.clone(), got_data: false };
        Ok((Box::new(reader), Box::new(stdin)))
    }

    fn retry_delay(&self, attempt: u32) -> Duration {
        match self.last_failure() {
            // Login, host key or a missing Runwick do not fix themselves: check rarely.
            Some(f) if !f.transient() => Duration::from_secs(60),
            _ => Duration::from_secs([2, 5, 10, 20, 30][attempt.min(4) as usize]),
        }
    }

    fn describe(&self) -> String {
        self.route.describe()
    }
}

/// Reads the relay's stdout; at the end, turns ssh's exit and stderr into the reason.
struct ProcReader {
    out: ChildStdout,
    proc: Proc,
    stderr: Arc<Mutex<String>>,
    last: Arc<Mutex<Option<Failure>>>,
    got_data: bool,
}

impl ProcReader {
    fn finish(&mut self) -> std::io::Error {
        let deadline = Instant::now() + Duration::from_secs(2);
        let code = loop {
            let mut g = self.proc.lock().unwrap_or_else(|p| p.into_inner());
            let Some(child) = g.as_mut() else { break None };
            match child.try_wait() {
                Ok(Some(st)) => break st.code(),
                Ok(None) if Instant::now() < deadline => {
                    drop(g);
                    std::thread::sleep(Duration::from_millis(20));
                }
                _ => {
                    let _ = child.kill();
                    break None;
                }
            }
        };
        // Let the stderr thread catch the last line.
        std::thread::sleep(Duration::from_millis(30));
        let err = self.stderr.lock().map(|s| s.clone()).unwrap_or_default();
        let failure = classify(&err, code).unwrap_or_else(|| Failure::new(Kind::Dropped, ""));
        let msg = failure.explain("기기");
        if let Ok(mut l) = self.last.lock() {
            *l = Some(failure);
        }
        std::io::Error::other(msg)
    }
}

impl Read for ProcReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self.out.read(buf) {
            Ok(0) => Err(self.finish()),
            Ok(n) => {
                if !self.got_data {
                    self.got_data = true;
                    if let Ok(mut l) = self.last.lock() {
                        *l = None;
                    }
                }
                Ok(n)
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => Err(e),
            Err(_) => Err(self.finish()),
        }
    }
}

impl Drop for ProcReader {
    fn drop(&mut self) {
        if let Ok(mut g) = self.proc.lock()
            && let Some(mut child) = g.take()
        {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kind(stderr: &str, code: i32) -> Option<Kind> {
        classify(stderr, Some(code)).map(|f| f.kind)
    }

    #[test]
    fn stderr_is_classified_by_cause() {
        assert_eq!(
            kind("ssh: Could not resolve hostname mini: nodename nor servname provided, or not known\n", 255),
            Some(Kind::NoAddress)
        );
        assert_eq!(kind("ssh: connect to host 10.0.0.9 port 22: Operation timed out\n", 255), Some(Kind::Timeout));
        assert_eq!(kind("Timeout, server mini not responding.\n", 255), Some(Kind::Timeout));
        assert_eq!(kind("ssh: connect to host mini port 22: Connection refused\n", 255), Some(Kind::SshOff));
        assert_eq!(kind("ssh: connect to host mini port 22: No route to host\n", 255), Some(Kind::NoRoute));
        assert_eq!(
            kind("me@mini: Permission denied (publickey,password,keyboard-interactive).\n", 255),
            Some(Kind::Denied)
        );
        assert_eq!(
            kind(
                "No ED25519 host key is known for mini and you have requested strict checking.\nHost key verification failed.\n",
                255
            ),
            Some(Kind::UnknownKey)
        );
        assert_eq!(
            kind(
                "@@@@@\n@    WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED!     @\n@@@@@\nHost key verification failed.\n",
                255
            ),
            Some(Kind::ChangedKey)
        );
        assert_eq!(
            kind("zsh:1: no such file or directory: /Users/me/.runwick/bin/runwick\n", 127),
            Some(Kind::Missing)
        );
        assert_eq!(kind("bash: runwick: command not found\n", 127), Some(Kind::Missing));
        assert_eq!(kind("Connection reset by 100.64.0.2 port 22\n", 255), Some(Kind::Dropped));
        assert_eq!(kind("", 0), None);
        assert_eq!(kind("", 255), Some(Kind::NoRoute));
        assert_eq!(kind("something odd\n", 1), Some(Kind::Other));
        assert_eq!(classify("", None).map(|f| f.kind), Some(Kind::Dropped));
    }

    #[test]
    fn causes_and_retry_policy() {
        let f = classify("Permission denied (publickey).", Some(255)).unwrap();
        assert_eq!(f.cause, Cause::Auth);
        assert!(!f.transient());
        assert!(f.explain("집 Mac mini").contains("네트워크 문제는 아니에요"));
        assert_eq!(f.detail, "Permission denied (publickey).");
        let f = classify("Timeout, server mini not responding.", Some(255)).unwrap();
        assert_eq!(f.cause, Cause::NoAnswer);
        assert!(f.transient());
        let t = SshTransport::new(Route::Ssh { target: "mini".into(), port: None, exe: "/x".into() });
        assert_eq!(t.retry_delay(0), Duration::from_secs(2));
        assert_eq!(t.retry_delay(9), Duration::from_secs(30));
        *t.last_failure.lock().unwrap() = Some(Failure::new(Kind::Denied, ""));
        assert_eq!(t.retry_delay(0), Duration::from_secs(60));
    }

    #[test]
    fn commands_never_prompt_and_quote_the_path() {
        let r = Route::Ssh { target: "me@mini".into(), port: Some(2222), exe: "/Users/me/it's/runwick".into() };
        let c = r.host_command();
        let args: Vec<String> = c.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        assert_eq!(c.get_program(), "ssh");
        for want in ["BatchMode=yes", "ServerAliveInterval=15", "ServerAliveCountMax=3", "-T", "--", "me@mini"] {
            assert!(args.iter().any(|a| a == want), "{want} in {args:?}");
        }
        let port = args.iter().position(|a| a == "-p").unwrap();
        assert_eq!(args[port + 1], "2222");
        assert!(args.iter().position(|a| a == "--").unwrap() < args.iter().position(|a| a == "me@mini").unwrap());
        assert_eq!(args.last().unwrap(), r"'/Users/me/it'\''s/runwick' host --stdio");
        assert!(!args.iter().any(|a| a.contains("accept-new")), "unknown keys fail outside the add flow");
    }

    #[test]
    fn a_real_ssh_failure_reaches_the_reader_classified() {
        if std::process::Command::new("ssh").arg("-V").output().is_err() {
            return; // no ssh on this machine
        }
        // `.invalid` never resolves: ssh fails before any host key or login step.
        let t = SshTransport::new(Route::Ssh { target: "runwick-test.invalid".into(), port: None, exe: "/x".into() });
        let (mut r, _w) = t.open().unwrap();
        let mut buf = [0u8; 64];
        let err = loop {
            match r.read(&mut buf) {
                Ok(0) => panic!("expected a classified ssh failure, got end of stream"),
                Ok(_) => continue,
                Err(e) => break e,
            }
        };
        let f = t.last_failure().expect("classified");
        assert_eq!(f.kind, Kind::NoAddress, "{f:?}");
        assert!(err.to_string().contains("주소를 찾을 수 없어요"));
        assert_eq!(t.retry_delay(0), Duration::from_secs(2));
    }

    #[test]
    fn simulated_route_needs_the_dev_switch() {
        let d = Device {
            id: uuid::Uuid::nil(),
            name: "sim".into(),
            target: "sim:/tmp/x/home".into(),
            port: None,
            os: None,
            arch: None,
            remote_host_id: None,
            remote_version: None,
            remote_exe: None,
            fingerprint: None,
            via: None,
            added_at: runwick_core::now(),
            last_seen_at: None,
        };
        let r = Route::for_device(&d, std::path::Path::new("/bin/runwick"));
        if simulated_allowed() {
            assert_eq!(r.unwrap(), Route::Simulated { home: "/tmp/x/home".into(), exe: "/bin/runwick".into() });
        } else {
            assert!(r.unwrap_err().contains(SIMULATED_ENV));
        }
        let ssh = Device { target: "mini".into(), remote_exe: None, ..d };
        assert!(Route::for_device(&ssh, std::path::Path::new("/bin/runwick")).is_err());
    }
}
