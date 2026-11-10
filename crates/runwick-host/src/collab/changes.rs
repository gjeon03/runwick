//! What a worker changed, read from git in the worker's folder: files against the request's
//! delivery state (committed or not, plus untracked files), a fingerprint of the working tree
//! (to tell whether files changed after a reported check) and one file's diff. Paths a worker
//! names (check logs) are never opened here.
//!
//! The folder is the worker's: links in it are never followed (a link to a device, a pipe or a
//! file outside would otherwise be read), file reads are capped and run off the async runtime,
//! and git gets a time limit. Non-git folders give no files and no fingerprint: unknown stays
//! unknown.

use crate::git::safe_git;
use runwick_core::api::collab::{ChangedFile, FileDiff};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::Read;
use std::path::{Component, Path};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncReadExt;

/// Diffs larger than this are cut (the result view is not an editor).
const DIFF_LIMIT: usize = 200 * 1024;
/// Untracked files whose lines are counted at most this large each.
const COUNT_LIMIT: u64 = 1024 * 1024;
/// Untracked files listed (and looked at) at most.
const MAX_UNTRACKED: usize = 2000;
/// Paths looked at for the fingerprint at most.
const MAX_FINGERPRINT_PATHS: usize = 20_000;
/// Bound git output too; a truncated delivery baseline must never look like a clean tree.
const GIT_OUTPUT_LIMIT: u64 = 4 * 1024 * 1024;
/// A git call that takes longer gives up (a lock, a huge repository): unknown, not wrong.
#[cfg(not(test))]
const GIT_TIMEOUT: Duration = Duration::from_secs(20);
/// Tests run hundreds of git calls in parallel on a busy machine; only stuck calls should fail.
#[cfg(test)]
const GIT_TIMEOUT: Duration = Duration::from_secs(120);

async fn run_git(cwd: &Path, args: &[&str]) -> Result<String, String> {
    let read = async {
        let mut cmd = safe_git(cwd, args).await?;
        let mut child =
            cmd.stdout(Stdio::piped()).stderr(Stdio::null()).kill_on_drop(true).spawn().map_err(|e| e.to_string())?;
        let mut bytes = Vec::new();
        child
            .stdout
            .take()
            .ok_or("git 출력이 없어요")?
            .take(GIT_OUTPUT_LIMIT + 1)
            .read_to_end(&mut bytes)
            .await
            .map_err(|e| e.to_string())?;
        if bytes.len() as u64 > GIT_OUTPUT_LIMIT {
            return Err("git 출력이 너무 커서 변경 상태를 확인하지 못했어요".into());
        }
        if !child.wait().await.map_err(|e| e.to_string())?.success() {
            return Err("git에서 변경 상태를 읽지 못했어요".into());
        }
        String::from_utf8(bytes).map(|s| s.trim_end().to_string()).map_err(|e| e.to_string())
    };
    match tokio::time::timeout(GIT_TIMEOUT, read).await {
        Ok(r) => r,
        Err(_) => Err("git이 응답하지 않았어요".into()),
    }
}

pub async fn head(cwd: &Path) -> Option<String> {
    run_git(cwd, &["rev-parse", "HEAD"]).await.ok().filter(|s| !s.is_empty())
}

/// Host-only delivery baseline. None for a path means it could not be safely inspected.
#[derive(Debug, Serialize, Deserialize)]
pub struct TreeState {
    paths: BTreeMap<String, Option<PathState>>,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
enum PathState {
    Missing,
    Present(String),
}

/// A bounded alternative to hashing file contents. ctime catches same-size rewrites even if
/// mtime is restored; device/inode catch replacements. Ignore atime so reads do not count.
/// Never follow links or read special files, including in intermediate path components.
fn path_state(root: &Path, rel: &str) -> Option<PathState> {
    use std::os::unix::fs::MetadataExt;
    let mut full = root.to_path_buf();
    let mut parts = Path::new(rel).components().peekable();
    while let Some(part) = parts.next() {
        let Component::Normal(name) = part else { return None };
        full.push(name);
        let m = match std::fs::symlink_metadata(&full) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Some(PathState::Missing),
            Err(_) => return None,
        };
        if parts.peek().is_some() {
            if !m.is_dir() {
                return None;
            }
            continue;
        }
        // A directory's timestamps do not describe changes inside a submodule.
        if m.is_dir() {
            return None;
        }
        let mut h = Sha256::new();
        for n in [m.dev(), m.ino(), m.size(), u64::from(m.mode())] {
            h.update(n.to_le_bytes());
        }
        for n in [m.mtime(), m.mtime_nsec(), m.ctime(), m.ctime_nsec()] {
            h.update(n.to_le_bytes());
        }
        return Some(PathState::Present(format!("{:x}", h.finalize())));
    }
    None
}

