//! Two (or three) data folders syncing through a local bare repository (no network).

use super::engine::{Engine, WorkingCopy};
use super::git::{Git, GitEnv};
use super::kinds::{Cx, Device};
use super::{repo, state};
use crate::env::{self, EnvService, secrets::FakeStore};
use crate::store::{Store, ts_to_sql};
use crate::wiki::ops;
use runwick_core::api::env::SetVar;
use runwick_core::api::sync::{ConnectPreview, RepoContent, SyncKind, SyncReport, SyncStep};
use runwick_core::api::wiki::{Author, PageStatus, SaveOutcome, SavedBy};
use rusqlite::params;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use uuid::Uuid;

struct Dev {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    store: Store,
    env: EnvService,
    device: Device,
    git: Git,
}

fn bare_repo() -> (tempfile::TempDir, String) {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("data.git");
    let git = Git::new(GitEnv::process()).unwrap();
    git.ok(None, &["init", "-q", "--bare", "-b", "main", path.to_str().unwrap()], super::git::LOCAL).unwrap();
    (d, path.to_string_lossy().to_string())
}

#[test]
fn project_order_and_groups_sync_with_empty_groups_collapse_and_deletion() {
    use crate::projects::{
        create_group, delete_group, get_project, list_groups, list_projects, move_group, move_project, update_group,
    };
    use runwick_core::api::projects::{ProjectGroupCreate, ProjectGroupMove, ProjectGroupUpdate, ProjectMove};
    let (_repo, url) = bare_repo();
    let a = Dev::new("A");
    let b = Dev::new("B");
    let (p, _) = a.project("one");
    let (q, _) = a.project("two");
    let group = create_group(&a.store, &ProjectGroupCreate { name: "Work".into(), project: Some(p) }).unwrap();
    let empty = create_group(&a.store, &ProjectGroupCreate { name: "Empty".into(), project: None }).unwrap();
    update_group(&a.store, &ProjectGroupUpdate { id: group.id, collapsed: Some(true), ..Default::default() }).unwrap();
    move_group(&a.store, &ProjectGroupMove { id: empty.id, before: Some(group.id) }).unwrap();
    move_project(&a.store, &ProjectMove { id: q, group_id: Some(group.id), before: Some(p) }).unwrap();
    a.connect(&url);
    assert!(a.sync().skipped.is_empty());
    let preview = b.connect(&url);
    assert_eq!(preview.new_here.iter().find(|k| k.kind == SyncKind::Projects).unwrap().count, 2);
    assert!(b.sync().skipped.is_empty());
    assert_eq!(list_groups(&b.store).unwrap(), list_groups(&a.store).unwrap());
    assert_eq!(get_project(&b.store, p).unwrap().group_id, Some(group.id));
    let placement = |d: &Dev| {
        list_projects(&d.store).unwrap().into_iter().map(|p| (p.id, p.group_id, p.order_index)).collect::<Vec<_>>()
    };
    assert_eq!(placement(&a), placement(&b));
    update_group(&b.store, &ProjectGroupUpdate { id: empty.id, name: Some("Renamed".into()), ..Default::default() })
        .unwrap();
    assert!(b.sync().skipped.is_empty());
    assert!(a.sync().skipped.is_empty());
    assert_eq!(list_groups(&a.store).unwrap()[0].name, "Renamed");
    delete_group(&a.store, group.id).unwrap();
    assert!(a.sync().skipped.is_empty());
    assert!(b.sync().skipped.is_empty());
    assert_eq!(list_groups(&b.store).unwrap(), list_groups(&a.store).unwrap());
    assert_eq!(placement(&a), placement(&b));
    assert_eq!(get_project(&b.store, p).unwrap().group_id, None);
    assert_eq!(list_projects(&b.store).unwrap().len(), 2);
    // An old client omitting organization metadata must not reset existing placement.
    let wiki = b.wiki();
    let cx = Cx { store: &b.store, home: &b.home, wiki_root: &wiki, env: &b.env, device: &b.device };
    let path = super::kinds::projects::path_of(p);
    let mut v = b.store.with(|c| super::kinds::render_shared(&cx, c)).unwrap().remove(&path).unwrap();
    v.as_object_mut().unwrap().remove("placement");
    let before = get_project(&b.store, p).unwrap();
    b.store
        .with(|c| {
            super::kinds::projects::apply(&cx, c, &path, Some(&v)).unwrap();
            Ok(())
        })
        .unwrap();
    assert_eq!(get_project(&b.store, p).unwrap().order_index, before.order_index);
    let c = Dev::new("C");
    c.connect(&url);
    assert!(c.sync().skipped.is_empty());
    assert_eq!(list_groups(&c.store).unwrap(), list_groups(&a.store).unwrap());
    assert_eq!(placement(&c), placement(&a));
}

impl Dev {
    fn new(name: &str) -> Dev {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        std::fs::create_dir_all(home.join("wiki")).unwrap();
        Dev {
            store: Store::open(&home.join("runwick.db")).unwrap(),
            home,
            _tmp: tmp,
            env: EnvService::new(Box::new(FakeStore::default())),
            device: Device { host_id: Uuid::now_v7(), name: name.into() },
            git: Git::new(GitEnv::process()).unwrap(),
        }
    }

    fn wiki(&self) -> PathBuf {
        self.home.join("wiki")
    }

    fn wc(&self) -> PathBuf {
        self.home.join("sync")
    }

    fn preview(&self, url: &str) -> (ConnectPreview, repo::Preview) {
        let dir = self.home.join("sync-preview").join(Uuid::new_v4().to_string());
        repo::preview(&self.git, &self.store, &self.home, &self.device, url, &dir).unwrap()
    }

    fn connect(&self, url: &str) -> ConnectPreview {
        let (out, pre) = self.preview(url);
        repo::install(&pre, &self.wc()).unwrap();
        state::set_base(&self.store, None).unwrap();
        state::clear_marks(&self.store).unwrap();
        out
    }

    fn sync_with(&self, step: &(dyn Fn(SyncStep) + Sync)) -> SyncReport {
        let wiki = self.wiki();
        let cx = Cx { store: &self.store, home: &self.home, wiki_root: &wiki, env: &self.env, device: &self.device };
        let wc = WorkingCopy { dir: self.wc(), branch: "main".into() };
        let engine = Engine { cx: &cx, git: &self.git, wc: &wc, step, deadline: None };
        match engine.run() {
            Ok(o) => o.report,
            Err(f) => panic!("{} sync failed at {:?}: {}", self.device.name, f.step, f.error.message),
        }
    }

    fn sync(&self) -> SyncReport {
        self.sync_with(&|_| {})
    }

    fn try_sync(&self) -> Result<SyncReport, super::engine::Failure> {
        let wiki = self.wiki();
        let cx = Cx { store: &self.store, home: &self.home, wiki_root: &wiki, env: &self.env, device: &self.device };
        let wc = WorkingCopy { dir: self.wc(), branch: "main".into() };
        Engine { cx: &cx, git: &self.git, wc: &wc, step: &|_| {}, deadline: None }.run().map(|o| o.report)
    }

    fn remote_files(&self) -> Vec<String> {
        self.git.ok(Some(&self.wc()), &["fetch", "-q", "origin"], super::git::LOCAL).unwrap();
        self.git.files(&self.wc(), "refs/remotes/origin/main").unwrap()
    }

    fn conflicts(&self) -> Vec<runwick_core::api::sync::SyncConflictItem> {
        self.store.with(|c| super::kinds::conflicts::list(c, &self.device)).unwrap()
    }

    /// A project with a real folder and its default workspace.
    fn project(&self, name: &str) -> (Uuid, Uuid) {
        let dir = self.home.parent().unwrap().join("code").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let o = crate::projects::open_folder(&self.store, dir.to_str().unwrap()).unwrap();
        (o.project.id, o.workspace.id)
    }

    fn session(&self, project: Uuid, ws: Uuid, title: &str) -> Uuid {
        let id = Uuid::now_v7();
        self.exec(
            "INSERT INTO sessions (id, project_id, workspace_id, title, tool, provider_conv_id, cwd, created_at)
             VALUES (?1, ?2, ?3, ?4, 'claude', ?5, '/tmp', ?6)",
            params![id, project, ws, title, Uuid::new_v4().to_string(), now()],
        );
        id
    }

