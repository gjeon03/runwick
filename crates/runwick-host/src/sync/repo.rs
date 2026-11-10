//! Choosing a repository: clone it into a separate folder, read what it holds and what would
//! be new here (nothing is imported yet), then make it the working copy on connect.

use super::format::{self, content_of};
use super::git::{CLONE, Git, GitError, LOCAL, check_url};
use super::kinds::{self, Device, messages, projects, wiki};
use crate::store::Store;
use runwick_core::api::sync::{
    ConnectPreview, DEFAULT_BRANCH, FailKind, KindCount, RepoContent, RepoDevice, RepoRef, SyncCommit, SyncKind,
    display_name,
};
use rusqlite::{Connection, OptionalExtension};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// A cloned repository waiting for the person to connect it.
pub struct Preview {
    pub dir: PathBuf,
    pub repo: RepoRef,
    pub content: RepoContent,
}

/// Clone `url` into `dir` and read it. `dir` must not exist.
pub fn preview(
    git: &Git,
    store: &Store,
    home: &Path,
    device: &Device,
    url: &str,
    dir: &Path,
) -> Result<(ConnectPreview, Preview), GitError> {
    let url = check_url(url)?;
    if let Some(parent) = dir.parent() {
        std::fs::create_dir_all(parent).map_err(|e| GitError::other(format!("임시 폴더를 만들지 못했어요: {e}")))?;
    }
    let dir_s = dir.to_string_lossy().to_string();
    let cloned = git.ok(
        None,
        // No checkout yet: the tree is inspected before any file is written.
        &["clone", "-q", "--no-tags", "--no-checkout", "--", &url, &dir_s],
        CLONE,
    );
    if let Err(e) = cloned {
        let _ = std::fs::remove_dir_all(dir);
        return Err(e);
    }
    let read = || -> Result<(ConnectPreview, Preview), GitError> {
        let head = git.raw(Some(dir), &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"], None, LOCAL)?;
        let branch = if head.ok { head.text().strip_prefix("origin/").map(str::to_string) } else { None }
            .filter(|b| !b.is_empty())
            .unwrap_or_else(|| DEFAULT_BRANCH.to_string());
        let origin = git.rev(dir, &format!("refs/remotes/origin/{branch}"))?;
        if origin.is_none() {
            // Empty repository: start our branch there.
            git.ok(Some(dir), &["symbolic-ref", "HEAD", &format!("refs/heads/{branch}")], LOCAL)?;
        }
        let files = match &origin {
            Some(o) => git.files(dir, o)?,
            None => Vec::new(),
        };
        let manifest = match &origin {
            Some(o) => git.read_at(dir, o, &[format::MANIFEST.to_string()])?.remove(format::MANIFEST).flatten(),
            None => None,
        };
        let (mut content, m) = content_of(&files, manifest.as_deref());
        // Links and submodules never belong in a data repository; such a repository is refused.
        let odd: Vec<String> = match &origin {
            Some(o) => git.tree_problems(dir, o)?.into_iter().map(|(p, why)| format!("{p}: {why}")).collect(),
            None => Vec::new(),
        };
        if !odd.is_empty() {
            content = RepoContent::Other;
        }
        let repo = RepoRef { display: display_name(&url), url: url.clone(), branch };
        let mut out = ConnectPreview {
            id: Uuid::new_v4(),
            repo: repo.clone(),
            content,
            devices: Vec::new(),
            in_repo: Vec::new(),
            new_here: Vec::new(),
            unlinked_projects: Vec::new(),
            problems: Vec::new(),
        };
        if let Some(m) = m {
            // Computers with this computer's name, or sharing one, are told apart.
            let shared = |id: Uuid, name: &str| {
                name == device.name || m.devices.iter().any(|(other, e)| *other != id && e.name == name)
            };
            out.devices = m
                .devices
                .iter()
                .map(|(id, e)| RepoDevice {
                    host_id: *id,
                    name: if *id != device.host_id && shared(*id, &e.name) {
                        kinds::told_apart(&e.name, *id)
                    } else {
                        e.name.clone()
                    },
                    last_sync: e.last_sync.as_deref().and_then(format::parse_ts),
                    this: *id == device.host_id,
                })
                .collect();
        }
        for p in odd.iter().take(5) {
            out.problems.push(format!("{p} · Runwick 기록 저장소에는 일반 파일만 있어야 해요"));
        }
        if content == RepoContent::Runwick {
            git.ok(Some(dir), &["reset", "-q", "--hard"], LOCAL)?;
            survey(store, home, device, dir, &files, &mut out)
                .map_err(|e| GitError::other(format!("기록 저장소를 읽지 못했어요: {e}")))?;
        }
        Ok((out, Preview { dir: dir.to_path_buf(), repo, content }))
    };
    read().inspect_err(|_| {
        let _ = std::fs::remove_dir_all(dir);
    })
}

/// Count records per kind in the checked-out tree and how many this computer lacks.
fn survey(
    store: &Store,
    home: &Path,
    device: &Device,
    dir: &Path,
    files: &[String],
    out: &mut ConnectPreview,
) -> rusqlite::Result<()> {
    let mut total: BTreeMap<SyncKind, u64> = BTreeMap::new();
    let mut new: BTreeMap<SyncKind, u64> = BTreeMap::new();
    for f in files {
        let Some(kind) = kinds::kind_of(f) else {
            continue;
        };
        // Read outside the store lock, never through a link and never more than MAX_BLOB.
        let Some(bytes) = read_plain(&dir.join(f)) else {
            out.problems.push(format!("{f}: 읽지 못했어요"));
            continue;
        };
        let (n_total, n_new, unlinked) = store.with(|c| count(c, home, device, kind, f, &bytes))?;
        *total.entry(kind).or_default() += n_total;
        *new.entry(kind).or_default() += n_new;
        out.unlinked_projects.extend(unlinked);
    }
    let list =
        |m: &BTreeMap<SyncKind, u64>| m.iter().map(|(k, n)| KindCount { kind: *k, count: *n }).collect::<Vec<_>>();
    out.in_repo = list(&total);
    out.new_here = list(&new);
    out.unlinked_projects.sort();
    Ok(())
}

fn read_plain(p: &Path) -> Option<Vec<u8>> {
    let m = std::fs::symlink_metadata(p).ok()?;
    (m.is_file() && m.len() <= super::git::MAX_BLOB).then(|| std::fs::read(p).ok()).flatten()
}

/// Records in one file and how many this computer lacks (plus an unlinked project's name).
fn count(
    c: &Connection,
    home: &Path,
    device: &Device,
    kind: SyncKind,
    f: &str,
    bytes: &[u8],
) -> rusqlite::Result<(u64, u64, Option<String>)> {
    // Group metadata travels with projects, but is not another project or missing folder.
    if f == projects::GROUPS_PATH {
        return Ok((0, 0, None));
    }
    match kind {
        SyncKind::Env => {
            let vars = format::parse(bytes)
                .and_then(|v| v.get("vars").and_then(|m| m.as_object()).cloned())
                .unwrap_or_default();
            let mut st = c.prepare_cached("SELECT 1 FROM env_vars WHERE key = ?1")?;
            let mut missing = 0;
            for key in vars.keys() {
                if st.query_row([key], |_| Ok(())).optional()?.is_none() {
                    missing += 1;
                }
            }
            Ok((vars.len() as u64, missing, None))
        }
        SyncKind::Messages => {
            let ids = messages::ids(bytes);
            let mut st = c.prepare_cached("SELECT 1 FROM messages WHERE id = ?1")?;
            let mut missing = 0;
            for id in &ids {
                if st.query_row([id], |_| Ok(())).optional()?.is_none() {
                    missing += 1;
                }
            }
            Ok((ids.len() as u64, missing, None))
        }
        _ => {
            let missing = !known(c, kind, f)? as u64;
            let unlinked = match (kind, format::parse(bytes)) {
                (SyncKind::Projects, Some(v)) if !linked_here(c, home, device, &v)? => {
                    Some(kinds::opt_str(&v, "name").unwrap_or_else(|| "이름 없음".into()))
                }
                _ => None,
            };
            Ok((1, missing, unlinked))
        }
    }
}

/// Whether this computer already has the record in `f`.
fn known(c: &Connection, kind: SyncKind, f: &str) -> rusqlite::Result<bool> {
    let exists = |sql: &str, id: Uuid| c.query_row(sql, [id], |_| Ok(())).optional().map(|o| o.is_some());
    let id1 = kinds::uuid_at(f, 1);
    let id2 = kinds::uuid_at(f, 2);
    match kind {
        SyncKind::Projects => id1.map_or(Ok(false), |id| exists("SELECT 1 FROM projects WHERE id = ?1", id)),
        SyncKind::Sessions => id2.map_or(Ok(false), |id| exists("SELECT 1 FROM sessions WHERE id = ?1", id)),
        SyncKind::Pins => id1.map_or(Ok(false), |id| exists("SELECT 1 FROM pins WHERE id = ?1", id)),
        SyncKind::Roles => id1.map_or(Ok(false), |id| exists("SELECT 1 FROM roles WHERE id = ?1", id)),
        SyncKind::Collab => id2.map_or(Ok(false), |id| exists("SELECT 1 FROM collab_requests WHERE id = ?1", id)),
        SyncKind::Wiki => match wiki::parse_path(f) {
            Some((p, slug)) => c
                .query_row(
                    "SELECT 1 FROM wiki_pages WHERE project_id = ?1 AND slug = ?2 AND deleted = 0",
                    rusqlite::params![p, slug],
                    |_| Ok(()),
                )
                .optional()
                .map(|o| o.is_some()),
            None => Ok(false),
        },
        SyncKind::Timeline => {
            let Some(p) = id1 else { return Ok(false) };
            match f.rsplit('/').next() {
                Some("memo.json") => exists("SELECT 1 FROM project_notes WHERE project_id = ?1", p),
                Some(day) => c
                    .query_row(
                        "SELECT 1 FROM timeline_summaries WHERE project_id = ?1 AND day = ?2",
                        rusqlite::params![p, day.trim_end_matches(".json")],
                        |_| Ok(()),
                    )
                    .optional()
                    .map(|o| o.is_some()),
                None => Ok(false),
            }
        }
        SyncKind::Env | SyncKind::Prefs | SyncKind::Messages => Ok(true),
    }
}

fn linked_here(c: &Connection, home: &Path, device: &Device, v: &serde_json::Value) -> rusqlite::Result<bool> {
    let Some(id) = kinds::opt_uuid(v, "id") else {
        return Ok(true);
    };
    let root: Option<String> = c.query_row("SELECT root FROM projects WHERE id = ?1", [id], |r| r.get(0)).optional()?;
    if let Some(root) = root {
        return Ok(!projects::is_unlinked(home, &root));
    }
    // Not here yet: it will be linked when this computer's folder is recorded and still exists.
    let mine = v.get("roots").and_then(|r| r.get(device.host_id.to_string())).and_then(|e| kinds::opt_str(e, "path"));
    Ok(mine.is_some_and(|p| Path::new(&p).is_dir()))
}

/// Recent sync commits of the working copy, each with the computer that made it named for the
/// person (from the commit's host trailer, see `kinds::Names`).
pub fn history(git: &Git, wc: &Path, names: &kinds::Names) -> Result<Vec<SyncCommit>, GitError> {
    if git.rev(wc, "HEAD")?.is_none() {
        return Ok(Vec::new());
    }
    let format = format!(
        "--format=%h%x1f%an%x1f%aI%x1f%s%x1f%(trailers:key={},valueonly,separator=%x2c)%x1e",
        super::git::HOST_TRAILER
    );
    let text = git.text(wc, &["log", "-n", "40", &format])?;
    Ok(text
        .split('\u{1e}')
        .filter_map(|l| {
            let mut p = l.trim_start_matches('\n').split('\u{1f}');
            let (id, device, at, summary) = (p.next()?, p.next()?, p.next()?, p.next()?);
            let host = p.next().and_then(|h| h.split(',').next()).and_then(|h| Uuid::parse_str(h.trim()).ok());
            Some(SyncCommit {
                id: id.into(),
                at: format::parse_ts(at)?,
                device: names.label(host, device),
                summary: summary.into(),
            })
        })
        .collect())
}

/// Make a previewed clone the working copy.
pub fn install(p: &Preview, wc: &Path) -> Result<(), GitError> {
    if !matches!(p.content, RepoContent::Empty | RepoContent::Runwick) {
        return Err(GitError {
            kind: FailKind::Format,
            message: match p.content {
                RepoContent::Newer => "저장소가 더 새로운 Runwick 형식이에요. Runwick을 업데이트해 주세요".into(),
                _ => "Runwick 기록 저장소가 아닌 파일이 들어 있어요. 비어 있는 저장소나 Runwick 기록 저장소를 골라 주세요".into(),
            },
        });
    }
    if wc.exists() {
        // A previous working copy is only a cache of the remote and of these records.
        std::fs::remove_dir_all(wc).map_err(|e| GitError::other(format!("이전 작업 사본을 지우지 못했어요: {e}")))?;
    }
    if let Some(parent) = wc.parent() {
        std::fs::create_dir_all(parent).map_err(|e| GitError::other(e.to_string()))?;
    }
    std::fs::rename(&p.dir, wc).map_err(|e| GitError::other(format!("작업 사본을 옮기지 못했어요: {e}")))
}
