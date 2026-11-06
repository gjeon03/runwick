//! What a worker changed, read from git in the worker's folder: files against the request's
//! start revision (committed or not, plus untracked files), a fingerprint of the working tree
//! (to tell whether files changed after a reported check) and one file's diff. Paths a worker
//! names (check logs) are never opened here.
//!
//! The folder is the worker's: links in it are never followed (a link to a device, a pipe or a
//! file outside would otherwise be read), file reads are capped and run off the async runtime,
//! and git gets a time limit. Non-git folders give no files and no fingerprint: unknown stays
//! unknown.

use crate::git::git;
use runwick_core::api::collab::{ChangedFile, FileDiff};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::Read;
use std::path::{Component, Path};
use std::time::Duration;

/// Diffs larger than this are cut (the result view is not an editor).
const DIFF_LIMIT: usize = 200 * 1024;
/// Untracked files whose lines are counted at most this large each.
const COUNT_LIMIT: u64 = 1024 * 1024;
/// Untracked files listed (and looked at) at most.
const MAX_UNTRACKED: usize = 2000;
/// Paths looked at for the fingerprint at most.
const MAX_FINGERPRINT_PATHS: usize = 20_000;
/// A git call that takes longer gives up (a lock, a huge repository): unknown, not wrong.
#[cfg(not(test))]
const GIT_TIMEOUT: Duration = Duration::from_secs(20);
/// Tests run hundreds of git calls in parallel on a busy machine; only stuck calls should fail.
#[cfg(test)]
const GIT_TIMEOUT: Duration = Duration::from_secs(120);

async fn run_git(cwd: &Path, args: &[&str]) -> Result<String, String> {
    match tokio::time::timeout(GIT_TIMEOUT, git(cwd, args)).await {
        Ok(r) => r,
        Err(_) => Err("git이 응답하지 않았어요".into()),
    }
}

pub async fn head(cwd: &Path) -> Option<String> {
    run_git(cwd, &["rev-parse", "HEAD"]).await.ok().filter(|s| !s.is_empty())
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
    if let Ok(status) = run_git(cwd, &["diff", "--name-status", "-M", "-z", base]).await {
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
    if let Ok(numstat) = run_git(cwd, &["diff", "--numstat", "-M", base]).await {
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

/// Fingerprint of the working tree: HEAD, what git reports as changed or untracked, and each of
/// those paths' size and modification time (links not followed, contents not read).
pub async fn fingerprint(cwd: &Path) -> Option<String> {
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
