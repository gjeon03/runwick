//! Detection of installed CLI tools: where each is on the login PATH, its version, and whether
//! it looks logged in. Login is judged from files and variables without reading secret values;
//! when that cannot tell, the state is `Unknown`.

use crate::host::Ctx;
use crate::router::Router;
use crate::sessions::env::{LoginEnv, login_env};
use runwick_core::api::sessions::Tool;
use runwick_core::api::tools::*;
use runwick_core::josa;
use runwick_core::rpc::{RpcError, topic};
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const VERSION_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Default)]
pub struct ToolCache {
    cached: Mutex<Option<Vec<ToolInfo>>>,
}

pub fn register(r: &mut Router) {
    r.add::<ToolsList, _, _>(|ctx: Ctx, q: ToolsQuery| async move {
        if !q.refresh
            && let Some(list) = ctx.host.tools.cached.lock().ok().and_then(|c| c.clone())
        {
            return Ok(list);
        }
        let list = tokio::task::spawn_blocking(detect_all).await.map_err(RpcError::internal)?;
        if let Ok(mut c) = ctx.host.tools.cached.lock() {
            *c = Some(list.clone());
        }
        if q.refresh {
            ctx.host.bus.changed(&[topic::TOOLS]);
        }
        Ok(list)
    });
}

fn detect_all() -> Vec<ToolInfo> {
    let env = login_env();
    let handles: Vec<_> = Tool::ALL
        .into_iter()
        .map(|tool| {
            let env = env.clone();
            std::thread::spawn(move || detect(tool, &env))
        })
        .collect();
    handles.into_iter().zip(Tool::ALL).map(|(h, tool)| h.join().unwrap_or_else(|_| missing(tool, None))).collect()
}

fn detect(tool: Tool, env: &LoginEnv) -> ToolInfo {
    let path = match tool {
        Tool::Shell => Some(PathBuf::from(env.shell())).filter(|p| is_executable(p)),
        Tool::Claude => resolve("claude", env.path()),
        Tool::Codex => resolve("codex", env.path()),
    };
    let Some(path) = path else {
        return missing(tool, Some(env));
    };
    let version = version_of(&path, env);
    let (auth, note) = match tool {
        Tool::Claude => (claude_auth(env), None),
        Tool::Codex => (codex_auth(env), None),
        Tool::Shell => (AuthState::LoggedIn, Some("로그인이 필요 없는 기본 셸이에요".to_string())),
    };
    ToolInfo {
        tool,
        installed: true,
        path: Some(runwick_core::paths::display_path(&path)),
        version,
        auth,
        support: support(tool),
        note,
        checked_at: runwick_core::now(),
    }
}

fn missing(tool: Tool, env: Option<&LoginEnv>) -> ToolInfo {
    let name = match tool {
        Tool::Claude => "claude".to_string(),
        Tool::Codex => "codex".to_string(),
        Tool::Shell => env.map(|e| e.shell()).unwrap_or_else(|| "셸".into()),
    };
    ToolInfo {
        tool,
        installed: false,
        path: None,
        version: None,
        auth: AuthState::Unknown,
        support: support(tool),
        note: Some(format!("로그인 셸의 PATH에서 {} 찾지 못했어요", josa::eul_reul(&format!("‘{name}’")))),
        checked_at: runwick_core::now(),
    }
}

fn support(tool: Tool) -> ToolSupport {
    let agent = tool.is_agent();
    ToolSupport { resume: agent, transcript: agent, delegate: agent, refresh: agent }
}

/// First executable named `name` on a `PATH` string.
pub fn resolve(name: &str, path: &str) -> Option<PathBuf> {
    path.split(':').filter(|d| !d.is_empty()).map(|d| Path::new(d).join(name)).find(|p| is_executable(p))
}

fn is_executable(p: &Path) -> bool {
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// `--version`, first version-looking word of the first line ("2.1.319 (Claude Code)",
/// "codex-cli 0.164.0", "zsh 5.9 (…)").
fn version_of(path: &Path, env: &LoginEnv) -> Option<String> {
    let mut child = Command::new(path)
        .arg("--version")
        .env_clear()
        .envs(env.vars.iter().filter(|(k, _)| !crate::sessions::env::is_stripped(k)))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .ok()?;
    let deadline = Instant::now() + VERSION_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                unsafe {
                    libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL);
                }
                let _ = child.wait();
                return None;
            }
        }
    }
    let mut out = String::new();
    child.stdout.take()?.take(4096).read_to_string(&mut out).ok()?;
    parse_version(&out)
}

fn parse_version(out: &str) -> Option<String> {
    let line = out.lines().find(|l| !l.trim().is_empty())?;
    line.split_whitespace()
        .map(|w| w.trim_matches(|c: char| c == ',' || c == '(' || c == ')'))
        .find(|w| w.chars().next().is_some_and(|c| c.is_ascii_digit()))
        .map(str::to_string)
}

/// Logged in when the account is recorded in Claude's config or an API key is set (only the
/// presence of the key or variable is checked).
fn claude_auth(env: &LoginEnv) -> AuthState {
    if env.get("ANTHROPIC_API_KEY").is_some_and(|v| !v.is_empty()) {
        return AuthState::LoggedIn;
    }
    let config = match env.get("CLAUDE_CONFIG_DIR").filter(|d| !d.is_empty()) {
        Some(dir) => PathBuf::from(dir).join(".claude.json"),
        None => runwick_core::paths::user_home().join(".claude.json"),
    };
    let has_account = std::fs::File::open(&config)
        .ok()
        .and_then(|f| serde_json::from_reader::<_, serde_json::Value>(std::io::BufReader::new(f)).ok())
        .is_some_and(|v| v.get("oauthAccount").is_some_and(|a| !a.is_null()));
    if has_account { AuthState::LoggedIn } else { AuthState::Unknown }
}

fn codex_auth(env: &LoginEnv) -> AuthState {
    let home = env
        .get("CODEX_HOME")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| runwick_core::paths::user_home().join(".codex"));
    if home.join("auth.json").is_file() { AuthState::LoggedIn } else { AuthState::Unknown }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_are_read_from_common_formats() {
        assert_eq!(parse_version("2.1.319 (Claude Code)\n").as_deref(), Some("2.1.319"));
        assert_eq!(parse_version("codex-cli 0.164.0\n").as_deref(), Some("0.164.0"));
        assert_eq!(parse_version("zsh 5.9 (arm64-apple-darwin24.0)\n").as_deref(), Some("5.9"));
        assert_eq!(parse_version("GNU bash, version 3.2.57(1)-release").as_deref(), Some("3.2.57(1)-release"));
        assert_eq!(parse_version("\n"), None);
    }

    #[test]
    fn resolves_on_the_given_path_only() {
        let d = tempfile::tempdir().unwrap();
        let bin = d.path().join("faketool");
        std::fs::write(&bin, "#!/bin/sh\necho 1.2.3\n").unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = format!("/nonexistent:{}", d.path().display());
        assert_eq!(resolve("faketool", &path), Some(bin.clone()));
        assert_eq!(resolve("faketool", "/usr/bin"), None);
        let env = LoginEnv {
            vars: [("PATH".to_string(), path)].into_iter().collect(),
            source: crate::sessions::env::Source::LoginShell,
        };
        assert_eq!(version_of(&bin, &env).as_deref(), Some("1.2.3"));
    }
}
