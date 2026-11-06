//! Settings › 역할 (작업 방식): reusable instructions, a preferred tool and an optional checklist
//! that delegation can hand to a session. Create, duplicate, edit and delete, with a preview of
//! exactly what a delegated session receives. Not a list of team members.

use super::general::header;
use crate::App;
use crate::client::HostKey;
use crate::icons;
use crate::shell::{data, widgets};
use crate::theme::{self, c, font, medium, mono, ty};
use egui::{CornerRadius, RichText, Stroke, Vec2};
use runwick_core::api::projects::ById;
use runwick_core::api::roles::*;
use runwick_core::api::sessions::Tool;

const HOST: HostKey = HostKey::Local;

#[derive(Default)]
pub struct State {
    /// The role being edited (`None` id = a new role).
    draft: Option<Draft>,
    /// Another role the person picked while the draft had unsaved changes.
    switch_to: Option<Option<RoleId>>,
    confirm_delete: bool,
    saving: bool,
    error: Option<String>,
}

#[derive(Clone, PartialEq)]
struct Draft {
    id: Option<RoleId>,
    name: String,
    instructions: String,
    tool: Option<Tool>,
    steps: Vec<String>,
}

impl Draft {
    fn new() -> Self {
        Draft { id: None, name: String::new(), instructions: String::new(), tool: None, steps: Vec::new() }
    }

    fn of(r: &Role) -> Self {
        Draft {
            id: Some(r.id),
            name: r.name.clone(),
            instructions: r.instructions.clone(),
            tool: r.preferred_tool,
            steps: r.procedure.clone(),
        }
    }

    fn preview(&self) -> String {
        Role {
            id: self.id.unwrap_or_default(),
            name: if self.name.trim().is_empty() { "(이름 없음)".into() } else { self.name.trim().into() },
            instructions: self.instructions.clone(),
            preferred_tool: self.tool,
            procedure: self.steps.clone(),
            created_at: runwick_core::now(),
            updated_at: runwick_core::now(),
        }
        .brief()
    }
}

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    header(
        ui,
        "역할·작업 방식",
        "위임할 때 다시 쓸 지시문·선호 도구·절차를 저장해요. 실제 팀원이나 실행 중인 세션 목록이 아니에요.",
    );
    let q = app.hub.query::<RolesList>(HOST, &());
    let Some(list) = q.data else {
        let text = match &q.error {
            Some(e) => format!("역할을 불러오지 못했어요: {}", e.message),
            None => "역할을 불러오는 중…".into(),
        };
        ui.label(RichText::new(text).font(font(ty::BODY)).color(c::TEXT_3));
        return;
    };
    // The edited role was deleted elsewhere: keep the text as a new role instead of losing it.
    if let Some(d) = app.feat.settings.roles.draft.as_mut()
        && let Some(id) = d.id
        && !list.iter().any(|r| r.id == id)
    {
        d.id = None;
    }
    let narrow = ui.available_width() < 640.0;
    if narrow {
        role_list(app, ui, &list);
        ui.add_space(12.0);
        editor(app, ui, &list);
    } else {
        ui.horizontal_top(|ui| {
            ui.vertical(|ui| {
                ui.set_width(220.0);
                role_list(app, ui, &list);
            });
            ui.add_space(12.0);
            ui.vertical(|ui| editor(app, ui, &list));
        });
    }
}

fn dirty(app: &App, list: &[Role]) -> bool {
    match &app.feat.settings.roles.draft {
        None => false,
        Some(d) => match d.id.and_then(|id| list.iter().find(|r| r.id == id)) {
            Some(r) => *d != Draft::of(r),
            None => *d != Draft::new(),
        },
    }
}

fn select(app: &mut App, list: &[Role], target: Option<RoleId>) {
    let st = &mut app.feat.settings.roles;
    st.confirm_delete = false;
    st.error = None;
    st.switch_to = None;
    st.draft = match target {
        Some(id) => list.iter().find(|r| r.id == id).map(Draft::of),
        None => Some(Draft::new()),
    };
}

