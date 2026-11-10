use super::*;

fn open(store: &Store, root: &Path, name: &str) -> Opened {
    let path = root.join(name);
    std::fs::create_dir_all(&path).unwrap();
    open_folder(store, path.to_str().unwrap()).unwrap()
}

fn ids(store: &Store) -> Vec<Uuid> {
    list_projects(store).unwrap().into_iter().map(|p| p.id).collect()
}

fn group(store: &Store, name: &str) -> ProjectGroup {
    create_group(store, &ProjectGroupCreate { name: name.into(), project: None }).unwrap()
}

#[test]
fn new_projects_prepend_and_open_touch_rename_favorite_do_not_reorder() {
    let store = Store::open_in_memory().unwrap();
    let root = tempfile::tempdir().unwrap();
    let a = open(&store, root.path(), "a");
    let b = open(&store, root.path(), "b");
    let c = open(&store, root.path(), "c");
    assert_eq!(ids(&store), [c.project.id, b.project.id, a.project.id]);
    move_project(&store, &ProjectMove { id: a.project.id, group_id: None, before: Some(c.project.id) }).unwrap();
    let want = vec![a.project.id, c.project.id, b.project.id];
    open_folder(&store, &b.project.root).unwrap();
    update_workspace(&store, &WorkspaceUpdate { id: c.workspace.id, touch: true, ..Default::default() }).unwrap();
    update_project(
        &store,
        &ProjectUpdate {
            id: b.project.id,
            name: Some("first".into()),
            favorite: Some(true),
            touch: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(ids(&store), want);
    update_project(&store, &ProjectUpdate { id: c.project.id, archived: Some(true), ..Default::default() }).unwrap();
    open_folder(&store, &c.project.root).unwrap();
    assert_eq!(ids(&store), want);
    let d = open(&store, root.path(), "d");
    assert_eq!(ids(&store), [vec![d.project.id], want].concat());
}

#[test]
fn group_crud_order_and_collapse_survive_reopening() {
    let root = tempfile::tempdir().unwrap();
    let db = root.path().join("runwick.db");
    let store = Store::open(&db).unwrap();
    let a = open(&store, root.path(), "a");
    let g = create_group(&store, &ProjectGroupCreate { name: "  Work  ".into(), project: Some(a.project.id) }).unwrap();
    let h = group(&store, "Personal");
    assert_eq!(g.name, "Work");
    assert_eq!(get_project(&store, a.project.id).unwrap().group_id, Some(g.id));
    update_group(&store, &ProjectGroupUpdate { id: g.id, name: Some("Renamed".into()), collapsed: Some(true) })
        .unwrap();
    move_group(&store, &ProjectGroupMove { id: h.id, before: Some(g.id) }).unwrap();
    drop(store);
    let store = Store::open(&db).unwrap();
    let groups = list_groups(&store).unwrap();
    assert_eq!(groups.iter().map(|g| g.id).collect::<Vec<_>>(), [h.id, g.id]);
    assert!(groups[1].collapsed);
    assert_eq!(groups[1].name, "Renamed");
    assert_eq!(get_project(&store, a.project.id).unwrap().group_id, Some(g.id));
}

#[test]
fn deleting_a_group_appends_its_projects_and_preserves_archived_projects_and_workspaces() {
    let store = Store::open_in_memory().unwrap();
    let root = tempfile::tempdir().unwrap();
    let a = open(&store, root.path(), "a");
    let b = open(&store, root.path(), "b");
    let c = open(&store, root.path(), "c");
    let g = group(&store, "Work");
    for id in [a.project.id, b.project.id] {
        move_project(&store, &ProjectMove { id, group_id: Some(g.id), before: None }).unwrap();
    }
    update_project(&store, &ProjectUpdate { id: b.project.id, archived: Some(true), ..Default::default() }).unwrap();
    delete_group(&store, g.id).unwrap();
    assert!(list_groups(&store).unwrap().is_empty());
    let projects = list_projects(&store).unwrap();
    assert_eq!(projects.iter().map(|p| p.id).collect::<Vec<_>>(), [c.project.id, a.project.id, b.project.id]);
    assert!(projects.iter().all(|p| p.group_id.is_none()));
    assert!(projects[2].archived);
    assert_eq!(list_workspaces(&store, &WorkspaceQuery::default()).unwrap().len(), 3);
    assert!(root.path().join("a").is_dir());
}

#[test]
fn stale_targets_invalid_names_and_remote_only_moves_are_atomic_errors() {
    let store = Store::open_in_memory().unwrap();
    let root = tempfile::tempdir().unwrap();
    let a = open(&store, root.path(), "a");
    let b = open(&store, root.path(), "b");
    let g = group(&store, "Work");
    let before = list_projects(&store).unwrap();
    for movement in [
        ProjectMove { id: a.project.id, group_id: Some(Uuid::new_v4()), before: None },
        ProjectMove { id: a.project.id, group_id: Some(g.id), before: Some(b.project.id) },
        ProjectMove { id: a.project.id, group_id: None, before: Some(Uuid::new_v4()) },
    ] {
        assert!(move_project(&store, &movement).is_err());
    }
    assert_eq!(list_projects(&store).unwrap(), before);
    assert!(
        update_group(&store, &ProjectGroupUpdate { id: g.id, name: Some("  ".into()), collapsed: Some(true) }).is_err()
    );
    assert!(!list_groups(&store).unwrap()[0].collapsed);
    assert!(move_group(&store, &ProjectGroupMove { id: g.id, before: Some(Uuid::new_v4()) }).is_err());
    assert!(create_group(&store, &ProjectGroupCreate { name: "Oops".into(), project: Some(Uuid::new_v4()) }).is_err());
    assert_eq!(list_groups(&store).unwrap().len(), 1);
    store
        .with(|c| {
            c.execute(
                "UPDATE workspaces SET remote_device_id = ?2, remote_workspace_id = ?3 WHERE id = ?1",
                params![b.workspace.id, Uuid::new_v4(), Uuid::new_v4()],
            )
        })
        .unwrap();
    assert!(get_project(&store, b.project.id).unwrap().remote_only);
    assert!(move_project(&store, &ProjectMove { id: b.project.id, group_id: Some(g.id), before: None }).is_err());
    assert!(create_group(&store, &ProjectGroupCreate { name: "Remote".into(), project: Some(b.project.id) }).is_err());
    assert_eq!(list_groups(&store).unwrap().len(), 1);
}

#[test]
fn moving_between_groups_and_ungrouped_uses_the_requested_anchor() {
    let store = Store::open_in_memory().unwrap();
    let root = tempfile::tempdir().unwrap();
    let a = open(&store, root.path(), "a");
    let b = open(&store, root.path(), "b");
    let c = open(&store, root.path(), "c");
    let g = group(&store, "Work");
    let h = group(&store, "Personal");
    move_project(&store, &ProjectMove { id: a.project.id, group_id: Some(g.id), before: None }).unwrap();
    move_project(&store, &ProjectMove { id: b.project.id, group_id: Some(g.id), before: Some(a.project.id) }).unwrap();
    assert_eq!(store.with(|c| Ok(bucket(c, Some(g.id)).unwrap())).unwrap(), [b.project.id, a.project.id]);
    move_project(&store, &ProjectMove { id: b.project.id, group_id: Some(h.id), before: None }).unwrap();
    move_project(&store, &ProjectMove { id: b.project.id, group_id: None, before: Some(c.project.id) }).unwrap();
    assert_eq!(store.with(|c| Ok(bucket(c, None).unwrap())).unwrap(), [b.project.id, c.project.id]);
    let d = open(&store, root.path(), "d");
    assert_eq!(get_project(&store, d.project.id).unwrap().group_id, None);
    assert_eq!(store.with(|c| Ok(bucket(c, None).unwrap())).unwrap(), [d.project.id, b.project.id, c.project.id]);
}

#[test]
fn migration_preserves_existing_recent_order_and_runs_only_once() {
    let root = tempfile::tempdir().unwrap();
    let db = root.path().join("old.db");
    let old = Connection::open(&db).unwrap();
    old.execute_batch("CREATE TABLE schema_migrations (name TEXT PRIMARY KEY, applied_at TEXT NOT NULL)").unwrap();
    for (name, sql) in crate::store::all_migrations() {
        if name == "0021_project_organization" {
            continue;
        }
        old.execute_batch(sql).unwrap();
        old.execute("INSERT INTO schema_migrations VALUES (?1, '2026-01-01')", [name]).unwrap();
    }
    let a = Uuid::new_v4();
    let b = Uuid::new_v4();
    let c = Uuid::new_v4();
    for (id, name, date) in [(a, "a", "2026-01-02"), (b, "b", "2026-01-03"), (c, "c", "2026-01-01")] {
        old.execute(
            "INSERT INTO projects (id, name, root, created_at) VALUES (?1, ?2, ?2, ?3)",
            params![id, name, date],
        )
        .unwrap();
        old.execute("INSERT INTO workspaces (id, project_id, name, path, kind, created_at) VALUES (?1, ?2, 'default', ?3, 'default', ?4)", params![Uuid::new_v4(), id, name, date]).unwrap();
    }
    old.execute("UPDATE projects SET last_opened_at = '2026-01-04' WHERE id = ?1", [c]).unwrap();
    drop(old);
    let store = Store::open(&db).unwrap();
    assert_eq!(ids(&store), [c, b, a]);
    assert!(list_groups(&store).unwrap().is_empty());
    move_project(&store, &ProjectMove { id: a, group_id: None, before: Some(c) }).unwrap();
    drop(store);
    let store = Store::open(&db).unwrap();
    assert_eq!(ids(&store), [a, c, b]);
    assert_eq!(list_workspaces(&store, &WorkspaceQuery::default()).unwrap().len(), 3);
}
