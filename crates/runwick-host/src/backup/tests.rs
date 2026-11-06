//! Backup archives: made from a live store, verified, tampering detected, restored, and a
//! failed restore leaving the current data in place.

use super::archive::{self, Source};
use super::restore;
use crate::store::{Store, ts_to_sql};
use crate::wiki::ops;
use runwick_core::api::sync::SyncKind;
use runwick_core::api::wiki::{Author, PageStatus, SavedBy};
use rusqlite::params;
use std::path::{Path, PathBuf};
use uuid::Uuid;

struct Live {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    store: Store,
    project: Uuid,
}

impl Live {
    fn new() -> Live {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        std::fs::create_dir_all(home.join("wiki")).unwrap();
        let store = Store::open(&home.join("runwick.db")).unwrap();
        let code = tmp.path().join("code");
        std::fs::create_dir_all(&code).unwrap();
        let project = crate::projects::open_folder(&store, code.to_str().unwrap()).unwrap().project.id;
        Live { _tmp: tmp, home, store, project }
    }

    fn wiki(&self) -> PathBuf {
        self.home.join("wiki")
    }

    fn src(&self) -> Source<'_> {
        Source { home: &self.home, host_id: Uuid::nil(), device: "MacBook Pro", signer: None }
    }

    fn page(&self, title: &str, body: &str) -> String {
        ops::create(
            &self.store,
            &self.wiki(),
            ops::NewPage {
                project_id: self.project,
                title: title.into(),
                body: body.into(),
                tags: Vec::new(),
                sources: Vec::new(),
                created_by: Author::User,
                status: PageStatus::Reviewed,
                saved_by: SavedBy::User,
                note: None,
                slug: None,
            },
        )
        .unwrap()
        .summary
        .slug
    }

    fn message(&self, text: &str) {
        let ws: Uuid = self.store.with(|c| c.query_row("SELECT id FROM workspaces", [], |r| r.get(0))).unwrap();
        let s = Uuid::now_v7();
        let now = ts_to_sql(&runwick_core::now());
        self.store
            .with(|c| {
                c.execute(
                    "INSERT INTO sessions (id, project_id, workspace_id, title, tool, cwd, created_at) VALUES (?1, ?2, ?3, 't', 'claude', '/tmp', ?4)",
                    params![s, self.project, ws, now],
                )?;
                c.execute(
                    "INSERT INTO messages (id, session_id, project_id, workspace_id, provider_msg_id, seq, role, text, at, collected_at)
                     VALUES (?1, ?2, ?3, ?4, 'm', 0, 'user', ?5, ?6, ?6)",
                    params![Uuid::now_v7(), s, self.project, ws, text, now],
                )
            })
            .unwrap();
    }

    fn count(&self, sql: &str) -> i64 {
        self.store.with(|c| c.query_row(sql, [], |r| r.get(0))).unwrap()
    }
}

fn search(store: &Store, q: &str) -> i64 {
    store
        .with(|c| {
            c.query_row("SELECT COUNT(*) FROM messages_fts WHERE messages_fts MATCH ?1", [format!("\"{q}\"")], |r| {
                r.get(0)
            })
        })
        .unwrap()
}

#[test]
fn a_backup_is_made_while_the_store_is_open_and_verifies() {
    let live = Live::new();
    live.page("배포 절차", "# 배포 절차\n\n1. 빌드\n");
    live.message("결제 오류를 고쳤어요");
    live.store
        .with(|c| {
            c.execute("INSERT INTO kv (key, value, updated_at) VALUES ('host.id', '\"x\"', '2026-01-01T00:00:00Z')", [])
        })
        .unwrap();
    let dir = live.home.join("backups");
    let (path, side) = archive::create(&live.src(), &dir, None).unwrap();
    assert!(archive::is_archive(&path.file_name().unwrap().to_string_lossy()));
    assert_eq!(side.manifest.counts.iter().find(|c| c.kind == SyncKind::Messages).unwrap().count, 1);
    assert!(side.manifest.files.iter().any(|f| f.path.starts_with("wiki/") && f.path.ends_with(".md")));
    // No leftovers next to the archive.
    let names: Vec<String> =
        std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect();
    assert_eq!(names.len(), 2, "{names:?}");
    let check = live.home.join("check");
    assert!(archive::verify(&path, &check).is_ok());
    // The index is left out of the archive and rebuilt on restore.
    let c = rusqlite::Connection::open(check.join("runwick.db")).unwrap();
    let indexed: i64 = c.query_row("SELECT COUNT(*) FROM messages_fts_docsize", [], |r| r.get(0)).unwrap();
    assert_eq!(indexed, 0);
    // The manifest is readable from the sidecar and from the archive alone.
    std::fs::remove_file(archive::sidecar_path(&path)).unwrap();
    assert!(archive::read_manifest(&path).is_some());
}

