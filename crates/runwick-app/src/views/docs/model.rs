//! One open document: the disk version the edit started from (the base), the edit buffer, and
//! what has been observed on disk since. Only state transitions live here (no drawing) so the
//! rules — never overwrite, "저장됨" only after the host confirms — are covered by tests.

use crate::state::WsKey;
use runwick_core::Ts;
use runwick_core::api::files::{FileBody, FileContent, FileStat, WriteOutcome};
use runwick_core::rpc::{RpcError, codes};
use serde::{Deserialize, Serialize};
use std::time::Instant;

pub type DocKey = (WsKey, String);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Markdown,
    Text,
    Code,
}

const CODE_EXT: &[&str] = &[
    "rs",
    "ts",
    "tsx",
    "js",
    "jsx",
    "mjs",
    "cjs",
    "py",
    "go",
    "java",
    "kt",
    "kts",
    "swift",
    "c",
    "h",
    "cc",
    "cpp",
    "hpp",
    "m",
    "mm",
    "cs",
    "rb",
    "php",
    "lua",
    "sh",
    "bash",
    "zsh",
    "fish",
    "ps1",
    "sql",
    "json",
    "jsonc",
    "toml",
    "yaml",
    "yml",
    "xml",
    "html",
    "htm",
    "css",
    "scss",
    "sass",
    "less",
    "vue",
    "svelte",
    "dart",
    "scala",
    "ex",
    "exs",
    "erl",
    "hs",
    "ml",
    "clj",
    "r",
    "pl",
    "proto",
    "graphql",
    "gql",
    "tf",
    "nix",
    "zig",
    "lock",
    "ini",
    "cfg",
    "conf",
    "gradle",
    "cmake",
    "dockerfile",
    "makefile",
    "diff",
    "patch",
];

impl Kind {
    pub fn of(path: &str) -> Kind {
        let name = runwick_core::api::files::file_name(path).to_ascii_lowercase();
        let ext = name.rsplit_once('.').map(|(_, e)| e.to_string()).unwrap_or_default();
        match ext.as_str() {
            "md" | "markdown" | "mdx" | "mdown" => Kind::Markdown,
            e if CODE_EXT.contains(&e) => Kind::Code,
            _ if matches!(name.as_str(), "makefile" | "dockerfile" | "justfile" | "rakefile" | "gemfile") => Kind::Code,
            _ => Kind::Text,
        }
    }