fn role_list(app: &mut App, ui: &mut egui::Ui, list: &[Role]) {
    let mut pick: Option<Option<RoleId>> = None;
    if widgets::pill_button(ui, Some(icons::PLUS), "새 역할", 28.0, c::CONTROL).clicked() {
        pick = Some(None);
    }
    ui.add_space(6.0);
    let current = app.feat.settings.roles.draft.as_ref().and_then(|d| d.id);
    egui::Frame::new().fill(c::PANEL).corner_radius(CornerRadius::same(12)).inner_margin(egui::Margin::same(4)).show(
        ui,
        |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 1.0;
            if list.is_empty() {
                widgets::empty_note(ui, "아직 저장한 역할이 없어요. ‘새 역할’로 자주 맡기는 일을 정리해 두세요.");
            }
            for r in list {
                let on = current == Some(r.id);
                let resp = widgets::row(ui, 40.0, on, &r.name);
                let rect = resp.rect.shrink2(Vec2::new(10.0, 4.0));
                let top = egui::Rect::from_min_size(rect.min, Vec2::new(rect.width(), 17.0));
                widgets::paint_clipped(ui, top, &r.name, medium(ty::BODY), c::TEXT_STRONG, true);
                let sub = match r.preferred_tool {
                    Some(t) => format!("{} · 절차 {}단계", t.label(), r.procedure.len()),
                    None => format!("도구 지정 없음 · 절차 {}단계", r.procedure.len()),
                };
                let bottom = egui::Rect::from_min_size(rect.min + Vec2::new(0.0, 17.0), Vec2::new(rect.width(), 15.0));
                widgets::paint_clipped(ui, bottom, &sub, font(11.5), c::TEXT_3, true);
                if resp.on_hover_text(&r.name).clicked() && !on {
                    pick = Some(Some(r.id));
                }
            }
        },
    );
    if let Some(target) = pick {
        if dirty(app, list) {
            app.feat.settings.roles.switch_to = Some(target);
        } else {
            select(app, list, target);
        }
    }
}

