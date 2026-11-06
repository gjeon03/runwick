//! Small git helpers. Cheap reads (branch) parse files directly; everything else shells out
//! to the user's `git` so behaviour matches their own command line — but never runs a program
//! the repository's own config names (see [`safe_git`]).

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

/// Folder containing `.git` (file or directory) at or above `dir`.
pub fn find_repo_root(dir: &Path) -> Option<PathBuf> {
    let mut cur = Some(dir);
    while let Some(d) = cur {
        if d.join(".git").exists() {
            return Some(d.to_path_buf());
        }
        cur = d.parent();
    }
    None
}

/// The git dir for a work tree root: `.git/` or the target of a `.git` file (worktrees).
pub fn git_dir(root: &Path) -> Option<PathBuf> {
    let dot = root.join(".git");
    if dot.is_dir() {
        return Some(dot);
    }
    let text = std::fs::read_to_string(&dot).ok()?;
    let target = text.lines().find_map(|l| l.strip_prefix("gitdir:"))?.trim();
    let p = PathBuf::from(target);
    Some(if p.is_absolute() { p } else { root.join(p) })
}

/// Current branch name without running git. None for detached HEAD or non-git folders.
pub fn read_branch(dir: &Path) -> Option<String> {
    let root = find_repo_root(dir)?;
    let head = std::fs::read_to_string(git_dir(&root)?.join("HEAD")).ok()?;
    head.trim().strip_prefix("ref: refs/heads/").map(|s| s.to_string())
}

/// Main repository folder for a linked worktree (the folder that owns the common git dir).
pub fn main_worktree_root(root: &Path) -> Option<PathBuf> {
    let gd = git_dir(root)?;
    let common = std::fs::read_to_string(gd.join("commondir")).ok();
    let common_dir = match common {
        Some(c) => {
            let p = PathBuf::from(c.trim());
            if p.is_absolute() { p } else { gd.join(p) }
        }
        None => gd,
    };
    let common_dir = common_dir.canonicalize().ok()?;
    if common_dir.file_name().is_some_and(|n| n == ".git") {
        common_dir.parent().map(|p| p.to_path_buf())
    } else {
        None
    }
}

pub fn origin_url(root: &Path) -> Option<String> {
    let cfg_dir = main_worktree_root(root).and_then(|m| git_dir(&m)).or_else(|| git_dir(root))?;
    let cfg = std::fs::read_to_string(cfg_dir.join("config")).ok()?;
    let mut in_origin = false;
    for line in cfg.lines() {
        let l = line.trim();
        if l.starts_with('[') {
            in_origin = l == "[remote \"origin\"]";
            continue;
        }
        if in_origin && let Some(v) = l.strip_prefix("url") {
            return Some(runwick_core::api::sync::without_credentials(v.trim_start_matches([' ', '=']).trim()));
        }
    }
    None
}

/// Settings that stop git from running programs a repository's config names. A downloaded
/// project can carry a `.git/config` with `core.fsmonitor`, hooks or signature programs; the
/// host must not run them because a folder was opened or listed.
const HARDEN: &[&str] = &[
    "-c",
    "core.fsmonitor=false",
    "-c",
    "core.hooksPath=/dev/null",
    "-c",
    "protocol.ext.allow=never",
    "-c",
    "log.showSignature=false",
    "-c",
    "merge.verifySignatures=false",
    "-c",
    "commit.gpgSign=false",
    // Attribute files that name drivers: only the repository's own (whose drivers are switched
    // off by name) and the person's global file are read, never another one set by config.
    "-c",
    "core.attributesFile=/dev/null",
];

/// Subcommands that can run diff drivers: they get `--no-ext-diff --no-textconv`.
const DIFFS: &[&str] = &["diff", "log", "show"];

/// Reading the repository's config takes longer than this: give up. A backstop for callers
/// without a limit of their own (pipes are refused before git reads anything); as generous as
/// the longest caller limit, so a busy computer is not mistaken for a hostile repository.
#[cfg(not(test))]
const CONFIG_TIMEOUT: Duration = Duration::from_secs(20);
/// Tests share the computer with hundreds of other processes (the whole suite, other builds):
/// a single git start can stall for seconds there, which is not what these tests check.
#[cfg(test)]
const CONFIG_TIMEOUT: Duration = Duration::from_secs(120);

