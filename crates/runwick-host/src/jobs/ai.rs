//! One non-interactive AI call through the person's own CLI, in an empty temporary folder,
//! with no tools: `claude -p --output-format json` when Claude Code is installed, otherwise
//! `codex exec` in a read-only sandbox. The child runs in its own process group so a stop
//! kills everything it started.
//!
//! The CLI gets the same environment a newly started terminal run would: the login
//! environment with Runwick's global environment applied (and the CLI is looked up on that
//! PATH), without terminal markers or Runwick's own session variables.

use crate::host::Host;
use crate::sessions::env::{EnvOverlay, LoginEnv, is_stripped, login_env};
use runwick_core::josa;
use std::collections::BTreeMap;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};
use tokio::io::AsyncReadExt;

/// Longest an AI call may take before it is stopped.
pub const TIMEOUT: Duration = Duration::from_secs(10 * 60);

#[derive(Debug, Clone, PartialEq)]
pub enum Provider {
    Claude(PathBuf),
    Codex(PathBuf),
}

impl Provider {
    pub fn label(&self) -> &'static str {
        match self {
            Provider::Claude(_) => "Claude Code",
            Provider::Codex(_) => "Codex",
        }
    }
}

/// The CLI to use on a `PATH`: Claude Code, else Codex.
pub fn provider_on(path: &str) -> Option<Provider> {
    crate::tools::resolve("claude", path)
        .map(Provider::Claude)
        .or_else(|| crate::tools::resolve("codex", path).map(Provider::Codex))
}

pub type Env = BTreeMap<String, String>;

/// Environment of a job's CLI: the login environment without terminal markers and Runwick's
/// session variables, then the global environment's unset and set applied.
pub fn job_env(base: &LoginEnv, overlay: &EnvOverlay) -> Env {
    let mut vars: Env = base
        .vars
        .iter()
        .filter(|(k, _)| !is_stripped(k) && !k.starts_with("RUNWICK_"))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    for k in &overlay.unset {
        vars.remove(k);
    }
    for (k, v) in &overlay.set {
        vars.insert(k.clone(), v.clone());
    }
    vars
}

/// What a job would run now. Blocking (reads the login environment and the global one).
pub struct Setup {
    pub provider: Option<Provider>,
    pub env: Env,
}