#[test]
fn runtime_files_logs_screens_and_the_sync_copy_never_enter_an_archive() {
    let live = Live::new();
    live.page("메모", "# 메모\n");
    for f in [
        "gui.lock",
        "menubar.lock",
        "host.pid",
        "host.sock",
        "logs/host.log",
        "logs/host-stderr.log",
        "logs/menubar.log",
        "screens/s.bin",
        "sync/README.md",
        "sync/messages/x.jsonl",
        "secrets/env.json",
        "restore/preview-1/runwick.db",
        "unlinked/x/y",
    ] {
        let p = live.home.join(f);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, "runtime state").unwrap();
    }
    let (path, side) = archive::create(&live.src(), &live.home.join("backups"), None).unwrap();
    for f in &side.manifest.files {
        assert!(f.path == "runwick.db" || f.path.starts_with("wiki/"), "unexpected file in a backup: {}", f.path);
    }
    let check = live.home.join("check");
    archive::verify(&path, &check).unwrap();
    let mut names = Vec::new();
    for e in std::fs::read_dir(&check).unwrap().flatten() {
        names.push(e.file_name().to_string_lossy().to_string());
    }
    names.sort();
    assert_eq!(names, ["manifest.json", "runwick.db", "wiki"]);
}

#[test]
fn tampering_is_detected() {
    let live = Live::new();
    let slug = live.page("메모", "# 메모\n\n원래 내용\n");
    let dir = live.home.join("backups");
    let (path, _) = archive::create(&live.src(), &dir, None).unwrap();

    // 1. The archive bytes changed after it was made.
    let original = std::fs::read(&path).unwrap();
    let mut bytes = original.clone();
    let mid = bytes.len() / 2;
    bytes[mid] ^= 0xff;
    std::fs::write(&path, &bytes).unwrap();
    let problems = archive::verify(&path, &live.home.join("v1")).unwrap_err();
    assert!(problems.iter().any(|p| p.contains("해시 불일치") || p.contains("압축")), "{problems:?}");

    // 2. A file inside was changed and the archive rebuilt without its sidecar.
    std::fs::write(&path, &original).unwrap();
    let x = live.home.join("x");
    std::fs::create_dir_all(&x).unwrap();
    let st = std::process::Command::new("tar").arg("-xzf").arg(&path).arg("-C").arg(&x).status().unwrap();
    assert!(st.success());
    let page = x.join("wiki").join(live.project.to_string()).join(format!("{slug}.md"));
    let text = std::fs::read_to_string(&page).unwrap().replace("원래 내용", "몰래 바꾼 내용");
    std::fs::write(&page, text).unwrap();
    std::fs::remove_file(archive::sidecar_path(&path)).unwrap();
    let st = std::process::Command::new("tar")
        .arg("-czf")
        .arg(&path)
        .arg("-C")
        .arg(&x)
        .args(["manifest.json", "runwick.db", "wiki"])
        .status()
        .unwrap();
    assert!(st.success());
    let problems = archive::verify(&path, &live.home.join("v2")).unwrap_err();
    assert!(problems.iter().any(|p| p.contains(&slug) && p.contains("바뀌었어요")), "{problems:?}");
}

fn tar(args: &[&str], archive: &Path, dir: &Path, names: &[&str]) {
    let st = std::process::Command::new("tar").args(args).arg(archive).arg("-C").arg(dir).args(names).status().unwrap();
    assert!(st.success());
}