/// The one way the host runs `git` in a person's folder: `git <args>` in `dir` with
/// [`HARDEN`], `--no-ext-diff --no-textconv` for diffs, and every filter or merge driver the
/// repository's own config defines switched off (read first; the person's global drivers such
/// as git-lfs stay). Fails when that config cannot be read, rather than run git unprotected.
/// The sync repository has its own runner (`sync::git`).
pub async fn safe_git(dir: &Path, args: &[&str]) -> Result<tokio::process::Command, String> {
    // The drivers are read before the command runs, so every file git reads its config from
    // must give the same text each time: no pipes, no links leading out of the git dir.
    let stamps = check_config_files(dir)?;
    let drivers = match cached_drivers(&stamps) {
        Some(d) => d,
        None => read_drivers(dir, &stamps).await?,
    };
    let mut cmd = base(dir);
    for d in &drivers {
        cmd.args(["-c", d]);
    }
    if let Some((sub, rest)) = args.split_first() {
        cmd.arg(sub);
        if DIFFS.contains(sub) {
            cmd.args(["--no-ext-diff", "--no-textconv"]);
        }
        cmd.args(rest);
    }
    Ok(cmd)
}

/// A config file as found: absent, or which file it is and when it last changed (`ctime`
/// moves with every write or rename and cannot be set back by hand).
type Stamp = (PathBuf, Option<(u64, u64, u64, i64, i64)>);
/// The repository's common git dir and the stamps of every config file it reads.
type ConfigStamps = Option<(PathBuf, Vec<Stamp>)>;

fn stamp(path: &Path, meta: Option<&std::fs::Metadata>) -> Stamp {
    use std::os::unix::fs::MetadataExt;
    let id = meta.map(|m| (m.dev(), m.ino(), m.size(), m.ctime(), m.ctime_nsec()));
    (path.to_path_buf(), id)
}

/// Driver settings already read, per common git dir, with the config files they came from.
type DriverCache = std::sync::Mutex<std::collections::HashMap<PathBuf, (Vec<Stamp>, Vec<String>)>>;

/// Driver settings already read for these exact config files (saves a git run per call).
fn drivers_cache() -> &'static DriverCache {
    static CACHE: std::sync::OnceLock<DriverCache> = std::sync::OnceLock::new();
    CACHE.get_or_init(Default::default)
}

fn cached_drivers(stamps: &ConfigStamps) -> Option<Vec<String>> {
    let (key, st) = stamps.as_ref()?;
    let cache = drivers_cache().lock().ok()?;
    cache.get(key).filter(|(s, _)| s == st).map(|(_, d)| d.clone())
}

/// Ask git which drivers the repository's config defines. The config files must be the same
/// ones, unchanged, after the read as before it; otherwise git is not run.
async fn read_drivers(dir: &Path, stamps: &ConfigStamps) -> Result<Vec<String>, String> {
    let list = base(dir).args(["config", "--show-scope", "--includes", "-z", "--name-only", "--list"]).output();
    let drivers = match tokio::time::timeout(CONFIG_TIMEOUT, list).await {
        Ok(Ok(out)) if out.status.success() => repo_drivers_off(&String::from_utf8_lossy(&out.stdout)),
        // No readable config: the command itself would fail the same way.
        Ok(Ok(out)) => return Err(String::from_utf8_lossy(&out.stderr).trim().to_string()),
        Ok(Err(e)) => return Err(format!("git을 실행하지 못했어요: {e}")),
        Err(_) => {
            return Err("저장소 설정을 읽는 데 너무 오래 걸려 git을 실행하지 않았어요".into());
        }
    };
    // A file rewritten or swapped (for a pipe, say) while git read it: not what was checked.
    if &check_config_files(dir)? != stamps {
        return Err("저장소 설정이 읽는 동안 바뀌어 git을 실행하지 않았어요. 잠시 뒤 다시 시도해 주세요".into());
    }
    if let Some((key, st)) = stamps
        && let Ok(mut cache) = drivers_cache().lock()
    {
        if cache.len() > 64 {
            cache.clear();
        }
        cache.insert(key.clone(), (st.clone(), drivers.clone()));
    }
    Ok(drivers)
}

/// Includes followed at most this deep (git's own limit is 10).
const MAX_INCLUDE_DEPTH: usize = 10;
/// Config files larger than this are not read for includes (and refused).
const MAX_CONFIG_BYTES: u64 = 1024 * 1024;

