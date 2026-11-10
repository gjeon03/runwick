//! Sidebar organization controls; remote links keep their existing flat listing.

use super::*;
use runwick_core::api::projects::*;
use uuid::Uuid;

#[derive(Default)]
pub struct State {
    edit: Option<Edit>,
    saving: bool,
}

struct Edit {
    id: Option<Uuid>,
    project: Option<Uuid>,
    name: String,
    focus: bool,
}

pub enum Action {
    Move(ProjectMove),
    MoveGroup(ProjectGroupMove),
    Create(Option<Uuid>),
    Rename(ProjectGroup),
    Collapse(Uuid, bool),
    Delete(Uuid),
}

#[derive(Clone, Copy)]
enum Drag {
    Project(Uuid),
    Group(Uuid),
}

pub enum Entry<'a> {
    Ungrouped,
    Group(&'a ProjectGroup),
    Project(&'a Project),
}

pub fn entries<'a>(shown: &[&'a Project], groups: &'a [ProjectGroup], filtering: bool) -> Vec<Entry<'a>> {
    let mut out = Vec::new();
    // The project and group queries refresh independently after a group is deleted.
    let ungrouped: Vec<_> =
        shown.iter().copied().filter(|p| !p.remote_only && !groups.iter().any(|g| Some(g.id) == p.group_id)).collect();
    if !groups.is_empty() && (!filtering || !ungrouped.is_empty()) {
        out.push(Entry::Ungrouped);
    }
    out.extend(ungrouped.into_iter().map(Entry::Project));
    for group in groups {
        let members: Vec<_> =
            shown.iter().copied().filter(|p| !p.remote_only && p.group_id == Some(group.id)).collect();
        if filtering && members.is_empty() {
            continue;
        }
        out.push(Entry::Group(group));
        if !group.collapsed {
            out.extend(members.into_iter().map(Entry::Project));
        }
    }
    out.extend(shown.iter().copied().filter(|p| p.remote_only).map(Entry::Project));
    out
}

fn after(projects: &[Project], id: Uuid, group: Option<Uuid>) -> Option<Uuid> {
    let mut ids = projects.iter().filter(|p| !p.remote_only && p.group_id == group).map(|p| p.id);
    ids.find(|p| *p == id)?;
    ids.next()
}

fn step(shown: &[&Project], all: &[Project], project: &Project, down: bool) -> Option<ProjectMove> {
    let peers: Vec<_> = shown.iter().filter(|p| !p.remote_only && p.group_id == project.group_id).collect();
    let index = peers.iter().position(|p| p.id == project.id)?;
    let before = if down {
        after(all, peers.get(index + 1)?.id, project.group_id)
    } else {
        Some(peers.get(index.checked_sub(1)?)?.id)
    };
    Some(ProjectMove { id: project.id, group_id: project.group_id, before })
}

pub fn project_menu(
    ui: &mut egui::Ui,
    p: &Project,
    shown: &[&Project],
    all: &[Project],
    groups: &[ProjectGroup],
    acts: &mut Vec<Action>,
) {
    if p.remote_only {
        return;
    }
    for (down, icon, label) in [(false, icons::ARROW_UP, "위로 이동"), (true, icons::ARROW_DOWN, "아래로 이동")]
    {
        let movement = step(shown, all, p, down);
        if ui.add_enabled_ui(movement.is_some(), |ui| widgets::menu_item(ui, icon, label)).inner.clicked()
            && let Some(movement) = movement
        {
            acts.push(Action::Move(movement));
        }
    }
    ui.menu_button("그룹으로 이동", |ui| {
        if ui
            .add_enabled_ui(p.group_id.is_some(), |ui| widgets::menu_item(ui, icons::FOLDER, "그룹 없음"))
            .inner
            .clicked()
        {
            acts.push(Action::Move(ProjectMove { id: p.id, group_id: None, before: None }));
            ui.close_kind(egui::UiKind::Menu);
        }
        for group in groups {
            if ui
                .add_enabled_ui(p.group_id != Some(group.id), |ui| widgets::menu_item(ui, icons::FOLDER, &group.name))
                .inner
                .clicked()
            {
                acts.push(Action::Move(ProjectMove { id: p.id, group_id: Some(group.id), before: None }));
                ui.close_kind(egui::UiKind::Menu);
            }
        }
        widgets::menu_sep(ui);
        if widgets::menu_item(ui, icons::FOLDER_PLUS, "새 그룹…").clicked() {
            acts.push(Action::Create(Some(p.id)));
            ui.close_kind(egui::UiKind::Menu);
        }
    });
    widgets::menu_sep(ui);
}

pub fn project_drop(ui: &mut egui::Ui, resp: &egui::Response, p: &Project, all: &[Project], acts: &mut Vec<Action>) {
    if p.remote_only {
        return;
    }
    resp.dnd_set_drag_payload(Drag::Project(p.id));
    if let Some(drag) = egui::DragAndDrop::payload::<Drag>(ui.ctx())
        && let Drag::Project(id) = *drag
        && id != p.id
        && let Some(pos) = pointer_over(ui, resp.rect)
    {
        let below = pos.y >= resp.rect.center().y;
        let before = if below { after(all, p.id, p.group_id) } else { Some(p.id) };
        drop_line(ui, resp.rect, below);
        if released(ui) {
            acts.push(Action::Move(ProjectMove { id, group_id: p.group_id, before }));
        }
    }
}

