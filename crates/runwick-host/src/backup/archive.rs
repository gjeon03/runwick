//! Making and checking a backup archive.
//!
//! The record store is copied with `VACUUM INTO` on a separate read connection: a consistent
//! snapshot that never blocks the host (CLIs keep running and records keep arriving). The
//! search index is emptied in the copy (it is rebuilt on restore) and the wiki pages are written
//! from the snapshot itself, so pages and their index always match. Every file is listed in the
//! manifest with its SHA-256, and the archive's own hash is kept next to it.

use crate::wiki::index;
use runwick_core::Ts;
use runwick_core::api::backup::{EXCLUDED, FORMAT, FORMAT_VERSION};
use runwick_core::api::sync::{KindCount, SyncKind};
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use uuid::Uuid;

pub const DB_FILE: &str = "runwick.db";
pub const WIKI_DIR: &str = "wiki";
pub const MANIFEST: &str = "manifest.json";
const PREFIX: &str = "runwick-";
const SUFFIX: &str = ".tar.gz";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FileEntry {
    pub path: String,
    pub bytes: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Manifest {
    pub format: String,
    pub version: u32,
    pub app_version: String,
    pub host_id: Uuid,
    pub device: String,
    pub created_at: Ts,
    /// `before_restore` for the copy kept automatically before a restore.
    #[serde(default)]
    pub reason: Option<String>,
    pub counts: Vec<KindCount>,
    pub files: Vec<FileEntry>,
    pub excluded: Vec<String>,
}

/// Kept next to the archive (`<archive>.json`): its manifest and the archive's own hash.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sidecar {
    pub manifest: Manifest,
    pub archive_sha256: String,
    pub archive_bytes: u64,
    /// This computer's signature of `archive_sha256` and `manifest.host_id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<super::sign::Signature>,
}

/// Backups and their unpacked copies hold every conversation: only this user may open them.
/// Folders created here are 0700 (an existing folder the person chose keeps its permissions);
/// `fix` also tightens `p` itself when it already exists (folders Runwick owns).
pub fn private_dir(p: &Path, fix: bool) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(p)?;
    if fix {
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// A new file readable only by this user (0600 from the moment it exists).
pub fn private_file(p: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(p)?;
    f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    Ok(f)
}

pub fn is_archive(name: &str) -> bool {
    name.starts_with(PREFIX) && name.ends_with(SUFFIX)
}

pub fn sidecar_path(archive: &Path) -> PathBuf {
    let mut s = archive.as_os_str().to_owned();
    s.push(".json");
    PathBuf::from(s)
}

pub fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// Records per kind in a store (for manifests and previews).
pub fn counts(c: &Connection) -> rusqlite::Result<Vec<KindCount>> {
    let q = |sql: &str| c.query_row(sql, [], |r| r.get::<_, i64>(0)).map(|n| n.max(0) as u64);
    let list = [
        (SyncKind::Projects, q("SELECT COUNT(*) FROM projects")?),
        (SyncKind::Sessions, q("SELECT COUNT(*) FROM sessions")?),
        (SyncKind::Messages, q("SELECT COUNT(*) FROM messages")?),
        (SyncKind::Pins, q("SELECT COUNT(*) FROM pins")?),
        (SyncKind::Wiki, q("SELECT COUNT(*) FROM wiki_pages WHERE deleted = 0")?),
        (
            SyncKind::Timeline,
            q("SELECT (SELECT COUNT(*) FROM project_notes) + (SELECT COUNT(*) FROM timeline_summaries)")?,
        ),
        (SyncKind::Roles, q("SELECT COUNT(*) FROM roles")?),
        (SyncKind::Env, q("SELECT COUNT(*) FROM env_vars")?),
        (SyncKind::Collab, q("SELECT COUNT(*) FROM collab_requests")?),
    ];
    Ok(list.into_iter().map(|(kind, count)| KindCount { kind, count }).collect())
}

fn slug(name: &str) -> String {
    let s: String =
        name.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' }).collect();
    let s = s.split('-').filter(|p| !p.is_empty()).collect::<Vec<_>>().join("-");
    if s.is_empty() { "device".into() } else { s.chars().take(40).collect() }
}

pub struct Source<'a> {
    /// Folder holding `runwick.db` (the live data folder).
    pub home: &'a Path,
    pub host_id: Uuid,
    pub device: &'a str,
    /// Signs the sidecar (see `sign`); unsigned when this computer's key could not be read.
    pub signer: Option<&'a super::sign::Signer>,
}