#[test]
fn a_swapped_archive_with_recomputed_hashes_fails_the_signature() {
    use super::sign::{Check, Signer};
    let live = Live::new();
    let slug = live.page("메모", "# 메모\n\n원래 내용\n");
    let me = Signer::from_key(vec![3; 32]);
    let src = Source { signer: Some(&me), ..live.src() };
    let (path, side) = archive::create(&src, &live.home.join("backups"), None).unwrap();
    assert!(side.signature.is_some());
    assert_eq!(archive::signature(&path, Ok(Some(&me))), Check::Valid);

    // Someone rebuilds the archive with a changed page and recomputes every hash.
    let x = live.home.join("x");
    std::fs::create_dir_all(&x).unwrap();
    tar(&["-xzf"], &path, &x, &[]);
    let rel = format!("wiki/{}/{slug}.md", live.project);
    std::fs::write(x.join(&rel), "# 메모\n\n몰래 바꾼 내용\n").unwrap();
    let mut manifest: archive::Manifest =
        serde_json::from_slice(&std::fs::read(x.join("manifest.json")).unwrap()).unwrap();
    let entry = manifest.files.iter_mut().find(|f| f.path == rel).unwrap();
    entry.bytes = std::fs::metadata(x.join(&rel)).unwrap().len();
    entry.sha256 = archive::sha256_file(&x.join(&rel)).unwrap();
    std::fs::write(x.join("manifest.json"), serde_json::to_string_pretty(&manifest).unwrap()).unwrap();
    tar(&["-czf"], &path, &x, &["manifest.json", "runwick.db", "wiki"]);
    let forged = archive::Sidecar {
        manifest,
        archive_sha256: archive::sha256_file(&path).unwrap(),
        archive_bytes: std::fs::metadata(&path).unwrap().len(),
        signature: side.signature.clone(),
    };
    let put = |s: &archive::Sidecar| {
        std::fs::write(archive::sidecar_path(&path), serde_json::to_string_pretty(s).unwrap()).unwrap()
    };
    put(&forged);
    // Every hash matches, so only the signature tells.
    archive::verify(&path, &live.home.join("v")).unwrap();
    assert_eq!(archive::signature(&path, Ok(Some(&me))), Check::Bad);
    // Without a signature it is no longer vouched for: the restore asks the person.
    put(&archive::Sidecar { signature: None, ..forged.clone() });
    assert_eq!(archive::signature(&path, Ok(Some(&me))), Check::Unsigned);
    std::fs::remove_file(archive::sidecar_path(&path)).unwrap();
    assert_eq!(archive::signature(&path, Ok(Some(&me))), Check::Unsigned);
    // A key replaced since then reads as another key, not as tampering.
    put(&forged);
    let replaced = Signer::from_key(vec![4; 32]);
    assert_eq!(archive::signature(&path, Ok(Some(&replaced))), Check::OtherKey);
}

#[test]
fn backup_files_and_unpacked_copies_are_readable_only_by_this_user() {
    use std::os::unix::fs::PermissionsExt;
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    let live = Live::new();
    live.page("메모", "# 메모\n");
    // A folder made for the backup.
    let made = live.home.join("chosen").join("backups");
    let (path, _) = archive::create(&live.src(), &made, None).unwrap();
    assert_eq!(mode(&path), 0o600);
    assert_eq!(mode(&archive::sidecar_path(&path)), 0o600);
    assert_eq!(mode(&made), 0o700);
    let left: Vec<_> = std::fs::read_dir(&made).unwrap().flatten().map(|e| e.file_name()).collect();
    assert_eq!(left.len(), 2, "only the archive and its sidecar: {left:?}");
    // A folder the person already had keeps its permissions; the files are still private.
    let existing = live.home.join("mine");
    std::fs::create_dir_all(&existing).unwrap();
    std::fs::set_permissions(&existing, std::fs::Permissions::from_mode(0o755)).unwrap();
    let (path, _) = archive::create(&live.src(), &existing, None).unwrap();
    assert_eq!(mode(&existing), 0o755);
    assert_eq!(mode(&path), 0o600);
    assert_eq!(mode(&archive::sidecar_path(&path)), 0o600);
    // An unpacked copy is private too.
    let into = live.home.join("restore").join("preview-x");
    archive::verify(&path, &into).unwrap();
    assert_eq!(mode(&into), 0o700);
}

