//! Checks for adding a device, each a blocking call for a background thread: where the SSH
//! address leads, whether it answers, the host key, logging in, and whether Runwick is ready
//! there (with an optional copy of this computer's executable when the platform matches).
//! Nothing here changes SSH settings; a first-seen host key is remembered by ssh itself
//! (`StrictHostKeyChecking=accept-new`) only after the person confirmed its fingerprint.

use super::proc;
use super::ssh::{self, Failure, Kind, SshOpts, sh_quote};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

/// Where an SSH address really goes, from `ssh -G` (reads the person's SSH config; no network).
#[derive(Debug, Clone, PartialEq)]
pub struct Resolved {
    pub user: String,
    pub host: String,
    pub port: u16,
    /// A ProxyJump/ProxyCommand is configured: a direct TCP check says nothing.
    pub proxied: bool,
    pub known_hosts: Vec<String>,
    /// `HostKeyAlias` when set (the name keys are stored under).
    pub key_alias: Option<String>,
}

impl Resolved {
    /// The name host keys are stored under (`host` or `[host]:port`).
    pub fn key_name(&self) -> String {
        let h = self.key_alias.clone().unwrap_or_else(|| self.host.clone());
        if self.port == 22 { h } else { format!("[{h}]:{}", self.port) }
    }
}

pub fn parse_ssh_g(out: &str) -> Option<Resolved> {
    let mut r = Resolved {
        user: String::new(),
        host: String::new(),
        port: 22,
        proxied: false,
        known_hosts: vec![],
        key_alias: None,
    };
    for line in out.lines() {
        let Some((k, v)) = line.split_once(' ') else {
            continue;
        };
        let v = v.trim();
        match k.to_ascii_lowercase().as_str() {
            "user" => r.user = v.to_string(),
            "hostname" => r.host = v.to_string(),
            "port" => r.port = v.parse().unwrap_or(22),
            "proxycommand" | "proxyjump" if v != "none" => r.proxied = true,
            "userknownhostsfile" => r.known_hosts = v.split_whitespace().map(str::to_string).collect(),
            "hostkeyalias" if v != "none" => r.key_alias = Some(v.to_string()),
            _ => {}
        }
    }
    (!r.host.is_empty()).then_some(r)
}

pub fn resolve(target: &str, port: Option<u16>) -> Result<Resolved, String> {
    let mut c = Command::new("ssh");
    c.arg("-G");
    if let Some(p) = port {
        c.args(["-p", &p.to_string()]);
    }
    c.args(["--", target]);
    let o = proc::run(c, None, Duration::from_secs(5))
        .map_err(|e| format!("이 컴퓨터에서 ssh를 실행하지 못했어요: {e}"))?;
    if !o.ok() {
        let line = o.stderr.lines().next().unwrap_or("").trim().to_string();
        return Err(format!("SSH 설정을 읽지 못했어요 · {line}"));
    }
    parse_ssh_g(&o.stdout).ok_or_else(|| "SSH 설정에서 주소를 찾지 못했어요".to_string())
}

/// TCP connect to the SSH port; the time it took.
pub fn reach(r: &Resolved) -> Result<Duration, Failure> {
    let addrs: Vec<_> = (r.host.as_str(), r.port)
        .to_socket_addrs()
        .map_err(|e| Failure::new(Kind::NoAddress, e.to_string()))?
        .collect();
    if addrs.is_empty() {
        return Err(Failure::new(Kind::NoAddress, ""));
    }
    let mut last = None;
    for a in addrs {
        let started = Instant::now();
        match TcpStream::connect_timeout(&a, Duration::from_secs(5)) {
            Ok(_) => return Ok(started.elapsed()),
            Err(e) => last = Some(e),
        }
    }
    let e = last.map(|e| (e.kind(), e.to_string()));
    Err(match e {
        Some((std::io::ErrorKind::ConnectionRefused, m)) => Failure::new(Kind::SshOff, m),
        Some((std::io::ErrorKind::TimedOut, m)) => Failure::new(Kind::Timeout, m),
        Some((_, m)) => Failure::new(Kind::NoRoute, m),
        None => Failure::new(Kind::NoRoute, ""),
    })
}

/// A host key: algorithm and SHA256 fingerprint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostKey {
    pub algo: String,
    pub fingerprint: String,
}

