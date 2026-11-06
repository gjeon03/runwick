//! Running `git` and `gh` for sync: the person's own programs, credentials and configuration,
//! never interactive, always with a time limit, and errors sorted into what the person can do.

use runwick_core::api::sync::FailKind;
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Local git commands (status, commit, cat-file).
pub const LOCAL: Duration = Duration::from_secs(60);
/// Network git commands (ls-remote, fetch, push).
pub const NETWORK: Duration = Duration::from_secs(180);
/// A first clone can be large.
pub const CLONE: Duration = Duration::from_secs(600);

/// Variables that would point git at another repository than the one we name.
const DROPPED: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_NAMESPACE",
    "GIT_PREFIX",
];

/// How git and gh are started: the login environment (PATH, SSH agent, HOME) without
/// anything that could prompt.
#[derive(Clone, Debug)]
pub struct GitEnv {
    pub vars: BTreeMap<String, String>,
}

impl GitEnv {
    /// The person's login environment (the same one CLIs get).
    pub fn login() -> Self {
        let env = crate::sessions::env::login_env();
        Self::from_vars(env.vars.clone())
    }

    /// This process's environment (tests).
    pub fn process() -> Self {
        Self::from_vars(std::env::vars().collect())
    }

    fn from_vars(mut vars: BTreeMap<String, String>) -> Self {
        for k in DROPPED {
            vars.remove(*k);
        }
        vars.insert("GIT_TERMINAL_PROMPT".into(), "0".into());
        vars.insert("GCM_INTERACTIVE".into(), "never".into());
        vars.insert("GH_PROMPT_DISABLED".into(), "1".into());
        vars.insert("GH_NO_UPDATE_NOTIFIER".into(), "1".into());
        vars.insert("LC_ALL".into(), "C".into());
        Self { vars }
    }

    fn path(&self) -> &str {
        self.vars.get("PATH").map(String::as_str).unwrap_or("/usr/bin:/bin:/usr/sbin:/sbin")
    }

    pub fn find(&self, name: &str) -> Option<PathBuf> {
        crate::tools::resolve(name, self.path())
    }
}

/// A failed command: what kind of failure and git's message (never shown raw alone).
#[derive(Debug, Clone)]
pub struct GitError {
    pub kind: FailKind,
    pub message: String,
}

impl GitError {
    pub fn other(message: impl Into<String>) -> Self {
        Self { kind: FailKind::Other, message: message.into() }
    }
}

impl std::fmt::Display for GitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

pub struct Output {
    pub ok: bool,
    pub stdout: Vec<u8>,
    pub stderr: String,
}

impl Output {
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).trim_end().to_string()
    }
}

/// Run a program with a time limit; stdin gets `input` (or nothing).
pub fn run(
    env: &GitEnv,
    program: &Path,
    dir: Option<&Path>,
    args: &[&str],
    input: Option<Vec<u8>>,
    limit: Duration,
) -> Result<Output, GitError> {
    let mut cmd = Command::new(program);
    // Its own process group, so a cut-off command takes its helpers (ssh, credential
    // helpers) with it.
    std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
    cmd.args(args).env_clear().envs(&env.vars).stdout(Stdio::piped()).stderr(Stdio::piped());
    cmd.stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() });
    if let Some(d) = dir {
        cmd.current_dir(d);
    }
    let name = program.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let mut child = cmd
        .spawn()
        .map_err(|e| GitError::other(format!("{} 실행하지 못했어요: {e}", runwick_core::josa::eul_reul(&name))))?;
    let feeder = match (input, child.stdin.take()) {
        (Some(bytes), Some(mut stdin)) => Some(std::thread::spawn(move || {
            let _ = stdin.write_all(&bytes);
        })),
        _ => None,
    };
    let mut out_pipe = child.stdout.take();
    let mut err_pipe = child.stderr.take();
    let out_t = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(p) = out_pipe.as_mut() {
            let _ = p.read_to_end(&mut buf);
        }
        buf
    });
    let err_t = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(p) = err_pipe.as_mut() {
            let _ = p.read_to_end(&mut buf);
        }
        buf
    });
    let deadline = Instant::now() + limit;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) if Instant::now() >= deadline => {
                // SAFETY: signalling the process group created for this child only.
                unsafe {
                    libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL);
                }
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => {
                return Err(GitError::other(format!("{name} 실행을 기다리지 못했어요: {e}")));
            }
        }
    };
    if let Some(f) = feeder {
        let _ = f.join();
    }
    let stdout = out_t.join().unwrap_or_default();
    let stderr = String::from_utf8_lossy(&err_t.join().unwrap_or_default()).trim().to_string();
    match status {
        None => Err(GitError {
            kind: FailKind::Network,
            message: format!("{} {}초 안에 끝나지 않아 멈췄어요", runwick_core::josa::i_ga(&name), limit.as_secs()),
        }),
        Some(s) => Ok(Output { ok: s.success(), stdout, stderr }),
    }
}