#[test]
fn restore_replaces_the_data_and_rebuilds_the_index() {
    let live = Live::new();
    let slug = live.page("배포 절차", "# 배포 절차\n\n백업 때 내용\n");
    live.message("백업 때 있던 대화");
    let dir = live.home.join("backups");
    let (path, _) = archive::create(&live.src(), &dir, None).unwrap();
    live.store
        .with(|c| {
            c.execute(
                "INSERT INTO kv (key, value, updated_at) VALUES ('host.id', '\"this-host\"', '2026-01-01T00:00:00Z'),
                 ('sync.base', '\"abc\"', '2026-01-01T00:00:00Z'), ('backup.verified', '{\"x\":1}', '2026-01-01T00:00:00Z')",
                [],
            )
        })
        .unwrap();

    // Changes after the backup.
    live.message("백업 뒤의 대화");
    live.page("나중 문서", "# 나중\n");
    assert_eq!(live.count("SELECT COUNT(*) FROM messages"), 2);

    let unpacked = live.home.join("restore").join("p");
    archive::verify(&path, &unpacked).unwrap();
    restore::activate(&live.store, &live.wiki(), &unpacked).unwrap();
    assert_eq!(live.count("SELECT COUNT(*) FROM messages"), 1);
    assert_eq!(live.count("SELECT COUNT(*) FROM wiki_pages WHERE deleted = 0"), 1);
    assert_eq!(search(&live.store, "백업 때 있던"), 1);
    assert_eq!(search(&live.store, "백업 뒤의"), 0);
    assert!(ops::get(&live.store, &live.wiki(), live.project, &slug).unwrap().body.contains("백업 때 내용"));
    // Identity is kept; the sync merge base is dropped so the next sync is a union.
    assert_eq!(live.count("SELECT COUNT(*) FROM kv WHERE key = 'host.id' AND value = '\"this-host\"'"), 1);
    assert_eq!(live.count("SELECT COUNT(*) FROM kv WHERE key = 'sync.base'"), 0);
    assert_eq!(live.count("SELECT COUNT(*) FROM kv WHERE key = 'backup.verified'"), 1);
    // Triggers are back: new messages are indexed again.
    live.message("복구 후 새 대화");
    assert_eq!(search(&live.store, "복구 후 새"), 1);
}

#[test]
fn a_failed_restore_keeps_the_current_data() {
    let live = Live::new();
    live.message("지금 데이터");
    let slug = live.page("지금 문서", "# 지금\n");
    let (path, _) = archive::create(&live.src(), &live.home.join("backups"), None).unwrap();
    let unpacked = live.home.join("restore").join("p");
    archive::verify(&path, &unpacked).unwrap();
    // Made by a newer version: refused before anything moves.
    rusqlite::Connection::open(unpacked.join("runwick.db"))
        .unwrap()
        .execute("INSERT INTO schema_migrations (name, applied_at) VALUES ('9999_future', 'x')", [])
        .unwrap();
    assert!(restore::activate(&live.store, &live.wiki(), &unpacked).is_err());
    assert_eq!(live.count("SELECT COUNT(*) FROM messages"), 1);
    assert!(ops::get(&live.store, &live.wiki(), live.project, &slug).is_ok());

    // Rows the live schema rejects: the transaction rolls back and the wiki folder comes back.
    let unpacked = live.home.join("restore").join("q");
    archive::verify(&path, &unpacked).unwrap();
    rusqlite::Connection::open(unpacked.join("runwick.db"))
        .unwrap()
        .execute_batch(
            "PRAGMA foreign_keys = OFF; UPDATE sessions SET project_id = x'00000000000000000000000000000001';",
        )
        .unwrap();
    let err = restore::activate(&live.store, &live.wiki(), &unpacked).unwrap_err();
    assert!(err.contains("지금 데이터를 그대로"), "{err}");
    assert_eq!(live.count("SELECT COUNT(*) FROM messages"), 1);
    assert_eq!(search(&live.store, "지금 데이터"), 1);
    assert!(Path::new(&live.wiki()).join(live.project.to_string()).join(format!("{slug}.md")).is_file());
}