fn editor(app: &mut App, ui: &mut egui::Ui, list: &[Role]) {
    let is_dirty = dirty(app, list);
    if let Some(target) = app.feat.settings.roles.switch_to {
        let mut go = false;
        let mut stay = false;
        egui::Frame::new()
            .fill(theme::Tone::Check.bg())
            .corner_radius(CornerRadius::same(8))
            .inner_margin(egui::Margin::symmetric(12, 8))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new("저장하지 않은 변경이 있어요.").font(font(12.5)).color(c::TEXT));
                    go = theme::link_button(ui, "버리고 이동").clicked();
                    stay = theme::link_button(ui, "계속 편집").clicked();
                });
            });
        ui.add_space(8.0);
        if go {
            select(app, list, target);
        } else if stay {
            app.feat.settings.roles.switch_to = None;
        }
    }
    let Some(mut d) = app.feat.settings.roles.draft.clone() else {
        egui::Frame::new()
            .fill(c::PANEL)
            .corner_radius(CornerRadius::same(12))
            .inner_margin(egui::Margin::same(16))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(
                    RichText::new("왼쪽에서 역할을 고르거나 새 역할을 만드세요.").font(font(12.5)).color(c::TEXT_3),
                );
            });
        return;
    };
    let mut act = None;
    egui::Frame::new().fill(c::PANEL).corner_radius(CornerRadius::same(12)).inner_margin(egui::Margin::same(16)).show(
        ui,
        |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 6.0;
            field(ui, "이름");
            ui.add(
                egui::TextEdit::singleline(&mut d.name)
                    .hint_text("예: 회귀 검토")
                    .char_limit(60)
                    .desired_width(f32::INFINITY)
                    .margin(egui::Margin::symmetric(10, 7)),
            );
            field(ui, "선호 도구");
            let tools = [None, Some(Tool::Claude), Some(Tool::Codex)];
            let items = [theme::Seg::text("지정 안 함"), theme::Seg::text("Claude Code"), theme::Seg::text("Codex")];
            let sel = tools.iter().position(|t| *t == d.tool).unwrap_or(0);
            if let Some(i) = theme::segmented(ui, &items, sel, 26.0) {
                d.tool = tools[i];
            }
            field(ui, "지시문");
            ui.add(
                egui::TextEdit::multiline(&mut d.instructions)
                    .hint_text("이 역할을 맡은 세션이 무엇을, 어떤 기준으로 하면 되는지 적어요")
                    .desired_rows(6)
                    .desired_width(f32::INFINITY)
                    .margin(egui::Margin::symmetric(10, 8)),
            );
            field(ui, "절차 (선택)");
            let mut remove = None;
            for (i, s) in d.steps.iter_mut().enumerate() {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("{}.", i + 1)).font(mono(12.0)).color(c::TEXT_3));
                    let w = ui.available_width() - 32.0;
                    ui.add(egui::TextEdit::singleline(s).desired_width(w).margin(egui::Margin::symmetric(8, 5)));
                    if theme::icon_button(ui, icons::X, &format!("{}단계 빼기", i + 1)).clicked() {
                        remove = Some(i);
                    }
                });
            }
            if let Some(i) = remove {
                d.steps.remove(i);
            }
            if d.steps.len() < 30 && theme::link_button(ui, "+ 단계 추가").clicked() {
                d.steps.push(String::new());
            }
            ui.add_space(4.0);
            field(ui, "미리보기 · 위임할 때 이렇게 전달돼요");
            egui::Frame::new()
                .fill(c::TERMINAL)
                .corner_radius(CornerRadius::same(8))
                .inner_margin(egui::Margin::symmetric(12, 10))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.add(egui::Label::new(RichText::new(d.preview()).font(mono(12.0)).color(c::TEXT)).wrap());
                });
            let st = &app.feat.settings.roles;
            if let Some(e) = &st.error {
                ui.label(RichText::new(e).font(font(12.0)).color(c::ROSE));
            }
            ui.add_space(4.0);
            let r = ui.available_rect_before_wrap();
            ui.painter().hline(r.x_range(), r.top(), Stroke::new(1.0, c::LINE));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if d.id.is_some() {
                    if st.confirm_delete {
                        ui.label(RichText::new("정말 삭제할까요?").font(font(12.5)).color(c::ROSE));
                        if theme::danger_button(ui, "삭제").clicked() {
                            act = Some(Act::Delete);
                        }
                        if theme::link_button(ui, "아니요").clicked() {
                            act = Some(Act::KeepRole);
                        }
                    } else {
                        if theme::danger_button(ui, "삭제…").clicked() {
                            act = Some(Act::AskDelete);
                        }
                        if theme::secondary_button(ui, "복제").clicked() {
                            act = Some(Act::Duplicate);
                        }
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let can = (is_dirty || d.id.is_none()) && !d.name.trim().is_empty() && !st.saving;
                    let label = if st.saving { "저장하는 중…" } else { "저장" };
                    let b = ui.add_enabled(
                        can,
                        egui::Button::new(RichText::new(label).font(medium(ty::BODY)).color(c::ON_EMBER))
                            .fill(c::EMBER)
                            .corner_radius(CornerRadius::same(7))
                            .min_size(Vec2::new(64.0, 30.0)),
                    );
                    if b.clicked() {
                        act = Some(Act::Save);
                    }
                    if is_dirty {
                        if theme::link_button(ui, "되돌리기").clicked() {
                            act = Some(Act::Revert);
                        }
                        ui.label(RichText::new("저장 안 됨").font(font(12.0)).color(c::YELLOW));
                    }
                });
            });
            if let Some(r) = d.id.and_then(|id| list.iter().find(|r| r.id == id)) {
                ui.label(RichText::new(format!("{} 저장", data::ago(r.updated_at))).font(font(11.5)).color(c::TEXT_3));
            }
        },
    );
    app.feat.settings.roles.draft = Some(d.clone());
    match act {
        Some(Act::Save) => save(app, d),
        Some(Act::AskDelete) => app.feat.settings.roles.confirm_delete = true,
        Some(Act::KeepRole) => app.feat.settings.roles.confirm_delete = false,
        Some(Act::Revert) => select(app, list, d.id),
        Some(Act::Delete) => {
            if let Some(id) = d.id {
                app.hub.call::<RolesDelete>(HOST, ById { id }, |app, res| match res {
                    Ok(()) => {
                        let st = &mut app.feat.settings.roles;
                        st.draft = None;
                        st.confirm_delete = false;
                        app.toast_info("역할을 삭제했어요");
                    }
                    Err(e) => app.feat.settings.roles.error = Some(e.message),
                });
            }
        }
        Some(Act::Duplicate) => {
            if let Some(id) = d.id {
                app.hub.call::<RolesDuplicate>(HOST, ById { id }, |app, res| match res {
                    Ok(r) => {
                        app.feat.settings.roles.draft = Some(Draft::of(&r));
                        app.toast_info(&format!("‘{}’을 만들었어요", r.name));
                    }
                    Err(e) => app.feat.settings.roles.error = Some(e.message),
                });
            }
        }
        None => {}
    }
}

enum Act {
    Save,
    AskDelete,
    KeepRole,
    Delete,
    Duplicate,
    Revert,
}

fn save(app: &mut App, d: Draft) {
    let st = &mut app.feat.settings.roles;
    st.saving = true;
    st.error = None;
    let p =
        RoleSave { id: d.id, name: d.name, instructions: d.instructions, preferred_tool: d.tool, procedure: d.steps };
    app.hub.call::<RolesSave>(HOST, p, |app, res| {
        let st = &mut app.feat.settings.roles;
        st.saving = false;
        match res {
            Ok(r) => {
                st.draft = Some(Draft::of(&r));
                app.toast_info(&format!("‘{}’ 역할을 저장했어요", r.name));
            }
            Err(e) => st.error = Some(e.message),
        }
    });
}

fn field(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).font(medium(12.0)).color(c::TEXT_3));
}