fn pointer_over(ui: &egui::Ui, rect: Rect) -> Option<Pos2> {
    ui.input(|i| i.pointer.interact_pos()).filter(|pos| rect.intersect(ui.clip_rect()).contains(*pos))
}

fn drop_line(ui: &egui::Ui, r: Rect, below: bool) {
    ui.painter().hline(
        r.shrink(2.0).x_range(),
        if below { r.bottom() } else { r.top() },
        egui::Stroke::new(2.0, c::EMBER),
    );
    ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
}

fn released(ui: &egui::Ui) -> bool {
    if ui.input(|i| i.pointer.any_released()) {
        egui::DragAndDrop::clear_payload(ui.ctx());
        true
    } else {
        false
    }
}

fn bucket_drop(ui: &egui::Ui, r: Rect, group: Option<Uuid>, acts: &mut Vec<Action>) {
    if let Some(drag) = egui::DragAndDrop::payload::<Drag>(ui.ctx())
        && let Drag::Project(id) = *drag
        && pointer_over(ui, r).is_some()
    {
        ui.painter().rect_stroke(r.shrink(1.0), 5, egui::Stroke::new(1.5, c::EMBER), egui::StrokeKind::Inside);
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
        if released(ui) {
            acts.push(Action::Move(ProjectMove { id, group_id: group, before: None }));
        }
    }
}

pub fn ungrouped_row(ui: &mut egui::Ui, acts: &mut Vec<Action>) {
    let (r, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 26.0), Sense::hover());
    ui.painter().text(
        Pos2::new(r.left() + 10.0, r.center().y),
        Align2::LEFT_CENTER,
        "그룹 없음",
        font(11.5),
        c::TEXT_3,
    );
    bucket_drop(ui, r, None, acts);
}

pub fn group_row(ui: &mut egui::Ui, group: &ProjectGroup, groups: &[ProjectGroup], acts: &mut Vec<Action>) {
    ui.add_space(4.0);
    let resp =
        ui.push_id(group.id, |ui| widgets::row(ui, 28.0, false, &group.name).interact(Sense::click_and_drag())).inner;
    let r = resp.rect;
    let icon = if group.collapsed { icons::CHEVRON_RIGHT } else { icons::CHEVRON_DOWN };
    ui.painter().text(
        Pos2::new(r.left() + 10.0, r.center().y),
        Align2::CENTER_CENTER,
        icon,
        icon_font(12.0),
        c::TEXT_3,
    );
    widgets::paint_clipped(
        ui,
        Rect::from_min_max(Pos2::new(r.left() + 23.0, r.top()), Pos2::new(r.right() - 6.0, r.bottom())),
        &group.name,
        medium(12.5),
        c::TEXT_3,
        true,
    );
    if resp.clicked() {
        acts.push(Action::Collapse(group.id, !group.collapsed));
    }
    resp.dnd_set_drag_payload(Drag::Group(group.id));
    resp.clone()
        .on_hover_text(format!("{}\n끌어서 그룹 순서를 바꿔요. 프로젝트를 놓으면 이 그룹으로 옮겨요", group.name));
    resp.context_menu(|ui| {
        if widgets::menu_item(ui, icons::PENCIL, "이름 바꾸기").clicked() {
            acts.push(Action::Rename(group.clone()));
        }
        let index = groups.iter().position(|g| g.id == group.id).unwrap_or(0);
        if ui.add_enabled_ui(index > 0, |ui| widgets::menu_item(ui, icons::ARROW_UP, "위로 이동")).inner.clicked() {
            acts.push(Action::MoveGroup(ProjectGroupMove { id: group.id, before: Some(groups[index - 1].id) }));
        }
        if ui
            .add_enabled_ui(index + 1 < groups.len(), |ui| widgets::menu_item(ui, icons::ARROW_DOWN, "아래로 이동"))
            .inner
            .clicked()
        {
            acts.push(Action::MoveGroup(ProjectGroupMove {
                id: group.id,
                before: groups.get(index + 2).map(|g| g.id),
            }));
        }
        widgets::menu_sep(ui);
        if widgets::menu_item_danger(ui, icons::TRASH_2, "그룹 삭제")
            .on_hover_text("프로젝트는 ‘그룹 없음’으로 옮겨요. 폴더와 기록은 그대로예요")
            .clicked()
        {
            acts.push(Action::Delete(group.id));
        }
    });
    bucket_drop(ui, r, Some(group.id), acts);
    if let Some(drag) = egui::DragAndDrop::payload::<Drag>(ui.ctx())
        && let Drag::Group(id) = *drag
        && id != group.id
        && let Some(pos) = pointer_over(ui, r)
    {
        let below = pos.y >= r.center().y;
        let before = if below {
            groups.iter().position(|g| g.id == group.id).and_then(|i| groups.get(i + 1)).map(|g| g.id)
        } else {
            Some(group.id)
        };
        drop_line(ui, r, below);
        if released(ui) {
            acts.push(Action::MoveGroup(ProjectGroupMove { id, before }));
        }
    }
}