const NOT_REGULAR: &str =
    "저장소 설정이 일반 파일이 아닌 곳(파이프나 git 폴더 밖을 가리키는 링크)을 읽게 되어 있어 git을 실행하지 않았어요";

/// The repository's own config files — `config` of the common git dir, `config.worktree`
/// files and `info/attributes` — and every `include.path` / `includeIf.*.path` they lead to
/// (relative to the including file's folder, `~/` from the home folder): each must be absent or
/// a regular file, and a link must stay inside the git dir. Folders that are not in a
/// repository have no such files. Returns what was found, to tell later whether it changed.
fn check_config_files(dir: &Path) -> Result<ConfigStamps, String> {
    let Some(root) = find_repo_root(dir) else {
        return Ok(None);
    };
    let Some(gd) = git_dir(&root) else {
        return Ok(None);
    };
    let common = std::fs::read_to_string(gd.join("commondir"))
        .ok()
        .map(|c| {
            let p = PathBuf::from(c.trim());
            if p.is_absolute() { p } else { gd.join(p) }
        })
        .unwrap_or_else(|| gd.clone());
    let common = std::fs::canonicalize(&common).map_err(|e| format!("{NOT_REGULAR}: {e}"))?;
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut seen = std::collections::HashSet::new();
    let mut stamps = Vec::new();
    for f in [
        common.join("config"),
        common.join("config.worktree"),
        gd.join("config.worktree"),
        common.join("info").join("attributes"),
    ] {
        let follow = f.file_name().is_some_and(|n| n != "attributes");
        check_file(&f, &common, home.as_deref(), follow, 0, &mut seen, &mut stamps)?;
    }
    Ok(Some((common, stamps)))
}

/// `path` is absent or a regular file (a link only into `git_root`); with `follow`, the
/// includes it names are checked too.
fn check_file(
    path: &Path,
    git_root: &Path,
    home: Option<&Path>,
    follow: bool,
    depth: usize,
    seen: &mut std::collections::HashSet<PathBuf>,
    stamps: &mut Vec<Stamp>,
) -> Result<(), String> {
    let refuse = || Err(format!("{NOT_REGULAR}: {}", path.display()));
    let meta = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        // git skips an include that is not there (one created later changes the stamps).
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            stamps.push(stamp(path, None));
            return Ok(());
        }
        Err(_) => return refuse(),
    };
    stamps.push(stamp(path, Some(&meta)));
    let real = if meta.file_type().is_symlink() {
        match std::fs::canonicalize(path) {
            Ok(t) if t.starts_with(git_root) => t,
            _ => return refuse(),
        }
    } else {
        path.to_path_buf()
    };
    // Checked without opening it: opening a pipe would wait for a writer.
    match std::fs::metadata(&real) {
        Ok(m) if m.is_file() && m.len() <= MAX_CONFIG_BYTES => {
            if real != path {
                stamps.push(stamp(&real, Some(&m)));
            }
        }
        _ => return refuse(),
    }
    if !follow || !seen.insert(real.clone()) {
        return Ok(());
    }
    if depth >= MAX_INCLUDE_DEPTH {
        return refuse();
    }
    let text = std::fs::read_to_string(&real).unwrap_or_default();
    let base = path.parent().unwrap_or(Path::new("/"));
    for inc in include_paths(&text) {
        let Some(target) = resolve_include(base, &inc, home) else {
            return Err(format!("{NOT_REGULAR}: {inc}"));
        };
        check_file(&target, git_root, home, true, depth + 1, seen, stamps)?;
    }
    Ok(())
}

/// The `path` values of `[include]` and `[includeIf "…"]` sections of a config file.
fn include_paths(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_include = false;
    for raw in text.lines() {
        let mut line = raw.trim();
        if let Some(head) = line.strip_prefix('[') {
            let Some(end) = head.find(']') else { continue };
            let name = head[..end].split(|c: char| c.is_whitespace() || c == '"' || c == '.').next().unwrap_or("");
            in_include = name.eq_ignore_ascii_case("include") || name.eq_ignore_ascii_case("includeif");
            // `[include] path = x` on one line is valid too.
            line = head[end + 1..].trim();
        }
        if !in_include {
            continue;
        }
        if let Some((key, value)) = line.split_once('=')
            && key.trim().eq_ignore_ascii_case("path")
        {
            out.push(config_value(value));
        }
    }
    out
}