fn tar() -> Command {
    let mut c = Command::new(if Path::new("/usr/bin/tar").is_file() { "/usr/bin/tar" } else { "tar" });
    c.env("LC_ALL", "C").stdin(std::process::Stdio::null());
    c
}

fn run(mut cmd: Command, what: &str) -> Result<(), String> {
    let out = cmd.output().map_err(|e| format!("{what}: {e}"))?;
    if out.status.success() { Ok(()) } else { Err(format!("{what}: {}", String::from_utf8_lossy(&out.stderr).trim())) }
}

/// Make a backup archive in `dir`. Returns the archive path and its sidecar.
pub fn create(src: &Source, dir: &Path, reason: Option<&str>) -> Result<(PathBuf, Sidecar), String> {
    private_dir(dir, false).map_err(|e| format!("백업 폴더를 만들지 못했어요: {e}"))?;
    let staging = dir.join(format!(".runwick-backup-{}", Uuid::new_v4()));
    let out = make(src, dir, &staging, reason);
    let _ = std::fs::remove_dir_all(&staging);
    out
}

fn make(src: &Source, dir: &Path, staging: &Path, reason: Option<&str>) -> Result<(PathBuf, Sidecar), String> {
    private_dir(staging, true).map_err(|e| format!("임시 폴더를 만들지 못했어요: {e}"))?;
    let snap = staging.join(DB_FILE);
    {
        // A separate read connection: WAL lets it read a consistent state while the host writes.
        let live = Connection::open_with_flags(
            src.home.join(DB_FILE),
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|e| format!("기록 저장소를 열지 못했어요: {e}"))?;
        live.busy_timeout(std::time::Duration::from_secs(30)).map_err(|e| e.to_string())?;
        live.execute("VACUUM INTO ?1", [snap.to_string_lossy().as_ref()])
            .map_err(|e| format!("스냅샷을 만들지 못했어요: {e}"))?;
    }
    let counts = {
        let c = Connection::open(&snap).map_err(|e| e.to_string())?;
        // The search index can be rebuilt from the messages; leave it out of the archive.
        c.execute_batch("INSERT INTO messages_fts(messages_fts) VALUES('delete-all'); VACUUM;")
            .map_err(|e| format!("스냅샷을 정리하지 못했어요: {e}"))?;
        write_wiki(&c, &staging.join(WIKI_DIR))?;
        counts(&c).map_err(|e| e.to_string())?
    };
    let mut files = Vec::new();
    collect(staging, staging, &mut files)?;
    files.sort_by(|a, b| a.path.cmp(&b.path));
    let manifest = Manifest {
        format: FORMAT.into(),
        version: FORMAT_VERSION,
        app_version: runwick_core::VERSION.into(),
        host_id: src.host_id,
        device: src.device.into(),
        created_at: runwick_core::now(),
        reason: reason.map(str::to_string),
        counts,
        files,
        excluded: EXCLUDED.iter().map(|s| s.to_string()).collect(),
    };
    let text = serde_json::to_string_pretty(&manifest).map_err(|e| e.to_string())?;
    std::fs::write(staging.join(MANIFEST), text).map_err(|e| e.to_string())?;

    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let tag = if reason == Some("before_restore") { "-before-restore" } else { "" };
    let mut name = format!("{PREFIX}{}-{stamp}{tag}{SUFFIX}", slug(src.device));
    let mut n = 2;
    while dir.join(&name).exists() {
        name = format!("{PREFIX}{}-{stamp}{tag}-{n}{SUFFIX}", slug(src.device));
        n += 1;
    }
    let archive = dir.join(&name);
    let tmp = dir.join(format!(".{name}.part"));
    // tar writes into a file that is private from the start (never readable by others, even
    // while it is being written).
    let out = private_file(&tmp).map_err(|e| format!("백업 파일을 만들지 못했어요: {e}"))?;
    let mut cmd = tar();
    cmd.arg("-czf").arg("-").arg("-C").arg(staging).args([MANIFEST, DB_FILE, WIKI_DIR]).stdout(out);
    if let Err(e) = run(cmd, "묶지 못했어요") {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    let sha = sha256_file(&tmp).map_err(|e| e.to_string())?;
    let bytes = std::fs::metadata(&tmp).map(|m| m.len()).unwrap_or(0);
    let side = Sidecar {
        signature: src.signer.map(|s| s.sign(manifest.host_id, &sha)),
        manifest,
        archive_sha256: sha,
        archive_bytes: bytes,
    };
    {
        use std::io::Write;
        let text = serde_json::to_string_pretty(&side).map_err(|e| e.to_string())?;
        private_file(&sidecar_path(&archive))
            .and_then(|mut f| f.write_all(text.as_bytes()).and_then(|_| f.sync_all()))
            .map_err(|e| format!("백업 정보를 쓰지 못했어요: {e}"))?;
    }
    std::fs::rename(&tmp, &archive).map_err(|e| format!("백업 파일을 저장하지 못했어요: {e}"))?;
    Ok((archive, side))
}

/// Write every live wiki page of a store as files under `wiki` (the same text the wiki writes).
pub fn write_wiki(c: &Connection, wiki: &Path) -> Result<(), String> {
    std::fs::create_dir_all(wiki).map_err(|e| e.to_string())?;
    for row in index::list(c, None).map_err(|e| e.to_string())? {
        // A page name comes from the record store; a crafted one must not leave this folder.
        if !crate::sync::kinds::wiki::valid_slug(&row.slug) || row.slug != runwick_core::nfc::hangul(&row.slug) {
            return Err(format!("위키 문서 이름이 올바르지 않아 쓰지 않았어요: {:?}", row.slug));
        }
        let body = index::body(c, row.id, row.version).map_err(|e| e.to_string())?.map(|(_, b)| b).unwrap_or_default();
        index::write_file(wiki, &row, &body).map_err(|e| format!("위키 문서를 쓰지 못했어요: {e}"))?;
    }
    Ok(())
}

fn collect(root: &Path, dir: &Path, out: &mut Vec<FileEntry>) -> Result<(), String> {
    for e in std::fs::read_dir(dir).map_err(|e| e.to_string())?.flatten() {
        let p = e.path();
        // Never follow a link: an archive may only hold plain folders and files.
        let meta = std::fs::symlink_metadata(&p).map_err(|e| e.to_string())?;
        if meta.is_dir() {
            collect(root, &p, out)?;
        } else if !meta.is_file() {
            let rel = p.strip_prefix(root).unwrap_or(&p).to_string_lossy().to_string();
            return Err(format!("허용되지 않는 파일(링크 등)이 들어 있어요: {rel}"));
        } else {
            let rel = p.strip_prefix(root).map_err(|e| e.to_string())?.to_string_lossy().to_string();
            if rel == MANIFEST {
                continue;
            }
            out.push(FileEntry {
                bytes: e.metadata().map(|m| m.len()).unwrap_or(0),
                sha256: sha256_file(&p).map_err(|e| e.to_string())?,
                path: rel,
            });
        }
    }
    Ok(())
}

/// The manifest of an archive: from its sidecar, else read out of the archive.
pub fn read_manifest(archive: &Path) -> Option<(Manifest, Option<Sidecar>)> {
    if let Some(side) =
        std::fs::read(sidecar_path(archive)).ok().and_then(|b| serde_json::from_slice::<Sidecar>(&b).ok())
    {
        return Some((side.manifest.clone(), Some(side)));
    }
    let out = tar().arg("-xzOf").arg(archive).arg(MANIFEST).output().ok()?;
    if !out.status.success() {
        return None;
    }
    serde_json::from_slice::<Manifest>(&out.stdout).ok().map(|m| (m, None))
}

/// What this computer's signature in the sidecar says about `archive` (the archive itself is
/// compared with the sidecar's hash by `verify`).
pub fn signature(archive: &Path, signer: Result<Option<&super::sign::Signer>, ()>) -> super::sign::Check {
    match std::fs::read(sidecar_path(archive)).ok().and_then(|b| serde_json::from_slice::<Sidecar>(&b).ok()) {
        Some(side) => super::sign::check(signer, side.signature.as_ref(), side.manifest.host_id, &side.archive_sha256),
        None => super::sign::Check::Unsigned,
    }
}

/// Unpack `archive` into `into` (a new folder) and check every file against the manifest.
/// Returns the manifest when everything matches, or the list of problems.
pub fn verify(archive: &Path, into: &Path) -> Result<Manifest, Vec<String>> {
    let mut problems = Vec::new();
    let side = std::fs::read(sidecar_path(archive)).ok().and_then(|b| serde_json::from_slice::<Sidecar>(&b).ok());
    if let Some(side) = &side {
        match sha256_file(archive) {
            Ok(h) if h == side.archive_sha256 => {}
            Ok(_) => problems.push("백업 파일이 만든 뒤에 바뀌었거나 손상됐어요 (해시 불일치)".to_string()),
            Err(e) => return Err(vec![format!("백업 파일을 읽지 못했어요: {e}")]),
        }
    }
    if private_dir(into, true).is_err() {
        return Err(vec!["확인용 폴더를 만들지 못했어요".into()]);
    }
    let mut cmd = tar();
    cmd.arg("-xzf").arg(archive).arg("-C").arg(into);
    if let Err(e) = run(cmd, "압축을 풀지 못했어요") {
        problems.push(e);
        return Err(problems);
    }
    let manifest =
        match std::fs::read(into.join(MANIFEST)).ok().and_then(|b| serde_json::from_slice::<Manifest>(&b).ok()) {
            Some(m) => m,
            None => {
                problems.push("백업 목록(manifest)을 읽지 못했어요".into());
                return Err(problems);
            }
        };
    if manifest.format != FORMAT || manifest.version > FORMAT_VERSION {
        problems.push("이 Runwick이 읽을 수 없는 백업 형식이에요".into());
        return Err(problems);
    }
    if let Some(side) = &side
        && side.manifest != manifest
    {
        problems.push("백업 안의 목록이 함께 저장한 정보와 달라요".into());
    }
    let mut present = Vec::new();
    if let Err(e) = collect(into, into, &mut present) {
        problems.push(e);
        return Err(problems);
    }
    // File names may come back in another Unicode normalization (macOS tar); compare files by
    // what the file system resolves, not by the bytes of their names.
    let mut expected = std::collections::HashSet::new();
    for f in &manifest.files {
        let p = into.join(&f.path);
        if !p.is_file() {
            problems.push(format!("{} 파일이 없어요", f.path));
            continue;
        }
        if let Ok(real) = p.canonicalize() {
            expected.insert(real);
        }
        let bytes = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
        if bytes != f.bytes || sha256_file(&p).ok().as_deref() != Some(f.sha256.as_str()) {
            problems.push(format!("{} 파일 내용이 바뀌었어요 (해시 불일치)", f.path));
        }
    }
    for p in &present {
        let real = into.join(&p.path).canonicalize().ok();
        if !real.is_some_and(|r| expected.contains(&r)) {
            problems.push(format!("목록에 없는 파일이 들어 있어요: {}", p.path));
        }
    }
    if problems.is_empty() {
        match Connection::open_with_flags(into.join(DB_FILE), OpenFlags::SQLITE_OPEN_READ_ONLY)
            .and_then(|c| c.query_row("PRAGMA quick_check", [], |r| r.get::<_, String>(0)))
        {
            Ok(s) if s == "ok" => {}
            Ok(s) => problems.push(format!("기록 저장소 검사 실패: {s}")),
            Err(e) => problems.push(format!("기록 저장소를 열지 못했어요: {e}")),
        }
    }
    if problems.is_empty() { Ok(manifest) } else { Err(problems) }
}
