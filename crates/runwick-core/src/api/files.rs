//! Files and documents of a workspace, served by the host that owns the workspace folder (this
//! computer or a remote device). The GUI never touches the filesystem itself.
//!
//! Paths are workspace-relative with `/` separators; `""` is the workspace root. The host keeps
//! every path inside the workspace folder (no absolute paths, no `..`, no symlinks leading out).
//! Saving is conditional: `files.write` carries the hash of the version an edit started from and
//! returns [`WriteOutcome::Conflict`] instead of overwriting a file that changed on disk since.

use crate::Ts;
use crate::api::projects::WorkspaceId;
use crate::method;
use crate::rpc::topic;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;

/// Largest file `files.read` returns (5 MB). Bigger files are reported as too large.
pub const MAX_READ_BYTES: u64 = 5 * 1024 * 1024;
/// Entries returned per directory; the rest are counted in [`DirListing::more`].
pub const LIST_LIMIT: usize = 2000;
/// Host key/value setting with the editor command for "외부 에디터로 열기" (e.g. `code`, `zed`).
pub const EDITOR_SETTING: &str = "files.editor";

/// What git says about a path. Folders get [`GitMark::Contains`] when something inside changed.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum GitMark {
    Modified,
    Added,
    Deleted,
    Renamed,
    Untracked,
    Conflicted,
    Contains,
}