/// `git` in a working copy (or anywhere with `dir = None`).
pub struct Git {
    pub env: GitEnv,
    pub program: PathBuf,
}

/// Options that keep our commands safe and predictable whatever the person's config says.
const SAFE: &[&str] = &[
    "-c",
    "protocol.ext.allow=never",
    "-c",
    "core.quotepath=false",
    "-c",
    "core.autocrlf=false",
    "-c",
    "core.precomposeunicode=true",
    // A link in the data repository is checked out as a plain file, never followed.
    "-c",
    "core.symlinks=false",
    "-c",
    "commit.gpgsign=false",
    "-c",
    "gc.auto=0",
];

/// macOS's `/usr/bin/git` only forwards to the selected developer folder; it fails until the
/// Xcode license is accepted when full Xcode is selected. The Command Line Tools' own git works.
const XCODE_SHIM: &str = "/usr/bin/git";
const CLT_GIT: &str = "/Library/Developer/CommandLineTools/usr/bin/git";

impl Git {
    pub fn new(env: GitEnv) -> Result<Self, GitError> {
        let mut program = env
            .find("git")
            .ok_or_else(|| GitError::other("git을 찾을 수 없어요. Xcode 명령줄 도구나 git을 설치해 주세요"))?;
        if program == Path::new(XCODE_SHIM) {
            let probe = run(&env, &program, None, &["--version"], None, Duration::from_secs(15))?;
            if !probe.ok {
                if Path::new(CLT_GIT).is_file() {
                    program = PathBuf::from(CLT_GIT);
                } else {
                    return Err(GitError::other(if probe.stderr.contains("license") {
                        "Xcode 사용권에 동의해야 git을 쓸 수 있어요. 터미널에서 `sudo xcodebuild -license`를 실행해 주세요".to_string()
                    } else {
                        format!("git을 실행하지 못했어요: {}", last_lines(&probe.stderr))
                    }));
                }
            }
        }
        Ok(Self { env, program })
    }

    pub fn raw(
        &self,
        dir: Option<&Path>,
        args: &[&str],
        input: Option<Vec<u8>>,
        limit: Duration,
    ) -> Result<Output, GitError> {
        let mut all: Vec<&str> = SAFE.to_vec();
        all.extend_from_slice(args);
        run(&self.env, &self.program, dir, &all, input, limit)
    }

    /// Run and require success; the error is classified from git's message.
    pub fn ok(&self, dir: Option<&Path>, args: &[&str], limit: Duration) -> Result<Output, GitError> {
        let out = self.raw(dir, args, None, limit)?;
        if out.ok { Ok(out) } else { Err(classify(&out.stderr)) }
    }

    pub fn text(&self, dir: &Path, args: &[&str]) -> Result<String, GitError> {
        self.ok(Some(dir), args, LOCAL).map(|o| o.text())
    }

    /// Commit id of a revision, or None when it does not exist.
    pub fn rev(&self, dir: &Path, rev: &str) -> Result<Option<String>, GitError> {
        let spec = format!("{rev}^{{commit}}");
        let out = self.raw(Some(dir), &["rev-parse", "--verify", "--quiet", &spec], None, LOCAL)?;
        Ok(out.ok.then(|| out.text()).filter(|s| !s.is_empty()))
    }

