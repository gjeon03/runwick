use super::*;

fn project(name: &str, group_id: Option<Uuid>, remote_only: bool) -> Project {
    Project {
        id: Uuid::new_v4(),
        name: name.into(),
        root: format!("/tmp/{name}"),
        is_git: false,
        favorite: false,
        archived: false,
        created_at: runwick_core::now(),
        last_opened_at: None,
        root_exists: true,
        remote_url: None,
        order_index: 0,
        group_id,
        remote_only,
    }
}

fn group(name: &str, collapsed: bool) -> ProjectGroup {
    ProjectGroup { id: Uuid::new_v4(), name: name.into(), order_index: 0, collapsed }
}

fn labels(entries: Vec<Entry<'_>>) -> Vec<String> {
    entries
        .into_iter()
        .map(|e| match e {
            Entry::Ungrouped => "ungrouped".into(),
            Entry::Group(g) => format!("group:{}", g.name),
            Entry::Project(p) => p.name.clone(),
        })
        .collect()
}

#[test]
fn ungrouped_precedes_groups_and_remote_links_stay_flat() {
    let groups = [group("Work", false), group("Empty", false)];
    let projects = [
        project("in-group", Some(groups[0].id), false),
        project("loose", None, false),
        project("remote", Some(groups[0].id), true),
    ];
    assert_eq!(
        labels(entries(&projects.iter().collect::<Vec<_>>(), &groups, false)),
        ["ungrouped", "loose", "group:Work", "in-group", "group:Empty", "remote"]
    );
}

#[test]
fn a_deleted_group_never_hides_projects_while_queries_refresh_independently() {
    let p = project("still here", Some(Uuid::new_v4()), false);
    assert_eq!(labels(entries(&[&p], &[], false)), ["still here"]);
    assert_eq!(labels(entries(&[&p], &[], true)), ["still here"]);
}

#[test]
fn filtering_hides_empty_groups_and_respects_collapsed_state() {
    let mut groups = [group("Work", true), group("Empty", false)];
    let p = project("match", Some(groups[0].id), false);
    assert_eq!(labels(entries(&[&p], &groups, false)), ["ungrouped", "group:Work", "group:Empty"]);
    assert_eq!(labels(entries(&[&p], &groups, true)), ["group:Work"]);
    assert!(groups[0].collapsed);
    assert!(entries(&[], &groups, true).is_empty());
    groups[0].collapsed = false;
    assert_eq!(labels(entries(&[&p], &groups, true)), ["group:Work", "match"]);
}

#[test]
fn filtered_movement_anchors_to_visible_neighbors_without_replacing_hidden_projects() {
    let all = [
        project("a", None, false),
        project("hidden", None, false),
        project("b", None, false),
        project("tail", None, false),
    ];
    let shown = [&all[0], &all[2]];
    assert_eq!(step(&shown, &all, &all[0], true).unwrap().before, Some(all[3].id));
    assert_eq!(step(&shown, &all, &all[2], false).unwrap().before, Some(all[0].id));
    assert!(step(&shown, &all, &all[0], false).is_none());
    assert!(step(&shown, &all, &all[2], true).is_none());
}

#[test]
fn movement_stays_in_its_group_and_remote_projects_are_excluded() {
    let g = Uuid::new_v4();
    let all = [
        project("a", None, false),
        project("remote", None, true),
        project("grouped", Some(g), false),
        project("b", None, false),
    ];
    let shown = all.iter().collect::<Vec<_>>();
    assert_eq!(after(&all, all[0].id, None), Some(all[3].id));
    assert_eq!(step(&shown, &all, &all[0], true).unwrap().before, None);
    assert!(step(&shown, &all, &all[2], true).is_none());
    assert!(step(&shown, &all, &all[1], true).is_none());
}

#[test]
fn dragging_a_project_into_a_collapsed_group_moves_without_selecting_it() {
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    let p = project("project", None, false);
    let groups = [group("Work", true)];
    let draw = |events: Vec<egui::Event>| {
        let mut acts = Vec::new();
        let mut organization_acts = Vec::new();
        let mut source = Pos2::ZERO;
        let mut target = Pos2::ZERO;
        let raw = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(260.0, 300.0))),
            events,
            ..Default::default()
        };
        ctx.run_ui(raw, |ui| {
            let resp = crate::shell::sidebar::projects::project_row(ui, &p, 1, 0, false, &mut acts);
            source = Pos2::new(resp.rect.left() + 80.0, resp.rect.center().y);
            project_drop(ui, &resp, &p, std::slice::from_ref(&p), &mut organization_acts);
            target = Pos2::new(source.x, ui.cursor().top() + 4.0 + 14.0);
            group_row(ui, &groups[0], &groups, &mut organization_acts);
        })
        .textures_delta
        .clear();
        (source, target, acts, organization_acts)
    };
    let (source, target, _, _) = draw(vec![]);
    let pointer = |pos, pressed| egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    draw(vec![egui::Event::PointerMoved(source), pointer(source, true)]);
    let (_, _, acts, _) = draw(vec![egui::Event::PointerMoved(target)]);
    assert!(acts.is_empty());
    assert!(matches!(egui::DragAndDrop::payload::<Drag>(&ctx).as_deref(), Some(Drag::Project(id)) if *id == p.id));
    let (_, _, acts, moves) = draw(vec![pointer(target, false)]);
    assert!(acts.is_empty());
    assert!(
        matches!(moves.as_slice(), [Action::Move(m)] if m.id == p.id && m.group_id == Some(groups[0].id) && m.before.is_none())
    );
}

#[test]
fn project_drop_on_ungrouped_and_group_reorder_emit_separate_operations() {
    for group_drag in [false, true] {
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        let source = Uuid::new_v4();
        let groups = [group("Target", false)];
        let pos = Pos2::new(80.0, 10.0);
        let raw = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(260.0, 300.0))),
            events: vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
            ..Default::default()
        };
        let mut acts = Vec::new();
        egui::DragAndDrop::set_payload(&ctx, if group_drag { Drag::Group(source) } else { Drag::Project(source) });
        ctx.run_ui(raw, |ui| {
            if group_drag {
                group_row(ui, &groups[0], &groups, &mut acts);
            } else {
                ungrouped_row(ui, &mut acts);
            }
        })
        .textures_delta
        .clear();
        if group_drag {
            assert!(
                matches!(acts.as_slice(), [Action::MoveGroup(m)] if m.id == source && m.before == Some(groups[0].id))
            );
        } else {
            assert!(matches!(acts.as_slice(), [Action::Move(m)] if m.id == source && m.group_id.is_none()));
        }
    }
}