    fn message(&self, session: Uuid, seq: i64, text: &str) -> Uuid {
        let id = Uuid::now_v7();
        let (p, w): (Uuid, Uuid) = self
            .store
            .with(|c| {
                c.query_row("SELECT project_id, workspace_id FROM sessions WHERE id = ?1", [session], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })
            })
            .unwrap();
        self.exec(
            "INSERT INTO messages (id, session_id, project_id, workspace_id, provider_msg_id, seq, role, text, at, collected_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'assistant', ?7, ?8, ?8)",
            params![id, session, p, w, format!("m{seq}"), seq, text, now()],
        );
        id
    }

    fn pin(&self, project: Uuid, title: &str) -> Uuid {
        let id = Uuid::now_v7();
        let t = now();
        self.exec(
            "INSERT INTO pins (id, kind, title, note, text, project_id, session_title, source_at, created_at, updated_at)
             VALUES (?1, 'excerpt', ?2, '', '고정한 내용', ?3, '세션', ?4, ?4, ?4)",
            params![id, title, project, t],
        );
        id
    }

    fn page(&self, project: Uuid, title: &str, body: &str) -> String {
        let p = ops::create(
            &self.store,
            &self.wiki(),
            ops::NewPage {
                project_id: project,
                title: title.into(),
                body: body.into(),
                tags: vec!["배포".into()],
                sources: Vec::new(),
                created_by: Author::User,
                status: PageStatus::Reviewed,
                saved_by: SavedBy::User,
                note: None,
                slug: None,
            },
        )
        .unwrap();
        p.summary.slug
    }

    fn edit_page(&self, project: Uuid, slug: &str, body: &str) {
        let page = ops::get(&self.store, &self.wiki(), project, slug).unwrap();
        let out = ops::save(
            &self.store,
            &self.wiki(),
            ops::Save {
                project_id: project,
                slug,
                base_version: page.summary.version,
                title: &page.summary.title,
                body,
                tags: None,
                mark_reviewed: false,
                add_sources: &[],
                saved_by: SavedBy::User,
                note: None,
            },
        )
        .unwrap();
        assert!(matches!(out, SaveOutcome::Saved { .. }));
    }

    fn page_body(&self, project: Uuid, slug: &str) -> String {
        ops::get(&self.store, &self.wiki(), project, slug).unwrap().body
    }

    fn exec(&self, sql: &str, p: impl rusqlite::Params) {
        self.store.with(|c| c.execute(sql, p)).unwrap();
    }

    fn count(&self, sql: &str) -> i64 {
        self.store.with(|c| c.query_row(sql, [], |r| r.get(0))).unwrap()
    }

    fn text(&self, sql: &str, id: Uuid) -> String {
        self.store.with(|c| c.query_row(sql, [id], |r| r.get(0))).unwrap()
    }

    fn with_cx<T>(&self, f: impl FnOnce(&Cx) -> T) -> T {
        let wiki = self.wiki();
        let cx = Cx { store: &self.store, home: &self.home, wiki_root: &wiki, env: &self.env, device: &self.device };
        f(&cx)
    }

    /// Files put into the repository by someone else, without Runwick.
    fn push_raw(&self, files: &[(String, String)]) {
        let wc = self.wc();
        for args in [&["fetch", "-q", "origin"][..], &["reset", "-q", "--hard", "origin/main"][..]] {
            self.git.ok(Some(&wc), args, super::git::LOCAL).unwrap();
        }
        for (p, text) in files {
            let f = wc.join(p);
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
            std::fs::write(f, text).unwrap();
        }
        for args in [
            &["add", "-A"][..],
            &["-c", "user.name=e", "-c", "user.email=e@e", "commit", "-qm", "raw"][..],
            &["push", "-q", "origin", "HEAD:main"][..],
        ] {
            self.git.ok(Some(&wc), args, super::git::LOCAL).unwrap();
        }
    }

    /// A file of the repository as it is at origin now.
    fn remote_text(&self, path: &str) -> String {
        self.remote_files();
        self.git
            .read_at(&self.wc(), "refs/remotes/origin/main", &[path.to_string()])
            .unwrap()
            .remove(path)
            .flatten()
            .map(|b| String::from_utf8_lossy(&b).to_string())
            .unwrap_or_default()
    }

    fn set_var(&self, key: &str, value: &str) {
        env::set(
            &self.store,
            &self.env,
            SetVar { key: key.into(), value: Some(value.into()), description: String::new(), secret: false },
        )
        .unwrap();
    }

    fn var(&self, key: &str) -> Option<String> {
        env::list(&self.store, &self.env).unwrap().vars.into_iter().find(|v| v.key == key).and_then(|v| v.value)
    }

    fn waiting(&self) -> Vec<runwick_core::api::sync::StagedVar> {
        super::kinds::env::staged(&self.store, &self.device).unwrap()
    }
}

#[test]
fn migration_placement_does_not_override_an_explicit_edit_on_an_upgraded_computer() {
    use super::format::{Prefer, merge3};
    let a = Dev::new("A");
    let p = Uuid::new_v4();
    a.exec("INSERT INTO projects (id, name, root, created_at) VALUES (?1, 'existing', '/missing/existing', '2000-01-01T00:00:00Z')", [p]);
    a.exec("INSERT INTO workspaces (id, project_id, name, path, kind, created_at) VALUES (?1, ?2, 'default', '/missing/existing', 'default', '2000-01-01T00:00:00Z')", params![Uuid::new_v4(), p]);
    let path = super::kinds::projects::path_of(p);
    let local = a.with_cx(|cx| a.store.with(|c| super::kinds::render_shared(cx, c))).unwrap().remove(&path).unwrap();
    let mut base = local.clone();
    base.as_object_mut().unwrap().remove("placement");
    let mut remote = base.clone();
    remote["placement"] =
        serde_json::json!({ "order_index": 42, "group_id": Uuid::new_v4(), "updated_at": "2000-01-02T00:00:00Z" });
    let merged = merge3(Some(&base), Some(&local), Some(&remote), Prefer::Newer).unwrap();
    assert_eq!(merged["placement"], remote["placement"]);
}

#[test]
fn restoring_organization_conflicts_is_a_new_edit_for_every_nested_record() {
    use super::format::{Prefer, canonical, merge3};
    use super::kinds::conflicts::{Found, Side, record, restore};
    use runwick_core::api::projects::ProjectGroupCreate;
    let a = Dev::new("A");
    let (p, _) = a.project("app");
    let g = crate::projects::create_group(&a.store, &ProjectGroupCreate { name: "Restored".into(), project: Some(p) })
        .unwrap();
    let records = a.with_cx(|cx| a.store.with(|c| super::kinds::render_shared(cx, c))).unwrap();
    for (path, title) in
        [(super::kinds::projects::path_of(p), "placement"), (super::kinds::projects::GROUPS_PATH.into(), "groups")]
    {
        let mut lost = records[&path].clone();
        super::kinds::projects::stamp_restored(&mut lost, "2000-01-01T00:00:00Z");
        let mut base = lost.clone();
        let mut remote = lost.clone();
        if title == "placement" {
            base["placement"]["order_index"] = serde_json::json!(1);
            remote["placement"]["order_index"] = serde_json::json!(2);
        } else {
            base["groups"][g.id.to_string()]["name"] = serde_json::json!("Kept");
            remote["groups"][g.id.to_string()]["name"] = serde_json::json!("Intermediate");
        }
        super::kinds::projects::stamp_restored(&mut base, "2000-01-02T00:00:00Z");
        super::kinds::projects::stamp_restored(&mut remote, "2000-01-03T00:00:00Z");
        a.with_cx(|cx| {
            a.store.tx(|c| {
                super::kinds::projects::apply(cx, c, &path, Some(&base)).unwrap();
                record(
                    cx,
                    c,
                    &Found {
                        kind: SyncKind::Projects,
                        path: path.clone(),
                        title: title.into(),
                        lost: Side::Local,
                        content: canonical(&lost),
                    },
                    &(None, "B".into()),
                )
            })
        })
        .unwrap();
        let conflict = a.conflicts().into_iter().find(|c| c.title == title).unwrap();
        a.with_cx(|cx| restore(cx, conflict.id)).unwrap();
        let local =
            a.with_cx(|cx| a.store.with(|c| super::kinds::render_shared(cx, c))).unwrap().remove(&path).unwrap();
        let merged = merge3(Some(&base), Some(&local), Some(&remote), Prefer::Newer).unwrap();
        if title == "placement" {
            assert_eq!(merged["placement"]["order_index"], lost["placement"]["order_index"]);
        } else {
            assert_eq!(merged["groups"][g.id.to_string()]["name"], lost["groups"][g.id.to_string()]["name"]);
        }
    }
}

fn now() -> String {
    ts_to_sql(&runwick_core::now())
}

fn later() {
    // Timestamps have millisecond resolution; keep "newer" unambiguous.
    std::thread::sleep(std::time::Duration::from_millis(5));
}

fn imported(r: &SyncReport, kind: SyncKind) -> u64 {
    r.imported.iter().find(|k| k.kind == kind).map(|k| k.count).unwrap_or(0)
}

/// Everything ever pushed, as text (history included).
fn repo_history(dev: &Dev) -> String {
    dev.git.text(&dev.wc(), &["log", "-p", "--all", "--format=%H"]).unwrap()
}

