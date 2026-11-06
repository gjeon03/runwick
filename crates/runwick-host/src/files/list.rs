//! One folder of the file tree at a time (the GUI expands lazily), respecting `.gitignore`,
//! with git status marks when git answers quickly.

use super::paths::{join_rel, resolve_existing};
use runwick_core::api::files::*;
use runwick_core::rpc::RpcError;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Status entries parsed at most (a huge untracked tree should not stall listings).
const GIT_MAX_ENTRIES: usize = 50_000;
const GIT_TIMEOUT: Duration = Duration::from_millis(1500);
const GIT_FRESH: Duration = Duration::from_secs(3);

/// Changed paths of one repository, relative to its root.
#[derive(Debug, Default)]
pub struct GitMarks {
    files: HashMap<String, GitMark>,
    dirs: HashSet<String>,
}

impl GitMarks {
    pub fn parse(porcelain_z: &[u8]) -> Self {
        let mut marks = GitMarks::default();
        for rec in porcelain_z.split(|b| *b == 0).take(GIT_MAX_ENTRIES) {
            if rec.len() < 4 {
                continue;
            }
            let (x, y) = (rec[0], rec[1]);
            let Ok(path) = std::str::from_utf8(&rec[3..]) else {
                continue;
            };
            let mark = match (x, y) {
                (b'?', _) => GitMark::Untracked,
                (b'!', _) => continue,
                (b'U', _) | (_, b'U') | (b'A', b'A') | (b'D', b'D') => GitMark::Conflicted,
                (b'A', _) => GitMark::Added,
                (b'D', _) | (_, b'D') => GitMark::Deleted,
                (b'R', _) | (b'C', _) => GitMark::Renamed,
                _ => GitMark::Modified,
            };
            // git on macOS reports composed (NFC) names; disk names may be decomposed.
            let composed = compose_hangul(path.trim_end_matches('/'));
            let path = composed.as_ref();
            let mut cur = path;
            while let Some((parent, _)) = cur.rsplit_once('/') {
                if !marks.dirs.insert(parent.to_string()) {
                    break;
                }
                cur = parent;
            }
            marks.files.insert(path.to_string(), mark);
        }
        marks
    }

    fn mark(&self, repo_rel: &str, is_dir: bool) -> Option<GitMark> {
        let repo_rel = compose_hangul(repo_rel);
        let repo_rel = repo_rel.as_ref();
        if let Some(m) = self.files.get(repo_rel) {
            return Some(*m);
        }
        (is_dir && self.dirs.contains(repo_rel)).then_some(GitMark::Contains)
    }
}

/// Git status per repository with the time it was read.
type GitCache = Mutex<HashMap<PathBuf, (Instant, Arc<GitMarks>)>>;

/// Git status for the repository containing `root`, cached briefly. None when `root` is not in
/// a repository or git did not answer in time.
pub async fn git_marks(root: &Path) -> Option<(Arc<GitMarks>, String)> {
    static CACHE: OnceLock<GitCache> = OnceLock::new();
    let repo = crate::git::find_repo_root(root)?;
    let repo = std::fs::canonicalize(&repo).unwrap_or(repo);
    let prefix = super::paths::to_rel(&repo, root)?;
    let cache = CACHE.get_or_init(Default::default);
    if let Some((at, m)) = cache.lock().unwrap().get(&repo)
        && at.elapsed() < GIT_FRESH
    {
        return Some((m.clone(), prefix));
    }
    let args = ["status", "--porcelain=v1", "-z", "--untracked-files=all", "--no-renames"];
    // The limit covers reading the config and the status run: listing never waits longer.
    let run = async {
        let mut cmd = crate::git::safe_git(&repo, &args).await.ok()?;
        cmd.env("GIT_OPTIONAL_LOCKS", "0").output().await.ok()
    };
    let out = tokio::time::timeout(GIT_TIMEOUT, run).await.ok()??;
    if !out.status.success() {
        return None;
    }
    let marks = Arc::new(GitMarks::parse(&out.stdout));
    let mut map = cache.lock().unwrap();
    if map.len() > 64 {
        map.clear();
    }
    map.insert(repo, (Instant::now(), marks.clone()));
    Some((marks, prefix))
}

