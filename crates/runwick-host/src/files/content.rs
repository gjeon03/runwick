//! Reading, conditional saving, creating and checking files. Blocking: call from
//! `spawn_blocking`. Saving never overwrites a version the editor has not seen.

use super::paths::{join_rel, resolve_existing, resolve_for_write, to_rel};
use runwick_core::api::files::*;
use runwick_core::rpc::{RpcError, codes};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;

/// Bytes checked for NUL when telling text from binary.
const SNIFF: usize = 8000;
/// More replacement characters than this share of the text means binary.
const MAX_INVALID_RATIO: f64 = 0.05;
/// Files checked per `files.stat`.
const STAT_MAX: usize = 64;

pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

fn mtime_of(meta: &std::fs::Metadata) -> Option<runwick_core::Ts> {
    meta.modified().ok().map(chrono::DateTime::<chrono::Utc>::from)
}

fn io_err(what: &str, rel: &str, e: std::io::Error) -> RpcError {
    match e.kind() {
        std::io::ErrorKind::NotFound => RpcError::not_found(format!("파일을 찾을 수 없어요: {rel}")),
        std::io::ErrorKind::PermissionDenied => RpcError::new(codes::DENIED, format!("{what} 권한이 없어요: {rel}")),
        _ => RpcError::internal(format!("{what} 못했어요: {rel} ({e})")),
    }
}

/// Text or binary, as people would expect from an editor.
pub fn classify(bytes: &[u8]) -> FileBody {
    if bytes[..bytes.len().min(SNIFF)].contains(&0) {
        return FileBody::Binary;
    }
    match std::str::from_utf8(bytes) {
        Ok(s) => FileBody::Text { text: s.to_string(), lossy: false },
        Err(_) => {
            let text = String::from_utf8_lossy(bytes).to_string();
            let total = text.chars().count().max(1);
            let bad = text.chars().filter(|c| *c == char::REPLACEMENT_CHARACTER).count();
            if bad as f64 / total as f64 > MAX_INVALID_RATIO {
                FileBody::Binary
            } else {
                FileBody::Text { text, lossy: true }
            }
        }
    }
}

pub fn read(root: &Path, rel: &str, max_bytes: Option<u64>) -> Result<FileContent, RpcError> {
    let path = resolve_existing(root, rel)?;
    let meta = std::fs::metadata(&path).map_err(|e| io_err("읽을", rel, e))?;
    if meta.is_dir() {
        return Err(RpcError::invalid("폴더는 문서로 열 수 없어요"));
    }
    if !meta.is_file() {
        // FIFOs, sockets and device files would block or never end.
        return Err(RpcError::invalid("일반 파일이 아니라서 열 수 없어요"));
    }
    let limit = max_bytes.unwrap_or(MAX_READ_BYTES).min(MAX_READ_BYTES);
    let readonly = meta.permissions().readonly();
    if meta.len() > limit {
        return Ok(FileContent {
            path: rel.to_string(),
            body: FileBody::TooLarge { limit },
            size: meta.len(),
            mtime: mtime_of(&meta),
            hash: None,
            readonly,
        });
    }
    let bytes = std::fs::read(&path).map_err(|e| io_err("읽을", rel, e))?;
    let hash = sha256_hex(&bytes);
    remember_hash(&path, &meta, &hash);
    Ok(FileContent {
        path: rel.to_string(),
        body: classify(&bytes),
        size: bytes.len() as u64,
        mtime: mtime_of(&meta),
        hash: Some(hash),
        readonly,
    })
}

/// One lock per file so two saves to the same file cannot interleave.
fn file_lock(path: &Path) -> Arc<Mutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>> = OnceLock::new();
    let mut map = LOCKS.get_or_init(Default::default).lock().unwrap();
    if map.len() > 4096 {
        map.retain(|_, l| Arc::strong_count(l) > 1);
    }
    map.entry(path.to_path_buf()).or_default().clone()
}

/// The disk version right now: (hash, bytes) or None when the file is missing.
fn disk_version(path: &Path, rel: &str) -> Result<Option<(String, Vec<u8>)>, RpcError> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some((sha256_hex(&bytes), bytes))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io_err("읽을", rel, e)),
    }
}

fn conflict(path: &Path, disk: Option<(String, Vec<u8>)>) -> WriteOutcome {
    let disk_mtime = std::fs::metadata(path).ok().as_ref().and_then(mtime_of);
    match disk {
        None => WriteOutcome::Conflict { disk_hash: None, disk_text: None, disk_mtime },
        Some((hash, bytes)) => {
            let disk_text = match classify(&bytes) {
                FileBody::Text { text, lossy: false } if bytes.len() as u64 <= MAX_READ_BYTES => Some(text),
                _ => None,
            };
            WriteOutcome::Conflict { disk_hash: Some(hash), disk_text, disk_mtime }
        }
    }
}