#[test]
fn a_crafted_page_name_or_link_in_a_backup_is_refused() {
    let live = Live::new();
    live.page("메모", "# 메모\n");
    let (path, _) = archive::create(&live.src(), &live.home.join("backups"), None).unwrap();
    // A page name that would leave the wiki folder, planted in the backup's records.
    let unpacked = live.home.join("restore").join("p");
    archive::verify(&path, &unpacked).unwrap();
    rusqlite::Connection::open(unpacked.join("runwick.db"))
        .unwrap()
        .execute("UPDATE wiki_pages SET slug = '../../../escape'", [])
        .unwrap();
    let err = restore::activate(&live.store, &live.wiki(), &unpacked).unwrap_err();
    assert!(err.contains("위키 문서 이름"), "{err}");
    assert!(!live.home.join("escape.md").exists() && !live.home.parent().unwrap().join("escape.md").exists());
    assert_eq!(live.count("SELECT COUNT(*) FROM wiki_pages WHERE deleted = 0"), 1);

    // A link inside an archive (whatever the manifest says) fails verification.
    let x = live.home.join("x");
    std::fs::create_dir_all(&x).unwrap();
    let st = std::process::Command::new("tar").arg("-xzf").arg(&path).arg("-C").arg(&x).status().unwrap();
    assert!(st.success());
    std::os::unix::fs::symlink("/etc/hosts", x.join("wiki").join("link.md")).unwrap();
    std::fs::remove_file(archive::sidecar_path(&path)).unwrap();
    let st = std::process::Command::new("tar")
        .arg("-czf")
        .arg(&path)
        .arg("-C")
        .arg(&x)
        .args(["manifest.json", "runwick.db", "wiki"])
        .status()
        .unwrap();
    assert!(st.success());
    let problems = archive::verify(&path, &live.home.join("v")).unwrap_err();
    assert!(problems.iter().any(|p| p.contains("링크")), "{problems:?}");
}

#[test]
fn a_restore_keeps_this_computers_editor_command_and_marks_the_next_sync() {
    let live = Live::new();
    let set = |v: &str| {
        live.store
            .with(|c| {
                c.execute(
                    "INSERT INTO kv (key, value, updated_at) VALUES ('files.editor', ?1, '2026-01-01T00:00:00Z')
                     ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                    [v],
                )
            })
            .unwrap();
    };
    set("\"/usr/bin/evil --run\"");
    let (path, _) = archive::create(&live.src(), &live.home.join("backups"), None).unwrap();
    set("\"code\"");
    let unpacked = live.home.join("restore").join("p");
    archive::verify(&path, &unpacked).unwrap();
    restore::activate(&live.store, &live.wiki(), &unpacked).unwrap();
    assert_eq!(live.count("SELECT COUNT(*) FROM kv WHERE key = 'files.editor' AND value = '\"code\"'"), 1);
    assert!(crate::sync::state::restored(&live.store));
}

#[test]
fn a_restore_keeps_this_computers_variables_devices_and_folders() {
    let live = Live::new();
    let set_env = |v: &str| {
        live.store
            .with(|c| {
                c.execute(
                    "INSERT INTO env_vars (key, description, secret, value, changed_in, created_at, updated_at)
                     VALUES ('API_BASE', '', 0, ?1, 1, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')
                     ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                    [v],
                )
            })
            .unwrap();
    };
    set_env("https://old.example");
    let (path, _) = archive::create(&live.src(), &live.home.join("backups"), None).unwrap();
    set_env("https://now.example");
    live.store
        .with(|c| c.execute("INSERT INTO devices (id, name, target, added_at) VALUES (?1, '집 Mac mini', 'mini', '2026-01-01T00:00:00Z')", [Uuid::now_v7()]))
        .unwrap();
    let moved = live.home.parent().unwrap().join("moved");
    std::fs::create_dir_all(&moved).unwrap();
    let moved_s = moved.canonicalize().unwrap().to_string_lossy().to_string();
    live.store
        .with(|c| {
            c.execute(
                "UPDATE workspaces SET path = ?1 WHERE project_id = ?2",
                rusqlite::params![moved_s, live.project],
            )?;
            c.execute("UPDATE projects SET root = ?1 WHERE id = ?2", rusqlite::params![moved_s, live.project])
        })
        .unwrap();
    let unpacked = live.home.join("restore").join("p");
    archive::verify(&path, &unpacked).unwrap();
    restore::activate(&live.store, &live.wiki(), &unpacked).unwrap();
    assert_eq!(live.count("SELECT COUNT(*) FROM env_vars WHERE value = 'https://now.example'"), 1);
    assert_eq!(live.count("SELECT COUNT(*) FROM devices"), 1);
    let root: String = live
        .store
        .with(|c| c.query_row("SELECT root FROM projects WHERE id = ?1", [live.project], |r| r.get(0)))
        .unwrap();
    assert_eq!(root, moved_s);
    let ws: String = live
        .store
        .with(|c| c.query_row("SELECT path FROM workspaces WHERE project_id = ?1", [live.project], |r| r.get(0)))
        .unwrap();
    assert_eq!(ws, moved_s);
}