/// A config value as git reads it: quotes removed, `\\` escapes applied, a `;` or `#` outside
/// quotes starts a comment.
fn config_value(raw: &str) -> String {
    let mut out = String::new();
    let mut quoted = false;
    let mut chars = raw.trim().chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => quoted = !quoted,
            '\\' => match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some(o) => out.push(o),
                None => {}
            },
            ';' | '#' if !quoted => break,
            c => out.push(c),
        }
    }
    if quoted { out } else { out.trim_end().to_string() }
}

/// Where git reads an include from: `~/` from the home folder, relative paths from the
/// including file's folder. None for forms that cannot be checked here (`~user/`).
fn resolve_include(base: &Path, p: &str, home: Option<&Path>) -> Option<PathBuf> {
    if let Some(rest) = p.strip_prefix("~/") {
        return home.map(|h| h.join(rest));
    }
    if p.starts_with('~') || p.is_empty() {
        return None;
    }
    let path = PathBuf::from(p);
    Some(if path.is_absolute() { path } else { base.join(path) })
}

fn base(dir: &Path) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new("git");
    cmd.args(HARDEN)
        .current_dir(dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        // An editor would be another program named by config (core.editor).
        .env("GIT_EDITOR", "true")
        .stdin(Stdio::null())
        .kill_on_drop(true);
    cmd
}