/// Capture all dirty paths, including both sides of renames, immediately before delivery.
/// Missing/oversized git output is unknown, never a partial baseline used for attribution.
pub async fn tree_state(cwd: &Path, base: Option<&str>) -> Option<TreeState> {
    let base = base?;
    let tracked = run_git(cwd, &["diff", "--name-only", "--no-renames", "--relative", "-z", base, "--"]).await.ok()?;
    let others = run_git(cwd, &["ls-files", "--others", "--exclude-standard", "-z"]).await.ok()?;
    let mut names = BTreeSet::new();
    for path in tracked.split('\0').chain(others.split('\0')).filter(|p| !p.is_empty()) {
        names.insert(path.to_string());
        if names.len() > MAX_FINGERPRINT_PATHS {
            return None;
        }
    }
    let root = cwd.to_path_buf();
    let paths =
        tokio::task::spawn_blocking(move || names.into_iter().map(|p| (p.clone(), path_state(&root, &p))).collect())
            .await
            .ok()?;
    (head(cwd).await.as_deref() == Some(base)).then_some(TreeState { paths })
}

/// Filter HEAD-relative candidates against delivery, also noticing a dirty file restored to
/// HEAD or a pre-existing untracked file deleted (neither appears in the current git diff).
pub async fn files_since(cwd: &Path, base: Option<&str>, start: Option<TreeState>) -> Vec<ChangedFile> {
    let Some(start) = start else { return Vec::new() };
    let candidates = files(cwd, base).await;
    if candidates.len() > MAX_FINGERPRINT_PATHS {
        return Vec::new();
    }
    let root = cwd.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let mut out = BTreeMap::new();
        for f in candidates {
            let Some(now) = path_state(&root, &f.path) else { continue };
            let changed = match start.paths.get(&f.path) {
                Some(Some(before)) => before != &now,
                Some(None) => false,
                None => true,
            };
            if changed {
                out.insert(f.path.clone(), f);
            }
        }
        for (path, before) in start.paths {
            if out.contains_key(&path) {
                continue;
            }
            let (Some(before), Some(now)) = (before, path_state(&root, &path)) else { continue };
            if before != now {
                let status = match (&before, &now) {
                    (_, PathState::Missing) => "D",
                    (PathState::Missing, _) => "A",
                    _ => "M",
                };
                out.insert(path.clone(), ChangedFile { path, status: status.into(), added: None, removed: None });
            }
        }
        out.into_values().collect()
    })
    .await
    .unwrap_or_default()
}

async fn untracked(cwd: &Path) -> Vec<String> {
    run_git(cwd, &["ls-files", "--others", "--exclude-standard", "-z"])
        .await
        .map(|s| s.split('\0').filter(|p| !p.is_empty()).take(MAX_UNTRACKED).map(str::to_string).collect())
        .unwrap_or_default()
}

/// Whether git lists `path` (as given, no pattern) among the untracked, not ignored files.
async fn is_untracked(cwd: &Path, path: &str) -> Result<bool, String> {
    let spec = format!(":(literal){path}");
    run_git(cwd, &["ls-files", "--others", "--exclude-standard", "-z", "--", &spec])
        .await
        .map(|s| s.split('\0').any(|p| p == path))
}

