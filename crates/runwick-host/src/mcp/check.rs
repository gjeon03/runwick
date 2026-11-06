//! Connection status as the provider CLI reports it: `claude mcp list` (tests each server) and
//! `codex mcp list --json` (configuration and login only; Codex does not test connections).
//! Runwick only reads the CLI's answer and never changes provider settings.

use runwick_core::api::mcp::*;
use runwick_core::api::sessions::Tool;
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// `claude mcp list` connects to every server; slow ones can take a while.
const TIMEOUT: Duration = Duration::from_secs(60);

/// Ask the CLI. `env` is the environment a new run would get (login environment and global
/// variables), so servers that read variables see the same values as the session.
pub fn run(tool: Tool, cwd: Option<&str>, env: &[(String, String)]) -> McpCheckResult {
    let checked_at = runwick_core::now();
    let fail = |e: String| McpCheckResult {
        provider: tool,
        cwd: cwd.map(str::to_string),
        checked_at,
        statuses: Vec::new(),
        error: Some(e),
    };
    let (name, args): (&str, &[&str]) = match tool {
        Tool::Claude => ("claude", &["mcp", "list"]),
        Tool::Codex => ("codex", &["mcp", "list", "--json"]),
        Tool::Shell => return fail("셸에는 MCP 설정이 없어요".into()),
    };
    let path = env.iter().find(|(k, _)| k == "PATH").map(|(_, v)| v.as_str()).unwrap_or("");
    let Some(program) = crate::tools::resolve(name, path) else {
        return fail(format!("‘{name}’를 찾지 못해 확인할 수 없어요"));
    };
    let dir =
        cwd.map(Path::new).filter(|d| d.is_dir()).map(Path::to_path_buf).unwrap_or_else(runwick_core::paths::user_home);
    let out = match capture(&program, args, &dir, env) {
        Ok(o) => o,
        Err(e) => return fail(e),
    };
    let statuses = match tool {
        Tool::Claude => parse_claude(&out),
        _ => match parse_codex(&out) {
            Some(s) => s,
            None => return fail("Codex의 응답을 읽지 못했어요".into()),
        },
    };
    McpCheckResult { provider: tool, cwd: cwd.map(str::to_string), checked_at, statuses, error: None }
}