/// Entries of one folder. `git` is the repository status and the workspace's path inside it.
pub fn list(
    root: &Path,
    dir_rel: &str,
    show_hidden: bool,
    git: Option<(&GitMarks, &str)>,
) -> Result<DirListing, RpcError> {
    let dir = resolve_existing(root, dir_rel)?;
    if !dir.is_dir() {
        return Err(RpcError::invalid("폴더가 아니에요"));
    }
    let filter = !show_hidden;
    let walker = ignore::WalkBuilder::new(&dir)
        .max_depth(Some(1))
        .follow_links(false)
        .hidden(filter)
        .ignore(filter)
        .git_ignore(filter)
        .git_global(filter)
        .git_exclude(filter)
        .parents(true)
        .filter_entry(|e| e.file_name() != ".git")
        .build();
    let mut entries = Vec::new();
    for item in walker {
        let Ok(e) = item else { continue };
        if e.depth() == 0 {
            continue;
        }
        let Some(name) = e.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let symlink = e.path_is_symlink();
        // Follow links for the kind and size; a broken link shows as a file.
        let meta = std::fs::metadata(e.path()).ok();
        let is_dir = meta.as_ref().is_some_and(|m| m.is_dir());
        let path = join_rel(dir_rel.trim_matches('/'), &name);
        let git = git.and_then(|(marks, prefix)| marks.mark(&join_rel(prefix, &path), is_dir));
        entries.push(FileEntry {
            name,
            path,
            is_dir,
            size: meta.as_ref().filter(|m| m.is_file()).map(|m| m.len()),
            mtime: meta.as_ref().and_then(|m| m.modified().ok()).map(chrono::DateTime::<chrono::Utc>::from),
            git,
            symlink,
        });
    }
    entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| natural_key(&a.name).cmp(&natural_key(&b.name))));
    let more = entries.len().saturating_sub(LIST_LIMIT);
    entries.truncate(LIST_LIMIT);
    Ok(DirListing { dir: dir_rel.to_string(), entries, more, git_checked: git.is_some() })
}

/// Case-insensitive order with numbers compared by value (`2.md` before `10.md`).
fn natural_key(name: &str) -> Vec<(u8, u64, String)> {
    let mut out = Vec::new();
    let mut chars = name.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_ascii_digit() {
            let mut n = String::new();
            while let Some(&d) = chars.peek().filter(|d| d.is_ascii_digit()) {
                n.push(d);
                chars.next();
            }
            out.push((0, n.parse().unwrap_or(u64::MAX), n));
        } else {
            let mut s = String::new();
            while let Some(&d) = chars.peek().filter(|d| !d.is_ascii_digit()) {
                s.extend(d.to_lowercase());
                chars.next();
            }
            out.push((1, 0, s));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_folders_first_respecting_gitignore() {
        let d = tempfile::tempdir().unwrap();
        let r = std::fs::canonicalize(d.path()).unwrap();
        std::fs::create_dir_all(r.join(".git")).unwrap();
        std::fs::write(r.join(".gitignore"), "target/\n*.log\n").unwrap();
        std::fs::create_dir_all(r.join("target")).unwrap();
        std::fs::create_dir_all(r.join("src")).unwrap();
        std::fs::create_dir_all(r.join("docs")).unwrap();
        for f in ["b.md", "a.md", "10.md", "2.md", "x.log", ".env", "한글 문서.md"] {
            std::fs::write(r.join(f), "x").unwrap();
        }
        std::fs::write(r.join("docs/inner.md"), "x").unwrap();
        let l = list(&r, "", false, None).unwrap();
        let names: Vec<&str> = l.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["docs", "src", "2.md", "10.md", "a.md", "b.md", "한글 문서.md"]);
        assert!(l.entries[0].is_dir && l.entries[0].size.is_none());
        let all = list(&r, "", true, None).unwrap();
        let names: Vec<&str> = all.entries.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"target") && names.contains(&".env") && names.contains(&"x.log"));
        assert!(!names.contains(&".git"));
        let sub = list(&r, "docs", false, None).unwrap();
        assert_eq!(sub.entries[0].path, "docs/inner.md");
        assert!(list(&r, "../", false, None).is_err());
        assert!(list(&r, "a.md", false, None).is_err());
    }

    #[test]
    fn caps_huge_folders() {
        let d = tempfile::tempdir().unwrap();
        let r = std::fs::canonicalize(d.path()).unwrap();
        for i in 0..(LIST_LIMIT + 5) {
            std::fs::write(r.join(format!("f{i}.txt")), "").unwrap();
        }
        let l = list(&r, "", false, None).unwrap();
        assert_eq!(l.entries.len(), LIST_LIMIT);
        assert_eq!(l.more, 5);
    }

    #[test]
    fn parses_git_status_marks() {
        let raw = " M src/a.rs\0?? docs/new.md\0A  b.rs\0UU c.rs\0 D gone.rs\0?? 회의.md\0".as_bytes();
        let m = GitMarks::parse(raw);
        assert_eq!(m.mark("src/a.rs", false), Some(GitMark::Modified));
        assert_eq!(m.mark("docs/new.md", false), Some(GitMark::Untracked));
        assert_eq!(m.mark("b.rs", false), Some(GitMark::Added));
        assert_eq!(m.mark("c.rs", false), Some(GitMark::Conflicted));
        assert_eq!(m.mark("gone.rs", false), Some(GitMark::Deleted));
        assert_eq!(m.mark("src", true), Some(GitMark::Contains));
        assert_eq!(m.mark("src", false), None);
        assert_eq!(m.mark("lib", true), None);
        // A decomposed name on disk matches git's composed one.
        assert_eq!(m.mark("\u{1112}\u{116c}\u{110b}\u{1174}.md", false), Some(GitMark::Untracked));
    }
}