/// Save `text` only if the disk still holds `base_hash` (None: the file must not exist yet).
/// Writes a temporary file next to the target and renames it over, keeping permissions.
pub fn write(root: &Path, rel: &str, text: &str, base_hash: Option<&str>) -> Result<WriteOutcome, RpcError> {
    let path = resolve_for_write(root, rel)?;
    let lock = file_lock(&path);
    let _guard = lock.lock().unwrap();
    let meta = std::fs::metadata(&path).ok();
    if meta.as_ref().is_some_and(|m| m.is_dir()) {
        return Err(RpcError::invalid("같은 이름의 폴더가 있어 저장할 수 없어요"));
    }
    if meta.as_ref().is_some_and(|m| m.permissions().readonly()) {
        return Err(RpcError::new(codes::DENIED, format!("읽기 전용 파일이라 저장하지 않았어요: {rel}")));
    }
    let disk = disk_version(&path, rel)?;
    if disk.as_ref().map(|(h, _)| h.as_str()) != base_hash {
        return Ok(conflict(&path, disk));
    }
    let dir = path.parent().ok_or_else(|| RpcError::invalid("저장할 폴더가 없어요"))?;
    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let tmp = dir.join(format!(".{name}.runwick-{}-{}.tmp", std::process::id(), uuid::Uuid::new_v4().simple()));
    let result = (|| -> Result<Option<WriteOutcome>, RpcError> {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
            .map_err(|e| io_err("저장할", rel, e))?;
        f.write_all(text.as_bytes()).map_err(|e| io_err("저장하지", rel, e))?;
        if let Some(m) = &meta {
            f.set_permissions(m.permissions()).map_err(|e| io_err("권한을 맞추지", rel, e))?;
        }
        f.sync_all().map_err(|e| io_err("저장하지", rel, e))?;
        drop(f);
        // Last check right before replacing: a CLI may have written in the meantime.
        let again = disk_version(&path, rel)?;
        if again.as_ref().map(|(h, _)| h.as_str()) != base_hash {
            return Ok(Some(conflict(&path, again)));
        }
        std::fs::rename(&tmp, &path).map_err(|e| io_err("저장하지", rel, e))?;
        Ok(None)
    })();
    match result {
        Ok(Some(c)) => {
            let _ = std::fs::remove_file(&tmp);
            Ok(c)
        }
        Ok(None) => {
            let meta = std::fs::metadata(&path).map_err(|e| io_err("확인하지", rel, e))?;
            let hash = sha256_hex(text.as_bytes());
            remember_hash(&path, &meta, &hash);
            Ok(WriteOutcome::Saved { hash, mtime: mtime_of(&meta), size: meta.len() })
        }
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// File name for a new document: trimmed, one path segment, with the kind's extension.
pub fn new_doc_name(name: &str, kind: NewDocKind) -> Result<String, RpcError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(RpcError::invalid("문서 이름을 적어 주세요"));
    }
    if name.contains('/') || name.contains('\0') || name == "." || name == ".." {
        return Err(RpcError::invalid("이름에 / 를 쓸 수 없어요. 위치는 따로 고르세요"));
    }
    let ext = kind.extension();
    let has_ext = name.rsplit_once('.').is_some_and(|(stem, e)| !stem.is_empty() && e.eq_ignore_ascii_case(ext));
    Ok(if has_ext { name.to_string() } else { format!("{name}.{ext}") })
}

/// Create a new, empty document. Never replaces an existing file.
pub fn create(root: &Path, rel_dir: &str, name: &str, kind: NewDocKind) -> Result<Created, RpcError> {
    let file = new_doc_name(name, kind)?;
    let dir = resolve_existing(root, rel_dir)?;
    if !dir.is_dir() {
        return Err(RpcError::invalid("새 문서를 만들 위치가 폴더가 아니에요"));
    }
    let path = dir.join(&file);
    let rel = to_rel(root, &path).unwrap_or_else(|| join_rel(rel_dir, &file));
    let f = std::fs::OpenOptions::new().write(true).create_new(true).open(&path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::AlreadyExists {
            RpcError::new(codes::CONFLICT, format!("같은 이름의 파일이 이미 있어요: {rel}"))
        } else {
            io_err("만들", &rel, e)
        }
    })?;
    f.sync_all().ok();
    let meta = std::fs::metadata(&path).map_err(|e| io_err("확인하지", &rel, e))?;
    Ok(Created { path: rel, hash: sha256_hex(b""), mtime: mtime_of(&meta) })
}