fn capture(program: &Path, args: &[&str], cwd: &Path, env: &[(String, String)]) -> Result<String, String> {
    let mut child = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .map_err(|e| format!("확인 명령을 실행하지 못했어요: {e}"))?;
    let mut stdout = child.stdout.take().ok_or("출력을 읽지 못했어요")?;
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = (&mut stdout).take(4 * 1024 * 1024).read_to_end(&mut buf);
        buf
    });
    let deadline = Instant::now() + TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            _ => {
                unsafe {
                    libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL);
                }
                let _ = child.wait();
                return Err(format!("{}초 안에 끝나지 않아 확인을 멈췄어요", TIMEOUT.as_secs()));
            }
        }
    }
    let bytes = reader.join().unwrap_or_default();
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Remove terminal escapes (colors, OSC 8 links) from CLI output.
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('[') => {
                for c in it.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
            Some(']') => {
                while let Some(c) = it.next() {
                    if c == '\u{7}' {
                        break;
                    }
                    if c == '\u{1b}' {
                        it.next();
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// Lines like `name: target - ✔ Connected` or `name: target - ✘ Failed to connect — reason`.
pub fn parse_claude(out: &str) -> Vec<McpStatus> {
    const MARKS: [&str; 8] = [" - ✔ ", " - ✓ ", " - ✘ ", " - ✗ ", " - ! ", " - ⏸ ", " - ⚠ ", " - - "];
    let text = strip_ansi(out);
    let mut statuses = Vec::new();
    for line in text.lines() {
        let Some((at, mark)) = MARKS.iter().filter_map(|m| line.rfind(m).map(|i| (i, *m))).max_by_key(|(i, _)| *i)
        else {
            continue;
        };
        let Some((name, _)) = line[..at].split_once(": ") else {
            continue;
        };
        let status = line[at + mark.len()..].trim();
        let (word, detail) = match status.split_once(" — ") {
            Some((w, d)) => (w.trim(), Some(d.trim().to_string())),
            None => (status, None),
        };
        let w = word.to_ascii_lowercase();
        let health = if w.starts_with("connected") {
            McpHealth::Connected
        } else if w.contains("auth") {
            McpHealth::NeedsAuth
        } else if w.contains("pending") || w.contains("approval") {
            McpHealth::PendingApproval
        } else if w.contains("disabled") {
            McpHealth::Disabled
        } else if w.contains("fail") || w.contains("error") {
            McpHealth::Failed
        } else {
            McpHealth::Unknown
        };
        let detail = match health {
            McpHealth::Unknown => Some(detail.map(|d| format!("{word} — {d}")).unwrap_or_else(|| word.to_string())),
            _ => detail,
        };
        statuses.push(McpStatus { name: name.trim().to_string(), health, detail });
    }
    statuses
}

/// `codex mcp list --json`: `[{name, enabled, auth_status, ...}]`.
pub fn parse_codex(out: &str) -> Option<Vec<McpStatus>> {
    let start = out.find('[')?;
    let v: serde_json::Value = serde_json::from_str(out[start..].trim()).ok()?;
    let list = v.as_array()?;
    Some(
        list.iter()
            .filter_map(|s| {
                let name = s.get("name")?.as_str()?.to_string();
                let enabled = s.get("enabled").and_then(|e| e.as_bool()).unwrap_or(true);
                let auth = s.get("auth_status").and_then(|a| a.as_str()).unwrap_or("");
                let (health, detail) = if !enabled {
                    (McpHealth::Disabled, None)
                } else if auth == "not_logged_in" {
                    (McpHealth::NeedsAuth, Some("‘codex mcp login’으로 로그인해야 해요".to_string()))
                } else if auth == "o_auth" || auth == "bearer_token" {
                    (McpHealth::Unknown, Some("인증 정보 있음 · 연결은 Codex 안에서만 확인돼요".to_string()))
                } else {
                    (McpHealth::Unknown, Some("Codex는 연결을 미리 시험하지 않아요".to_string()))
                };
                Some(McpStatus { name, health, detail })
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_claude_mcp_list_output() {
        let out = "Checking MCP server health…\n\n\
            claude.ai Claude Docs: https://api.example.com/mcp - ✔ Connected\n\
            plugin:design:slack: https://mcp.slack.com/mcp (HTTP) - ! Needs authentication\n\
            purple-hub: npx -y mcp-remote https://x.ts.net/mcp - ✘ Failed to connect — CONNECTION_CLOSED: Connection closed\n\
            plugin:design:gmail:  (HTTP) - - Not configured\n\
            shared: https://s.example/sse (SSE) - ⏸ Pending approval\n\
            dash-args: tool --a - b - ✔ Connected\n\
            \n\u{1b}[33mMCP config diagnostics ⚠\u{1b}[0m\n\
            For help, see: \u{1b}]8;id=x;https://code.claude.com/docs\u{7}link\u{1b}]8;;\u{7}\n\
            \u{20}└ [Warning] [weekly] mcpServers.weekly: Missing environment variables: WEEKLY_MCP_TOKEN\n";
        let s = parse_claude(out);
        let got: Vec<(&str, McpHealth)> = s.iter().map(|s| (s.name.as_str(), s.health)).collect();
        assert_eq!(
            got,
            vec![
                ("claude.ai Claude Docs", McpHealth::Connected),
                ("plugin:design:slack", McpHealth::NeedsAuth),
                ("purple-hub", McpHealth::Failed),
                ("plugin:design:gmail", McpHealth::Unknown),
                ("shared", McpHealth::PendingApproval),
                ("dash-args", McpHealth::Connected),
            ]
        );
        assert_eq!(s[2].detail.as_deref(), Some("CONNECTION_CLOSED: Connection closed"));
        assert_eq!(s[3].detail.as_deref(), Some("Not configured"));
    }

    #[test]
    fn reads_codex_json_and_never_claims_a_connection() {
        let out = r#"[{"name":"notion","enabled":true,"auth_status":"not_logged_in"},
            {"name":"memory","enabled":true,"auth_status":"unsupported"},
            {"name":"ctx","enabled":true,"auth_status":"o_auth"},
            {"name":"off","enabled":false,"auth_status":"unsupported"}]"#;
        let s = parse_codex(out).unwrap();
        let got: Vec<(&str, McpHealth)> = s.iter().map(|s| (s.name.as_str(), s.health)).collect();
        assert_eq!(
            got,
            vec![
                ("notion", McpHealth::NeedsAuth),
                ("memory", McpHealth::Unknown),
                ("ctx", McpHealth::Unknown),
                ("off", McpHealth::Disabled)
            ]
        );
        assert!(parse_codex("not json").is_none());
    }
}