    /// Whether `a` is an ancestor of (or equal to) `b`.
    pub fn is_ancestor(&self, dir: &Path, a: &str, b: &str) -> Result<bool, GitError> {
        let out = self.raw(Some(dir), &["merge-base", "--is-ancestor", a, b], None, LOCAL)?;
        Ok(out.ok)
    }

    /// Files (NUL separated paths) changed between two commits, renames as delete + add.
    pub fn changed(&self, dir: &Path, from: &str, to: &str) -> Result<Vec<String>, GitError> {
        let out = self.ok(Some(dir), &["diff", "--name-only", "--no-renames", "-z", from, to, "--"], LOCAL)?;
        Ok(split_z(&out.stdout))
    }

    /// Every file of a commit.
    pub fn files(&self, dir: &Path, rev: &str) -> Result<Vec<String>, GitError> {
        let out = self.ok(Some(dir), &["ls-tree", "-r", "--name-only", "-z", rev], LOCAL)?;
        Ok(split_z(&out.stdout))
    }

    /// Entries of a commit Runwick never reads or writes, with why: anything that is not a plain
    /// file (symbolic links, submodules) and paths that differ only in letter case or Unicode
    /// form (on macOS they are the same file).
    pub fn tree_problems(&self, dir: &Path, rev: &str) -> Result<Vec<(String, &'static str)>, GitError> {
        let out = self.ok(Some(dir), &["ls-tree", "-r", "-z", rev], LOCAL)?;
        let mut problems = Vec::new();
        let mut seen: BTreeMap<String, String> = BTreeMap::new();
        for e in split_z(&out.stdout) {
            let Some((meta, path)) = e.split_once('\t') else {
                continue;
            };
            let mode = meta.split(' ').next().unwrap_or("");
            if !matches!(mode, "100644" | "100755") {
                problems.push((path.to_string(), "일반 파일이 아닌 항목 (링크 등)"));
                continue;
            }
            let folded = runwick_core::nfc::hangul(path).to_lowercase();
            if let Some(other) = seen.insert(folded, path.to_string()) {
                problems.push((path.to_string(), "대소문자나 글자 조합만 다른 같은 이름"));
                problems.push((other, "대소문자나 글자 조합만 다른 같은 이름"));
            }
        }
        Ok(problems)
    }

    /// Which computer last wrote `path` at `rev`: the host id from the commit trailer and the
    /// author (device) name.
    pub fn author_of(&self, dir: &Path, rev: &str, path: &str) -> (Option<uuid::Uuid>, String) {
        let fmt = format!("--format=%an%x1f%(trailers:key={HOST_TRAILER},valueonly,separator=%x2c)");
        let out = self.raw(Some(dir), &["log", "-1", &fmt, rev, "--", path], None, LOCAL);
        let text = out.ok().filter(|o| o.ok).map(|o| o.text()).unwrap_or_default();
        let (name, host) = text.split_once('\u{1f}').unwrap_or((text.as_str(), ""));
        let name = if name.trim().is_empty() { "다른 컴퓨터".to_string() } else { name.trim().to_string() };
        (host.trim().parse().ok(), name)
    }

    /// Paths of `paths` at `rev` that are larger than [`MAX_BLOB`].
    pub fn too_big(&self, dir: &Path, rev: &str, paths: &[String]) -> Result<Vec<String>, GitError> {
        let input: Vec<u8> =
            paths.iter().filter(|p| !p.contains('\n')).flat_map(|p| format!("{rev}:{p}\n").into_bytes()).collect();
        let res = self.raw(Some(dir), &["cat-file", "--batch-check"], Some(input), LOCAL)?;
        if !res.ok {
            return Err(classify(&res.stderr));
        }
        let text = String::from_utf8_lossy(&res.stdout).to_string();
        Ok(paths
            .iter()
            .filter(|p| !p.contains('\n'))
            .zip(text.lines())
            .filter(|(_, line)| {
                line.rsplit(' ').next().and_then(|n| n.parse::<u64>().ok()).is_some_and(|n| n > MAX_BLOB)
            })
            .map(|(p, _)| p.clone())
            .collect())
    }

