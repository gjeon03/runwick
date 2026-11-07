//! Collaboration settings (Settings › 협업) and the short guidance agent CLIs get at launch so
//! they know the `runwick` command exists.

use crate::host::Host;
use crate::store::{Store, db_err};
use runwick_core::api::collab::CollabSettings;
use runwick_core::api::sessions::{Session, Tool};
use runwick_core::rpc::RpcError;
use std::path::Path;
use std::sync::Arc;

const KEY: &str = "collab.settings";
pub const MAX_LIMIT: u32 = 16;

/// Launch guidance for both agent CLIs. Codex gets absolute command paths and its local-socket
/// approval instructions below; Claude Code keeps the original text.
pub const GUIDANCE: &str = "You are running inside Runwick, a workroom that can run other AI coding CLIs next to you. \
Delegate only when it clearly helps (independent work, a second opinion, a review); otherwise do the work yourself.
- runwick delegate --tool codex|claude [--role <role>] [--title <title>] [--split <new-branch>] \"<task>\"  starts a worker and prints its id (--split works in a separate git worktree)
- runwick wait <id> [--timeout <seconds>]  waits until the worker finished its turn and prints its answer; a timeout is not a failure
- runwick send <id> \"<message>\" [--now]  gives a worker a follow-up (queued while it is busy)
- runwick workers | runwick result <id> | runwick status
If another session gave you this work, end it with: runwick report --summary \"<what you did>\" [--file <path>]... [--check \"<command>=pass|fail\"]... [--not-done \"<item>\"]...
A worker's answer is not verified work: check its result before relying on it.";

const REPORT_LINE: &str = "If another session gave you this work, end it with: runwick report --summary \"<what you did>\" [--file <path>]... [--check \"<command>=pass|fail\"]... [--not-done \"<item>\"]...";
const CODEX_REPORT_LINE: &str = "If another session gave you this work, end with a final answer that says what you did, \
the files you changed, the checks you ran and what is not done. Runwick reads it from your conversation record; \
do not request an escalation only to run a report command.";

/// Use the host's bundled command even when a tool shell filters or rebuilds PATH.
pub fn codex_command() -> String {
    shell_command(&runwick_core::paths::bin_dir().join("runwick"))
}

/// Quote only when needed: Codex reads a quoted command as a shell script (`zsh -lc "..."`), so
/// the `prefix_rule` with the bare path would not match it and every call would ask again.
fn shell_command(path: &Path) -> String {
    let text = path.to_string_lossy();
    if !text.is_empty() && text.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-' | '+')) {
        return text.into_owned();
    }
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// Subcommands a standing approval may cover: they only read or wait. `delegate` and `send`
/// start or instruct workers, so each one keeps its own approval, and the bare executable is
/// never proposed (it would cover every current and future subcommand outside the sandbox).
const STANDING_SUBCOMMANDS: [&str; 4] = ["wait", "result", "workers", "status"];

fn approval_prefix(path: &Path, subcommand: &str) -> String {
    serde_json::json!([path.to_string_lossy(), subcommand]).to_string()
}

fn standing_prefixes(path: &Path) -> String {
    STANDING_SUBCOMMANDS.iter().map(|sub| approval_prefix(path, sub)).collect::<Vec<_>>().join(", ")
}

fn launch_guidance(tool: Tool, enabled: bool) -> Option<String> {
    if !enabled {
        return None;
    }
    match tool {
        Tool::Claude => Some(GUIDANCE.to_string()),
        // A Codex worker answers in its final message instead of `runwick report`: reporting
        // would need an escalation, and every delegated task would then stop at an approval screen.
        Tool::Codex => Some(format!(
            "{}\nRunwick commands connect to a local Unix socket. For these commands, request escalated \
permissions through Codex's normal approval flow (sandbox_permissions=\"require_escalated\") and explain \
that the command contacts the local Runwick host. For a command that only reads or waits, propose in \
the same shell tool call the matching prefix_rule, one of: {}, so the person can choose to stop being \
asked for that subcommand. Each is an argv array of the executable's absolute path and the subcommand, \
without shell quoting or further arguments. For delegate and send, propose no prefix_rule: they start or \
instruct workers and the person approves each one. Never propose the executable alone. Invoke the Runwick \
command directly so the prefix matches. Let the person approve on screen; never auto-approve. Do not \
change the sandbox or approval policy. If escalation is unavailable or denied, explain the block.",
            GUIDANCE.replace(REPORT_LINE, CODEX_REPORT_LINE).replace("runwick ", &format!("{} ", codex_command())),
            standing_prefixes(&runwick_core::paths::bin_dir().join("runwick"))
        )),
        Tool::Shell => None,
    }
}