pub fn add_row(app: &mut App, ui: &mut egui::Ui, acts: &mut Vec<Act>) {
    ui.add_space(4.0);
    let mut create = false;
    ui.menu_button(egui::RichText::new("＋ 추가").font(font(12.5)).color(c::TEXT_3), |ui| {
        if widgets::menu_item(ui, icons::FOLDER_OPEN, "폴더 열기…").clicked() {
            acts.push(Act::OpenFolder);
        }
        if widgets::menu_item(ui, icons::FOLDER_PLUS, "새 그룹…").clicked() {
            create = true;
        }
    });
    if create {
        apply(app, vec![Action::Create(None)]);
    }
    if egui::DragAndDrop::payload::<Drag>(ui.ctx()).is_some()
        && let Some(pos) = ui.input(|i| i.pointer.interact_pos()).filter(|p| ui.clip_rect().contains(*p))
    {
        let clip = ui.clip_rect();
        let dy = if pos.y < clip.top() + 20.0 {
            8.0
        } else if pos.y > clip.bottom() - 20.0 {
            -8.0
        } else {
            0.0
        };
        if dy != 0.0 {
            ui.scroll_with_delta(Vec2::new(0.0, dy));
            ui.ctx().request_repaint();
        }
    }
}

pub fn apply(app: &mut App, acts: Vec<Action>) {
    for act in acts {
        match act {
            Action::Move(p) => app.hub.send::<ProjectsMove>(HostKey::Local, p),
            Action::MoveGroup(p) => app.hub.send::<ProjectGroupsMove>(HostKey::Local, p),
            Action::Collapse(id, collapsed) => app.hub.send::<ProjectGroupsUpdate>(
                HostKey::Local,
                ProjectGroupUpdate { id, collapsed: Some(collapsed), ..Default::default() },
            ),
            Action::Delete(id) => app.hub.send::<ProjectGroupsDelete>(HostKey::Local, ById { id }),
            Action::Create(project) if !app.shell.sidebar.organization.saving => {
                app.shell.sidebar.organization.edit = Some(Edit { id: None, project, name: String::new(), focus: true })
            }
            Action::Rename(g) if !app.shell.sidebar.organization.saving => {
                app.shell.sidebar.organization.edit =
                    Some(Edit { id: Some(g.id), project: None, name: g.name, focus: true })
            }
            _ => {}
        }
    }
}

pub fn editor(app: &mut App, ui: &mut egui::Ui) {
    let st = &mut app.shell.sidebar.organization;
    let Some(edit) = &mut st.edit else { return };
    let mut save = false;
    let mut cancel = false;
    egui::Frame::new().fill(c::PANEL).corner_radius(6).inner_margin(8).show(ui, |ui| {
        ui.label(
            egui::RichText::new(if edit.id.is_some() { "그룹 이름 바꾸기" } else { "새 그룹" }).font(medium(12.5)),
        );
        ui.add_enabled_ui(!st.saving, |ui| {
            let resp = ui.add(
                egui::TextEdit::singleline(&mut edit.name).hint_text("그룹 이름").desired_width(ui.available_width()),
            );
            if std::mem::take(&mut edit.focus) {
                resp.request_focus();
                resp.scroll_to_me(Some(egui::Align::Min));
            }
            save = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            cancel = resp.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape));
            ui.horizontal(|ui| {
                save |= ui
                    .add_enabled_ui(!edit.name.trim().is_empty(), |ui| theme::secondary_button(ui, "저장"))
                    .inner
                    .clicked();
                cancel |= theme::link_button(ui, "취소").clicked();
            });
        });
        if st.saving {
            ui.label(egui::RichText::new("저장하는 중…").font(font(11.5)).color(c::TEXT_3));
        }
    });
    if cancel && !st.saving {
        st.edit = None;
        return;
    }
    if save && !st.saving && !edit.name.trim().is_empty() {
        let name = edit.name.trim().to_string();
        st.saving = true;
        if let Some(id) = edit.id {
            app.hub.call::<ProjectGroupsUpdate>(
                HostKey::Local,
                ProjectGroupUpdate { id, name: Some(name), ..Default::default() },
                saved,
            );
        } else {
            app.hub.call::<ProjectGroupsCreate>(
                HostKey::Local,
                ProjectGroupCreate { name, project: edit.project },
                saved,
            );
        }
    }
}

fn saved(app: &mut App, result: Result<ProjectGroup, runwick_core::RpcError>) {
    app.shell.sidebar.organization.saving = false;
    match result {
        Ok(_) => {
            app.shell.sidebar.organization.edit = None;
        }
        Err(e) => app.toast_error(&e.message),
    }
}

#[cfg(test)]
mod tests;