/// `-c` settings that switch off the filter and merge drivers defined in the repository's own
/// config (scopes `local` and `worktree`, includes followed), from `git config --show-scope -z
/// --name-only --list` output. A filter becomes a no-op; a merge driver becomes `false`, so the
/// file is reported as a conflict instead of being merged by an unknown program.
fn repo_drivers_off(listing: &str) -> Vec<String> {
    let mut parts = listing.split('\0');
    let mut out = Vec::new();
    while let (Some(scope), Some(key)) = (parts.next(), parts.next()) {
        if !matches!(scope, "local" | "worktree") || key.contains('=') {
            continue;
        }
        let Some((section, rest)) = key.split_once('.') else {
            continue;
        };
        let Some((sub, name)) = rest.rsplit_once('.') else {
            continue;
        };
        match (section.to_ascii_lowercase().as_str(), name.to_ascii_lowercase().as_str()) {
            ("filter", "clean" | "smudge" | "process") => {
                out.push(format!("{key}="));
                out.push(format!("filter.{sub}.required=false"));
            }
            ("merge", "driver") => out.push(format!("{key}=false")),
            _ => {}
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Run git (through [`safe_git`]) and return trimmed stdout, or the trimmed stderr as the error.
pub async fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = safe_git(dir, args).await?.output().await.map_err(|e| format!("git을 실행하지 못했어요: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_branch_and_origin_from_files() {
        let d = tempfile::tempdir().unwrap();
        let g = d.path().join(".git");
        std::fs::create_dir_all(&g).unwrap();
        std::fs::write(g.join("HEAD"), "ref: refs/heads/fix/login\n").unwrap();
        std::fs::write(
            g.join("config"),
            "[core]\n\tbare = false\n[remote \"origin\"]\n\turl = git@github.com:me/app.git\n",
        )
        .unwrap();
        let sub = d.path().join("src");
        std::fs::create_dir_all(&sub).unwrap();
        assert_eq!(read_branch(&sub).as_deref(), Some("fix/login"));
        assert_eq!(origin_url(d.path()).as_deref(), Some("git@github.com:me/app.git"));
        assert!(read_branch(&std::env::temp_dir().join("definitely-not-a-repo-xyz")).is_none());
    }

    #[test]
    fn only_the_repositorys_own_drivers_are_switched_off() {
        let listing = "global\0filter.lfs.clean\0local\0filter.Evil.Name.clean\0local\0core.bare\0\
                       worktree\0merge.drv.driver\0local\0filter.inc.smudge\0local\0diff.x.textconv\0";
        assert_eq!(
            repo_drivers_off(listing),
            vec![
                "filter.Evil.Name.clean=",
                "filter.Evil.Name.required=false",
                "filter.inc.required=false",
                "filter.inc.smudge=",
                "merge.drv.driver=false",
            ]
        );
        assert!(repo_drivers_off("").is_empty());
    }

    /// A repository whose own config and hooks run `mark` (writing to the log): fsmonitor,
    /// hooks, a clean/smudge filter, a merge driver, textconv, an external diff, signature
    /// checks and an editor. Armed after its first commit. Returns (folder, repo, log).
    fn hostile_repo() -> (tempfile::TempDir, PathBuf, PathBuf) {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(d.path()).unwrap();
        let repo = base.join("repo");
        let log = base.join("ran.log");
        std::fs::create_dir_all(&repo).unwrap();
        let script = |name: &str, body: &str| {
            let p = base.join(name);
            std::fs::write(&p, format!("#!/bin/sh\necho \"{name} $*\" >> '{}'\n{body}", log.display())).unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
            p.display().to_string()
        };
        let mark = script("mark", "exit 0\n");
        let pass = script("pass", "exec cat\n");
        sh(&repo, &["init", "-q", "-b", "main"]);
        sh(&repo, &["config", "user.email", "t@example.com"]);
        sh(&repo, &["config", "user.name", "t"]);
        std::fs::write(repo.join("a.txt"), "one\n").unwrap();
        std::fs::write(repo.join(".gitattributes"), "*.txt filter=evil diff=evil merge=evil\n").unwrap();
        sh(&repo, &["add", "."]);
        sh(&repo, &["commit", "-qm", "first"]);
        for (k, v) in [
            ("core.fsmonitor", format!("{mark} fsmonitor")),
            ("filter.evil.clean", format!("{pass} clean")),
            ("filter.evil.smudge", format!("{pass} smudge")),
            ("filter.evil.required", "true".into()),
            ("merge.evil.driver", format!("{mark} merge %O %A %B")),
            ("diff.evil.textconv", format!("{mark} textconv")),
            ("diff.evil.command", format!("{mark} diffcmd")),
            ("diff.external", format!("{mark} external")),
            ("log.showSignature", "true".into()),
            ("gpg.program", format!("{mark} gpg")),
            ("core.editor", format!("{mark} editor")),
        ] {
            sh(&repo, &["config", k, &v]);
        }
        for hook in ["post-checkout", "pre-commit", "post-merge", "post-commit", "reference-transaction"] {
            let p = repo.join(".git/hooks").join(hook);
            std::fs::write(&p, format!("#!/bin/sh\n'{mark}' hook-{hook}\n")).unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        (d, repo, log)
    }

    /// Plain git for the test's own setup (unprotected on purpose where noted).
    fn sh(dir: &Path, args: &[&str]) {
        let st = std::process::Command::new("git").args(args).current_dir(dir).output().unwrap();
        assert!(st.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&st.stderr));
    }

    /// Setup commits in the armed repository without running anything it names.
    fn quiet_commit(dir: &Path, msg: &str) {
        sh(
            dir,
            &[
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "core.fsmonitor=false",
                "-c",
                "filter.evil.clean=",
                "-c",
                "filter.evil.smudge=",
                "-c",
                "filter.evil.required=false",
                "commit",
                "-qam",
                msg,
            ],
        );
    }

    fn ran(log: &Path) -> String {
        std::fs::read_to_string(log).unwrap_or_default()
    }

    #[tokio::test]
    async fn listing_splitting_and_diffs_never_run_what_the_repository_names() {
        let (_d, repo, log) = hostile_repo();
        // The fixture is armed: plain git runs it.
        std::fs::write(repo.join("a.txt"), "one\ntwo\n").unwrap();
        let _ = std::process::Command::new("git").args(["status"]).current_dir(&repo).output();
        let _ = std::process::Command::new("git").args(["diff", "HEAD"]).current_dir(&repo).output();
        let control = repo.parent().unwrap().join("control");
        let control_s = control.display().to_string();
        let _ = std::process::Command::new("git")
            .args(["worktree", "add", "-q", "-b", "control", &control_s])
            .current_dir(&repo)
            .output();
        let armed = ran(&log);
        assert!(armed.contains("fsmonitor") && armed.contains("clean"), "{armed}");
        assert!(armed.contains("hook-post-checkout") && armed.contains("smudge"), "{armed}");
        assert!(armed.contains("textconv") || armed.contains("external") || armed.contains("diffcmd"), "{armed}");
        let _ = std::fs::remove_file(&log);

        // files.list (git status for the tree's marks), as a window asks for it.
        let store = crate::store::Store::open_in_memory().unwrap();
        let ws = crate::projects::open_folder(&store, repo.to_str().unwrap()).unwrap().workspace.id;
        let info = runwick_core::api::host::HostInfo {
            host_id: uuid::Uuid::new_v4(),
            device_name: "test".into(),
            version: "0".into(),
            os: "test".into(),
            arch: "test".into(),
            home: "/tmp".into(),
            started_at: runwick_core::now(),
            pid: 0,
            client_id: 0,
        };
        let host = crate::host::Host::new(info, store);
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let client = std::sync::Arc::new(crate::host::Client::new(1, tx));
        client.interactive.store(true, std::sync::atomic::Ordering::Relaxed);
        let ctx = crate::host::Ctx { host: host.clone(), client };
        let params = serde_json::json!({ "workspace_id": ws, "dir": "", "show_hidden": false });
        let listed = crate::router::build().dispatch(ctx, "files.list", params).await.unwrap();
        assert!(listed.to_string().contains("a.txt"), "{listed}");
        assert_eq!(ran(&log), "", "files.list");

        // worktree.create: status, worktree add (checkout: post-checkout hook, smudge), apply.
        let target = repo.parent().unwrap().join("wt");
        let created = crate::worktree::create(
            &host,
            runwick_core::api::worktree::Create {
                workspace_id: ws,
                name: "hostile".into(),
                branch: "feat/hostile".into(),
                base: None,
                use_existing_branch: false,
                path: Some(target.display().to_string()),
                carry_changes: true,
                tool: None,
                cols: 80,
                rows: 24,
            },
        )
        .await
        .unwrap();
        assert!(created.workspace.is_some(), "{:?}", created.steps);
        assert_eq!(std::fs::read_to_string(target.join("a.txt")).unwrap(), "one\ntwo\n");
        assert_eq!(ran(&log), "", "worktree.create");

        // What collaboration, the timeline and integration read (all through `git`).
        for args in [
            &["rev-parse", "HEAD"][..],
            &["ls-files", "--others", "--exclude-standard", "-z"],
            &["diff", "HEAD", "--numstat"],
            &["diff", "HEAD", "--", "a.txt"],
            &["diff", "HEAD", "--stat", "--patch"],
            &["log", "--branches", "-5", "--format=%H%x1f%s"],
            &["status", "--porcelain", "-z"],
        ] {
            git(&target, args).await.unwrap();
            git(&repo, args).await.unwrap();
        }
        assert_eq!(ran(&log), "", "diff, log, status");

        // A merge where both sides changed the file: the repository's merge driver and
        // post-merge hook do not run; the file is reported as a conflict instead.
        std::fs::write(target.join("a.txt"), "theirs\n").unwrap();
        quiet_commit(&target, "theirs");
        std::fs::write(repo.join("a.txt"), "ours\n").unwrap();
        quiet_commit(&repo, "ours");
        let _ = std::fs::remove_file(&log);
        let merged = git(&repo, &["merge", "--no-ff", "--no-commit", "feat/hostile"]).await;
        assert!(merged.is_err(), "conflict expected");
        assert_eq!(ran(&log), "", "merge");
    }

    #[test]
    fn includes_are_read_like_git_reads_them() {
        let cfg = "[core]\n\tbare = false\n[include]\n\tpath = inc.cfg ; note\n\
                   [includeIf \"gitdir:~/w/\"]\n\tPath = \"~/x y.cfg\"\n[Include] path = /abs.cfg\n\
                   [user]\n\tpath = not-an-include\n";
        assert_eq!(include_paths(cfg), ["inc.cfg", "~/x y.cfg", "/abs.cfg"]);
        let base = Path::new("/r/.git");
        let home = Path::new("/home/me");
        assert_eq!(resolve_include(base, "inc.cfg", Some(home)), Some(PathBuf::from("/r/.git/inc.cfg")));
        assert_eq!(resolve_include(base, "~/x.cfg", Some(home)), Some(PathBuf::from("/home/me/x.cfg")));
        assert_eq!(resolve_include(base, "/abs.cfg", None), Some(PathBuf::from("/abs.cfg")));
        assert_eq!(resolve_include(base, "~other/x.cfg", Some(home)), None);
    }

    /// A repository whose config includes `name` (written straight into `.git/config`, so the
    /// test's own git never reads it).
    fn repo_including(name: &str) -> (tempfile::TempDir, PathBuf) {
        let d = tempfile::tempdir().unwrap();
        let repo = std::fs::canonicalize(d.path()).unwrap().join("w");
        std::fs::create_dir_all(&repo).unwrap();
        sh(&repo, &["init", "-q", "-b", "main"]);
        std::fs::write(repo.join("a.txt"), "hello\n").unwrap();
        let mut cfg = std::fs::read_to_string(repo.join(".git/config")).unwrap();
        cfg.push_str(&format!("[include]\n\tpath = {name}\n"));
        std::fs::write(repo.join(".git/config"), cfg).unwrap();
        (d, repo)
    }

    #[tokio::test]
    async fn a_pipe_or_a_link_out_of_the_git_dir_in_the_config_is_refused() {
        // The critic's case: an include that is a pipe could give the driver check one text
        // and the real run another. Refused before git reads it (no waiting on the pipe).
        let (_d, repo) = repo_including("inc.fifo");
        let fifo = repo.join(".git/inc.fifo");
        let st = std::process::Command::new("mkfifo").arg(&fifo).status().unwrap();
        assert!(st.success());
        // A guard against hanging on the pipe, not a speed check (tests run under heavy load).
        let started = std::time::Instant::now();
        let err = tokio::time::timeout(Duration::from_secs(150), git(&repo, &["status", "--porcelain"]))
            .await
            .expect("refused without waiting on the pipe")
            .unwrap_err();
        assert!(err.contains("일반 파일이 아닌"), "{err}");
        assert!(started.elapsed() < CONFIG_TIMEOUT, "refused before git read the config");

        // A pipe further down the include chain is found too.
        let (_d2, repo2) = repo_including("first.cfg");
        std::fs::write(repo2.join(".git/first.cfg"), "[include]\n\tpath = second.fifo\n").unwrap();
        let st = std::process::Command::new("mkfifo").arg(repo2.join(".git/second.fifo")).status().unwrap();
        assert!(st.success());
        let nested = tokio::time::timeout(Duration::from_secs(150), git(&repo2, &["status"])).await;
        assert!(nested.expect("refused without waiting on the pipe").is_err());

        // A link to a file outside the git dir is refused; one inside it is fine.
        let (_d3, repo3) = repo_including("outside.cfg");
        let outside = repo3.parent().unwrap().join("elsewhere.cfg");
        std::fs::write(&outside, "[core]\n").unwrap();
        std::os::unix::fs::symlink(&outside, repo3.join(".git/outside.cfg")).unwrap();
        assert!(git(&repo3, &["status"]).await.is_err());
        std::fs::remove_file(repo3.join(".git/outside.cfg")).unwrap();
        std::fs::write(repo3.join(".git/inside.cfg"), "[core]\n").unwrap();
        std::os::unix::fs::symlink(repo3.join(".git/inside.cfg"), repo3.join(".git/outside.cfg")).unwrap();
        git(&repo3, &["status", "--porcelain"]).await.unwrap();

        // An include that is not there is skipped, as git skips it.
        let (_d4, repo4) = repo_including("missing.cfg");
        git(&repo4, &["status", "--porcelain"]).await.unwrap();
    }

    #[tokio::test]
    async fn a_driver_added_after_a_cached_read_is_still_switched_off() {
        use std::os::unix::fs::PermissionsExt;
        let (d, repo) = repo_including("missing.cfg");
        let log = d.path().join("ran.log");
        let mark = d.path().join("mark");
        std::fs::write(&mark, format!("#!/bin/sh\necho ran >> '{}'\nexec cat\n", log.display())).unwrap();
        std::fs::set_permissions(&mark, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(repo.join(".gitattributes"), "*.txt filter=late\n").unwrap();
        sh(&repo, &["add", "."]);
        // Read (and remembered) while the config defines no driver.
        git(&repo, &["status", "--porcelain"]).await.unwrap();
        // Then the repository gains one, and the file must be cleaned again.
        let mut cfg = std::fs::read_to_string(repo.join(".git/config")).unwrap();
        cfg.push_str(&format!("[filter \"late\"]\n\tclean = {}\n", mark.display()));
        std::fs::write(repo.join(".git/config"), cfg).unwrap();
        std::fs::write(repo.join("a.txt"), "changed\n").unwrap();
        git(&repo, &["status", "--porcelain"]).await.unwrap();
        git(&repo, &["diff", "--stat"]).await.unwrap();
        assert_eq!(ran(&log), "", "the new driver was switched off too");
        // The fixture is armed: plain git runs it.
        let _ = std::process::Command::new("git").args(["add", "a.txt"]).current_dir(&repo).output();
        assert!(ran(&log).contains("ran"));
    }
}