impl GitMark {
    /// One-letter mark for the tree.
    pub fn letter(self) -> &'static str {
        match self {
            GitMark::Modified => "M",
            GitMark::Added => "A",
            GitMark::Deleted => "D",
            GitMark::Renamed => "R",
            GitMark::Untracked => "U",
            GitMark::Conflicted => "C",
            GitMark::Contains => "•",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            GitMark::Modified => "수정됨",
            GitMark::Added => "추가됨",
            GitMark::Deleted => "삭제됨",
            GitMark::Renamed => "이름 바뀜",
            GitMark::Untracked => "새 파일 · git에 아직 없음",
            GitMark::Conflicted => "병합 충돌",
            GitMark::Contains => "안에 바뀐 파일이 있어요",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FileEntry {
    pub name: String,
    /// Workspace-relative path.
    pub path: String,
    pub is_dir: bool,
    #[serde(default)]
    pub size: Option<u64>,
    #[serde(default)]
    pub mtime: Option<Ts>,
    #[serde(default)]
    pub git: Option<GitMark>,
    #[serde(default)]
    pub symlink: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ListDir {
    pub workspace_id: WorkspaceId,
    #[serde(default)]
    pub dir: String,
    /// Also list dotfiles and paths excluded by `.gitignore`.
    #[serde(default)]
    pub show_hidden: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DirListing {
    pub dir: String,
    /// Folders first, then files, by name.
    pub entries: Vec<FileEntry>,
    /// Entries left out after [`LIST_LIMIT`].
    #[serde(default)]
    pub more: usize,
    /// Git status was read for this listing (false: not a repository, or git was too slow).
    #[serde(default)]
    pub git_checked: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FindFiles {
    pub workspace_id: WorkspaceId,
    pub query: String,
    /// Maximum hits (default 50, at most 200).
    #[serde(default)]
    pub limit: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FileHit {
    /// Workspace-relative path.
    pub path: String,
    /// Character positions in [`display_path`]`(path)` that matched the query.
    pub matched: Vec<u32>,
    pub score: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct FindResult {
    pub hits: Vec<FileHit>,
    /// Files in the search index.
    pub indexed: usize,
    /// The index stopped at its size limit; some files were not searched.
    #[serde(default)]
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ReadFile {
    pub workspace_id: WorkspaceId,
    pub path: String,
    /// Lower limit than [`MAX_READ_BYTES`], if wanted.
    #[serde(default)]
    pub max_bytes: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FileBody {
    /// UTF-8 text. `lossy`: some bytes were not UTF-8 and show as `�` (do not save it back).
    Text {
        text: String,
        #[serde(default)]
        lossy: bool,
    },
    Binary,
    TooLarge {
        limit: u64,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FileContent {
    pub path: String,
    pub body: FileBody,
    pub size: u64,
    #[serde(default)]
    pub mtime: Option<Ts>,
    /// SHA-256 of the bytes on disk (hex). None when the file was too large to read.
    #[serde(default)]
    pub hash: Option<String>,
    /// The file is not writable.
    #[serde(default)]
    pub readonly: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WriteFile {
    pub workspace_id: WorkspaceId,
    pub path: String,
    pub text: String,
    /// Hash of the disk version this edit started from; None when the file should not exist yet.
    #[serde(default)]
    pub base_hash: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WriteOutcome {
    Saved {
        hash: String,
        #[serde(default)]
        mtime: Option<Ts>,
        size: u64,
    },
    /// The disk no longer holds the base version. Nothing was written.
    Conflict {
        /// None: the file does not exist (any more).
        #[serde(default)]
        disk_hash: Option<String>,
        /// Present when the disk version is readable text.
        #[serde(default)]
        disk_text: Option<String>,
        #[serde(default)]
        disk_mtime: Option<Ts>,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum NewDocKind {
    #[default]
    Md,
    Txt,
}

impl NewDocKind {
    pub fn extension(self) -> &'static str {
        match self {
            NewDocKind::Md => "md",
            NewDocKind::Txt => "txt",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CreateFile {
    pub workspace_id: WorkspaceId,
    /// Folder to create in ("" = workspace root).
    #[serde(default)]
    pub rel_dir: String,
    /// File name; the extension of `kind` is added when missing.
    pub name: String,
    #[serde(default)]
    pub kind: NewDocKind,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Created {
    pub path: String,
    pub hash: String,
    #[serde(default)]
    pub mtime: Option<Ts>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StatFiles {
    pub workspace_id: WorkspaceId,
    pub paths: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FileStat {
    pub path: String,
    pub exists: bool,
    #[serde(default)]
    pub hash: Option<String>,
    #[serde(default)]
    pub mtime: Option<Ts>,
    #[serde(default)]
    pub size: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FileRef {
    pub workspace_id: WorkspaceId,
    pub path: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum OpenWith {
    /// The editor from [`EDITOR_SETTING`], else the system's text editor.
    #[default]
    Editor,
    /// The system's default app for the file type (refused for executables).
    Default,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct OpenExternal {
    pub workspace_id: WorkspaceId,
    pub path: String,
    #[serde(default)]
    pub with: OpenWith,
}

method!(FilesList, "files.list", ListDir => DirListing, [topic::FILES]);
method!(FilesFind, "files.find", FindFiles => FindResult, [topic::FILES]);
method!(FilesRead, "files.read", ReadFile => FileContent);
method!(FilesWrite, "files.write", WriteFile => WriteOutcome);
method!(FilesCreate, "files.create", CreateFile => Created);
method!(FilesStat, "files.stat", StatFiles => Vec<FileStat>);
method!(FilesReveal, "files.reveal", FileRef => ());
method!(FilesOpenExternal, "files.open_external", OpenExternal => ());

/// A path as people read it: decomposed Hangul (common in macOS file names) is composed so it
/// renders and matches like typed text. Use the original path for file operations.
pub fn display_path(path: &str) -> Cow<'_, str> {
    compose_hangul(path)
}

/// Compose conjoining Hangul jamo (NFD) into syllables (NFC). Other text is unchanged.
pub fn compose_hangul(s: &str) -> Cow<'_, str> {
    const L0: u32 = 0x1100;
    const V0: u32 = 0x1161;
    const T0: u32 = 0x11A7;
    const S0: u32 = 0xAC00;
    let is_l = |c: u32| (L0..L0 + 19).contains(&c);
    let is_v = |c: u32| (V0..V0 + 21).contains(&c);
    let is_t = |c: u32| (T0 + 1..T0 + 28).contains(&c);
    let is_lv = |c: u32| (S0..S0 + 11172).contains(&c) && (c - S0).is_multiple_of(28);
    if !s.chars().any(|c| is_v(c as u32) || is_t(c as u32)) {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len());
    let mut last: Option<u32> = None;
    for ch in s.chars() {
        let c = ch as u32;
        match last {
            Some(l) if is_l(l) && is_v(c) => {
                last = Some(S0 + ((l - L0) * 21 + (c - V0)) * 28);
                continue;
            }
            Some(lv) if is_lv(lv) && is_t(c) => {
                last = Some(lv + (c - T0));
                continue;
            }
            _ => {}
        }
        if let Some(p) = last.and_then(char::from_u32) {
            out.push(p);
        }
        last = Some(c);
    }
    if let Some(p) = last.and_then(char::from_u32) {
        out.push(p);
    }
    Cow::Owned(out)
}

/// File name part of a workspace-relative path.
pub fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Parent folder of a workspace-relative path ("" for the root).
pub fn parent_dir(path: &str) -> &str {
    path.rsplit_once('/').map(|(d, _)| d).unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn composes_decomposed_hangul() {
        // "회의 메모" typed on macOS often reaches the disk decomposed.
        let nfd = "\u{1112}\u{116c}\u{110b}\u{1174} \u{1106}\u{1166}\u{1106}\u{1169}.md";
        assert_eq!(compose_hangul(nfd), "회의 메모.md");
        assert_eq!(compose_hangul("각"), "각");
        let with_final = "\u{1100}\u{1161}\u{11a8}"; // ㄱ ㅏ ㄱ
        assert_eq!(compose_hangul(with_final), "각");
        assert!(matches!(compose_hangul("plain/ascii.rs"), Cow::Borrowed(_)));
    }

    #[test]
    fn path_helpers() {
        assert_eq!(file_name("docs/a/b.md"), "b.md");
        assert_eq!(file_name("b.md"), "b.md");
        assert_eq!(parent_dir("docs/a/b.md"), "docs/a");
        assert_eq!(parent_dir("b.md"), "");
    }

    #[test]
    fn outcome_json_is_tagged() {
        let o = WriteOutcome::Conflict { disk_hash: None, disk_text: None, disk_mtime: None };
        let v = serde_json::to_value(&o).unwrap();
        assert_eq!(v["kind"], "conflict");
        let b: FileBody = serde_json::from_value(serde_json::json!({"kind": "text", "text": "x"})).unwrap();
        assert_eq!(b, FileBody::Text { text: "x".into(), lossy: false });
    }
}