    /// Contents of `paths` at `rev` (None for a path that does not exist there).
    pub fn read_at(
        &self,
        dir: &Path,
        rev: &str,
        paths: &[String],
    ) -> Result<BTreeMap<String, Option<Vec<u8>>>, GitError> {
        let mut out = BTreeMap::new();
        if paths.is_empty() {
            return Ok(out);
        }
        // Sizes first: nothing larger than MAX_BLOB is read into memory.
        let too_big = self.too_big(dir, rev, paths)?;
        if !too_big.is_empty() {
            return Err(GitError::other(format!(
                "저장소에 너무 큰 파일이 있어 읽지 않았어요 ({}MB 넘음): {}",
                MAX_BLOB >> 20,
                too_big.iter().take(3).cloned().collect::<Vec<_>>().join(", ")
            )));
        }
        let mut input = Vec::new();
        for p in paths {
            if p.contains('\n') {
                out.insert(p.clone(), None);
                continue;
            }
            input.extend_from_slice(format!("{rev}:{p}\n").as_bytes());
        }
        let res = self.raw(Some(dir), &["cat-file", "--batch"], Some(input), LOCAL)?;
        if !res.ok {
            return Err(classify(&res.stderr));
        }
        let mut buf = res.stdout.as_slice();
        for p in paths.iter().filter(|p| !p.contains('\n')) {
            let Some(nl) = buf.iter().position(|b| *b == b'\n') else {
                return Err(GitError::other("저장소 파일을 읽다가 응답이 끊겼어요"));
            };
            let header = String::from_utf8_lossy(&buf[..nl]).to_string();
            buf = &buf[nl + 1..];
            if header.ends_with(" missing") || header.ends_with(" ambiguous") {
                out.insert(p.clone(), None);
                continue;
            }
            let mut parts = header.split(' ');
            let (_, kind, size) = (parts.next(), parts.next(), parts.next());
            let size: usize =
                size.and_then(|s| s.parse().ok()).ok_or_else(|| GitError::other("저장소 파일 응답을 읽지 못했어요"))?;
            if buf.len() < size + 1 {
                return Err(GitError::other("저장소 파일을 읽다가 응답이 끊겼어요"));
            }
            let body = buf[..size].to_vec();
            buf = &buf[size + 1..];
            out.insert(p.clone(), (kind == Some("blob")).then_some(body));
        }
        Ok(out)
    }
}

fn split_z(bytes: &[u8]) -> Vec<String> {
    bytes.split(|b| *b == 0).filter(|s| !s.is_empty()).map(|s| String::from_utf8_lossy(s).to_string()).collect()
}

/// Sort a git/gh error message into what the person can act on.
pub fn classify(stderr: &str) -> GitError {
    let s = stderr.to_lowercase();
    let has = |needles: &[&str]| needles.iter().any(|n| s.contains(n));
    let tail = last_lines(stderr);
    if has(&[
        "could not resolve host",
        "network is unreachable",
        "connection timed out",
        "operation timed out",
        "connection refused",
        "failed to connect",
        "couldn't connect",
        "temporary failure in name resolution",
        "no route to host",
        "connection reset",
    ]) {
        return GitError {
            kind: FailKind::Network,
            message: format!("저장소 서버에 연결하지 못했어요. 네트워크를 확인해 주세요 ({tail})"),
        };
    }
    if has(&[
        "repository not found",
        "does not appear to be a git repository",
        "not found",
        "no such file or directory",
        "does not exist",
    ]) {
        return GitError {
            kind: FailKind::NotFound,
            message: format!("저장소를 찾을 수 없거나 이 계정으로 볼 수 없어요. 주소와 권한을 확인해 주세요 ({tail})"),
        };
    }
    if has(&[
        "authentication failed",
        "permission denied",
        "could not read username",
        "could not read password",
        "terminal prompts disabled",
        "invalid username or password",
        "403",
        "401",
        "host key verification failed",
        "publickey",
        "access denied",
    ]) {
        return GitError {
            kind: FailKind::Auth,
            message: format!(
                "GitHub 인증이 필요해요. 터미널에서 `gh auth login`을 하거나 SSH 키를 등록한 뒤 다시 시도해 주세요 ({tail})"
            ),
        };
    }
    GitError::other(if tail.is_empty() {
        "git 명령이 실패했어요".to_string()
    } else {
        format!("git 명령이 실패했어요: {tail}")
    })
}

