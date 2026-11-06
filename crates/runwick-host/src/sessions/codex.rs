//! Finding the thread id of a Codex run, so it can be resumed with `codex resume <id>`.
//!
//! Codex writes `$CODEX_HOME/sessions/YYYY/MM/DD/rollout-*.jsonl` (local date) whose first line
//! is `{"type":"session_meta","payload":{"id":…,"cwd":…,"timestamp":…,"source":…}}`. The file
//! may only appear once the first message is sent, so the run is watched while it is alive:
//! often at first, then rarely. Only the first line of new files is read.

use chrono::{Datelike, Local};
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

pub fn sessions_dir(codex_home: Option<&str>) -> PathBuf {
    match codex_home.filter(|s| !s.is_empty()) {
        Some(h) => PathBuf::from(h).join("sessions"),
        None => runwick_core::paths::user_home().join(".codex").join("sessions"),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub id: String,
    pub modified: SystemTime,
}

/// Rollout files modified since `since` whose session started in `cwd`, oldest first.
pub fn candidates(root: &Path, cwd: &Path, since: SystemTime) -> Vec<Candidate> {
    let mut found = Vec::new();
    let today = Local::now();
    for day in [today - chrono::Duration::days(1), today] {
        let dir = root
            .join(format!("{:04}", day.year()))
            .join(format!("{:02}", day.month()))
            .join(format!("{:02}", day.day()));
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for e in entries.flatten() {
            let name = e.file_name();
            let name = name.to_string_lossy();
            if !name.starts_with("rollout-") || !name.ends_with(".jsonl") {
                continue;
            }
            let Ok(meta) = e.metadata() else { continue };
            let created = meta.created().or_else(|_| meta.modified()).unwrap_or(SystemTime::UNIX_EPOCH);
            if created + Duration::from_secs(2) < since {
                continue;
            }
            if let Some((id, file_cwd, source)) = read_meta(&e.path())
                && same_dir(Path::new(&file_cwd), cwd)
                && source.as_deref() != Some("exec")
            {
                found.push(Candidate { id, modified: created });
            }
        }
    }
    found.sort_by_key(|c| c.modified);
    found
}

fn same_dir(a: &Path, b: &Path) -> bool {
    a == b || matches!((a.canonicalize(), b.canonicalize()), (Ok(x), Ok(y)) if x == y)
}

/// `(id, cwd, source)` from the first line, which is at most a few hundred kilobytes.
fn read_meta(path: &Path) -> Option<(String, String, Option<String>)> {
    let file = std::fs::File::open(path).ok()?;
    let mut line = String::new();
    BufReader::new(file.take(4 * 1024 * 1024)).read_line(&mut line).ok()?;
    let v: serde_json::Value = serde_json::from_str(line.trim_end()).ok()?;
    if v.get("type")?.as_str()? != "session_meta" {
        return None;
    }
    let p = v.get("payload")?;
    Some((
        p.get("id")?.as_str()?.to_string(),
        p.get("cwd")?.as_str()?.to_string(),
        p.get("source").and_then(|s| s.as_str()).map(str::to_string),
    ))
}

/// Delay before the next look, growing with the time the run has been alive.
pub fn next_poll(alive_for: Duration) -> Duration {
    if alive_for < Duration::from_secs(20) {
        Duration::from_secs(1)
    } else if alive_for < Duration::from_secs(10 * 60) {
        Duration::from_secs(5)
    } else {
        Duration::from_secs(30)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_rollout(root: &Path, name: &str, id: &str, cwd: &Path, source: &str) {
        let now = Local::now();
        let dir = root
            .join(format!("{:04}", now.year()))
            .join(format!("{:02}", now.month()))
            .join(format!("{:02}", now.day()));
        std::fs::create_dir_all(&dir).unwrap();
        let meta = serde_json::json!({"timestamp": "x", "type": "session_meta",
            "payload": {"id": id, "cwd": cwd, "source": source, "base_instructions": {"text": "long"}}});
        std::fs::write(dir.join(name), format!("{meta}\n{{\"type\":\"event_msg\"}}\n")).unwrap();
    }

    #[test]
    fn finds_new_interactive_thread_for_the_folder() {
        let root = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let since = SystemTime::now() - Duration::from_secs(1);
        write_rollout(root.path(), "rollout-a.jsonl", "id-other", other.path(), "cli");
        write_rollout(root.path(), "rollout-b.jsonl", "id-exec", work.path(), "exec");
        write_rollout(root.path(), "rollout-c.jsonl", "id-mine", work.path(), "cli");
        std::fs::write(root.path().join("ignored.txt"), "x").unwrap();
        let c = candidates(root.path(), work.path(), since);
        assert_eq!(c.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(), vec!["id-mine"]);
        // Files from before the run are not considered.
        let later = SystemTime::now() + Duration::from_secs(60);
        assert!(candidates(root.path(), work.path(), later).is_empty());
    }
}
