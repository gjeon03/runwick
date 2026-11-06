//! Keeping every path inside the workspace folder. Requests carry workspace-relative paths;
//! these helpers reject absolute paths, `..`, and symlinks that resolve outside the folder.

use runwick_core::rpc::{RpcError, codes};
use std::path::{Component, Path, PathBuf};

/// Canonical folder of a local workspace.
pub fn workspace_root(store: &crate::store::Store, id: uuid::Uuid) -> Result<PathBuf, RpcError> {
    let ws = crate::projects::get_workspace(store, id)?;
    if ws.remote.is_some() {
        return Err(RpcError::new(
            codes::UNSUPPORTED,
            "이 작업 공간의 파일은 다른 기기에 있어요. 그 기기에 연결해서 열어 주세요",
        ));
    }
    std::fs::canonicalize(&ws.path)
        .ok()
        .filter(|p| p.is_dir())
        .ok_or_else(|| RpcError::not_found(format!("작업 공간 폴더를 찾을 수 없어요: {}", ws.path)))
}

fn outside() -> RpcError {
    RpcError::new(codes::DENIED, "작업 공간 밖의 경로는 열 수 없어요")
}

/// Validate a workspace-relative path and return it as relative components.
pub fn clean_rel(rel: &str) -> Result<PathBuf, RpcError> {
    if rel.contains('\0') {
        return Err(RpcError::invalid("경로에 쓸 수 없는 글자가 있어요"));
    }
    let p = Path::new(rel);
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::Normal(part) => out.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(outside());
            }
        }
    }
    Ok(out)
}

/// An existing path inside `root` (canonical), following symlinks only when they stay inside.
pub fn resolve_existing(root: &Path, rel: &str) -> Result<PathBuf, RpcError> {
    let joined = root.join(clean_rel(rel)?);
    let canon = std::fs::canonicalize(&joined).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => RpcError::not_found(format!("파일을 찾을 수 없어요: {rel}")),
        std::io::ErrorKind::PermissionDenied => RpcError::new(codes::DENIED, format!("읽을 권한이 없어요: {rel}")),
        _ => RpcError::internal(format!("경로를 확인하지 못했어요 ({e})")),
    })?;
    if !canon.starts_with(root) {
        return Err(outside());
    }
    Ok(canon)
}

/// Where a write to `rel` lands: the resolved file when it exists (writing through a symlink
/// keeps the link), otherwise the new file inside an existing folder of the workspace.
pub fn resolve_for_write(root: &Path, rel: &str) -> Result<PathBuf, RpcError> {
    let clean = clean_rel(rel)?;
    let name = clean.file_name().ok_or_else(|| RpcError::invalid("파일 이름이 비어 있어요"))?.to_owned();
    let joined = root.join(&clean);
    if std::fs::symlink_metadata(&joined).is_ok() {
        return resolve_existing(root, rel);
    }
    let parent_rel = clean.parent().map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
    let parent = resolve_existing(root, &parent_rel)
        .map_err(|e| if e.code == codes::NOT_FOUND { RpcError::not_found("저장할 폴더가 없어요") } else { e })?;
    if !parent.is_dir() {
        return Err(RpcError::invalid("저장할 위치가 폴더가 아니에요"));
    }
    Ok(parent.join(name))
}

/// Workspace-relative `/` path of an absolute path under `root`.
pub fn to_rel(root: &Path, abs: &Path) -> Option<String> {
    let rel = abs.strip_prefix(root).ok()?;
    let parts: Option<Vec<&str>> = rel.components().map(|c| c.as_os_str().to_str()).collect();
    Some(parts?.join("/"))
}

/// Join a workspace-relative folder and a name.
pub fn join_rel(dir: &str, name: &str) -> String {
    if dir.is_empty() { name.to_string() } else { format!("{dir}/{name}") }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_escapes_and_absolute_paths() {
        assert!(clean_rel("../x").is_err());
        assert!(clean_rel("a/../../x").is_err());
        assert!(clean_rel("/etc/passwd").is_err());
        assert!(clean_rel("a\0b").is_err());
        assert_eq!(clean_rel("./a/b.md").unwrap(), PathBuf::from("a/b.md"));
        assert_eq!(clean_rel("").unwrap(), PathBuf::new());
    }

    #[test]
    fn symlinks_must_stay_inside() {
        let ws = tempfile::tempdir().unwrap();
        let outside_dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(ws.path()).unwrap();
        std::fs::write(outside_dir.path().join("secret.txt"), "s").unwrap();
        std::fs::write(root.join("in.txt"), "i").unwrap();
        std::os::unix::fs::symlink(outside_dir.path().join("secret.txt"), root.join("leak.txt")).unwrap();
        std::os::unix::fs::symlink(outside_dir.path(), root.join("leakdir")).unwrap();
        std::os::unix::fs::symlink(root.join("in.txt"), root.join("alias.txt")).unwrap();
        assert_eq!(resolve_existing(&root, "leak.txt").unwrap_err().code, codes::DENIED);
        assert_eq!(resolve_existing(&root, "leakdir/secret.txt").unwrap_err().code, codes::DENIED);
        assert_eq!(resolve_for_write(&root, "leakdir/new.txt").unwrap_err().code, codes::DENIED);
        assert_eq!(resolve_for_write(&root, "leak.txt").unwrap_err().code, codes::DENIED);
        assert_eq!(resolve_existing(&root, "alias.txt").unwrap(), root.join("in.txt"));
        assert_eq!(resolve_for_write(&root, "new.md").unwrap(), root.join("new.md"));
        assert_eq!(resolve_existing(&root, "nope.md").unwrap_err().code, codes::NOT_FOUND);
        assert_eq!(to_rel(&root, &root.join("a").join("b.md")).as_deref(), Some("a/b.md"));
    }
}