/// Lines from `ssh-keygen -l` (`256 SHA256:… host (ED25519)` or `host ED25519 SHA256:…`).
pub fn parse_fingerprints(out: &str) -> Vec<HostKey> {
    let mut keys = Vec::new();
    for line in out.lines().filter(|l| !l.trim_start().starts_with('#')) {
        let words: Vec<&str> = line.split_whitespace().collect();
        let Some(i) = words.iter().position(|w| w.starts_with("SHA256:")) else {
            continue;
        };
        let algo = words
            .last()
            .filter(|w| w.starts_with('(') && w.ends_with(')'))
            .map(|w| w.trim_matches(|c| c == '(' || c == ')').to_string())
            .or_else(|| i.checked_sub(1).map(|j| words[j].to_string()))
            .unwrap_or_default();
        let k = HostKey { algo: algo.to_uppercase(), fingerprint: words[i].to_string() };
        if !keys.contains(&k) {
            keys.push(k);
        }
    }
    // ED25519 first: it is what OpenSSH prefers and shows.
    keys.sort_by_key(|k| {
        if k.algo == "ED25519" {
            0
        } else if k.algo.starts_with("ECDSA") {
            1
        } else {
            2
        }
    });
    keys
}

/// Keys already remembered for this host in the person's known_hosts files.
pub fn known_keys(r: &Resolved) -> Vec<HostKey> {
    let mut keys = Vec::new();
    let files: Vec<String> = if r.known_hosts.is_empty() { vec![String::new()] } else { r.known_hosts.clone() };
    for f in files {
        if !f.is_empty() && !Path::new(&f).exists() {
            continue;
        }
        let mut c = Command::new("ssh-keygen");
        c.args(["-l", "-F", &r.key_name()]);
        if !f.is_empty() {
            c.args(["-f", &f]);
        }
        if let Ok(o) = proc::run(c, None, Duration::from_secs(5)) {
            keys.extend(parse_fingerprints(&o.stdout));
        }
    }
    keys
}

/// Ask the device for its host keys (first contact; nothing is saved).
pub fn scan(r: &Resolved) -> Result<Vec<HostKey>, Failure> {
    let mut c = Command::new("ssh-keyscan");
    c.args(["-T", "5", "-p", &r.port.to_string(), "--", &r.host]);
    let o = proc::run(c, None, Duration::from_secs(10)).map_err(|e| Failure::new(Kind::Other, e.to_string()))?;
    if o.stdout.trim().is_empty() {
        return Err(Failure::new(Kind::Timeout, o.stderr.lines().last().unwrap_or("").trim()));
    }
    let mut c = Command::new("ssh-keygen");
    c.args(["-l", "-f", "-"]);
    let fp = proc::run(c, Some(o.stdout.as_bytes()), Duration::from_secs(5))
        .map_err(|e| Failure::new(Kind::Other, e.to_string()))?;
    let keys = parse_fingerprints(&fp.stdout);
    if keys.is_empty() {
        return Err(Failure::new(Kind::Other, "호스트 키를 읽지 못했어요"));
    }
    Ok(keys)
}

/// What logging in told about the device.
#[derive(Debug, Clone, PartialEq)]
pub struct Platform {
    /// `std::env::consts::OS` style (`macos`, `linux`).
    pub os: String,
    /// `std::env::consts::ARCH` style (`aarch64`, `x86_64`).
    pub arch: String,
}

impl Platform {
    pub fn local() -> Self {
        Platform { os: std::env::consts::OS.into(), arch: std::env::consts::ARCH.into() }
    }

    pub fn from_uname(s: &str, m: &str) -> Self {
        let os = match s.trim() {
            "Darwin" => "macos".to_string(),
            "Linux" => "linux".to_string(),
            other => other.to_lowercase(),
        };
        let arch = match m.trim() {
            "arm64" | "aarch64" => "aarch64".to_string(),
            "x86_64" | "amd64" => "x86_64".to_string(),
            other => other.to_string(),
        };
        Platform { os, arch }
    }

    pub fn label(&self) -> String {
        let os = match self.os.as_str() {
            "macos" => "macOS",
            "linux" => "Linux",
            other => other,
        };
        let arch = match self.arch.as_str() {
            "aarch64" => "Apple 실리콘·ARM",
            "x86_64" => "x86_64",
            other => other,
        };
        format!("{os} · {arch}")
    }
}

fn run_ssh(
    target: &str,
    port: Option<u16>,
    o: &SshOpts,
    remote: &str,
    limit: Duration,
) -> Result<proc::Output, Failure> {
    let mut c = ssh::ssh(target, port, o);
    c.arg(remote);
    let out =
        proc::run(c, None, limit).map_err(|e| Failure::new(Kind::Other, format!("ssh를 실행하지 못했어요: {e}")))?;
    if out.timed_out {
        return Err(Failure::new(Kind::Timeout, "시간 초과"));
    }
    Ok(out)
}