    /// Syntax name or extension handed to the highlighter.
    pub fn language(path: &str) -> String {
        let name = runwick_core::api::files::file_name(path).to_ascii_lowercase();
        match name.rsplit_once('.') {
            Some((_, e)) => match e {
                "tsx" | "mjs" | "cjs" | "jsx" => "js".into(),
                "jsonc" => "json".into(),
                "yml" => "yaml".into(),
                "zsh" | "bash" | "fish" => "sh".into(),
                "kts" => "kt".into(),
                e => e.into(),
            },
            None if name == "makefile" => "Makefile".into(),
            None => name,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Body {
    Loading,
    Text,
    Binary,
    TooLarge {
        limit: u64,
    },
    /// Could not be read. `missing`: the file does not exist.
    Failed {
        message: String,
        missing: bool,
    },
}

/// A version seen on disk that differs from the base while there are local edits.
#[derive(Debug, Clone, PartialEq)]
pub struct DiskVersion {
    /// None: the file is gone.
    pub hash: Option<String>,
    /// None: gone, or not readable as text any more.
    pub text: Option<String>,
    pub mtime: Option<Ts>,
    pub seen: Ts,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SaveState {
    Idle,
    Saving {
        since: Instant,
    },
    Saved {
        at: Ts,
    },
    Failed {
        message: String,
    },
    /// The disk changed since the base; nothing was written.
    Conflict,
    /// The host could not be reached. Kept here and saved after reconnecting, once the disk
    /// version has been checked again.
    Queued,
}

#[derive(Debug, Clone)]
pub struct Doc {
    pub ws: WsKey,
    pub path: String,
    pub kind: Kind,
    pub body: Body,
    /// Text of the disk version the edit started from (LF line endings).
    pub base: String,
    /// Hash of that disk version; None when the file did not exist.
    pub base_hash: Option<String>,
    pub base_at: Ts,
    /// The edit buffer (LF line endings).
    pub text: String,
    /// The file uses CRLF; it is written back with CRLF.
    pub crlf: bool,
    /// Bytes that were not UTF-8 were replaced; saving would corrupt them.
    pub lossy: bool,
    pub readonly_file: bool,
    pub size: u64,
    pub save: SaveState,
    pub external: Option<DiskVersion>,
    /// The file no longer exists on disk.
    pub missing: bool,
    /// Reloaded from disk without local edits (shown as "디스크에서 바뀜").
    pub reloaded_at: Option<Ts>,
    /// The buffer came from a draft kept when the app last closed.
    pub restored: bool,
    /// The disk version was checked since loading/restoring (false after reconnecting).
    pub verified: bool,
    /// Bumps whenever the base changes so late answers about an older base are ignored.
    pub seq: u64,
}

/// Work with LF internally; remember CRLF files to write them back the same way.
pub fn normalize(text: &str) -> (String, bool) {
    let crlf_count = text.matches("\r\n").count();
    if crlf_count > 0 && crlf_count == text.matches('\n').count() {
        (text.replace("\r\n", "\n"), true)
    } else {
        (text.to_string(), false)
    }
}

pub fn denormalize(text: &str, crlf: bool) -> String {
    if crlf { text.replace('\n', "\r\n") } else { text.to_string() }
}

impl Doc {
    pub fn new(ws: WsKey, path: &str) -> Self {
        Self {
            ws,
            path: path.to_string(),
            kind: Kind::of(path),
            body: Body::Loading,
            base: String::new(),
            base_hash: None,
            base_at: runwick_core::now(),
            text: String::new(),
            crlf: false,
            lossy: false,
            readonly_file: false,
            size: 0,
            save: SaveState::Idle,
            external: None,
            missing: false,
            reloaded_at: None,
            restored: false,
            verified: false,
            seq: 0,
        }
    }

    pub fn from_draft(d: &Draft) -> Self {
        let mut doc = Doc::new(d.ws, &d.path);
        doc.body = Body::Text;
        doc.base = d.base.clone();
        doc.base_hash = d.base_hash.clone();
        doc.base_at = d.base_at;
        doc.text = d.text.clone();
        doc.crlf = d.crlf;
        doc.size = d.text.len() as u64;
        doc.restored = true;
        doc
    }

    pub fn draft(&self) -> Option<Draft> {
        self.dirty().then(|| Draft {
            ws: self.ws,
            path: self.path.clone(),
            text: self.text.clone(),
            base: self.base.clone(),
            base_hash: self.base_hash.clone(),
            base_at: self.base_at,
            crlf: self.crlf,
            saved_at: runwick_core::now(),
        })
    }

    pub fn is_text(&self) -> bool {
        self.body == Body::Text
    }

    pub fn dirty(&self) -> bool {
        self.is_text() && self.text != self.base
    }

    pub fn saving(&self) -> bool {
        matches!(self.save, SaveState::Saving { .. })
    }

    /// Something only the person can resolve is pending: unsaved edits or a disk change.
    pub fn needs_person(&self) -> bool {
        self.dirty() || self.external.is_some() || matches!(self.save, SaveState::Queued | SaveState::Conflict)
    }

    fn set_base(&mut self, text: String, hash: Option<String>) {
        self.base = text;
        self.base_hash = hash;
        self.base_at = runwick_core::now();
        self.external = None;
        self.missing = false;
        self.seq += 1;
    }

    fn disk_version(c: &FileContent) -> DiskVersion {
        let text = match &c.body {
            FileBody::Text { text, lossy: false } => Some(normalize(text).0),
            _ => None,
        };
        DiskVersion { hash: c.hash.clone(), text, mtime: c.mtime, seen: runwick_core::now() }
    }

    /// First read of the file, or the check of a restored draft against the disk.
    pub fn loaded(&mut self, c: FileContent) {
        self.size = c.size;
        self.readonly_file = c.readonly;
        self.verified = true;
        if self.is_text() {
            // A buffer already exists (restored draft or reconnect): compare, never replace.
            self.missing = false;
            if c.hash == self.base_hash {
                self.external = None;
            } else {
                let disk = Self::disk_version(&c);
                if disk.text.as_deref() == Some(self.text.as_str()) {
                    self.set_base(self.text.clone(), c.hash);
                } else if !self.dirty() {
                    self.apply_disk(disk);
                } else {
                    self.external = Some(disk);
                }
            }
            self.after_check();
            return;
        }
        match c.body {
            FileBody::Text { text, lossy } => {
                let (lf, crlf) = normalize(&text);
                self.text = lf.clone();
                self.crlf = crlf;
                self.lossy = lossy;
                self.set_base(lf, c.hash);
                self.body = Body::Text;
            }
            FileBody::Binary => self.body = Body::Binary,
            FileBody::TooLarge { limit } => self.body = Body::TooLarge { limit },
        }
    }

    pub fn load_failed(&mut self, e: &RpcError) {
        if e.code == codes::DISCONNECTED {
            // Unknown, not failed: checked again after reconnecting.
            self.verified = false;
            return;
        }
        let missing = e.code == codes::NOT_FOUND;
        if self.is_text() {
            self.verified = true;
            self.missing = missing;
            if !missing {
                self.save = SaveState::Failed { message: e.message.clone() };
            }
            self.after_check();
        } else {
            self.body = Body::Failed { message: e.message.clone(), missing };
        }
    }

    /// A queued save may go ahead once the disk still matches the base.
    fn after_check(&mut self) {
        if self.save == SaveState::Queued && (self.external.is_some() || self.missing) {
            self.save = SaveState::Conflict;
        }
    }

    /// Whether a periodic check found a disk version worth reading.
    pub fn needs_read(&mut self, st: &FileStat) -> bool {
        if !st.exists {
            self.missing = true;
            return false;
        }
        self.missing = false;
        let Some(hash) = &st.hash else { return false };
        if Some(hash) == self.base_hash.as_ref() {
            // The disk is back at the base: nothing to decide any more.
            self.external = None;
            if self.save == SaveState::Conflict {
                self.save = SaveState::Idle;
            }
            return false;
        }
        self.external.as_ref().and_then(|e| e.hash.as_ref()) != Some(hash)
    }

    fn apply_disk(&mut self, disk: DiskVersion) {
        match disk.text {
            Some(text) => {
                self.text = text.clone();
                self.set_base(text, disk.hash);
                self.reloaded_at = Some(runwick_core::now());
            }
            None => self.external = Some(disk),
        }
    }

    /// A newer disk version arrived. Without local edits it simply replaces the buffer;
    /// with local edits it is kept aside for comparison.
    pub fn disk_read(&mut self, c: FileContent) {
        if !self.is_text() {
            self.body = Body::Loading;
            self.loaded(c);
            return;
        }
        let disk = Self::disk_version(&c);
        if c.hash == self.base_hash {
            self.external = None;
        } else if disk.text.as_deref() == Some(self.text.as_str()) {
            self.set_base(self.text.clone(), c.hash);
        } else if !self.dirty() {
            self.apply_disk(disk);
        } else {
            self.external = Some(disk);
        }
    }

    /// Start saving. Returns the text to write (with the file's line endings) and the hash the
    /// disk must still have; None when nothing can be saved now.
    pub fn begin_save(&mut self, text: &str, over: Option<Option<String>>) -> Option<(String, Option<String>)> {
        if !self.is_text() || self.saving() || self.lossy {
            return None;
        }
        let base = match over {
            Some(h) => h,
            None if self.missing => None,
            None => self.base_hash.clone(),
        };
        self.save = SaveState::Saving { since: Instant::now() };
        Some((denormalize(text, self.crlf), base))
    }

    /// The host answered a save of `sent` (LF). `replace_buffer`: the sent text becomes the
    /// buffer (a merge), otherwise later typing stays unsaved.
    pub fn finish_save(&mut self, sent: String, out: WriteOutcome, replace_buffer: bool) {
        match out {
            WriteOutcome::Saved { hash, .. } => {
                if replace_buffer {
                    self.text = sent.clone();
                }
                self.set_base(sent, Some(hash));
                self.reloaded_at = None;
                self.restored = false;
                self.save = SaveState::Saved { at: runwick_core::now() };
            }
            WriteOutcome::Conflict { disk_hash: None, .. } => {
                // Deleted meantime: saving again recreates it (expecting no file).
                self.missing = true;
                self.external = None;
                self.save = SaveState::Conflict;
            }
            WriteOutcome::Conflict { disk_hash, disk_text, disk_mtime } => {
                self.missing = false;
                self.external = Some(DiskVersion {
                    hash: disk_hash,
                    text: disk_text.map(|t| normalize(&t).0),
                    mtime: disk_mtime,
                    seen: runwick_core::now(),
                });
                self.save = SaveState::Conflict;
            }
        }
    }

    pub fn save_failed(&mut self, e: &RpcError) {
        self.save = if e.code == codes::DISCONNECTED {
            self.verified = false;
            SaveState::Queued
        } else {
            SaveState::Failed { message: e.message.clone() }
        };
    }

    /// Throw away local edits and take the disk version that was compared.
    pub fn take_disk(&mut self) -> bool {
        let Some(ext) = self.external.take() else {
            return false;
        };
        match ext.text {
            Some(text) => {
                self.text = text.clone();
                self.set_base(text, ext.hash);
                self.save = SaveState::Idle;
                self.reloaded_at = Some(runwick_core::now());
                self.restored = false;
                true
            }
            None => {
                self.external = Some(ext);
                false
            }
        }
    }

    /// Three-way merge of base, my edit and the disk: (text, unresolved conflicts).
    pub fn merge(&self) -> Option<(String, usize)> {
        let disk = self.external.as_ref()?.text.as_ref()?;
        let mut m = similar::TextMerge::from_lines(self.base.as_str(), self.text.as_str(), disk.as_str());
        m.labels("기준 버전", "내 편집", "디스크");
        Some((m.to_string(), m.conflict_count()))
    }
}

/// Lines that still carry merge conflict markers.
pub fn conflict_markers(text: &str) -> usize {
    text.lines().filter(|l| l.starts_with("<<<<<<<") || l.starts_with(">>>>>>>")).count() / 2
}

/// Unsaved buffer kept on this computer between app runs (host kv `docs.drafts`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Draft {
    pub ws: WsKey,
    pub path: String,
    pub text: String,
    pub base: String,
    #[serde(default)]
    pub base_hash: Option<String>,
    pub base_at: Ts,
    #[serde(default)]
    pub crlf: bool,
    pub saved_at: Ts,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::HostKey;

    fn ws() -> WsKey {
        WsKey { host: HostKey::Local, id: uuid::Uuid::nil() }
    }

    fn content(text: &str) -> FileContent {
        FileContent {
            path: "a.md".into(),
            body: FileBody::Text { text: text.into(), lossy: false },
            size: text.len() as u64,
            mtime: None,
            hash: Some(format!("h:{text}")),
            readonly: false,
        }
    }

    fn stat(text: &str) -> FileStat {
        FileStat { path: "a.md".into(), exists: true, hash: Some(format!("h:{text}")), mtime: None, size: None }
    }

    fn loaded(text: &str) -> Doc {
        let mut d = Doc::new(ws(), "a.md");
        d.loaded(content(text));
        d
    }

    #[test]
    fn kinds_by_name() {
        assert_eq!(Kind::of("docs/a.MD"), Kind::Markdown);
        assert_eq!(Kind::of("src/main.rs"), Kind::Code);
        assert_eq!(Kind::of("Makefile"), Kind::Code);
        assert_eq!(Kind::of("notes.txt"), Kind::Text);
        assert_eq!(Kind::of("LICENSE"), Kind::Text);
        assert_eq!(Kind::language("a/b.tsx"), "js");
    }

    #[test]
    fn crlf_round_trips() {
        let (lf, crlf) = normalize("a\r\nb\r\n");
        assert_eq!((lf.as_str(), crlf), ("a\nb\n", true));
        assert_eq!(denormalize("a\nb\n", true), "a\r\nb\r\n");
        // Mixed endings are left alone.
        assert_eq!(normalize("a\r\nb\n"), ("a\r\nb\n".to_string(), false));
    }

    #[test]
    fn external_change_without_edits_reloads_silently() {
        let mut d = loaded("v1");
        assert!(d.needs_read(&stat("v2")));
        d.disk_read(content("v2"));
        assert_eq!(d.text, "v2");
        assert!(!d.dirty() && d.external.is_none() && d.reloaded_at.is_some());
        assert!(!d.needs_read(&stat("v2")));
    }

    #[test]
    fn external_change_with_edits_keeps_my_edit() {
        let mut d = loaded("v1");
        d.text = "v1 + mine".into();
        assert!(d.needs_read(&stat("cli")));
        d.disk_read(content("cli"));
        assert_eq!(d.text, "v1 + mine");
        assert_eq!(d.external.as_ref().and_then(|e| e.text.as_deref()), Some("cli"));
        // The same disk version is not read again.
        assert!(!d.needs_read(&stat("cli")));
        // A save now must carry the old base so the host refuses it.
        let (_, base) = d.begin_save(&d.text.clone(), None).unwrap();
        assert_eq!(base.as_deref(), Some("h:v1"));
    }

    #[test]
    fn saved_only_after_host_confirms() {
        let mut d = loaded("v1");
        d.text = "v2".into();
        let (sent, base) = d.begin_save("v2", None).unwrap();
        assert_eq!((sent.as_str(), base.as_deref()), ("v2", Some("h:v1")));
        assert!(d.saving());
        assert!(d.begin_save("v2", None).is_none(), "one save at a time");
        d.finish_save("v2".into(), WriteOutcome::Saved { hash: "h:v2".into(), mtime: None, size: 2 }, false);
        assert!(matches!(d.save, SaveState::Saved { .. }));
        assert!(!d.dirty());
        assert_eq!(d.base_hash.as_deref(), Some("h:v2"));
    }

    #[test]
    fn conflict_and_failures_keep_the_buffer() {
        let mut d = loaded("v1");
        d.text = "mine".into();
        d.begin_save("mine", None).unwrap();
        let out =
            WriteOutcome::Conflict { disk_hash: Some("h:cli".into()), disk_text: Some("cli".into()), disk_mtime: None };
        d.finish_save("mine".into(), out, false);
        assert_eq!(d.save, SaveState::Conflict);
        assert_eq!(d.text, "mine");
        // Merge combines both sides when they touch different lines.
        let mut d2 = loaded("a\nb\nc\n");
        d2.text = "A\nb\nc\n".into();
        d2.disk_read(content("a\nb\nC\n"));
        assert_eq!(d2.merge(), Some(("A\nb\nC\n".to_string(), 0)));
        // "내 편집으로 저장" saves over the disk version that was seen.
        let (_, base) = d.begin_save("mine", Some(Some("h:cli".into()))).unwrap();
        assert_eq!(base.as_deref(), Some("h:cli"));
        d.save_failed(&RpcError::new(codes::DISCONNECTED, "끊김"));
        assert_eq!(d.save, SaveState::Queued);
        assert_eq!(d.text, "mine");
        d.begin_save("mine", None).unwrap();
        d.save_failed(&RpcError::new(codes::DENIED, "읽기 전용"));
        assert_eq!(d.save, SaveState::Failed { message: "읽기 전용".into() });
    }

    #[test]
    fn taking_the_disk_version_discards_edits() {
        let mut d = loaded("v1");
        d.text = "mine".into();
        d.disk_read(content("cli"));
        assert!(d.take_disk());
        assert_eq!(d.text, "cli");
        assert!(!d.dirty() && d.external.is_none());
    }

    #[test]
    fn restored_draft_is_checked_against_disk() {
        let mut d = loaded("v1");
        d.text = "draft".into();
        let draft = d.draft().unwrap();
        // Disk unchanged: the draft stays as unsaved edits.
        let mut r = Doc::from_draft(&draft);
        r.loaded(content("v1"));
        assert!(r.dirty() && r.external.is_none() && r.verified);
        // Disk changed meanwhile: compare instead of overwriting either side.
        let mut r = Doc::from_draft(&draft);
        r.save = SaveState::Queued;
        r.loaded(content("cli"));
        assert_eq!(r.text, "draft");
        assert!(r.external.is_some());
        assert_eq!(r.save, SaveState::Conflict, "a queued save waits for the person");
        // The file was deleted.
        let mut r = Doc::from_draft(&draft);
        r.load_failed(&RpcError::not_found("없음"));
        assert!(r.missing && r.dirty());
        let (_, base) = r.begin_save("draft", None).unwrap();
        assert_eq!(base, None, "recreating a deleted file expects no file");
    }

    #[test]
    fn deleted_file_is_noticed() {
        let mut d = loaded("v1");
        let gone = FileStat { path: "a.md".into(), exists: false, hash: None, mtime: None, size: None };
        assert!(!d.needs_read(&gone));
        assert!(d.missing);
        assert!(!d.needs_read(&stat("v1")));
        assert!(!d.missing);
    }

    #[test]
    fn deleted_while_saving_recreates_on_next_save() {
        let mut d = loaded("v1");
        d.text = "mine".into();
        d.begin_save("mine", None).unwrap();
        d.finish_save(
            "mine".into(),
            WriteOutcome::Conflict { disk_hash: None, disk_text: None, disk_mtime: None },
            false,
        );
        assert!(d.missing && d.external.is_none());
        let (_, base) = d.begin_save("mine", None).unwrap();
        assert_eq!(base, None);
    }

    #[test]
    fn marker_count() {
        assert_eq!(conflict_markers("a\n<<<<<<< 내 편집\nx\n=======\ny\n>>>>>>> 디스크\n"), 1);
        assert_eq!(conflict_markers("plain"), 0);
    }
}
