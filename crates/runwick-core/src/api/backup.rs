//! Local backups of one host's data (S14 기기별 백업).
//!
//! A backup is one archive in a folder of this computer: a consistent snapshot of the record
//! store, the wiki pages and a manifest with a SHA-256 of every file. It is made while CLIs keep
//! running and verified after writing. Each host backs up its own disk; an archive made by
//! another computer is labelled a copy and is restored on the computer that made it.
//!
//! Restoring never overwrites the current data in place first: the archive is verified and
//! unpacked into a separate folder, shown as a preview, and only activated when no managed run
//! is alive and no document has unsaved edits. The current data is kept as a dated backup.

use crate::Ts;
use crate::api::sync::KindCount;
use crate::method;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Cache-invalidation topic for backup lists.
pub const TOPIC: &str = "backup";
/// `manifest.json` `format` value inside a backup archive.
pub const FORMAT: &str = "runwick-backup";
pub const FORMAT_VERSION: u32 = 1;

/// What a backup holds and leaves out, for the include/exclude boxes.
pub const INCLUDED: &[&str] = &["대화 원문", "핀", "위키 문서", "기록 사이 연결", "타임라인 메모·요약", "설정 참조"];
pub const EXCLUDED: &[&str] = &[
    "비밀값",
    "실행 중인 프로세스",
    "다시 만들 수 있는 색인",
    "터미널 화면 기록",
    "실행 상태 파일",
    "GitHub 동기화용 복사본",
];

/// Where this host keeps its data and how much space it takes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StorageInfo {
    pub host_id: Uuid,
    pub device_name: String,
    /// Data folder for display (`~/.runwick`).
    pub home: String,
    /// Record store (SQLite, including its search index).
    pub db_bytes: u64,
    pub wiki_bytes: u64,
    /// Saved terminal screens (never backed up or synced).
    pub screens_bytes: u64,
    /// GitHub sync working copy.
    pub sync_bytes: u64,
    pub backups_bytes: u64,
    /// Default backup folder for display.
    pub backups_dir: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Verify {
    NotChecked,
    Ok { at: Ts },
    Failed { at: Ts, problems: Vec<String> },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BackupItem {
    /// Absolute path of the archive on its host.
    pub path: String,
    pub file_name: String,
    pub bytes: u64,
    #[serde(default)]
    pub created_at: Option<Ts>,
    /// Computer that made it (from its manifest).
    #[serde(default)]
    pub device: Option<String>,
    #[serde(default)]
    pub host_id: Option<Uuid>,
    /// Made by another computer: a copy, restored on the computer that made it.
    pub copy: bool,
    /// Kept automatically before a restore replaced the data.
    #[serde(default)]
    pub before_restore: bool,
    pub verify: Verify,
    #[serde(default)]
    pub counts: Vec<KindCount>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BackupList {
    /// Folders that were scanned (the default one first).
    pub dirs: Vec<String>,
    pub items: Vec<BackupItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CreateBackup {
    /// Folder for the archive; the default backup folder when empty.
    #[serde(default)]
    pub dir: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BackupPath {
    pub path: String,
}

/// A backup unpacked into a separate folder, read but not active.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RestorePreview {
    pub id: Uuid,
    pub item: BackupItem,
    /// Records in the backup per kind, and in the current data.
    pub in_backup: Vec<KindCount>,
    pub now: Vec<KindCount>,
    /// Project names in the backup.
    pub projects: Vec<String>,
    /// Why it cannot be activated now (empty when it can).
    pub blockers: Vec<String>,
    /// This computer's settings the restore keeps as they are (variables, devices, folders).
    #[serde(default)]
    pub kept: Vec<String>,
    /// Why this computer's signature cannot vouch for the backup (`None`: signed here and
    /// unchanged). Activation then needs `ActivateRestore::accept_unsigned`.
    #[serde(default)]
    pub unsigned: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ActivateRestore {
    pub id: Uuid,
    /// Documents with unsaved edits in the asking window (activation waits for them).
    #[serde(default)]
    pub unsaved_documents: u32,
    /// The person confirmed restoring a backup this computer's signature cannot vouch for.
    #[serde(default)]
    pub accept_unsigned: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RestoreDone {
    /// The dated backup of the data that was active before.
    pub kept: String,
    /// The host restarts to load the restored data; windows reconnect by themselves.
    pub restarting: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PreviewRef {
    pub id: Uuid,
}

method!(BackupInfo, "backup.info", () => StorageInfo, [TOPIC]);
method!(BackupListGet, "backup.list", () => BackupList, [TOPIC]);
method!(BackupCreate, "backup.create", CreateBackup => BackupItem);
method!(BackupVerify, "backup.verify", BackupPath => BackupItem);
method!(BackupRestorePreview, "backup.restore_preview", BackupPath => RestorePreview);
method!(BackupRestoreDiscard, "backup.restore_discard", PreviewRef => ());
method!(BackupRestoreActivate, "backup.restore_activate", ActivateRestore => RestoreDone);