pub fn load(store: &Store) -> CollabSettings {
    crate::meta::kv_get(store, KEY)
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_value::<CollabSettings>(v).ok())
        .map(normalize)
        .unwrap_or_default()
}

pub fn save(store: &Store, s: CollabSettings) -> Result<CollabSettings, RpcError> {
    if s.auto_limit == 0 || s.auto_limit > MAX_LIMIT {
        return Err(RpcError::invalid(format!("자동 배정 한도는 1에서 {MAX_LIMIT} 사이로 정해 주세요")));
    }
    let s = normalize(s);
    let v = serde_json::to_value(&s).map_err(RpcError::internal)?;
    crate::meta::kv_put(store, KEY, &v).map_err(db_err)?;
    Ok(s)
}

fn normalize(mut s: CollabSettings) -> CollabSettings {
    s.auto_limit = s.auto_limit.clamp(1, MAX_LIMIT);
    s.allowed_tools.retain(|t| t.is_agent());
    s.allowed_tools.dedup();
    s
}

/// Install guidance for both new and resumed agent sessions while the setting is on.
pub fn install(host: &Arc<Host>) {
    host.sessions
        .set_guidance_hook(Arc::new(|host: &Host, s: &Session| launch_guidance(s.tool, load(&host.store).guidance)));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_guidance_respects_the_setting_and_provider() {
        for tool in Tool::ALL {
            assert!(launch_guidance(tool, false).is_none());
        }
        assert!(launch_guidance(Tool::Shell, true).is_none());
        assert_eq!(launch_guidance(Tool::Claude, true).as_deref(), Some(GUIDANCE));
        let text = launch_guidance(Tool::Codex, true).unwrap();
        assert!(GUIDANCE.contains(REPORT_LINE), "the Codex text replaces the report line");
        for command in ["delegate", "wait", "send", "workers", "result", "status"] {
            assert!(text.contains(&format!("{} {command}", codex_command())), "{command}");
        }
        assert!(text.contains("require_escalated") && text.contains("unavailable or denied"));
        let bin = runwick_core::paths::bin_dir().join("runwick");
        for sub in STANDING_SUBCOMMANDS {
            assert!(text.contains(&approval_prefix(&bin, sub)), "{sub}");
        }
        for sub in ["delegate", "send"] {
            assert!(!text.contains(&approval_prefix(&bin, sub)), "{sub} keeps a per-call approval");
        }
        let bare = serde_json::json!([bin.to_string_lossy()]).to_string();
        assert!(!text.contains(&bare), "the executable alone is never proposed");
        assert!(text.contains("Never propose the executable alone"));
        assert!(text.contains("Let the person approve on screen; never auto-approve"));
        assert!(text.contains("Do not change the sandbox or approval policy"));
        assert!(text.contains("conversation record") && !text.contains(&format!("{} report", codex_command())));
    }

    #[test]
    fn command_path_is_shell_quoted() {
        assert_eq!(shell_command(Path::new("/tmp/space ' 한글/$bin/runwick")), "'/tmp/space '\\'' 한글/$bin/runwick'");
        assert_eq!(shell_command(Path::new("/Users/me/.runwick/bin/runwick")), "/Users/me/.runwick/bin/runwick");
        let path = "/tmp/space ' \" 한글/$bin/runwick";
        let prefix: Vec<String> = serde_json::from_str(&approval_prefix(Path::new(path), "wait")).unwrap();
        assert_eq!(prefix, [path, "wait"], "approval matches argv, not shell-quoted text or the whole command");
    }

    #[test]
    fn settings_default_validate_and_persist() {
        let store = Store::open_in_memory().unwrap();
        assert_eq!(load(&store), CollabSettings::default());
        let bad = CollabSettings { auto_limit: 0, ..Default::default() };
        assert!(save(&store, bad).is_err());
        let s = CollabSettings { guidance: false, auto_limit: 5, allowed_tools: vec![Tool::Codex, Tool::Shell] };
        let saved = save(&store, s).unwrap();
        assert_eq!(saved.allowed_tools, vec![Tool::Codex]);
        assert_eq!(load(&store), saved);
        assert!(GUIDANCE.contains("runwick report") && GUIDANCE.len() < 1600);
    }
}