#[test]
fn a_new_computer_gets_everything_except_secrets() {
    let (_r, url) = bare_repo();
    let a = Dev::new("MacBook Pro");
    let (p, ws) = a.project("앱");
    let s = a.session(p, ws, "로그인 고치기");
    for i in 0..3 {
        a.message(s, i, &format!("답변 {i}"));
    }
    let pin = a.pin(p, "중요한 결정");
    let slug = a.page(p, "배포 절차", "# 배포 절차\n\n1. 빌드\n");
    a.exec(
        "INSERT INTO roles (id, name, instructions, procedure, created_at, updated_at) VALUES (?1, '리뷰', '꼼꼼히', '[\"테스트\"]', ?2, ?2)",
        params![Uuid::now_v7(), now()],
    );
    a.exec(
        "INSERT INTO project_notes (project_id, memo, items, updated_at) VALUES (?1, '내일 배포', '[]', ?2)",
        params![p, now()],
    );
    env::set(
        &a.store,
        &a.env,
        SetVar { key: "API_BASE".into(), value: Some("https://x".into()), description: "주소".into(), secret: false },
    )
    .unwrap();
    env::set(
        &a.store,
        &a.env,
        SetVar {
            key: "GITHUB_TOKEN".into(),
            value: Some("ghp_SECRET_never_synced".into()),
            description: String::new(),
            secret: true,
        },
    )
    .unwrap();

    for f in ["gui.lock", "menubar.lock", "host.pid", "logs/host.log", "screens/s.bin", "secrets/env.json"] {
        let p = a.home.join(f);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, "runtime state").unwrap();
    }
    let pre = a.connect(&url);
    assert_eq!(pre.content, RepoContent::Empty);
    let r = a.sync();
    assert!(r.commit.is_some());
    // The data repository holds records only: no runtime files, logs, screens or secrets.
    let known = [
        "projects/",
        "sessions/",
        "messages/",
        "pins/",
        "wiki/",
        "timeline/",
        "roles/",
        "settings/",
        "collab/",
        "tombstones/",
    ];
    for f in a.git.files(&a.wc(), "HEAD").unwrap() {
        assert!(
            f == "manifest.json" || f == "README.md" || known.iter().any(|k| f.starts_with(k)),
            "unexpected file in the data repository: {f}"
        );
    }
    assert!(!repo_history(&a).contains("ghp_SECRET_never_synced"), "secret value reached the repository");

    let b = Dev::new("Mac mini");
    let pre = b.preview(&url).0;
    assert_eq!(pre.content, RepoContent::Runwick);
    let new = |k: SyncKind| pre.new_here.iter().find(|c| c.kind == k).map(|c| c.count).unwrap_or(0);
    assert_eq!(new(SyncKind::Messages), 3);
    assert_eq!(new(SyncKind::Projects), 1);
    assert_eq!(pre.unlinked_projects, vec!["앱".to_string()]);
    assert!(pre.devices.iter().any(|d| d.name == "MacBook Pro" && !d.this));

    b.connect(&url);
    let r = b.sync();
    assert_eq!(imported(&r, SyncKind::Messages), 3);
    assert_eq!(b.count("SELECT COUNT(*) FROM projects"), 1);
    let root = b.text("SELECT root FROM projects WHERE id = ?1", p);
    assert!(super::kinds::projects::is_unlinked(&b.home, &root), "project without a folder here: {root}");
    assert_eq!(b.text("SELECT origin_device FROM sessions WHERE id = ?1", s), "MacBook Pro");
    assert_eq!(b.count("SELECT COUNT(*) FROM sessions WHERE provider_conv_id IS NOT NULL"), 0);
    assert_eq!(b.text("SELECT title FROM pins WHERE id = ?1", pin), "중요한 결정");
    assert!(b.page_body(p, &slug).contains("1. 빌드"));
    assert_eq!(b.count("SELECT COUNT(*) FROM roles WHERE name = '리뷰'"), 1);
    assert_eq!(b.text("SELECT memo FROM project_notes WHERE project_id = ?1", p), "내일 배포");
    // A value from another computer waits for review; accepting it uses it here.
    assert!(!env::list(&b.store, &b.env).unwrap().vars.iter().any(|v| v.key == "API_BASE"));
    super::kinds::env::decide(&b.store, &b.env, "API_BASE", true).unwrap();
    let st = env::list(&b.store, &b.env).unwrap();
    let api = st.vars.iter().find(|v| v.key == "API_BASE").unwrap();
    assert_eq!(api.value.as_deref(), Some("https://x"));
    let tok = st.vars.iter().find(|v| v.key == "GITHUB_TOKEN").unwrap();
    assert!(tok.secret && tok.value.is_none());
    assert!(env::reveal(&b.store, &b.env, "GITHUB_TOKEN").is_err(), "the secret must be entered on this computer");

    // Unlinked projects are listed with where they live elsewhere.
    let unlinked = b.store.with(|c| super::kinds::projects::unlinked(&b.home, c, &b.device)).unwrap();
    assert_eq!(unlinked.len(), 1);
    assert!(unlinked[0].elsewhere[0].starts_with("MacBook Pro · "));

    // Nothing changed: syncing again imports and writes nothing and makes no commit.
    let head = b.git.rev(&b.wc(), "HEAD").unwrap();
    let again = b.sync();
    assert!(again.imported.is_empty() && again.exported.is_empty(), "{again:?}");
    assert_eq!(b.git.rev(&b.wc(), "HEAD").unwrap(), head);
    let back = a.sync();
    assert!(back.imported.is_empty(), "A got its own records back: {back:?}");
    // Receiving only (B's device entry) moves A to the remote without a commit of its own.
    let b_head = b.git.rev(&b.wc(), "HEAD").unwrap();
    assert_eq!(a.git.rev(&a.wc(), "HEAD").unwrap(), b_head);
    a.sync();
    assert_eq!(a.git.rev(&a.wc(), "HEAD").unwrap(), b_head);
    assert!(!repo_history(&b).contains("ghp_SECRET_never_synced"));
}

#[test]
fn edits_on_both_computers_merge_by_field_and_newer_wins_on_the_same_field() {
    let (_r, url) = bare_repo();
    let a = Dev::new("A");
    let (p, _) = a.project("앱");
    let pin = a.pin(p, "처음 제목");
    a.connect(&url);
    a.sync();
    let b = Dev::new("B");
    b.connect(&url);
    b.sync();

    // Different fields of the same records.
    a.exec("UPDATE projects SET name = '새 이름' WHERE id = ?1", [p]);
    b.exec("UPDATE projects SET favorite = 1 WHERE id = ?1", [p]);
    a.exec("UPDATE pins SET note = 'A의 메모', updated_at = ?2 WHERE id = ?1", params![pin, now()]);
    later();
    b.exec("UPDATE pins SET title = 'B의 제목', updated_at = ?2 WHERE id = ?1", params![pin, now()]);
    a.sync();
    let r = b.sync();
    // Different fields merge without losing anything: nothing to report.
    assert!(r.conflicts.is_empty(), "{r:?}");
    a.sync();
    for d in [&a, &b] {
        assert_eq!(d.text("SELECT name FROM projects WHERE id = ?1", p), "새 이름");
        assert_eq!(d.count("SELECT favorite FROM projects"), 1);
        assert_eq!(d.text("SELECT note FROM pins WHERE id = ?1", pin), "A의 메모");
        assert_eq!(d.text("SELECT title FROM pins WHERE id = ?1", pin), "B의 제목");
    }

    // The same field: the later change wins on both computers.
    b.exec("UPDATE pins SET note = 'B가 먼저', updated_at = ?2 WHERE id = ?1", params![pin, now()]);
    later();
    a.exec("UPDATE pins SET note = 'A가 나중', updated_at = ?2 WHERE id = ?1", params![pin, now()]);
    b.sync();
    let r = a.sync();
    b.sync();
    for d in [&a, &b] {
        assert_eq!(d.text("SELECT note FROM pins WHERE id = ?1", pin), "A가 나중");
    }
    // The earlier text is not dropped: it is kept as a conflict on both computers.
    assert!(r.conflicts.iter().any(|c| c.kind == SyncKind::Pins), "{r:?}");
    assert!(a.conflicts().iter().any(|c| c.lost_text.contains("B가 먼저")));
    assert!(b.conflicts().iter().any(|c| c.lost_here && c.lost_text.contains("B가 먼저")));
}

#[test]
fn a_wiki_page_changed_on_both_keeps_local_text_and_offers_the_other_as_a_proposal() {
    let (_r, url) = bare_repo();
    let a = Dev::new("A");
    let (p, _) = a.project("앱");
    let slug = a.page(p, "배포 절차", "# 배포 절차\n\n공통\n");
    a.connect(&url);
    a.sync();
    let b = Dev::new("B");
    b.connect(&url);
    b.sync();

    a.edit_page(p, &slug, "# 배포 절차\n\nA가 쓴 내용\n");
    b.edit_page(p, &slug, "# 배포 절차\n\nB가 쓴 내용\n");
    a.sync();
    let r = b.sync();
    assert!(r.conflicts.iter().any(|c| c.kind == SyncKind::Wiki && c.resolution.contains("제안")), "{r:?}");
    assert!(b.page_body(p, &slug).contains("B가 쓴 내용"));
    let proposal: String = b
        .store
        .with(|c| c.query_row("SELECT body FROM wiki_proposals WHERE status = 'open'", [], |r| r.get(0)))
        .unwrap();
    assert!(proposal.contains("A가 쓴 내용"));
    let rationale: String =
        b.store.with(|c| c.query_row("SELECT rationale FROM wiki_proposals", [], |r| r.get(0))).unwrap();
    assert!(rationale.starts_with("다른 컴퓨터의 변경"));

    // A had not changed since: it takes the kept text, and its own text stays in its history.
    a.sync();
    assert!(a.page_body(p, &slug).contains("B가 쓴 내용"));
    let versions = ops::history(&a.store, &a.wiki(), p, &slug).unwrap();
    assert!(versions.len() >= 3);
    // Syncing again does not add the same proposal twice.
    b.sync();
    assert_eq!(b.count("SELECT COUNT(*) FROM wiki_proposals"), 1);

    // A page added on B arrives on A; a page removed on A is removed on B.
    let other = b.page(p, "새 문서", "# 새 문서\n\n내용\n");
    b.sync();
    a.sync();
    assert!(a.page_body(p, &other).contains("내용"));
    std::fs::remove_file(a.wiki().join(p.to_string()).join(format!("{other}.md"))).unwrap();
    a.sync();
    b.sync();
    assert!(ops::get(&b.store, &b.wiki(), p, &other).is_err());
}