pub fn setup(host: &Host) -> Setup {
    let env = job_env(&login_env(), &host.sessions.env_overlay(host));
    let provider = provider_on(env.get("PATH").map(String::as_str).unwrap_or("/usr/bin:/bin"));
    Setup { provider, env }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Output {
    /// The model's final answer text.
    pub text: String,
    pub model: Option<String>,
    pub duration_ms: u64,
}

/// Run `prompt`. `on_spawn` receives the child's pid (its process group) so it can be stopped.
pub async fn run(
    provider: &Provider,
    env: &Env,
    prompt: &str,
    workdir: &Path,
    on_spawn: impl FnOnce(u32),
) -> Result<Output, String> {
    std::fs::create_dir_all(workdir).map_err(|e| format!("작업 폴더를 만들지 못했어요: {e}"))?;
    let last_message = workdir.join("answer.txt");
    let mut cmd = match provider {
        Provider::Claude(path) => {
            let mut c = tokio::process::Command::new(path);
            c.args(["-p", "--output-format", "json", "--tools", "", "--strict-mcp-config", "--no-session-persistence"]);
            // The prompt goes through stdin: argv is visible to every local user in `ps`.
            c
        }
        Provider::Codex(path) => {
            let mut c = tokio::process::Command::new(path);
            c.args([
                "exec",
                "--skip-git-repo-check",
                "--sandbox",
                "read-only",
                "--ephemeral",
                // Summaries need no tools: don't start the person's MCP servers for them.
                "-c",
                "mcp_servers={}",
                "--color",
                "never",
                "-o",
            ]);
            c.arg(&last_message);
            c.arg("-"); // read the prompt from stdin
            c
        }
    };
    cmd.current_dir(workdir)
        .env_clear()
        .envs(env)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    cmd.as_std_mut().process_group(0);
    let started = Instant::now();
    let mut child = cmd.spawn().map_err(|e| format!("{} 시작하지 못했어요: {e}", josa::eul_reul(provider.label())))?;
    if let Some(pid) = child.id() {
        on_spawn(pid);
    }
    if let Some(mut stdin) = child.stdin.take() {
        use tokio::io::AsyncWriteExt;
        let prompt = prompt.to_owned();
        // Write in the background so a CLI that reads slowly cannot dead-lock with our output reads.
        tokio::spawn(async move {
            let _ = stdin.write_all(prompt.as_bytes()).await;
            let _ = stdin.shutdown().await;
        });
    }
    let mut stdout = child.stdout.take().ok_or("출력을 읽을 수 없어요")?;
    let mut stderr = child.stderr.take().ok_or("출력을 읽을 수 없어요")?;
    let read = async {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let (a, b) = tokio::join!(stdout.read_to_end(&mut out), stderr.read_to_end(&mut err));
        a.and(b).map(|_| (out, err))
    };
    let (out, err) = match tokio::time::timeout(TIMEOUT, read).await {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => return Err(format!("출력을 읽지 못했어요: {e}")),
        Err(_) => {
            kill_group(child.id());
            return Err("10분 안에 끝나지 않아 멈췄어요".into());
        }
    };
    let status = child.wait().await.map_err(|e| e.to_string())?;
    let duration_ms = started.elapsed().as_millis() as u64;
    let stdout = String::from_utf8_lossy(&out);
    if !status.success() {
        let detail = last_line(&String::from_utf8_lossy(&err)).or_else(|| last_line(&stdout));
        return Err(match (status.code(), detail) {
            (None, _) => "중간에 멈췄어요".to_string(),
            (Some(code), Some(d)) => {
                format!("{} 오류로 끝났어요 ({code}): {d}", josa::i_ga(provider.label()))
            }
            (Some(code), None) => format!("{} 오류로 끝났어요 ({code})", josa::i_ga(provider.label())),
        });
    }
    match provider {
        Provider::Claude(_) => parse_claude(&stdout).map(|(text, model)| Output { text, model, duration_ms }),
        Provider::Codex(_) => {
            let text = std::fs::read_to_string(&last_message).unwrap_or_else(|_| stdout.to_string());
            Ok(Output { text, model: None, duration_ms })
        }
    }
}

fn last_line(s: &str) -> Option<String> {
    s.lines().rev().map(str::trim).find(|l| !l.is_empty()).map(|l| l.chars().take(300).collect())
}

/// `claude -p --output-format json` prints one result object.
pub fn parse_claude(stdout: &str) -> Result<(String, Option<String>), String> {
    let v: serde_json::Value = stdout
        .lines()
        .rev()
        .find_map(|l| serde_json::from_str::<serde_json::Value>(l.trim()).ok().filter(|v| v.is_object()))
        .or_else(|| serde_json::from_str(stdout.trim()).ok())
        .ok_or_else(|| "Claude Code 응답을 읽지 못했어요".to_string())?;
    let text = v.get("result").and_then(|r| r.as_str()).unwrap_or_default().to_string();
    if v.get("is_error").and_then(|e| e.as_bool()).unwrap_or(false) {
        let msg = if text.is_empty() {
            "Claude Code가 오류를 알렸어요".to_string()
        } else {
            text.chars().take(300).collect()
        };
        return Err(msg);
    }
    let model = v
        .get("modelUsage")
        .and_then(|m| m.as_object())
        .and_then(|m| m.keys().next().cloned())
        .or_else(|| v.get("model").and_then(|m| m.as_str()).map(str::to_string));
    Ok((text, model))
}

pub fn kill_group(pid: Option<u32>) {
    if let Some(pid) = pid {
        unsafe {
            libc::kill(-(pid as libc::pid_t), libc::SIGKILL);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_claude_json_result() {
        let out = r#"{"type":"result","subtype":"success","is_error":false,"duration_ms":1200,"result":"{\"proposals\":[]}","modelUsage":{"claude-sonnet-4-5":{"inputTokens":10}},"total_cost_usd":0.01}"#;
        let (text, model) = parse_claude(out).unwrap();
        assert_eq!(text, "{\"proposals\":[]}");
        assert_eq!(model.as_deref(), Some("claude-sonnet-4-5"));
        let err = r#"{"type":"result","is_error":true,"result":"Not logged in"}"#;
        assert_eq!(parse_claude(err).unwrap_err(), "Not logged in");
        assert!(parse_claude("garbage").is_err());
    }

    fn login(vars: &[(&str, &str)]) -> LoginEnv {
        LoginEnv {
            vars: vars.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            source: crate::sessions::env::Source::LoginShell,
        }
    }

    #[test]
    fn global_environment_is_applied_over_the_login_one() {
        let base = login(&[
            ("PATH", "/usr/bin"),
            ("HOME", "/Users/me"),
            ("OLD_TOKEN", "x"),
            ("CLAUDECODE", "1"),
            ("RUNWICK_SESSION_ID", "s"),
        ]);
        let overlay = EnvOverlay {
            version: 3,
            set: vec![("PATH".into(), "/opt/tools/bin:/usr/bin".into()), ("API_BASE".into(), "https://a".into())],
            unset: vec!["OLD_TOKEN".into()],
        };
        let env = job_env(&base, &overlay);
        assert_eq!(env.get("PATH").map(String::as_str), Some("/opt/tools/bin:/usr/bin"));
        assert_eq!(env.get("API_BASE").map(String::as_str), Some("https://a"));
        assert_eq!(env.get("HOME").map(String::as_str), Some("/Users/me"));
        assert!(!env.contains_key("OLD_TOKEN"));
        assert!(!env.contains_key("CLAUDECODE"), "terminal markers are stripped");
        assert!(!env.contains_key("RUNWICK_SESSION_ID"));
    }

    fn fake_cli(dir: &Path, name: &str, script: &str) {
        use std::os::unix::fs::PermissionsExt;
        let p = dir.join(name);
        std::fs::write(&p, script).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn cli_is_found_on_the_job_path_and_sees_the_job_environment() {
        let bin = tempfile::tempdir().unwrap();
        let path = format!("/nonexistent:{}", bin.path().display());
        assert_eq!(provider_on(&path), None);
        fake_cli(bin.path(), "codex", "#!/bin/sh\nexit 0\n");
        assert!(matches!(provider_on(&path), Some(Provider::Codex(_))));
        // Claude is preferred; it echoes what it saw as its JSON result.
        fake_cli(
            bin.path(),
            "claude",
            "#!/bin/sh\nprintf '{\"type\":\"result\",\"is_error\":false,\"result\":\"%s|%s|%s\",\"modelUsage\":{\"m1\":{}}}' \"$API_BASE\" \"${OLD_TOKEN:-none}\" \"$1\"\n",
        );
        let provider = provider_on(&path).unwrap();
        assert!(matches!(provider, Provider::Claude(_)));
        let base = login(&[("PATH", "/usr/bin:/bin"), ("OLD_TOKEN", "x")]);
        let overlay = EnvOverlay {
            version: 1,
            set: vec![("API_BASE".into(), "https://a".into()), ("PATH".into(), format!("{path}:/usr/bin:/bin"))],
            unset: vec!["OLD_TOKEN".into()],
        };
        let env = job_env(&base, &overlay);
        let work = tempfile::tempdir().unwrap();
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let mut pid = None;
        let out = rt.block_on(run(&provider, &env, "프롬프트", &work.path().join("w"), |p| pid = Some(p))).unwrap();
        assert_eq!(out.text, "https://a|none|-p");
        assert_eq!(out.model.as_deref(), Some("m1"));
        assert!(pid.is_some());
        // A failing CLI reports its exit code and last line, not a fake success.
        fake_cli(bin.path(), "claude", "#!/bin/sh\necho '로그인이 필요해요' >&2\nexit 3\n");
        let err = rt.block_on(run(&provider, &env, "x", &work.path().join("w"), |_| {})).unwrap_err();
        assert!(err.contains("(3)") && err.contains("로그인이 필요해요"), "{err}");
    }
}