/// git could not tell: say so, never an empty diff that reads as "unchanged".
fn unreadable(mut out: FileDiff, e: &str) -> FileDiff {
    out.text = format!("변경 내용을 읽지 못했어요: {e}");
    out
}

/// A regular file inside `root`, opened, and its size. No link is followed anywhere on the way
/// (every folder of `rel` is a real folder, the file is opened with `O_NOFOLLOW`), and what was
/// opened is checked again: a device or pipe swapped in meanwhile is not read (`O_NONBLOCK`
/// keeps a pipe from blocking the open).
fn regular_file(root: &Path, rel: &str) -> Option<(File, u64)> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut parts = Path::new(rel).components().peekable();
    let mut full = root.to_path_buf();
    while let Some(part) = parts.next() {
        let Component::Normal(name) = part else {
            return None;
        };
        full.push(name);
        if parts.peek().is_some() && !std::fs::symlink_metadata(&full).ok()?.file_type().is_dir() {
            return None;
        }
    }
    if full == root {
        return None;
    }
    let file =
        std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK).open(&full).ok()?;
    let m = file.metadata().ok()?;
    m.file_type().is_file().then_some((file, m.len()))
}

/// Lines of a small untracked regular file (unknown for links, big or unreadable files).
fn line_count(root: &Path, rel: &str) -> Option<u32> {
    let (file, len) = regular_file(root, rel).filter(|(_, len)| *len <= COUNT_LIMIT)?;
    let mut buf = Vec::with_capacity(len as usize);
    file.take(COUNT_LIMIT).read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);
    Some(text.lines().count() as u32)
}

/// Files changed since `base` (working tree included) and untracked files.
pub async fn files(cwd: &Path, base: Option<&str>) -> Vec<ChangedFile> {
    let Some(base) = base else { return Vec::new() };
    let mut out: Vec<ChangedFile> = Vec::new();
    if let Ok(status) = run_git(cwd, &["diff", "--name-status", "--relative", "-M", "-z", base]).await {
        let mut parts = status.split('\0').filter(|p| !p.is_empty());
        while let Some(code) = parts.next() {
            let letter: String = code.chars().take(1).collect();
            let path = if letter == "R" || letter == "C" {
                parts.next(); // old path
                parts.next()
            } else {
                parts.next()
            };
            if let Some(p) = path {
                out.push(ChangedFile { path: p.to_string(), status: letter, added: None, removed: None });
            }
        }
    }
    if let Ok(numstat) = run_git(cwd, &["diff", "--numstat", "--relative", "-M", base]).await {
        for line in numstat.lines() {
            let mut cols = line.splitn(3, '\t');
            let (Some(a), Some(d), Some(p)) = (cols.next(), cols.next(), cols.next()) else {
                continue;
            };
            // Renames read "old => new" or "dir/{old => new}"; match on the new name.
            let p = rename_target(p);
            if let Some(f) = out.iter_mut().find(|f| f.path == p) {
                f.added = a.parse().ok();
                f.removed = d.parse().ok();
            }
        }
    }
    let names = untracked(cwd).await;
    let root = cwd.to_path_buf();
    let counted =
        tokio::task::spawn_blocking(move || names.into_iter().map(|p| (line_count(&root, &p), p)).collect::<Vec<_>>())
            .await
            .unwrap_or_default();
    for (lines, p) in counted {
        out.push(ChangedFile { path: p, status: "?".into(), added: lines, removed: None });
    }
    out
}

fn rename_target(p: &str) -> String {
    if let (Some(open), Some(close)) = (p.find('{'), p.find('}'))
        && let Some((_, new)) = p[open + 1..close].split_once(" => ")
    {
        return format!("{}{}{}", &p[..open], new, &p[close + 1..]).replace("//", "/");
    }
    match p.split_once(" => ") {
        Some((_, new)) => new.to_string(),
        None => p.to_string(),
    }
}

#[cfg(test)]
tokio::task_local! {
    pub(super) static FINGERPRINT_CALLS: std::cell::Cell<usize>;
}