/// Log in without prompts and read the platform.
pub fn login(target: &str, port: Option<u16>, accept_new: bool) -> Result<Platform, Failure> {
    let o = SshOpts { accept_new, ..Default::default() };
    let out = run_ssh(target, port, &o, "uname -s; uname -m", Duration::from_secs(20))?;
    let lines: Vec<&str> = out.stdout.lines().collect();
    match (out.ok(), lines.as_slice()) {
        (true, [s, m, ..]) => Ok(Platform::from_uname(s, m)),
        _ => {
            Err(ssh::classify(&out.stderr, out.code)
                .unwrap_or_else(|| Failure::new(Kind::Other, "응답을 읽지 못했어요")))
        }
    }
}

/// Runwick found on a device: absolute path and version.
#[derive(Debug, Clone, PartialEq)]
pub struct Installed {
    pub path: String,
    pub version: String,
}

/// Remote script: the first runnable Runwick, its path and `--version`.
const FIND: &str = r#"for p in "$HOME/.runwick/bin/runwick" /opt/homebrew/bin/runwick /usr/local/bin/runwick "$HOME/.cargo/bin/runwick" "$(command -v runwick 2>/dev/null)"; do if [ -n "$p" ] && [ -x "$p" ]; then echo "$p"; "$p" --version; exit 0; fi; done; echo RUNWICK_MISSING"#;

pub fn parse_find(out: &str) -> Option<Installed> {
    let mut lines = out.lines().map(str::trim).filter(|l| !l.is_empty());
    let path = lines.next()?;
    if path == "RUNWICK_MISSING" || !path.starts_with('/') {
        return None;
    }
    let version = lines.next().map(|v| v.rsplit(' ').next().unwrap_or(v).to_string()).unwrap_or_default();
    Some(Installed { path: path.to_string(), version })
}

pub fn find_runwick(target: &str, port: Option<u16>) -> Result<Option<Installed>, Failure> {
    let out =
        run_ssh(target, port, &SshOpts::default(), &format!("sh -c {}", sh_quote(FIND)), Duration::from_secs(20))?;
    if !out.ok() {
        return Err(ssh::classify(&out.stderr, out.code).unwrap_or_else(|| Failure::new(Kind::Other, "")));
    }
    Ok(parse_find(&out.stdout))
}

/// Copy this computer's executable to `~/.runwick/bin/runwick` on the device (same platform
/// only; the caller checks). Replaces it atomically and returns what runs there now.
pub fn install(target: &str, port: Option<u16>, local_exe: &Path) -> Result<Installed, Failure> {
    let o = SshOpts::default();
    let prep = run_ssh(target, port, &o, "mkdir -p \"$HOME/.runwick/bin\"", Duration::from_secs(20))?;
    if !prep.ok() {
        return Err(ssh::classify(&prep.stderr, prep.code).unwrap_or_else(|| Failure::new(Kind::Other, "")));
    }
    let mut c = Command::new("scp");
    c.args(["-q", "-B", "-o", "BatchMode=yes", "-o", "ConnectTimeout=10"]);
    if let Some(p) = port {
        c.args(["-P", &p.to_string()]);
    }
    c.arg("--").arg(local_exe).arg(format!("{}:.runwick/bin/runwick.partial", scp_host(target)));
    let out = proc::run(c, None, Duration::from_secs(300))
        .map_err(|e| Failure::new(Kind::Other, format!("scp를 실행하지 못했어요: {e}")))?;
    if !out.ok() {
        return Err(
            ssh::classify(&out.stderr, out.code).unwrap_or_else(|| Failure::new(Kind::Other, "복사하지 못했어요"))
        );
    }
    let finish = "cd \"$HOME/.runwick/bin\" && chmod 755 runwick.partial && mv -f runwick.partial runwick && echo \"$HOME/.runwick/bin/runwick\" && ./runwick --version";
    let out = run_ssh(target, port, &o, finish, Duration::from_secs(30))?;
    match (out.ok(), parse_find(&out.stdout)) {
        (true, Some(i)) => Ok(i),
        _ => Err(ssh::classify(&out.stderr, out.code)
            .unwrap_or_else(|| Failure::new(Kind::Missing, "복사한 Runwick이 실행되지 않았어요"))),
    }
}