struct Known {
    modified: SystemTime,
    len: u64,
    hash: String,
}

fn hash_cache() -> &'static Mutex<HashMap<PathBuf, Known>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, Known>>> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

fn remember_hash(path: &Path, meta: &std::fs::Metadata, hash: &str) {
    let Ok(modified) = meta.modified() else {
        return;
    };
    let mut map = hash_cache().lock().unwrap();
    if map.len() > 4096 {
        map.clear();
    }
    map.insert(path.to_path_buf(), Known { modified, len: meta.len(), hash: hash.to_string() });
}

/// Hash for a file, re-reading only when its size or modification time changed. Very recent
/// modifications are always re-read (a same-size rewrite within the timestamp resolution).
fn current_hash(path: &Path, meta: &std::fs::Metadata) -> Option<String> {
    let modified = meta.modified().ok()?;
    let recent = modified.elapsed().map(|d| d.as_secs() < 2).unwrap_or(true);
    if !recent
        && let Some(k) = hash_cache().lock().unwrap().get(path)
        && k.modified == modified
        && k.len == meta.len()
    {
        return Some(k.hash.clone());
    }
    let bytes = std::fs::read(path).ok()?;
    let hash = sha256_hex(&bytes);
    remember_hash(path, meta, &hash);
    Some(hash)
}

