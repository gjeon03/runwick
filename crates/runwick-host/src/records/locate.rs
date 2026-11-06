//! Where each CLI keeps a conversation's transcript.

use chrono::{Datelike, Local};
use std::path::{Path, PathBuf};

/// Transcript folders of the CLIs on this computer.
#[derive(Debug, Clone)]
pub struct Roots {
    /// `~/.claude/projects` (or `$CLAUDE_CONFIG_DIR/projects`).
    pub claude: PathBuf,
    /// `~/.codex/sessions` (or `$CODEX_HOME/sessions`).
    pub codex: PathBuf,
}

impl Roots {
    /// From the person's login environment (what their CLIs see). `RUNWICK_TRANSCRIPTS=<dir>`
    /// points both at `<dir>/claude/projects` and `<dir>/codex/sessions` instead (tests and
    /// verification runs, so they never read the person's real transcripts).
    pub fn from_login_env() -> Self {
        // Verification builds only: a release host always reads the person's real CLI folders.
        #[cfg(any(debug_assertions, feature = "verify-hooks"))]
        if let Some(dir) = std::env::var_os("RUNWICK_TRANSCRIPTS").filter(|v| !v.is_empty()).map(PathBuf::from) {
            return Self { claude: dir.join("claude").join("projects"), codex: dir.join("codex").join("sessions") };
        }
        let env = crate::sessions::env::login_env();
        let home = runwick_core::paths::user_home();
        let claude = env
            .get("CLAUDE_CONFIG_DIR")
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".claude"))
            .join("projects");
        let codex = env
            .get("CODEX_HOME")
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".codex"))
            .join("sessions");
        Self { claude, codex }
    }
}

/// Claude Code's folder name for a working directory: every character that is not an ASCII
/// letter or digit becomes `-`.
pub fn claude_dir_name(cwd: &str) -> String {
    cwd.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect()
}

/// `<root>/<cwd as folder name>/<id>.jsonl`, or the same file name in any project folder
/// (Claude shortens very long folder names). `scan` allows the slower search.
pub fn claude_transcript(root: &Path, cwd: &str, id: &str, scan: bool) -> Option<PathBuf> {
    let file = format!("{id}.jsonl");
    let direct = root.join(claude_dir_name(cwd)).join(&file);
    if direct.is_file() {
        return Some(direct);
    }
    if !scan {
        return None;
    }
    std::fs::read_dir(root).ok()?.flatten().map(|e| e.path().join(&file)).find(|p| p.is_file())
}

/// `<root>/YYYY/MM/DD/rollout-…-<thread id>.jsonl`. Looks at the days around `near` first, and
/// at every day folder when `scan` is allowed.
pub fn codex_transcript(root: &Path, thread: &str, near: chrono::DateTime<Local>, scan: bool) -> Option<PathBuf> {
    let suffix = format!("-{thread}.jsonl");
    let matches = |dir: &Path| -> Option<PathBuf> {
        std::fs::read_dir(dir).ok()?.flatten().find_map(|e| {
            let name = e.file_name();
            let name = name.to_string_lossy();
            (name.starts_with("rollout-") && name.ends_with(&suffix)).then(|| e.path())
        })
    };
    let day_dir = |d: chrono::DateTime<Local>| {
        root.join(format!("{:04}", d.year())).join(format!("{:02}", d.month())).join(format!("{:02}", d.day()))
    };
    for offset in [0i64, -1, 1] {
        if let Some(p) = matches(&day_dir(near + chrono::Duration::days(offset))) {
            return Some(p);
        }
    }
    if !scan {
        return None;
    }
    let sorted = |p: &Path| -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> =
            std::fs::read_dir(p).map(|r| r.flatten().map(|e| e.path()).collect()).unwrap_or_default();
        v.sort();
        v.reverse(); // newest first
        v
    };
    for year in sorted(root) {
        for month in sorted(&year) {
            for day in sorted(&month) {
                if let Some(p) = matches(&day) {
                    return Some(p);
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_folder_names_replace_non_alphanumerics() {
        assert_eq!(claude_dir_name("/Users/me/Documents_LOCAL/run.wick"), "-Users-me-Documents-LOCAL-run-wick");
        assert_eq!(claude_dir_name("/작업/앱"), "-----");
    }

    #[test]
    fn finds_transcripts_directly_and_by_scan() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("-w-app");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("abc.jsonl"), "").unwrap();
        assert_eq!(claude_transcript(root.path(), "/w/app", "abc", false), Some(dir.join("abc.jsonl")));
        // Moved under a shortened folder name: only the scan finds it.
        assert_eq!(claude_transcript(root.path(), "/w/other", "abc", false), None);
        assert_eq!(claude_transcript(root.path(), "/w/other", "abc", true), Some(dir.join("abc.jsonl")));

        let codex = tempfile::tempdir().unwrap();
        let old = codex.path().join("2025").join("01").join("02");
        std::fs::create_dir_all(&old).unwrap();
        let file = old.join("rollout-2025-01-02T10-00-00-t-9.jsonl");
        std::fs::write(&file, "").unwrap();
        let now = Local::now();
        assert_eq!(codex_transcript(codex.path(), "t-9", now, false), None);
        assert_eq!(codex_transcript(codex.path(), "t-9", now, true), Some(file));
    }
}