/// Fingerprint of the working tree: HEAD, what git reports as changed or untracked, and each of
/// those paths' size and modification time (links not followed, contents not read).
pub async fn fingerprint(cwd: &Path) -> Option<String> {
    #[cfg(test)]
    let _ = FINGERPRINT_CALLS.try_with(|calls| calls.set(calls.get() + 1));
    let head = head(cwd).await?;
    let status = run_git(cwd, &["status", "--porcelain=v1", "-z", "--untracked-files=all"]).await.ok()?;
    let root = cwd.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let mut h = Sha256::new();
        h.update(head.as_bytes());
        h.update(status.as_bytes());
        for entry in status.split('\0').filter(|e| e.len() > 3).take(MAX_FINGERPRINT_PATHS) {
            let rel = &entry[3..];
            if rel.split('/').any(|c| c == "..") {
                continue;
            }
            if let Ok(m) = std::fs::symlink_metadata(root.join(rel)) {
                h.update(m.len().to_le_bytes());
                let at = m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok());
                h.update(at.map_or(0, |d| d.as_nanos()).to_le_bytes());
            }
        }
        format!("{:x}", h.finalize())[..16].to_string()
    })
    .await
    .ok()
}

/// One file's diff against `base` (the whole file for an untracked one).
pub async fn diff(cwd: &Path, base: Option<&str>, path: &str) -> FileDiff {
    let mut out = FileDiff { path: path.to_string(), text: String::new(), binary: false, truncated: false };
    if path.split('/').any(|c| c == "..") || Path::new(path).is_absolute() {
        out.text = "이 경로는 작업 폴더 밖이라 보여 줄 수 없어요".into();
        return out;
    }
    // The path as given, never a pattern (`*.txt` is a file name here).
    let spec = format!(":(literal){path}");
    let tracked = match base {
        Some(b) => match run_git(cwd, &["diff", "-M", b, "--", &spec]).await {
            Ok(t) => t,
            Err(e) => return unreadable(out, &e),
        },
        None => String::new(),
    };
    let text = if tracked.is_empty() {
        // An untracked file (one git lists as such, not an ignored or unchanged one): read only
        // a regular file inside the folder, only as much as shown.
        match is_untracked(cwd, path).await {
            Ok(true) => {}
            Ok(false) => return out,
            Err(e) => return unreadable(out, &e),
        }
        let (root, rel) = (cwd.to_path_buf(), path.to_string());
        let read = tokio::task::spawn_blocking(move || {
            let (file, _) = regular_file(&root, &rel)?;
            let mut buf = Vec::new();
            file.take(DIFF_LIMIT as u64 + 1).read_to_end(&mut buf).ok()?;
            Some(buf)
        })
        .await
        .ok()
        .flatten();
        match read {
            Some(bytes) if bytes.contains(&0) => {
                out.binary = true;
                String::new()
            }
            Some(bytes) => String::from_utf8_lossy(&bytes).lines().map(|l| format!("+{l}\n")).collect(),
            None => String::new(),
        }
    } else {
        out.binary = tracked.contains("Binary files ");
        tracked
    };
    if text.len() > DIFF_LIMIT {
        let mut cut = DIFF_LIMIT;
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        out.text = text[..cut].to_string();
        out.truncated = true;
    } else {
        out.text = text;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn sh(dir: &Path, args: &[&str]) {
        let ok = Command::new("git").args(args).current_dir(dir).output().unwrap().status.success();
        assert!(ok, "git {args:?}");
    }

    #[test]
    fn rename_targets() {
        assert_eq!(rename_target("src/{a.rs => b.rs}"), "src/b.rs");
        assert_eq!(rename_target("a.txt => b.txt"), "b.txt");
        assert_eq!(rename_target("plain.rs"), "plain.rs");
    }

    #[test]
    fn path_states_detect_rewrites_without_reading_file_contents_or_following_links() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path();
        std::fs::write(p.join("same.txt"), "old").unwrap();
        let old_time = std::fs::metadata(p.join("same.txt")).unwrap().modified().unwrap();
        let before = path_state(p, "same.txt").unwrap();
        std::thread::sleep(Duration::from_millis(2));
        std::fs::write(p.join("same.txt"), "new").unwrap();
        File::options()
            .write(true)
            .open(p.join("same.txt"))
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(old_time))
            .unwrap();
        assert_ne!(Some(before), path_state(p, "same.txt"), "ctime detects a same-size edit with restored mtime");
        let stable = path_state(p, "same.txt");
        std::fs::read(p.join("same.txt")).unwrap();
        assert_eq!(stable, path_state(p, "same.txt"), "reading does not count as an edit");

        File::create(p.join("huge")).unwrap().set_len(8 * 1024 * 1024 * 1024).unwrap();
        assert!(matches!(path_state(p, "huge"), Some(PathState::Present(_))));
        std::os::unix::fs::symlink("/dev/zero", p.join("zero")).unwrap();
        assert!(matches!(path_state(p, "zero"), Some(PathState::Present(_))));
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret"), "not inspected").unwrap();
        std::os::unix::fs::symlink(outside.path(), p.join("linked")).unwrap();
        assert_eq!(path_state(p, "linked/secret"), None);
        assert_eq!(path_state(p, "../secret"), None);
        assert_eq!(path_state(p, "/dev/zero"), None);
        assert_eq!(path_state(p, "missing/child"), Some(PathState::Missing));
    }

    #[tokio::test]
    async fn delivery_state_handles_deletions_restores_renames_and_commits() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path();
        sh(p, &["init", "-q", "-b", "main"]);
        sh(p, &["config", "user.email", "t@example.com"]);
        sh(p, &["config", "user.name", "t"]);
        for name in ["restore.txt", "deleted.txt", "rename.txt", "leave.txt"] {
            std::fs::write(p.join(name), "original\n").unwrap();
        }
        sh(p, &["add", "."]);
        sh(p, &["commit", "-qm", "init"]);
        let base = head(p).await.unwrap();
        std::fs::write(p.join("restore.txt"), "earlier edit\n").unwrap();
        std::fs::write(p.join("leave.txt"), "earlier edit to commit\n").unwrap();
        std::fs::remove_file(p.join("deleted.txt")).unwrap();
        sh(p, &["mv", "rename.txt", "renamed.txt"]);
        for name in ["remove.txt", "keep.txt"] {
            std::fs::write(p.join(name), "untracked\n").unwrap();
        }
        let start = tree_state(p, Some(&base)).await.unwrap();
        // The baseline survives serialization without storing any file contents.
        let persisted = serde_json::to_string(&start).unwrap();
        assert!(!persisted.contains("earlier edit"));
        assert!(files_since(p, Some(&base), Some(start)).await.is_empty());
        std::fs::remove_file(p.join("remove.txt")).unwrap();
        std::fs::write(p.join("restore.txt"), "original\n").unwrap();
        std::fs::write(p.join("deleted.txt"), "original\n").unwrap();
        std::fs::write(p.join("new.txt"), "worker\n").unwrap();
        sh(p, &["add", "."]);
        sh(p, &["commit", "-qm", "worker"]);
        let files = files_since(p, Some(&base), Some(serde_json::from_str(&persisted).unwrap())).await;
        assert_eq!(
            files.iter().map(|f| (f.path.as_str(), f.status.as_str())).collect::<Vec<_>>(),
            [("deleted.txt", "A"), ("new.txt", "A"), ("remove.txt", "D"), ("restore.txt", "M")]
        );
        assert!(files_since(p, Some(&base), None).await.is_empty(), "legacy records have no known baseline");
    }

    #[tokio::test]
    async fn files_fingerprint_and_diff_follow_the_working_tree() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path();
        sh(p, &["init", "-q", "-b", "main"]);
        sh(p, &["config", "user.email", "t@example.com"]);
        sh(p, &["config", "user.name", "t"]);
        std::fs::write(p.join("a.txt"), "one\n").unwrap();
        sh(p, &["add", "."]);
        sh(p, &["commit", "-qm", "init"]);
        let base = head(p).await.unwrap();
        let before = fingerprint(p).await.unwrap();

        std::fs::write(p.join("a.txt"), "one\ntwo\n").unwrap();
        std::fs::write(p.join("new.txt"), "x\ny\n").unwrap();
        let f = files(p, Some(&base)).await;
        let a = f.iter().find(|f| f.path == "a.txt").unwrap();
        assert_eq!((a.status.as_str(), a.added, a.removed), ("M", Some(1), Some(0)));
        let n = f.iter().find(|f| f.path == "new.txt").unwrap();
        assert_eq!((n.status.as_str(), n.added), ("?", Some(2)));

        let after = fingerprint(p).await.unwrap();
        assert_ne!(before, after);
        assert_eq!(after, fingerprint(p).await.unwrap(), "stable while nothing changes");

        let dd = diff(p, Some(&base), "a.txt").await;
        assert!(dd.text.contains("+two"), "{}", dd.text);
        let nd = diff(p, Some(&base), "new.txt").await;
        assert_eq!(nd.text, "+x\n+y\n");
        assert!(diff(p, Some(&base), "../etc/passwd").await.text.contains("밖"));
        // Only files git lists: not an ignored one, not an unchanged tracked one, no pattern.
        std::fs::write(p.join(".gitignore"), ".env\n").unwrap();
        std::fs::write(p.join(".env"), "TOKEN=hidden\n").unwrap();
        assert!(diff(p, Some(&base), ".env").await.text.is_empty());
        sh(p, &["add", "a.txt"]);
        sh(p, &["commit", "-qm", "a"]);
        assert!(diff(p, Some(&head(p).await.unwrap()), "a.txt").await.text.is_empty());
        assert!(diff(p, Some(&base), "*.txt").await.text.is_empty());
        // A git error is said, not shown as "unchanged".
        let bad = diff(p, Some("0000000000000000000000000000000000000000"), "a.txt").await;
        assert!(bad.text.starts_with("변경 내용을 읽지 못했어요"), "{}", bad.text);
        assert!(files(&std::env::temp_dir(), None).await.is_empty());

        // Untracked links are listed but never followed: not to a secret outside, not to a device.
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret"), "TOPSECRET\n").unwrap();
        std::os::unix::fs::symlink(outside.path().join("secret"), p.join("leak.txt")).unwrap();
        std::os::unix::fs::symlink("/dev/zero", p.join("zero")).unwrap();
        assert!(Command::new("mkfifo").arg(p.join("pipe")).status().unwrap().success());
        // Nothing blocks on a link, device or pipe (a generous bound: a busy machine is slow).
        let guarded = async {
            let f = files(p, Some(&base)).await;
            for name in ["leak.txt", "zero"] {
                let e = f.iter().find(|x| x.path == name).expect(name);
                assert_eq!(e.added, None, "{name} is not read");
            }
            // git does not list pipes; if it did, it would not be read either.
            assert!(f.iter().filter(|x| x.path == "pipe").all(|x| x.added.is_none()));
            assert!(fingerprint(p).await.is_some());
            let leak = diff(p, Some(&base), "leak.txt").await;
            assert!(!leak.text.contains("TOPSECRET"), "{}", leak.text);
            // A linked folder on the way is not followed either (git lists only the link itself).
            std::os::unix::fs::symlink(outside.path(), p.join("linked")).unwrap();
            let through = diff(p, Some(&base), "linked/secret").await;
            assert!(!through.text.contains("TOPSECRET"), "{}", through.text);
            assert!(regular_file(p, "linked/secret").is_none());
            assert!(regular_file(p, "new.txt").is_some());
            assert!(regular_file(p, "./new.txt").is_none() && regular_file(p, "").is_none());
            assert!(diff(p, Some(&base), "zero").await.text.is_empty());
            assert!(diff(p, Some(&base), "pipe").await.text.is_empty());
        };
        tokio::time::timeout(Duration::from_secs(300), guarded).await.expect("nothing blocked");
    }
}