/// Hash and time of open documents, for noticing changes made by CLIs or other programs.
pub fn stat(root: &Path, paths: &[String]) -> Vec<FileStat> {
    paths
        .iter()
        .take(STAT_MAX)
        .map(|rel| {
            let missing = FileStat { path: rel.clone(), exists: false, hash: None, mtime: None, size: None };
            let Ok(path) = resolve_existing(root, rel) else {
                return missing;
            };
            let Ok(meta) = std::fs::metadata(&path) else {
                return missing;
            };
            if meta.is_dir() {
                return missing;
            }
            let hash = if meta.len() <= MAX_READ_BYTES { current_hash(&path, &meta) } else { None };
            FileStat { path: rel.clone(), exists: true, hash, mtime: mtime_of(&meta), size: Some(meta.len()) }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn root() -> (tempfile::TempDir, PathBuf) {
        let d = tempfile::tempdir().unwrap();
        let r = std::fs::canonicalize(d.path()).unwrap();
        (d, r)
    }

    #[test]
    fn tells_text_binary_and_too_large_apart() {
        let (_d, r) = root();
        std::fs::write(r.join("a.md"), "# 제목\n본문\n").unwrap();
        std::fs::write(r.join("b.png"), [0x89, b'P', b'N', b'G', 0, 0, 1, 2]).unwrap();
        let mut latin1 = b"caf".to_vec();
        latin1.extend([0xE9, b' ', b'o', b'k', b' ', b'f', b'i', b'n', b'e', b' ', b't', b'e', b'x', b't', b'!', b'!']);
        latin1.extend(std::iter::repeat_n(b'a', 40));
        std::fs::write(r.join("c.txt"), &latin1).unwrap();
        std::fs::write(r.join("big.log"), vec![b'x'; 2048]).unwrap();
        let a = read(&r, "a.md", None).unwrap();
        assert_eq!(a.body, FileBody::Text { text: "# 제목\n본문\n".into(), lossy: false });
        assert_eq!(a.hash.as_deref(), Some(sha256_hex("# 제목\n본문\n".as_bytes()).as_str()));
        assert_eq!(read(&r, "b.png", None).unwrap().body, FileBody::Binary);
        assert!(matches!(read(&r, "c.txt", None).unwrap().body, FileBody::Text { lossy: true, .. }));
        let big = read(&r, "big.log", Some(1024)).unwrap();
        assert_eq!(big.body, FileBody::TooLarge { limit: 1024 });
        assert_eq!(big.size, 2048);
        assert!(big.hash.is_none());
        assert_eq!(read(&r, ".", None).unwrap_err().code, codes::INVALID);
    }

    #[test]
    fn write_refuses_when_disk_changed_since_base() {
        let (_d, r) = root();
        std::fs::write(r.join("n.md"), "v1\n").unwrap();
        let base = sha256_hex(b"v1\n");
        // Someone else (a CLI) writes v2.
        std::fs::write(r.join("n.md"), "v2 from cli\n").unwrap();
        let out = write(&r, "n.md", "my edit\n", Some(&base)).unwrap();
        match out {
            WriteOutcome::Conflict { disk_hash, disk_text, .. } => {
                assert_eq!(disk_hash.as_deref(), Some(sha256_hex(b"v2 from cli\n").as_str()));
                assert_eq!(disk_text.as_deref(), Some("v2 from cli\n"));
            }
            other => panic!("expected conflict, got {other:?}"),
        }
        assert_eq!(std::fs::read_to_string(r.join("n.md")).unwrap(), "v2 from cli\n");
        // Saving on top of the version we have now seen works.
        let seen = sha256_hex(b"v2 from cli\n");
        let out = write(&r, "n.md", "merged\n", Some(&seen)).unwrap();
        assert!(matches!(out, WriteOutcome::Saved { ref hash, size: 7, .. } if *hash == sha256_hex(b"merged\n")));
        assert_eq!(std::fs::read_to_string(r.join("n.md")).unwrap(), "merged\n");
        // No base: only for files that do not exist yet.
        assert!(matches!(write(&r, "n.md", "x", None).unwrap(), WriteOutcome::Conflict { .. }));
        assert!(matches!(write(&r, "fresh.md", "x", None).unwrap(), WriteOutcome::Saved { .. }));
        // A deleted file is a conflict too (disk_hash None).
        std::fs::remove_file(r.join("fresh.md")).unwrap();
        let out = write(&r, "fresh.md", "y", Some(&sha256_hex(b"x"))).unwrap();
        assert_eq!(out, WriteOutcome::Conflict { disk_hash: None, disk_text: None, disk_mtime: None });
        // No temporary files are left behind.
        let leftovers: Vec<_> = std::fs::read_dir(&r)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty());
    }

    #[test]
    fn write_keeps_permissions_and_symlinks() {
        let (_d, r) = root();
        std::fs::write(r.join("p.sh"), "echo 1\n").unwrap();
        std::fs::set_permissions(r.join("p.sh"), std::fs::Permissions::from_mode(0o750)).unwrap();
        write(&r, "p.sh", "echo 2\n", Some(&sha256_hex(b"echo 1\n"))).unwrap();
        let mode = std::fs::metadata(r.join("p.sh")).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o750);
        std::fs::write(r.join("real.md"), "a").unwrap();
        std::os::unix::fs::symlink(r.join("real.md"), r.join("link.md")).unwrap();
        write(&r, "link.md", "b", Some(&sha256_hex(b"a"))).unwrap();
        assert!(std::fs::symlink_metadata(r.join("link.md")).unwrap().file_type().is_symlink());
        assert_eq!(std::fs::read_to_string(r.join("real.md")).unwrap(), "b");
        std::fs::set_permissions(r.join("real.md"), std::fs::Permissions::from_mode(0o444)).unwrap();
        assert_eq!(write(&r, "real.md", "c", Some(&sha256_hex(b"b"))).unwrap_err().code, codes::DENIED);
        assert_eq!(write(&r, "../escape.md", "x", None).unwrap_err().code, codes::DENIED);
    }

    #[test]
    fn create_names_korean_docs_and_never_overwrites() {
        let (_d, r) = root();
        std::fs::create_dir(r.join("docs")).unwrap();
        let c = create(&r, "docs", " 회의 메모 ", NewDocKind::Md).unwrap();
        assert_eq!(c.path, "docs/회의 메모.md");
        assert!(r.join("docs/회의 메모.md").is_file());
        assert_eq!(create(&r, "docs", "회의 메모.md", NewDocKind::Md).unwrap_err().code, codes::CONFLICT);
        assert_eq!(create(&r, "", "notes", NewDocKind::Txt).unwrap().path, "notes.txt");
        assert_eq!(create(&r, "", "a/b", NewDocKind::Md).unwrap_err().code, codes::INVALID);
        assert_eq!(create(&r, "", "  ", NewDocKind::Md).unwrap_err().code, codes::INVALID);
        assert_eq!(create(&r, "../x", "y", NewDocKind::Md).unwrap_err().code, codes::DENIED);
        assert_eq!(new_doc_name(".md", NewDocKind::Md).unwrap(), ".md.md");
    }

    #[test]
    fn stat_reports_hash_changes_and_missing_files() {
        let (_d, r) = root();
        std::fs::write(r.join("s.md"), "one").unwrap();
        let s = stat(&r, &["s.md".into(), "gone.md".into(), "../x".into()]);
        assert!(s[0].exists);
        assert_eq!(s[0].hash.as_deref(), Some(sha256_hex(b"one").as_str()));
        assert!(!s[1].exists && !s[2].exists);
        std::fs::write(r.join("s.md"), "two").unwrap();
        assert_eq!(stat(&r, &["s.md".into()])[0].hash.as_deref(), Some(sha256_hex(b"two").as_str()));
    }
}