#[test]
fn messages_are_a_union_and_never_duplicated() {
    let (_r, url) = bare_repo();
    let a = Dev::new("A");
    let (p, ws) = a.project("앱");
    let s = a.session(p, ws, "A의 세션");
    a.message(s, 0, "첫 답");
    a.connect(&url);
    a.sync();
    let b = Dev::new("B");
    b.connect(&url);
    b.sync();

    a.message(s, 1, "둘째 답");
    a.message(s, 2, "셋째 답");
    // B works in the same project with its own session.
    let bws = b.store.with(|c| super::kinds::sessions::default_workspace(c, p)).unwrap();
    let bs = b.session(p, bws, "B의 세션");
    b.message(bs, 0, "B의 답");
    a.sync();
    let r = b.sync();
    assert_eq!(imported(&r, SyncKind::Messages), 2);
    a.sync();
    assert_eq!(a.count("SELECT COUNT(*) FROM messages"), 4);
    assert_eq!(b.count("SELECT COUNT(*) FROM messages"), 4);
    assert_eq!(a.text("SELECT origin_device FROM sessions WHERE id = ?1", bs), "B");

    // Re-importing everything from scratch (a lost working copy) adds nothing.
    std::fs::remove_dir_all(b.wc()).unwrap();
    b.connect(&url);
    let r = b.sync();
    assert_eq!(imported(&r, SyncKind::Messages), 0, "{r:?}");
    assert_eq!(b.count("SELECT COUNT(*) FROM messages"), 4);
    assert_eq!(b.count("SELECT COUNT(*) FROM sessions"), 2);
    assert_eq!(b.count("SELECT COUNT(*) FROM projects"), 1);

    // Messages are found by search on the other computer.
    let hits = b.count(r#"SELECT COUNT(*) FROM messages_fts WHERE messages_fts MATCH '"셋째 답"'"#);
    assert_eq!(hits, 1);
    // B never writes A's session files.
    let file = super::kinds::sessions::path_of(p, s);
    let log = b.git.text(&b.wc(), &["log", "--format=%an", "--", &file]).unwrap();
    assert!(log.lines().all(|l| l == "A"), "{log}");
}

#[test]
fn removals_travel_as_tombstones_even_to_a_computer_joining_later() {
    let (_r, url) = bare_repo();
    let a = Dev::new("A");
    let (p, _) = a.project("앱");
    let keep = a.pin(p, "남길 핀");
    let gone = a.pin(p, "지울 핀");
    let role = Uuid::now_v7();
    a.exec(
        "INSERT INTO roles (id, name, instructions, procedure, created_at, updated_at) VALUES (?1, '검토', '', '[]', ?2, ?2)",
        params![role, now()],
    );
    a.connect(&url);
    a.sync();
    let b = Dev::new("B");
    b.connect(&url);
    b.sync();
    // An older copy of the same records, restored on a third computer before it joins.
    let c = Dev::new("C");
    let (gone_row, keep_row): (String, String) =
        (a.text("SELECT title FROM pins WHERE id = ?1", gone), a.text("SELECT title FROM pins WHERE id = ?1", keep));
    assert_eq!((gone_row.as_str(), keep_row.as_str()), ("지울 핀", "남길 핀"));
    let old = "2020-01-01T00:00:00.000Z";
    c.exec(
        "INSERT INTO pins (id, kind, title, note, text, session_title, source_at, created_at, updated_at)
         VALUES (?1, 'excerpt', '지울 핀', '', '내용', '', ?2, ?2, ?2)",
        params![gone, old],
    );

    later();
    a.exec("DELETE FROM pins WHERE id = ?1", [gone]);
    a.exec("DELETE FROM roles WHERE id = ?1", [role]);
    a.sync();
    let tomb = a.wc().join("tombstones").join("pins").join(format!("{gone}.json"));
    assert!(tomb.is_file());
    assert!(!a.wc().join("pins").join(format!("{gone}.json")).exists());
    b.sync();
    assert_eq!(b.count("SELECT COUNT(*) FROM pins"), 1);
    assert_eq!(b.count("SELECT COUNT(*) FROM roles"), 0);

    c.connect(&url);
    c.sync();
    assert_eq!(c.count("SELECT COUNT(*) FROM pins WHERE title = '지울 핀'"), 0, "tombstone not applied on join");
    assert_eq!(c.count("SELECT COUNT(*) FROM pins WHERE title = '남길 핀'"), 1);
    // Applying a tombstone does not write another one.
    assert!(state::deleted(&c.store).unwrap().is_empty());
    assert_eq!(c.git.text(&c.wc(), &["status", "--porcelain"]).unwrap(), "");
}

#[test]
fn a_push_race_is_retried_and_loses_nothing() {
    let (_r, url) = bare_repo();
    let a = Dev::new("A");
    let (p, _) = a.project("앱");
    a.connect(&url);
    a.sync();
    let b = Dev::new("B");
    b.connect(&url);
    b.sync();

    let pa = a.pin(p, "A의 핀");
    let pb = b.pin(p, "B의 핀");
    // A pushes while B is about to push.
    let raced = AtomicBool::new(false);
    let r = b.sync_with(&|step| {
        if step == SyncStep::Push && !raced.swap(true, Ordering::SeqCst) {
            a.sync();
        }
    });
    assert_eq!(r.attempts, 2, "{r:?}");
    a.sync();
    for d in [&a, &b] {
        assert_eq!(d.text("SELECT title FROM pins WHERE id = ?1", pa), "A의 핀");
        assert_eq!(d.text("SELECT title FROM pins WHERE id = ?1", pb), "B의 핀");
    }
    // The remote history has both computers' commits and no textual merge.
    let log = a.git.text(&a.wc(), &["log", "--format=%an %s"]).unwrap();
    assert!(log.contains("A ") && log.contains("B "), "{log}");
}

#[test]
fn foreign_repositories_and_newer_formats_are_refused() {
    let (_r, url) = bare_repo();
    let seed = tempfile::tempdir().unwrap();
    let git = Git::new(GitEnv::process()).unwrap();
    let dir = seed.path().join("w");
    git.ok(None, &["clone", "-q", &url, dir.to_str().unwrap()], super::git::LOCAL).unwrap();
    std::fs::write(dir.join("manifest.json"), r#"{"format":"runwick-sync","version":99}"#).unwrap();
    let id = ["-c", "user.name=t", "-c", "user.email=t@t"];
    let mut args = id.to_vec();
    args.extend(["commit", "-q", "-m", "x"]);
    git.ok(Some(&dir), &["add", "-A"], super::git::LOCAL).unwrap();
    git.ok(Some(&dir), &args, super::git::LOCAL).unwrap();
    git.ok(Some(&dir), &["push", "-q", "origin", "HEAD:refs/heads/main"], super::git::LOCAL).unwrap();

    let a = Dev::new("A");
    let (pre, p) = a.preview(&url);
    assert_eq!(pre.content, RepoContent::Newer);
    assert!(repo::install(&p, &a.wc()).is_err());
    assert!(!a.wc().exists());
    // A missing repository is a clear error kind, never a crash.
    let missing = Path::new(&url).with_file_name("nope.git");
    let err = repo::preview(&a.git, &a.store, &a.home, &a.device, missing.to_str().unwrap(), &a.home.join("x"))
        .err()
        .unwrap();
    assert_eq!(err.kind, runwick_core::api::sync::FailKind::NotFound);
}

#[test]
fn a_reconnected_working_copy_is_a_union_without_losing_local_changes() {
    let (_r, url) = bare_repo();
    let a = Dev::new("A");
    let (p, _) = a.project("앱");
    let pin = a.pin(p, "처음");
    a.connect(&url);
    a.sync();
    // Disconnect, change things locally, connect again: local edits survive, nothing duplicates.
    state::disconnect(&a.store).unwrap();
    std::fs::remove_dir_all(a.wc()).unwrap();
    later();
    a.exec("UPDATE pins SET title = '바꾼 제목', updated_at = ?2 WHERE id = ?1", params![pin, now()]);
    a.connect(&url);
    a.sync();
    assert_eq!(a.text("SELECT title FROM pins WHERE id = ?1", pin), "바꾼 제목");
    assert_eq!(a.count("SELECT COUNT(*) FROM pins"), 1);
    let b = Dev::new("B");
    b.connect(&url);
    b.sync();
    assert_eq!(b.text("SELECT title FROM pins WHERE id = ?1", pin), "바꾼 제목");
}

fn fake_repo(dir: &Path, origin: &str) {
    std::fs::create_dir_all(dir.join(".git")).unwrap();
    std::fs::write(dir.join(".git").join("HEAD"), "ref: refs/heads/main\n").unwrap();
    std::fs::write(dir.join(".git").join("config"), format!("[remote \"origin\"]\n\turl = {origin}\n")).unwrap();
}

#[test]
fn opening_the_same_repository_links_the_synced_project() {
    let (_r, url) = bare_repo();
    let a = Dev::new("MacBook Pro");
    let dir_a = a.home.parent().unwrap().join("app");
    fake_repo(&dir_a, "git@github.com:me/app.git");
    let opened = crate::projects::open_folder(&a.store, dir_a.to_str().unwrap()).unwrap();
    let p = opened.project.id;
    let s = a.session(p, opened.workspace.id, "A의 세션");
    a.connect(&url);
    a.sync();
    let b = Dev::new("Mac mini");
    b.connect(&url);
    b.sync();

    let dir_b = b.home.parent().unwrap().join("checkout");
    fake_repo(&dir_b, "https://github.com/me/app");
    let dir_s = dir_b.canonicalize().unwrap().to_string_lossy().to_string();
    let (pid, ws) = super::kinds::projects::link_folder(&b.store, &b.home, &dir_s, true, "https://github.com/me/app")
        .unwrap()
        .unwrap();
    assert_eq!(pid, p);
    assert_eq!(b.text("SELECT root FROM projects WHERE id = ?1", p), dir_s);
    assert_eq!(b.text("SELECT path FROM workspaces WHERE id = ?1", ws), dir_s);
    // The synced session sits in that workspace and stays another computer's record.
    assert_eq!(b.text("SELECT origin_device FROM sessions WHERE id = ?1", s), "MacBook Pro");
    let unlinked = b.store.with(|c| super::kinds::projects::unlinked(&b.home, c, &b.device)).unwrap();
    assert!(unlinked.is_empty(), "{unlinked:?}");
    b.sync();
    a.sync();
    // A now knows where the project lives on B.
    assert_eq!(a.count("SELECT COUNT(*) FROM sync_roots"), 1);
}

#[test]
fn only_finished_delegation_records_travel() {
    let (_r, url) = bare_repo();
    let a = Dev::new("A");
    let (p, ws) = a.project("앱");
    let parent = a.session(p, ws, "맡긴 세션");
    let worker = a.session(p, ws, "작업자");
    for (status, title) in [("done", "끝난 일"), ("running", "하는 중인 일")] {
        a.exec(
            "INSERT INTO collab_requests (id, parent_id, worker_id, title, tool, prompt, source, via, status, created_at, updated_at, result)
             VALUES (?1, ?2, ?3, ?4, 'claude', '검토해 줘', 'agent', 'runwick', ?5, ?6, ?6, '{\"summary\":\"좋아요\"}')",
            params![Uuid::now_v7(), parent, worker, title, status, now()],
        );
    }
    a.connect(&url);
    a.sync();
    let b = Dev::new("B");
    b.connect(&url);
    let r = b.sync();
    assert_eq!(imported(&r, SyncKind::Collab), 1, "{r:?}");
    assert_eq!(b.count("SELECT COUNT(*) FROM collab_requests WHERE title = '끝난 일' AND status = 'done'"), 1);
    assert_eq!(b.count("SELECT COUNT(*) FROM collab_requests WHERE status = 'running'"), 0);
    // Nothing comes back changed.
    assert!(a.sync().imported.is_empty());
}

#[test]
fn a_slow_remote_is_cut_at_the_deadline_and_nothing_is_half_written() {
    let (_r, url) = bare_repo();
    let a = Dev::new("A");
    let (p, _) = a.project("앱");
    a.connect(&url);
    a.sync();
    let pin = a.pin(p, "종료 직전 핀");
    let base = state::base(&a.store);
    let head = a.git.rev(&a.wc(), "HEAD").unwrap();

    // A remote whose transport never answers (as on a dead network at shutdown).
    a.git
        .ok(Some(&a.wc()), &["remote", "set-url", "origin", "ssh://slow.invalid/data.git"], super::git::LOCAL)
        .unwrap();
    let mut env = GitEnv::process();
    env.vars.insert("GIT_SSH_COMMAND".into(), "sleep 30".into());
    let slow = Git::new(env).unwrap();
    let wiki = a.wiki();
    let cx = Cx { store: &a.store, home: &a.home, wiki_root: &wiki, env: &a.env, device: &a.device };
    let wc = WorkingCopy { dir: a.wc(), branch: "main".into() };
    let started = std::time::Instant::now();
    let engine = Engine {
        cx: &cx,
        git: &slow,
        wc: &wc,
        step: &|_| {},
        deadline: Some(started + std::time::Duration::from_secs(2)),
    };
    let err = match engine.run() {
        Ok(_) => panic!("a sync through a dead remote cannot succeed"),
        Err(e) => e,
    };
    assert!(started.elapsed() < std::time::Duration::from_secs(8), "waited {:?}", started.elapsed());
    assert_eq!(err.step, SyncStep::Fetch);
    // Nothing moved: no commit, no half-written working copy, the merge base and records intact.
    assert_eq!(a.git.rev(&a.wc(), "HEAD").unwrap(), head);
    assert_eq!(a.git.text(&a.wc(), &["status", "--porcelain"]).unwrap(), "");
    assert_eq!(state::base(&a.store), base);
    assert_eq!(a.text("SELECT title FROM pins WHERE id = ?1", pin), "종료 직전 핀");

    // A deadline that passes between steps stops before anything is committed.
    let engine = Engine { cx: &cx, git: &a.git, wc: &wc, step: &|_| {}, deadline: Some(std::time::Instant::now()) };
    assert!(engine.run().is_err());
    assert_eq!(a.git.rev(&a.wc(), "HEAD").unwrap(), head);

    // The change is still pending and goes up with the next sync.
    a.git.ok(Some(&a.wc()), &["remote", "set-url", "origin", &url], super::git::LOCAL).unwrap();
    let r = a.sync();
    assert!(r.exported.iter().any(|k| k.kind == SyncKind::Pins), "{r:?}");
    let b = Dev::new("B");
    b.connect(&url);
    b.sync();
    assert_eq!(b.text("SELECT title FROM pins WHERE id = ?1", pin), "종료 직전 핀");
}

// Regression tests for the critic findings.

#[test]
fn a_failed_export_never_makes_a_later_push_delete_another_computers_records() {
    use std::os::unix::fs::PermissionsExt;
    let (_r, url) = bare_repo();
    let a = Dev::new("A");
    let (p, ws) = a.project("앱");
    a.connect(&url);
    a.sync();
    let b = Dev::new("B");
    b.connect(&url);
    b.sync();
    // A's newest work arrives on B, but B's export fails (no space / killed host).
    let s = a.session(p, ws, "노트북 마지막 작업");
    a.message(s, 0, "중요한 결론");
    a.sync();
    b.exec("UPDATE projects SET name = 'B 이름' WHERE id = ?1", [p]);
    let dir = b.wc().join("projects");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
    let failed = b.try_sync();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(failed.is_err());
    // The next sync still carries A's session and messages.
    b.sync();
    let files = b.remote_files();
    let session_file = super::kinds::sessions::path_of(p, s);
    assert!(files.contains(&session_file), "B's push dropped A's session: {files:?}");
    assert!(files.contains(&super::kinds::messages::path_of(p, s)));
    let c = Dev::new("C");
    c.connect(&url);
    c.sync();
    assert_eq!(c.count("SELECT COUNT(*) FROM messages"), 1);
}

#[test]
fn a_push_never_removes_files_this_computer_does_not_own() {
    let (_r, url) = bare_repo();
    let a = Dev::new("A");
    let (p, ws) = a.project("앱");
    let s = a.session(p, ws, "A의 세션");
    a.message(s, 0, "답");
    a.connect(&url);
    a.sync();
    let b = Dev::new("B");
    b.connect(&url);
    b.sync();
    // Simulate a broken working copy on B: A's files gone from B's tree.
    for f in [super::kinds::sessions::path_of(p, s), super::kinds::messages::path_of(p, s)] {
        std::fs::remove_file(b.wc().join(&f)).unwrap();
    }
    b.git
        .ok(
            Some(&b.wc()),
            &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qam", "broken"],
            super::git::LOCAL,
        )
        .unwrap();
    b.pin(p, "B의 핀");
    let r = b.sync();
    assert!(r.conflicts.iter().any(|c| c.resolution.contains("지우지 않도록")), "{r:?}");
    let files = b.remote_files();
    assert!(files.contains(&super::kinds::sessions::path_of(p, s)));
    assert!(files.contains(&super::kinds::messages::path_of(p, s)));
}

#[test]
fn both_sides_changed_content_is_recorded_on_both_computers_and_can_be_restored() {
    let (_r, url) = bare_repo();
    let a = Dev::new("A");
    let (p, _) = a.project("앱");
    let pin = a.pin(p, "핀");
    a.exec(
        "INSERT INTO project_notes (project_id, memo, items, updated_at) VALUES (?1, '처음 메모', '[]', ?2)",
        params![p, now()],
    );
    a.connect(&url);
    a.sync();
    let b = Dev::new("B");
    b.connect(&url);
    b.sync();
    a.exec("UPDATE pins SET note = 'A의 메모', updated_at = ?2 WHERE id = ?1", params![pin, now()]);
    a.exec("UPDATE project_notes SET memo = 'A가 쓴 메모', updated_at = ?2 WHERE project_id = ?1", params![p, now()]);
    later();
    b.exec("UPDATE pins SET note = 'B의 메모', updated_at = ?2 WHERE id = ?1", params![pin, now()]);
    b.exec("UPDATE project_notes SET memo = 'B가 쓴 메모', updated_at = ?2 WHERE project_id = ?1", params![p, now()]);
    a.sync();
    let r = b.sync();
    assert!(r.conflicts.iter().any(|c| c.kind == SyncKind::Pins), "{r:?}");
    // B kept its newer text; A's text is recorded, not dropped.
    assert_eq!(b.text("SELECT note FROM pins WHERE id = ?1", pin), "B의 메모");
    let on_b = b.conflicts();
    assert!(on_b.iter().any(|c| c.kind == SyncKind::Pins && c.lost_text.contains("A의 메모") && !c.lost_here));
    assert!(on_b.iter().any(|c| c.kind == SyncKind::Timeline && c.lost_text.contains("A가 쓴 메모")));
    // A is told its edit was replaced, and sees the same record.
    let r = a.sync();
    assert!(r.conflicts.iter().any(|c| c.resolution.contains("다른 컴퓨터의 변경으로 바뀌었어요")), "{r:?}");
    let on_a = a.conflicts();
    let lost = on_a.iter().find(|c| c.kind == SyncKind::Pins).unwrap();
    assert!(lost.lost_here && lost.lost_text.contains("A의 메모"));
    // Restoring it is a new edit that syncs back; what it replaces is kept the same way.
    a.with_cx(|cx| super::kinds::conflicts::restore(cx, lost.id)).unwrap();
    assert_eq!(a.text("SELECT note FROM pins WHERE id = ?1", pin), "A의 메모");
    let on_a = a.conflicts();
    assert!(!on_a.iter().any(|c| c.id == lost.id), "settled");
    let replaced = on_a
        .iter()
        .find(|c| c.kind == SyncKind::Pins && c.lost_text.contains("B의 메모"))
        .expect("the replaced text is recorded")
        .clone();
    assert!(replaced.found_here && replaced.lost_device == "B" && !replaced.lost_here);
    assert!(
        a.with_cx(|cx| super::kinds::conflicts::restore(cx, lost.id)).is_err(),
        "a settled conflict is not offered again"
    );
    a.sync();
    b.sync();
    assert_eq!(b.text("SELECT note FROM pins WHERE id = ?1", pin), "A의 메모");
    // B stops offering the settled conflict and is told its text was replaced, with the text.
    let on_b = b.conflicts();
    assert!(!on_b.iter().any(|c| c.id == lost.id), "{on_b:?}");
    let told = on_b.iter().find(|c| c.id == replaced.id).unwrap();
    assert!(told.lost_here && !told.found_here, "{told:?}");
    // Closing it on B closes it on A too.
    b.with_cx(|cx| b.store.with(|c| super::kinds::conflicts::settle(cx, c, replaced.id, "dismissed"))).unwrap();
    b.sync();
    a.sync();
    assert!(!a.conflicts().iter().any(|c| c.id == replaced.id));
}

#[test]
fn a_restored_backup_wins_the_next_sync_and_newer_remote_content_is_kept_as_a_conflict() {
    let (_r, url) = bare_repo();
    let a = Dev::new("A");
    let (p, _) = a.project("앱");
    let pin = a.pin(p, "핀");
    a.exec("UPDATE pins SET note = '좋은 메모 v1' WHERE id = ?1", [pin]);
    a.connect(&url);
    a.sync();
    later();
    a.exec("UPDATE pins SET note = '망가진 메모 v2', updated_at = ?2 WHERE id = ?1", params![pin, now()]);
    a.sync();
    // What a restore leaves: older records, no merge base, the restore marker.
    a.exec("UPDATE pins SET note = '좋은 메모 v1', updated_at = '2020-01-01T00:00:00.000Z' WHERE id = ?1", [pin]);
    state::set_base(&a.store, None).unwrap();
    a.exec("INSERT INTO kv (key, value, updated_at) VALUES (?1, 'true', ?2)", params![state::K_RESTORED, now()]);
    a.sync();
    assert_eq!(a.text("SELECT note FROM pins WHERE id = ?1", pin), "좋은 메모 v1");
    assert!(!state::restored(&a.store));
    assert!(a.conflicts().iter().any(|c| c.lost_text.contains("망가진 메모 v2")));
    let b = Dev::new("B");
    b.connect(&url);
    b.sync();
    assert_eq!(b.text("SELECT note FROM pins WHERE id = ?1", pin), "좋은 메모 v1");
}

#[test]
fn renames_of_the_same_length_are_pending() {
    let (_r, url) = bare_repo();
    let a = Dev::new("A");
    let (p, ws) = a.project("앱");
    let s = a.session(p, ws, "가나다");
    a.connect(&url);
    a.sync();
    let before = state::fingerprint(&a.store).unwrap();
    a.exec("UPDATE sessions SET title = '라마바' WHERE id = ?1", [s]);
    assert_ne!(state::fingerprint(&a.store).unwrap(), before);
    let before = state::fingerprint(&a.store).unwrap();
    a.exec("UPDATE projects SET name = '웹' WHERE id = ?1", [p]);
    assert_ne!(state::fingerprint(&a.store).unwrap(), before);
}

#[test]
fn files_that_could_not_be_imported_are_retried_until_they_are() {
    let (_r, url) = bare_repo();
    let a = Dev::new("A");
    let (p, ws) = a.project("앱");
    a.connect(&url);
    a.sync();
    let b = Dev::new("B");
    b.connect(&url);
    b.sync();
    let s = a.session(p, ws, "세션");
    a.message(s, 0, "다시 시도할 답");
    a.sync();
    // A broken session file upstream: B cannot import the session, so not its messages.
    let fixer = Dev::new("F");
    fixer.connect(&url);
    let sf = fixer.wc().join(super::kinds::sessions::path_of(p, s));
    let good = std::fs::read_to_string(&sf).unwrap();
    std::fs::write(&sf, "{ broken").unwrap();
    let push = |msg: &str| {
        fixer
            .git
            .ok(
                Some(&fixer.wc()),
                &["-c", "user.name=f", "-c", "user.email=f@f", "commit", "-qam", msg],
                super::git::LOCAL,
            )
            .unwrap();
        fixer.git.ok(Some(&fixer.wc()), &["push", "-q", "origin", "HEAD:refs/heads/main"], super::git::LOCAL).unwrap();
    };
    push("break");
    let r = b.sync();
    assert!(r.skipped.iter().any(|s| s.contains("messages/")), "{r:?}");
    assert_eq!(b.count("SELECT COUNT(*) FROM messages"), 0);
    // Only the session file is fixed; the messages file did not change, yet it is retried.
    std::fs::write(&sf, good).unwrap();
    push("fix");
    b.sync();
    assert_eq!(b.count("SELECT COUNT(*) FROM messages"), 1);
    assert!(state::skips(&b.store).unwrap().is_empty());
}

#[test]
fn links_and_colliding_names_are_skipped_never_followed_and_preview_refuses_them() {
    let (_r, url) = bare_repo();
    let a = Dev::new("A");
    let (p, _) = a.project("앱");
    a.connect(&url);
    a.sync();
    let evil = Dev::new("E");
    evil.connect(&url);
    let target = evil.home.parent().unwrap().join("outside");
    std::fs::create_dir_all(&target).unwrap();
    std::os::unix::fs::symlink(&target, evil.wc().join("pins")).unwrap();
    // Two names that are one file on macOS (added through the index so both exist in the tree).
    let blob = |text: &str| {
        evil.git
            .raw(Some(&evil.wc()), &["hash-object", "-w", "--stdin"], Some(text.as_bytes().to_vec()), super::git::LOCAL)
            .unwrap()
            .text()
    };
    let (b1, b2) = (blob("{}"), blob("{\"x\":1}"));
    let path_lower = format!("roles/{}.json", uuid::Uuid::max());
    let path_upper = path_lower.to_uppercase().replace("ROLES", "roles").replace(".JSON", ".json");
    evil.git
        .ok(
            Some(&evil.wc()),
            &["update-index", "--add", "--cacheinfo", &format!("100644,{b1},{path_lower}")],
            super::git::LOCAL,
        )
        .unwrap();
    evil.git
        .ok(
            Some(&evil.wc()),
            &["update-index", "--add", "--cacheinfo", &format!("100644,{b2},{path_upper}")],
            super::git::LOCAL,
        )
        .unwrap();
    evil.git.ok(Some(&evil.wc()), &["-c", "core.symlinks=true", "add", "pins"], super::git::LOCAL).unwrap();
    evil.git
        .ok(Some(&evil.wc()), &["-c", "user.name=e", "-c", "user.email=e@e", "commit", "-qm", "odd"], super::git::LOCAL)
        .unwrap();
    evil.git.ok(Some(&evil.wc()), &["push", "-q", "origin", "HEAD:refs/heads/main"], super::git::LOCAL).unwrap();
    // A new computer is told the repository is not usable as is.
    let (pre, _) = Dev::new("N").preview(&url);
    assert_eq!(pre.content, RepoContent::Other);
    assert!(!pre.problems.is_empty());
    // A keeps syncing (nothing is bricked), reports the entries and never writes through the link.
    let pin = a.pin(p, "핀");
    let r = a.sync();
    assert!(r.skipped.iter().any(|s| s.starts_with("pins:")), "{r:?}");
    assert!(r.skipped.iter().filter(|s| s.contains("대소문자")).count() >= 2, "{r:?}");
    assert_eq!(std::fs::read_dir(&target).unwrap().count(), 0);
    assert!(a.remote_files().contains(&super::kinds::pins::path_of(pin)));
    assert_eq!(a.count("SELECT COUNT(*) FROM roles"), 0);
    // A's push leaves them out, so the repository is usable again.
    let (pre, _) = Dev::new("M").preview(&url);
    assert_eq!(pre.content, RepoContent::Runwick);
}

#[test]
fn every_value_from_another_computer_waits_for_review() {
    let (_r, url) = bare_repo();
    let a = Dev::new("A");
    let set = |d: &Dev, k: &str, v: &str| {
        env::set(
            &d.store,
            &d.env,
            SetVar { key: k.into(), value: Some(v.into()), description: String::new(), secret: false },
        )
        .unwrap();
    };
    set(&a, "ANTHROPIC_BASE_URL", "https://proxy.example/v1");
    set(&a, "API_BASE", "https://x");
    a.connect(&url);
    a.sync();
    let b = Dev::new("B");
    b.connect(&url);
    b.sync();
    let names = |d: &Dev| env::list(&d.store, &d.env).unwrap().vars.into_iter().map(|v| v.key).collect::<Vec<_>>();
    assert!(names(&b).is_empty(), "applied without review: {:?}", names(&b));
    let staged = super::kinds::env::staged(&b.store, &b.device).unwrap();
    assert_eq!(staged.len(), 2);
    assert!(staged.iter().any(|v| v.key == "ANTHROPIC_BASE_URL" && v.dangerous));
    // B's syncs never overwrite or remove A's values while they wait.
    b.sync();
    a.sync();
    assert_eq!(names(&a).len(), 2);
    assert_eq!(super::kinds::env::staged(&b.store, &b.device).unwrap().len(), 2);
    // Accepting uses the value here; the same key=value later is applied without asking.
    super::kinds::env::decide(&b.store, &b.env, "ANTHROPIC_BASE_URL", true).unwrap();
    super::kinds::env::decide(&b.store, &b.env, "API_BASE", false).unwrap();
    assert_eq!(names(&b), vec!["ANTHROPIC_BASE_URL".to_string()]);
    assert!(b.sync().exported.is_empty());
    later();
    set(&a, "ANTHROPIC_BASE_URL", "https://other.example");
    a.sync();
    b.sync();
    let v = |d: &Dev, k: &str| {
        env::list(&d.store, &d.env).unwrap().vars.into_iter().find(|v| v.key == k).and_then(|v| v.value)
    };
    assert_eq!(v(&b, "ANTHROPIC_BASE_URL").as_deref(), Some("https://proxy.example/v1"), "a new value must wait");
    later();
    set(&a, "ANTHROPIC_BASE_URL", "https://proxy.example/v1");
    a.sync();
    b.sync();
    assert_eq!(v(&b, "ANTHROPIC_BASE_URL").as_deref(), Some("https://proxy.example/v1"));
    assert!(
        !super::kinds::env::staged(&b.store, &b.device)
            .unwrap()
            .iter()
            .any(|s| s.key == "ANTHROPIC_BASE_URL" && !s.rejected)
    );
    // A removal waits too.
    env::remove(&a.store, &a.env, "ANTHROPIC_BASE_URL").unwrap();
    a.sync();
    b.sync();
    assert!(v(&b, "ANTHROPIC_BASE_URL").is_some());
    assert!(
        super::kinds::env::staged(&b.store, &b.device)
            .unwrap()
            .iter()
            .any(|s| s.key == "ANTHROPIC_BASE_URL" && s.remove)
    );
    super::kinds::env::decide(&b.store, &b.env, "ANTHROPIC_BASE_URL", true).unwrap();
    assert!(v(&b, "ANTHROPIC_BASE_URL").is_none());
    // Plain values that went into the repository are known.
    assert!(state::exposed(&a.store).unwrap().contains(&"API_BASE".to_string()));
}

#[test]
fn a_secret_here_is_never_made_plain_by_another_computer() {
    let (_r, url) = bare_repo();
    let a = Dev::new("A");
    env::set(
        &a.store,
        &a.env,
        SetVar { key: "TOKEN".into(), value: Some("s3cret".into()), description: String::new(), secret: true },
    )
    .unwrap();
    a.connect(&url);
    a.sync();
    let b = Dev::new("B");
    b.connect(&url);
    b.sync();
    later();
    env::set(
        &b.store,
        &b.env,
        SetVar { key: "TOKEN".into(), value: Some("plain-now".into()), description: String::new(), secret: false },
    )
    .unwrap();
    b.sync();
    let r = a.sync();
    assert!(r.conflicts.iter().any(|c| c.title == "TOKEN"), "{r:?}");
    let tok = env::list(&a.store, &a.env).unwrap().vars.into_iter().find(|v| v.key == "TOKEN").unwrap();
    assert!(tok.secret && tok.value.is_none());
    assert_eq!(env::reveal(&a.store, &a.env, "TOKEN").unwrap(), "s3cret");
    // A does not push its secret over B's plain value either.
    a.sync();
    b.sync();
    let b_tok = env::list(&b.store, &b.env).unwrap().vars.into_iter().find(|v| v.key == "TOKEN").unwrap();
    assert_eq!(b_tok.value.as_deref(), Some("plain-now"));
    // Using the other computer's value here keeps the variable secret here.
    super::kinds::env::decide(&a.store, &a.env, "TOKEN", true).unwrap();
    let tok = env::list(&a.store, &a.env).unwrap().vars.into_iter().find(|v| v.key == "TOKEN").unwrap();
    assert!(tok.secret && tok.value.is_none(), "{tok:?}");
    assert_eq!(env::reveal(&a.store, &a.env, "TOKEN").unwrap(), "plain-now");
    // Being secret travels, values never do: B keeps its value, now as a secret.
    a.sync();
    b.sync();
    let b_tok = env::list(&b.store, &b.env).unwrap().vars.into_iter().find(|v| v.key == "TOKEN").unwrap();
    assert!(b_tok.secret && b_tok.value.is_none(), "{b_tok:?}");
    assert_eq!(env::reveal(&b.store, &b.env, "TOKEN").unwrap(), "plain-now");
    a.remote_files();
    let file = a
        .git
        .read_at(&a.wc(), "refs/remotes/origin/main", &["settings/env.json".to_string()])
        .unwrap()
        .remove("settings/env.json")
        .flatten()
        .unwrap();
    assert!(!String::from_utf8_lossy(&file).contains("plain-now"));
}

#[test]
fn change_times_from_the_future_do_not_win_forever() {
    let (_r, url) = bare_repo();
    let a = Dev::new("A");
    let (p, _) = a.project("앱");
    let pin = a.pin(p, "핀");
    a.connect(&url);
    a.sync();
    let w = Dev::new("W");
    w.connect(&url);
    let f = w.wc().join(super::kinds::pins::path_of(pin));
    let text = std::fs::read_to_string(&f).unwrap();
    let mut v: serde_json::Value = serde_json::from_str(&text).unwrap();
    v["note"] = serde_json::json!("미래에서 온 메모");
    v["updated_at"] = serde_json::json!("2099-01-01T00:00:00.000Z");
    std::fs::write(&f, super::format::canonical(&v)).unwrap();
    w.git
        .ok(
            Some(&w.wc()),
            &["-c", "user.name=w", "-c", "user.email=w@w", "commit", "-qam", "future"],
            super::git::LOCAL,
        )
        .unwrap();
    w.git.ok(Some(&w.wc()), &["push", "-q", "origin", "HEAD:refs/heads/main"], super::git::LOCAL).unwrap();
    a.sync();
    assert_eq!(a.text("SELECT note FROM pins WHERE id = ?1", pin), "미래에서 온 메모");
    assert!(*a.text("SELECT updated_at FROM pins WHERE id = ?1", pin) < *"2030");
    // A later edit here still wins.
    later();
    a.exec("UPDATE pins SET note = '지금 고친 메모', updated_at = ?2 WHERE id = ?1", params![pin, now()]);
    a.sync();
    let b = Dev::new("B");
    b.connect(&url);
    b.sync();
    assert_eq!(b.text("SELECT note FROM pins WHERE id = ?1", pin), "지금 고친 메모");
}

#[test]
fn decomposed_korean_page_names_are_one_page() {
    let (_r, url) = bare_repo();
    let a = Dev::new("A");
    let (p, _) = a.project("앱");
    a.connect(&url);
    a.sync();
    // A page file pushed with a decomposed (NFD) name, as a Linux machine or a tool would.
    let w = Dev::new("W");
    w.connect(&url);
    let nfd = "\u{1112}\u{116C}\u{110B}\u{1174}-\u{1106}\u{1166}\u{1106}\u{1169}";
    let f = w.wc().join("wiki").join(p.to_string()).join(format!("{nfd}.md"));
    std::fs::create_dir_all(f.parent().unwrap()).unwrap();
    std::fs::write(&f, "---\ntitle: \"회의 메모\"\nstatus: reviewed\ntags: []\nsources: []\n---\n\n본문\n").unwrap();
    w.git.ok(Some(&w.wc()), &["-c", "core.precomposeunicode=false", "add", "-A"], super::git::LOCAL).unwrap();
    w.git
        .ok(Some(&w.wc()), &["-c", "user.name=w", "-c", "user.email=w@w", "commit", "-qm", "nfd"], super::git::LOCAL)
        .unwrap();
    w.git.ok(Some(&w.wc()), &["push", "-q", "origin", "HEAD:refs/heads/main"], super::git::LOCAL).unwrap();
    a.sync();
    let slugs: Vec<String> = a
        .store
        .with(|c| {
            let mut st = c.prepare("SELECT slug FROM wiki_pages WHERE deleted = 0")?;
            let rows = st.query_map([], |r| r.get(0))?;
            rows.collect()
        })
        .unwrap();
    assert_eq!(slugs, vec!["회의-메모".to_string()]);
}

#[test]
fn a_delegation_file_never_replaces_a_request_of_this_computer() {
    let a = Dev::new("A");
    let (p, ws) = a.project("앱");
    let worker = a.session(p, ws, "작업자");
    let id = Uuid::now_v7();
    a.exec(
        "INSERT INTO collab_requests (id, worker_id, title, tool, prompt, source, via, status, created_at, updated_at)
         VALUES (?1, ?2, '내 요청', 'claude', 'x', 'agent', 'runwick', 'done', ?3, ?3)",
        params![id, worker, now()],
    );
    // A crafted file from "another computer" with the same id and a foreign worker.
    let foreign = Uuid::now_v7();
    a.exec(
        "INSERT INTO sessions (id, project_id, workspace_id, title, tool, cwd, created_at, origin_host, origin_device)
         VALUES (?1, ?2, ?3, '남의 세션', 'claude', '/', ?4, ?5, 'X')",
        params![foreign, p, ws, now(), Uuid::now_v7()],
    );
    let v = serde_json::json!({ "id": id.to_string(), "worker_id": foreign.to_string(), "title": "바꿔치기", "tool": "claude",
        "prompt": "y", "source": "agent", "via": "runwick", "status": "done", "created_at": now(), "updated_at": now() });
    let wiki = a.wiki();
    let cx = Cx { store: &a.store, home: &a.home, wiki_root: &wiki, env: &a.env, device: &a.device };
    let res = a.store.with(|c| Ok(super::kinds::collab::apply(&cx, c, &v))).unwrap();
    assert!(res.is_err());
    assert_eq!(a.text("SELECT title FROM collab_requests WHERE id = ?1", id), "내 요청");
}

/// A session of `n` messages on `a`, synced to the repository at `url`.
fn big_session(a: &Dev, url: &str, n: usize) {
    let (p, w) = a.project("앱");
    let s = a.session(p, w, "긴 세션");
    let body = "가".repeat(300);
    a.store
        .tx(|c| {
            let mut st = c.prepare(
                "INSERT INTO messages (id, session_id, project_id, workspace_id, provider_msg_id, seq, role, text, at, collected_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'assistant', ?7, ?8, ?8)",
            )?;
            for i in 0..n {
                st.execute(params![Uuid::now_v7(), s, p, w, format!("m{i}"), i as i64, body, now()])?;
            }
            Ok(())
        })
        .unwrap();
    a.connect(url);
    a.sync();
}

/// While `b` imports, how often a short request got the store, and its longest wait (seconds).
fn import_with_probe(b: &Dev, n: usize) -> (usize, f64) {
    let importing = AtomicBool::new(false);
    let stop = AtomicBool::new(false);
    std::thread::scope(|sc| {
        let probe = sc.spawn(|| {
            let (mut got, mut worst) = (0, 0.0f64);
            while !stop.load(Ordering::Relaxed) {
                let t = std::time::Instant::now();
                b.store.with(|c| c.query_row("SELECT COUNT(*) FROM projects", [], |r| r.get::<_, i64>(0))).unwrap();
                if importing.load(Ordering::Relaxed) {
                    got += 1;
                    worst = worst.max(t.elapsed().as_secs_f64());
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            (got, worst)
        });
        let r = b.sync_with(&|st| importing.store(st == SyncStep::Import, Ordering::Relaxed));
        stop.store(true, Ordering::Relaxed);
        assert_eq!(imported(&r, SyncKind::Messages), n as u64);
        probe.join().unwrap()
    })
}

#[test]
fn a_large_import_lets_other_requests_use_the_store_in_between() {
    let (_r, url) = bare_repo();
    let n = 20_000;
    big_session(&Dev::new("A"), &url, n);
    let b = Dev::new("B");
    b.connect(&url);
    // Twenty transactions of about 0.1 s: without a pause between them the importer mostly takes
    // the lock straight back and a request waits for seconds (measured 2 s); with it, about one
    // transaction (0.1 s).
    let (got, worst) = import_with_probe(&b, n);
    assert!(worst < 0.6, "a request waited {worst:.3}s for the store (free {got} times)");
}

/// How long a short request waits for the store while 60,000 messages are imported.
#[test]
#[ignore = "measurement: cargo test -- --ignored large_import_latency --nocapture"]
fn large_import_latency() {
    let (_r, url) = bare_repo();
    let n: usize = std::env::var("RW_N").ok().and_then(|v| v.parse().ok()).unwrap_or(60_000);
    big_session(&Dev::new("A"), &url, n);
    let b = Dev::new("B");
    b.connect(&url);
    let t0 = std::time::Instant::now();
    let (got, worst) = import_with_probe(&b, n);
    eprintln!("{n} messages: {:.2}s, store free {got} times, worst wait {worst:.3}s", t0.elapsed().as_secs_f64());
}

#[test]
fn a_conflict_record_from_the_repository_never_sets_a_variable_or_speaks_for_this_computer() {
    let (_r, url) = bare_repo();
    let a = Dev::new("A");
    let (p, _) = a.project("앱");
    let pin = a.pin(p, "핀");
    a.set_var("PLAIN_X", "mine");
    a.connect(&url);
    a.sync();
    let evil = Dev::new("E");
    evil.connect(&url);
    let record = |path: &str, title: &str, kind: &str, lost: serde_json::Value| {
        let id = Uuid::now_v7();
        (
            format!("conflicts/{id}.json"),
            serde_json::json!({
                "id": id.to_string(), "host_id": evil.device.host_id.to_string(), "device": "회사 맥",
                "kind": kind, "path": path, "title": title, "kept_device": "회사 맥",
                "lost_host": a.device.host_id.to_string(), "lost_device": "A",
                "lost": lost.to_string(), "at": now(),
            })
            .to_string(),
        )
    };
    let evil_var = serde_json::json!({"key": "NODE_OPTIONS", "value": "--require /tmp/evil.js", "description": "", "secret": false});
    evil.push_raw(&[
        // A variable under another variable's name and path.
        record("settings/env.json", "PLAIN_X", "env", evil_var.clone()),
        // Consistent, but a variable all the same.
        record("settings/env.json#NODE_OPTIONS", "NODE_OPTIONS", "env", evil_var),
        // A pin text this computer never had, said to be this computer's.
        record(&format!("pins/{pin}.json"), "핀", "pins", serde_json::json!({"id": pin.to_string(), "note": "가짜"})),
        // A record whose path is not of its kind.
        record(&format!("roles/{pin}.json"), "핀", "pins", serde_json::json!({"id": pin.to_string()})),
    ]);
    let r = a.sync();
    assert_eq!(r.skipped.iter().filter(|s| s.contains("겹친 변경 기록이 맞지 않아")).count(), 2, "{r:?}");
    assert!(!r.conflicts.iter().any(|c| c.resolution.contains("바뀌었어요")), "{r:?}");
    let list = a.conflicts();
    assert_eq!(list.len(), 2, "{list:?}");
    assert!(list.iter().all(|c| !c.lost_here && !c.found_here), "{list:?}");
    // Putting the variable back only puts it up for review here, with the warning.
    let var = list.iter().find(|c| c.kind == SyncKind::Env).unwrap();
    a.with_cx(|cx| super::kinds::conflicts::restore(cx, var.id)).unwrap();
    assert_eq!(a.var("NODE_OPTIONS"), None);
    assert_eq!(a.var("PLAIN_X").as_deref(), Some("mine"));
    let waiting = a.waiting();
    assert!(waiting.iter().any(|s| s.key == "NODE_OPTIONS" && s.dangerous && !s.remove), "{waiting:?}");
    // It waits on this computer only: never written into the repository as another value.
    a.sync();
    assert!(!a.remote_text("settings/env.json").contains("NODE_OPTIONS"));
    super::kinds::env::decide(&a.store, &a.env, "NODE_OPTIONS", false).unwrap();
    assert!(a.waiting().is_empty());
    assert_eq!(a.var("NODE_OPTIONS"), None);
}

#[test]
fn an_edit_or_removal_here_replaces_what_waits_and_reaches_the_other_computer() {
    let (_r, url) = bare_repo();
    let a = Dev::new("A");
    a.connect(&url);
    let b = Dev::new("B");
    b.connect(&url);
    let waits = |d: &Dev, k: &str| d.waiting().into_iter().filter(|s| s.key == k).count();
    // B accepts K1=v1; A changes it to v2 (waits on B), then back to v1 (accepted before).
    a.set_var("K1", "v1");
    a.sync();
    b.sync();
    super::kinds::env::decide(&b.store, &b.env, "K1", true).unwrap();
    later();
    a.set_var("K1", "v2");
    a.sync();
    b.sync();
    assert_eq!(waits(&b, "K1"), 1);
    later();
    a.set_var("K1", "v1");
    a.sync();
    b.sync();
    assert_eq!(b.var("K1").as_deref(), Some("v1"));
    assert_eq!(waits(&b, "K1"), 0, "applied: nothing about it waits");
    // Removing it on B reaches A (for review there), and the stale v2 never comes back.
    later();
    env::remove(&b.store, &b.env, "K1").unwrap();
    b.sync();
    assert!(!b.remote_text("settings/env.json").contains("\"K1\""));
    a.sync();
    assert!(a.waiting().iter().any(|s| s.key == "K1" && s.remove));
    // B removes a variable while another value of it waits there: the removal is what goes out.
    b.set_var("S", "b0");
    b.sync();
    a.sync();
    super::kinds::env::decide(&a.store, &a.env, "S", true).unwrap();
    later();
    a.set_var("S", "a1");
    a.sync();
    b.sync();
    assert_eq!(waits(&b, "S"), 1);
    env::remove(&b.store, &b.env, "S").unwrap();
    assert_eq!(waits(&b, "S"), 0);
    b.sync();
    assert!(!b.remote_text("settings/env.json").contains("\"S\""));
    a.sync();
    assert!(a.waiting().iter().any(|s| s.key == "S" && s.remove));
    // B's own new value replaces the one waiting there, and reaches A.
    later();
    a.set_var("P", "from-a");
    a.sync();
    b.sync();
    assert_eq!(waits(&b, "P"), 1);
    b.set_var("P", "from-b");
    assert_eq!(waits(&b, "P"), 0);
    b.sync();
    a.sync();
    assert!(a.waiting().iter().any(|s| s.key == "P" && s.value == "from-b"));
}

#[test]
fn a_variable_changed_on_both_computers_waits_and_is_never_reported_as_replaced() {
    let (_r, url) = bare_repo();
    let a = Dev::new("A");
    a.set_var("PLAIN_X", "base");
    a.connect(&url);
    a.sync();
    let b = Dev::new("B");
    b.connect(&url);
    b.sync();
    super::kinds::env::decide(&b.store, &b.env, "PLAIN_X", true).unwrap();
    later();
    a.set_var("PLAIN_X", "A-changed");
    later();
    b.set_var("PLAIN_X", "B-changed");
    let mut reports = vec![a.sync(), b.sync(), a.sync(), b.sync()];
    assert!(reports.iter().all(|r| !r.conflicts.iter().any(|c| c.resolution.contains("바뀌었어요"))), "{reports:?}");
    let a_report = reports.remove(2);
    assert!(
        a_report
            .conflicts
            .iter()
            .any(|c| c.title == "PLAIN_X" && c.resolution == "다른 컴퓨터에서 다른 값이 와서 검토를 기다려요"),
        "{a_report:?}"
    );
    assert!(a.conflicts().is_empty() && b.conflicts().is_empty());
    assert_eq!(a.var("PLAIN_X").as_deref(), Some("A-changed"));
    assert_eq!(b.var("PLAIN_X").as_deref(), Some("B-changed"));
}

#[test]
fn opening_a_project_or_a_page_files_time_alone_is_not_a_change_to_sync() {
    let a = Dev::new("A");
    let (p, _) = a.project("앱");
    let slug = a.page(p, "문서", "본문\n");
    let n = || state::fingerprint(&a.store).unwrap();
    let before = n();
    a.exec("UPDATE projects SET last_opened_at = ?2 WHERE id = ?1", params![p, now()]);
    a.exec("UPDATE projects SET archived = 0, last_opened_at = ?2 WHERE id = ?1", params![p, now()]);
    let file = a.wiki().join(p.to_string()).join(format!("{slug}.md"));
    std::fs::File::options()
        .write(true)
        .open(&file)
        .unwrap()
        .set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(5))
        .unwrap();
    ops::refresh(&a.store, &a.wiki(), None).unwrap();
    a.exec("UPDATE env_vars SET changed_in = changed_in WHERE 1 = 0", []);
    assert_eq!(n(), before, "nothing that is written into the repository changed");
    a.exec("UPDATE projects SET name = 'xyz' WHERE id = ?1", [p]);
    assert_ne!(n(), before);
}

#[test]
fn computers_with_the_same_name_are_told_apart() {
    let (_r, url) = bare_repo();
    let a = Dev::new("gyeongyeon_jeon");
    let (p, _) = a.project("앱");
    let pin = a.pin(p, "핀");
    a.connect(&url);
    a.sync();
    let b = Dev::new("gyeongyeon_jeon");
    b.connect(&url);
    b.sync();
    a.exec("UPDATE pins SET note = 'A의 메모', updated_at = ?2 WHERE id = ?1", params![pin, now()]);
    later();
    b.exec("UPDATE pins SET note = 'B의 메모', updated_at = ?2 WHERE id = ?1", params![pin, now()]);
    a.sync();
    b.sync();
    a.sync();
    let other = |d: &Dev| {
        let id = d.device.host_id.simple().to_string();
        format!("gyeongyeon_jeon (다른 컴퓨터 · {})", &id[id.len() - 4..])
    };
    // B found it and kept its own text: this computer is always "이 컴퓨터".
    let on_b = b.conflicts();
    let c = on_b.iter().find(|c| c.kind == SyncKind::Pins).unwrap();
    assert_eq!(
        (c.found_on.as_str(), c.kept_device.as_str(), c.lost_device.clone()),
        ("이 컴퓨터", "이 컴퓨터", other(&a)),
        "{c:?}"
    );
    // A reads that its own text was replaced by the other computer of the same name.
    let on_a = a.conflicts();
    let c = on_a.iter().find(|c| c.kind == SyncKind::Pins).unwrap();
    assert!(c.lost_here, "{c:?}");
    assert_eq!(
        (c.lost_device.as_str(), c.kept_device.clone(), c.found_on.clone()),
        ("이 컴퓨터", other(&b), other(&b))
    );
    // A value waiting for review names the computer that wrote it the same way.
    b.set_var("K", "v");
    b.sync();
    a.sync();
    let w = a.waiting();
    assert_eq!(w.len(), 1);
    assert_eq!(w[0].device, other(&b));
    // The history names each commit's computer the same way.
    let names = a.store.with(|c| super::kinds::Names::load(c, &a.device)).unwrap();
    let history = super::repo::history(&a.git, &a.wc(), &names).unwrap();
    let who: std::collections::BTreeSet<String> = history.iter().map(|c| c.device.clone()).collect();
    assert_eq!(who, ["이 컴퓨터".to_string(), other(&b)].into_iter().collect(), "{history:?}");
    // A third computer of the same name tells both apart before connecting...
    let c3 = Dev::new("gyeongyeon_jeon");
    let (preview, _) = c3.preview(&url);
    let listed: std::collections::BTreeSet<String> = preview.devices.iter().map(|d| d.name.clone()).collect();
    assert_eq!(listed, [other(&a), other(&b)].into_iter().collect());
    // ...and lists A's folder with A told apart.
    c3.connect(&url);
    c3.sync();
    let unlinked = c3.store.with(|c| super::kinds::projects::unlinked(&c3.home, c, &c3.device)).unwrap();
    assert!(unlinked[0].elsewhere.iter().any(|e| e.starts_with(&format!("{} · ", other(&a)))), "{unlinked:?}");
}