/// `user@host` for scp, with IPv6 literals in brackets.
fn scp_host(target: &str) -> String {
    match target.rsplit_once('@') {
        Some((u, h)) if h.contains(':') => format!("{u}@[{h}]"),
        None if target.contains(':') => format!("[{target}]"),
        _ => target.to_string(),
    }
}

/// `git@github.com:acme/x.git`, `https://github.com/acme/x` → `github.com/acme/x` (to notice
/// the same repository on two computers; never used to merge anything by itself).
pub fn repo_key(url: &str) -> Option<String> {
    let u = url.trim().trim_end_matches('/').trim_end_matches(".git");
    let rest = if let Some(r) = u.split_once("://").map(|(_, r)| r) {
        let r = r.rsplit_once('@').map(|(_, h)| h).unwrap_or(r);
        r.to_string()
    } else if let Some((left, path)) = u.split_once(':') {
        let host = left.rsplit_once('@').map(|(_, h)| h).unwrap_or(left);
        format!("{host}/{path}")
    } else {
        return None;
    };
    let rest = rest.to_lowercase();
    // Drop a port in the host part.
    let (host, path) = rest.split_once('/')?;
    let host = host.split(':').next().unwrap_or(host);
    (!path.is_empty()).then(|| format!("{host}/{path}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssh_g_output_is_read() {
        let out = "user me\nhostname 100.64.0.2\nport 2222\nproxycommand none\nuserknownhostsfile /Users/me/.ssh/known_hosts /Users/me/.ssh/known_hosts2\n";
        let r = parse_ssh_g(out).unwrap();
        assert_eq!((r.user.as_str(), r.host.as_str(), r.port, r.proxied), ("me", "100.64.0.2", 2222, false));
        assert_eq!(r.known_hosts.len(), 2);
        assert_eq!(r.key_name(), "[100.64.0.2]:2222");
        let r = parse_ssh_g("hostname mini\nport 22\nproxyjump bastion\nhostkeyalias mini-key\n").unwrap();
        assert!(r.proxied);
        assert_eq!(r.key_name(), "mini-key");
        assert!(parse_ssh_g("user x\n").is_none());
    }

    #[test]
    fn fingerprints_are_read_from_both_formats() {
        let scanned =
            "3072 SHA256:rsaRSA mini (RSA)\n256 SHA256:k3J9vX2mQe8Lw2 mini (ED25519)\n256 SHA256:ecEC mini (ECDSA)\n";
        let k = parse_fingerprints(scanned);
        assert_eq!(k[0], HostKey { algo: "ED25519".into(), fingerprint: "SHA256:k3J9vX2mQe8Lw2".into() });
        assert_eq!(k.len(), 3);
        let known = "# Host mini found: line 4\nmini ED25519 SHA256:k3J9vX2mQe8Lw2\n";
        assert_eq!(parse_fingerprints(known), vec![k[0].clone()]);
    }

    #[test]
    fn platform_and_runwick_lookup() {
        assert_eq!(Platform::from_uname("Darwin", "arm64"), Platform { os: "macos".into(), arch: "aarch64".into() });
        assert_eq!(Platform::from_uname("Linux\n", "x86_64"), Platform { os: "linux".into(), arch: "x86_64".into() });
        assert_eq!(
            parse_find("/Users/me/.runwick/bin/runwick\nrunwick 0.1.0\n"),
            Some(Installed { path: "/Users/me/.runwick/bin/runwick".into(), version: "0.1.0".into() })
        );
        assert_eq!(parse_find("RUNWICK_MISSING\n"), None);
        assert_eq!(parse_find(""), None);
        assert!(FIND.contains("\"$HOME/.runwick/bin/runwick\"") && !FIND.contains('\''), "safe inside sh -c '…'");
    }

    #[test]
    fn scp_destinations_and_repo_keys() {
        assert_eq!(scp_host("me@mini"), "me@mini");
        assert_eq!(scp_host("me@fd7a::2"), "me@[fd7a::2]");
        assert_eq!(scp_host("fd7a::2"), "[fd7a::2]");
        let a = repo_key("git@github.com:Acme/payments-api.git");
        assert_eq!(a.as_deref(), Some("github.com/acme/payments-api"));
        assert_eq!(repo_key("https://github.com/acme/payments-api"), a);
        assert_eq!(repo_key("ssh://git@github.com:22/acme/payments-api.git/"), a);
        assert_eq!(repo_key("/srv/git/x"), None);
    }
}