fn last_lines(s: &str) -> String {
    let lines: Vec<&str> = s.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with("hint:")).collect();
    let n = lines.len();
    lines[n.saturating_sub(2)..].join(" · ").chars().take(300).collect()
}

/// Largest repository file Runwick reads (a session's messages can be tens of MB).
pub const MAX_BLOB: u64 = 256 << 20;

/// Commit trailer naming the computer that made a sync commit.
pub const HOST_TRAILER: &str = "Runwick-Host";

/// A URL we are willing to hand to git: no option-looking text, no shell transport, and no
/// password or token inside it (it would be saved and shown; git's own credentials are used).
pub fn check_url(url: &str) -> Result<String, GitError> {
    let u = url.trim();
    if u.is_empty() {
        return Err(GitError { kind: FailKind::NotFound, message: "저장소 주소를 입력해 주세요".into() });
    }
    if u.starts_with('-') || u.chars().any(|c| c.is_whitespace() || c.is_control()) || u.contains("::") {
        return Err(GitError { kind: FailKind::NotFound, message: "저장소 주소 형식이 아니에요".into() });
    }
    if let Some((scheme, rest)) = u.split_once("://")
        && let Some((userinfo, _)) = rest.split('/').next().unwrap_or("").rsplit_once('@')
    {
        // A password, or an http(s) user part that is really a token, would be saved and shown.
        let http = scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https");
        let tokenish = ["ghp_", "gho_", "ghu_", "ghs_", "ghr_", "github_pat_", "glpat-", "x-access-token"]
            .iter()
            .any(|p| userinfo.to_ascii_lowercase().starts_with(p))
            || userinfo.len() > 39;
        if userinfo.contains(':') || (http && tokenish) {
            return Err(GitError {
                kind: FailKind::Auth,
                message: "주소에 비밀번호나 토큰을 넣지 마세요. 저장되고 화면에 보일 수 있어요. 주소에서 빼고 `gh auth login`이나 git 자격 증명 도우미로 로그인해 주세요".into(),
            });
        }
    }
    Ok(u.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_are_sorted_by_what_to_do() {
        assert_eq!(
            classify("fatal: unable to access 'https://x/': Could not resolve host: github.com").kind,
            FailKind::Network
        );
        assert_eq!(
            classify("ERROR: Repository not found.\nfatal: Could not read from remote repository.").kind,
            FailKind::NotFound
        );
        assert_eq!(classify("git@github.com: Permission denied (publickey).").kind, FailKind::Auth);
        assert_eq!(
            classify("fatal: could not read Username for 'https://github.com': terminal prompts disabled").kind,
            FailKind::Auth
        );
        assert_eq!(classify("fatal: bad object").kind, FailKind::Other);
        assert!(check_url("--upload-pack=evil").is_err());
        assert!(check_url("ext::sh -c touch").is_err());
        assert!(check_url("git@github.com:me/data.git").is_ok());
        assert!(check_url("https://me@github.com/me/data.git").is_ok());
        let e = check_url("https://me:ghp_secret@github.com/me/data.git").unwrap_err();
        assert!(!e.message.contains("ghp_secret"));
        assert!(check_url("https://x-access-token:abc@github.com/me/data.git").is_err());
        // A token-shaped user part (built at run time so no token-looking literal is in the source).
        let token_url = format!("https://ghp_{}@github.com/me/data.git", "a".repeat(36));
        assert!(check_url(&token_url).is_err());
    }
}
