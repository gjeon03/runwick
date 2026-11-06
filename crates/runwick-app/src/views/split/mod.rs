//! S10 분리해서 작업 (SplitWork.dc.html, SplitFail.dc.html): name, start point, new branch, the
//! folder it will make and whether to carry uncommitted changes, then the creation steps with
//! their real results. A CLI failure keeps the new workspace and offers "CLI만 다시 시도";
//! nothing falls back to the default workspace. Merging is the `Merge` tab (`merge`).

mod merge;

pub use merge::{open_merge, show_tab as show_merge_tab};

use crate::App;
use crate::client::HostKey;
use crate::icons;
use crate::shell::{actions, data, dialog, widgets};
use crate::state::WsKey;
use crate::theme::{self, Tone, c, font, icon, medium, mono, semibold};
use egui::{CornerRadius, Margin, RichText};
use runwick_core::api::projects::ById;
use runwick_core::api::sessions::{SessionsStart, StartSession, Tool};
use runwick_core::api::worktree::*;
use runwick_core::josa;

#[derive(Default)]
pub struct State {
    form: Option<Form>,
    /// Merge tab state per worktree workspace.
    pub(crate) merge: merge::State,
}

struct Form {
    ws: WsKey,
    filled: bool,
    name: String,
    base: String,
    branch: String,
    path: String,
    path_edited: bool,
    carry: bool,
    advanced: bool,
    existing: bool,
    tool: Option<Tool>,
    show_changes: bool,
    /// Put the cursor in 작업 이름 on the first frame (opening the modal and typing just works).
    focus_name: bool,
    running: bool,
    result: Option<Created>,
}

impl Form {
    fn new(ws: WsKey) -> Self {
        Self {
            ws,
            filled: false,
            name: String::new(),
            base: String::new(),
            branch: String::new(),
            path: String::new(),
            path_edited: false,
            carry: false,
            advanced: false,
            existing: false,
            tool: Some(Tool::Claude),
            show_changes: false,
            focus_name: true,
            running: false,
            result: None,
        }
    }
}

enum Act {
    Create,
    Close,
    OpenSpace,
    RetryCli,
    WorkHere,
}

pub fn show_modal(app: &mut App, ctx: &egui::Context, ws: WsKey) -> bool {
    if ws.host != HostKey::Local {
        app.toast_info("다른 기기의 공간은 그 기기에서 분리해요");
        return false;
    }
    if app.feat.split.form.as_ref().is_none_or(|f| f.ws != ws) {
        app.feat.split.form = Some(Form::new(ws));
    }
    let pq = app.hub.query::<WorktreePlan>(ws.host, &ById { id: ws.id });
    let plan = pq.data.as_deref().cloned();
    let changes = if app.feat.split.form.as_ref().is_some_and(|f| f.show_changes) {
        app.hub.query::<WorktreeChanges>(ws.host, &ById { id: ws.id }).data.map(|s| s.as_ref().clone())
    } else {
        None
    };
    let tools = data::tool_lines(app, ws.host, true);
    let Some(form) = app.feat.split.form.as_mut() else {
        return false;
    };
    if let (Some(p), false) = (&plan, form.filled) {
        form.name = p.suggested_name.clone();
        form.branch = p.suggested_branch.clone();
        form.path = p.suggested_path.clone();
        form.base = p.base_branch.clone().unwrap_or_else(|| p.base_rev.clone().unwrap_or_default());
        form.filled = true;
    }
    let mut act = None;
    let ((), close) = dialog(ctx, "split-work", 820.0, |ui| {
        ui.label(RichText::new("분리해서 작업").font(semibold(17.0)).color(c::TEXT_STRONG));
        ui.label(
            RichText::new("별도 폴더와 branch에서 일합니다. 기본 공간의 파일은 건드리지 않아요.")
                .font(font(12.5))
                .color(c::TEXT_2),
        );
        ui.add_space(8.0);
        let Some(p) = &plan else {
            let t = pq
                .error
                .as_ref()
                .map(|e| format!("확인하지 못했어요: {}", e.message))
                .unwrap_or_else(|| "Git 상태를 확인하는 중…".into());
            widgets::empty_note(ui, &t);
            if theme::secondary_button(ui, "닫기").clicked() {
                act = Some(Act::Close);
            }
            return;
        };
        if !p.is_git || p.base_rev.is_none() {
            not_git(ui, p, &mut act);
            return;
        }
        let two = ui.available_width() > 640.0;
        let col = if two { (ui.available_width() - 20.0) / 2.0 } else { ui.available_width() };
        let body = |ui: &mut egui::Ui, form: &mut Form, act: &mut Option<Act>| {
            if two {
                ui.horizontal_top(|ui| {
                    ui.spacing_mut().item_spacing.x = 20.0;
                    ui.vertical(|ui| {
                        ui.set_width(col);
                        fields(ui, form, p, changes.as_deref(), &tools);
                    });
                    ui.vertical(|ui| {
                        ui.set_width(col);
                        steps(ui, form);
                    });
                });
            } else {
                fields(ui, form, p, changes.as_deref(), &tools);
                steps(ui, form);
            }
            ui.add_space(6.0);
            footer(ui, form, act);
        };
        body(ui, form, &mut act);
    });
    let keep = match act {
        Some(Act::Close) => false,
        Some(Act::WorkHere) => false,
        Some(Act::Create) => {
            create(app, ws);
            true
        }
        Some(Act::OpenSpace) => {
            open_created(app, false);
            false
        }
        Some(Act::RetryCli) => {
            retry_cli(app);
            true
        }
        None => !close || app.feat.split.form.as_ref().is_some_and(|f| f.running),
    };
    if !keep {
        app.feat.split.form = None;
    }
    keep
}

fn not_git(ui: &mut egui::Ui, p: &Plan, act: &mut Option<Act>) {
    egui::Frame::new().fill(c::ROW).corner_radius(CornerRadius::same(8)).inner_margin(Margin::same(12)).show(
        ui,
        |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_top(|ui| {
                ui.label(icon(Tone::Waiting.icon(), 14.0, Tone::Waiting.fg()));
                ui.add(
                    egui::Label::new(
                        RichText::new(p.reason.clone().unwrap_or_else(|| "분리할 수 없는 폴더예요".into()))
                            .font(font(12.5))
                            .color(c::TEXT),
                    )
                    .wrap(),
                );
            });
        },
    );
    ui.add_space(8.0);
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        if theme::primary_button(ui, "현재 공간에서 작업").clicked() {
            *act = Some(Act::WorkHere);
        }
        if theme::secondary_button(ui, "닫기").clicked() {
            *act = Some(Act::Close);
        }
    });
}

/// A new name: the planned folder and (for a new branch) `work/<name>` follow it.
fn set_name(form: &mut Form, p: &Plan, name: &str) {
    form.name = name.to_string();
    let name = name.trim();
    if !form.path_edited
        && let Some(parent) = std::path::Path::new(&p.suggested_path).parent()
    {
        form.path = parent.join(name).to_string_lossy().into_owned();
    }
    if !form.existing {
        form.branch = format!("work/{}", fixed_branch(name));
    }
}

/// Why a work name cannot be a folder name here (the host checks the same: letters, digits,
/// `-`, `_`, `.`, not starting with `.`).
fn name_problem(name: &str) -> Option<&'static str> {
    let n = name.trim();
    if n.is_empty() {
        Some("작업 이름을 적어 주세요")
    } else if n.starts_with('.') {
        Some("작업 이름은 .으로 시작할 수 없어요")
    } else if n.chars().any(char::is_whitespace) {
        Some("작업 이름에는 띄어쓰기를 쓸 수 없어요")
    } else if !n.chars().all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.')) {
        Some("작업 이름에는 글자, 숫자, -, _, . 만 쓸 수 있어요")
    } else {
        None
    }
}

/// Spaces become `-`; other characters a folder name cannot have are dropped.
fn fixed_name(name: &str) -> String {
    let mut out = String::new();
    for c in name.trim().chars() {
        if c.is_whitespace() {
            if !out.ends_with('-') {
                out.push('-');
            }
        } else if c.is_alphanumeric() || matches!(c, '-' | '_' | '.') {
            out.push(c);
        }
    }
    out.trim_start_matches('.').trim_matches('-').to_string()
}

/// What `git check-ref-format --branch` would refuse, in words.
fn branch_problem(branch: &str) -> Option<&'static str> {
    let b = branch.trim();
    if b.is_empty() {
        return Some("branch 이름을 적어 주세요");
    }
    if b.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Some("branch 이름에는 띄어쓰기를 쓸 수 없어요");
    }
    if b.chars().any(|c| matches!(c, '~' | '^' | ':' | '?' | '*' | '[' | '\\')) {
        return Some("branch 이름에는 ~ ^ : ? * [ \\ 를 쓸 수 없어요");
    }
    let bad_part = b.split('/').any(|part| part.is_empty() || part.starts_with('.') || part.ends_with(".lock"));
    if b.contains("..") || b.contains("@{") || b == "@" || b.starts_with('-') || b.ends_with('.') || bad_part {
        return Some("git이 받지 않는 branch 이름이에요 (.., @{, 빈 단계, .으로 시작·끝 등)");
    }
    None
}

/// A branch name git accepts, made from what was typed.
fn fixed_branch(branch: &str) -> String {
    let mut out = String::new();
    for c in branch.trim().chars() {
        if c.is_whitespace() {
            if !out.ends_with('-') {
                out.push('-');
            }
        } else if !c.is_control() && !matches!(c, '~' | '^' | ':' | '?' | '*' | '[' | '\\') {
            out.push(c);
        }
    }
    while out.contains("..") {
        out = out.replace("..", ".");
    }
    let parts: Vec<String> = out
        .replace("@{", "@")
        .split('/')
        .map(|p| p.trim_start_matches('.').trim_end_matches(".lock").to_string())
        .filter(|p| !p.is_empty())
        .collect();
    let fixed = parts.join("/").trim_start_matches('-').trim_end_matches('.').to_string();
    if fixed == "@" { String::new() } else { fixed }
}

/// A problem under a field, with a suggested fix to take.
fn problem_line(ui: &mut egui::Ui, why: &str, fix: Option<&str>, take: &mut bool) {
    ui.horizontal_wrapped(|ui| {
        ui.label(icon(icons::CIRCLE_ALERT, 12.5, c::ROSE));
        ui.label(RichText::new(why).font(font(12.0)).color(c::ROSE));
        if let Some(f) = fix {
            let link = egui::Label::new(
                RichText::new(format!("{} 바꾸기", josa::euro_ro(f))).font(font(12.0)).color(c::TEXT_2).underline(),
            )
            .sense(egui::Sense::click());
            if ui.add(link).clicked() {
                *take = true;
            }
        }
    });
}

fn field_label(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).font(font(12.0)).color(c::TEXT_3));
}

fn input(ui: &mut egui::Ui, value: &mut String, mono_font: bool, enabled: bool) -> egui::Response {
    let f = if mono_font { mono(12.0) } else { font(13.0) };
    ui.add_enabled(
        enabled,
        egui::TextEdit::singleline(value).font(f).desired_width(f32::INFINITY).margin(Margin::symmetric(10, 7)),
    )
}

fn fields(
    ui: &mut egui::Ui,
    form: &mut Form,
    p: &Plan,
    changes: Option<&str>,
    tools: &[(Tool, data::ToolLine, Option<runwick_core::api::tools::ToolInfo>)],
) {
    let editable = !form.running && form.result.as_ref().is_none_or(|r| r.workspace.is_none());
    ui.spacing_mut().item_spacing.y = 4.0;
    field_label(ui, "작업 이름");
    let name_field = input(ui, &mut form.name, false, editable);
    if form.focus_name && editable {
        name_field.request_focus();
        form.focus_name = false;
    }
    if name_field.changed() {
        let name = form.name.clone();
        set_name(form, p, &name);
    }
    // Checked as it is typed, with a fixed name to take (the host refuses the same names).
    if let Some(why) = name_problem(&form.name) {
        let fixed = fixed_name(&form.name);
        let mut take = false;
        problem_line(ui, why, (!fixed.is_empty() && fixed != form.name).then_some(fixed.as_str()), &mut take);
        if take && editable {
            set_name(form, p, &fixed);
        }
    }
    ui.add_space(8.0);
    ui.columns(2, |cols| {
        field_label(&mut cols[0], "출발 기준");
        input(&mut cols[0], &mut form.base, true, editable && !form.existing);
        field_label(&mut cols[1], if form.existing { "기존 branch" } else { "새 branch" });
        input(&mut cols[1], &mut form.branch, true, editable);
    });
    if let Some(why) = branch_problem(&form.branch) {
        let fixed = fixed_branch(&form.branch);
        let mut take = false;
        problem_line(ui, why, (!fixed.is_empty() && fixed != form.branch).then_some(fixed.as_str()), &mut take);
        if take && editable {
            form.branch = fixed;
        }
    }
    if let Some(rev) = &p.base_rev {
        ui.label(RichText::new(format!("지금 HEAD {rev}")).font(mono(11.0)).color(c::TEXT_3));
    }
    ui.add_space(8.0);
    field_label(ui, "만들어질 폴더");
    if form.advanced {
        if input(ui, &mut form.path, true, editable).changed() {
            form.path_edited = true;
        }
    } else {
        ui.add(egui::Label::new(RichText::new(data::tilde(&form.path)).font(mono(11.5)).color(c::TEXT)).wrap());
    }
    ui.add_space(8.0);
    let tracked = p.dirty.len();
    let title = if tracked == 0 {
        "커밋하지 않은 변경이 없어요".to_string()
    } else {
        format!("아직 커밋하지 않은 변경 {tracked}개 파일")
    };
    field_label(ui, &title);
    if p.untracked > 0 {
        ui.label(
            RichText::new(format!("추적하지 않는 파일 {}개는 옮기지 않아요", p.untracked))
                .font(font(12.0))
                .color(c::TEXT_3),
        );
    }
    if tracked > 0 {
        for (on, text) in [(false, "포함하지 않음"), (true, "확인한 patch만 포함")] {
            let selected = form.carry == on;
            egui::Frame::new()
                .fill(if selected { c::CONTROL } else { c::ROW })
                .corner_radius(CornerRadius::same(7))
                .inner_margin(Margin::symmetric(10, 4))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(
                                editable,
                                egui::RadioButton::new(selected, RichText::new(text).font(font(13.0))),
                            )
                            .clicked()
                        {
                            form.carry = on;
                        }
                        if on {
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                let t = if form.show_changes { "변경 접기" } else { "변경 보기" };
                                if ui
                                    .add(
                                        egui::Label::new(
                                            RichText::new(t).font(font(12.0)).color(c::TEXT_2).underline(),
                                        )
                                        .sense(egui::Sense::click()),
                                    )
                                    .clicked()
                                {
                                    form.show_changes = !form.show_changes;
                                }
                            });
                        }
                    });
                });
        }
        if form.show_changes {
            egui::Frame::new()
                .fill(c::TERMINAL)
                .corner_radius(CornerRadius::same(7))
                .inner_margin(Margin::same(8))
                .show(ui, |ui| {
                    egui::ScrollArea::vertical().max_height(160.0).show(ui, |ui| {
                        let text = changes.unwrap_or("변경을 불러오는 중…");
                        ui.add(egui::Label::new(RichText::new(text).font(mono(11.5)).color(c::TEXT_2)).extend());
                    });
                });
        }
    }
    ui.add_space(6.0);
    let glyph = if form.advanced { icons::CHEVRON_DOWN } else { icons::CHEVRON_RIGHT };
    let adv = ui.add(
        egui::Label::new(crate::views::collab::with_icon(
            glyph,
            "고급: 기존 branch 쓰기, 폴더 위치, 시작할 CLI",
            font(12.5),
            c::TEXT_2,
            false,
        ))
        .sense(egui::Sense::click()),
    );
    if adv.clicked() {
        form.advanced = !form.advanced;
    }
    if form.advanced {
        if ui
            .add_enabled(
                editable,
                egui::Checkbox::new(&mut form.existing, RichText::new("기존 branch 쓰기").font(font(12.5))),
            )
            .changed()
            && form.existing
            && !p.branches.contains(&form.branch)
        {
            form.branch = p.branches.first().cloned().unwrap_or_default();
        }
        field_label(ui, "시작할 CLI");
        ui.horizontal_wrapped(|ui| {
            if ui.add_enabled(editable, egui::RadioButton::new(form.tool.is_none(), "시작하지 않음")).clicked() {
                form.tool = None;
            }
            for (t, line, _) in tools {
                if *t == Tool::Shell || !line.startable {
                    continue;
                }
                if ui.add_enabled(editable, egui::RadioButton::new(form.tool == Some(*t), t.label())).clicked() {
                    form.tool = Some(*t);
                }
            }
        });
    }
}

fn step_look(state: Option<StepState>) -> (&'static str, egui::Color32, &'static str) {
    match state {
        Some(StepState::Done) => (icons::CHECK, c::MINT, "완료"),
        Some(StepState::Failed) => (icons::TRIANGLE_ALERT, c::ROSE, "실패"),
        Some(StepState::Skipped) => (icons::MINUS, c::TEXT_OFF, "진행하지 않음"),
        None => (icons::CIRCLE, c::TEXT_OFF, "대기"),
    }
}

fn steps(ui: &mut egui::Ui, form: &Form) {
    let mut kinds = vec![StepKind::CheckGit, StepKind::CreateWorktree];
    if form.carry {
        kinds.push(StepKind::CarryChanges);
    }
    kinds.push(StepKind::Link);
    if form.tool.is_some() {
        kinds.push(StepKind::StartCli);
    }
    let result = form.result.as_ref();
    let cli_failed =
        result.is_some_and(|r| r.workspace.is_some() && r.steps.iter().any(|s| s.state == StepState::Failed));
    egui::Frame::new().fill(c::PANEL).corner_radius(CornerRadius::same(10)).inner_margin(Margin::same(14)).show(
        ui,
        |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 8.0;
            let title = if form.running {
                "만드는 중…"
            } else if cli_failed {
                "공간은 만들어졌고, 일부 단계만 실패했어요"
            } else if result.is_some_and(|r| r.workspace.is_none()) {
                "공간을 만들지 못했어요"
            } else {
                "만들기를 누르면 이 순서로 진행해요"
            };
            ui.label(RichText::new(title).font(semibold(13.0)).color(c::TEXT_STRONG));
            for k in kinds {
                let s = result.and_then(|r| r.steps.iter().find(|s| s.kind == k));
                let (glyph, color, word) = step_look(s.map(|s| s.state));
                let label = if k == StepKind::StartCli {
                    format!("{} 시작", form.tool.map(|t| t.label()).unwrap_or("CLI"))
                } else {
                    k.label().to_string()
                };
                ui.horizontal_top(|ui| {
                    ui.label(icon(glyph, 13.0, color)).on_hover_text(word);
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 1.0;
                        let fg = if s.is_some_and(|s| s.state == StepState::Failed) {
                            c::TEXT_STRONG
                        } else if s.is_some() {
                            c::TEXT
                        } else {
                            c::TEXT_2
                        };
                        ui.label(RichText::new(label).font(font(13.0)).color(fg));
                        if let Some(m) = s.and_then(|s| s.message.clone()) {
                            ui.add(egui::Label::new(RichText::new(m).font(font(12.0)).color(c::TEXT_2)).wrap());
                        }
                    });
                });
            }
            if cli_failed {
                ui.label(
                    RichText::new("만들어진 공간은 그대로 있고, 기본 공간으로 돌아가 실행하지 않았습니다.")
                        .font(font(12.0))
                        .color(c::TEXT_2),
                );
            }
            ui.painter().hline(ui.max_rect().x_range(), ui.cursor().top() + 2.0, egui::Stroke::new(1.0, c::LINE));
            ui.add_space(6.0);
            ui.add(
                egui::Label::new(
                    RichText::new(
                        "의존성 설치와 .env 복사는 하지 않아요. 필요한 준비는 새 공간의 CLI에서 직접 해 주세요.",
                    )
                    .font(font(12.5))
                    .color(c::TEXT_2),
                )
                .wrap(),
            );
        },
    );
}

fn footer(ui: &mut egui::Ui, form: &Form, act: &mut Option<Act>) {
    let result = form.result.as_ref();
    let made = result.is_some_and(|r| r.workspace.is_some());
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        if made {
            if theme::primary_button(ui, "CLI만 다시 시도").clicked() {
                *act = Some(Act::RetryCli);
            }
            if theme::secondary_button(ui, "공간만 열기").clicked() {
                *act = Some(Act::OpenSpace);
            }
            if theme::secondary_button(ui, "닫기").clicked() {
                *act = Some(Act::Close);
            }
            return;
        }
        let ok = !form.running && name_problem(&form.name).is_none() && branch_problem(&form.branch).is_none();
        let b = egui::Button::new(
            RichText::new(if form.running { "만드는 중…" } else { "공간 만들기" })
                .font(medium(13.0))
                .color(c::ON_EMBER),
        )
        .fill(c::EMBER)
        .min_size(egui::vec2(0.0, 32.0))
        .corner_radius(CornerRadius::same(7));
        if ui.add_enabled(ok, b).clicked() {
            *act = Some(Act::Create);
        }
        if ui
            .add_enabled(
                !form.running,
                egui::Button::new(RichText::new("취소").font(font(13.0))).min_size(egui::vec2(0.0, 32.0)),
            )
            .clicked()
        {
            *act = Some(Act::Close);
        }
    });
}

fn create(app: &mut App, ws: WsKey) {
    let Some(f) = app.feat.split.form.as_mut() else {
        return;
    };
    f.running = true;
    f.result = None;
    let (cols, rows) = app.shell.center.terminal_grid();
    let p = Create {
        workspace_id: ws.id,
        name: f.name.trim().to_string(),
        branch: f.branch.trim().to_string(),
        base: Some(f.base.trim().to_string()).filter(|b| !b.is_empty() && !f.existing),
        use_existing_branch: f.existing,
        path: f.path_edited.then(|| f.path.trim().to_string()),
        carry_changes: f.carry,
        tool: f.tool,
        cols,
        rows,
    };
    app.hub.call::<WorktreeCreate>(ws.host, p, |app, res| {
        let Some(f) = app.feat.split.form.as_mut() else {
            return;
        };
        f.running = false;
        match res {
            Ok(out) => {
                let all_ok = out.workspace.is_some() && out.steps.iter().all(|s| s.state == StepState::Done);
                f.result = Some(out);
                if all_ok {
                    open_created(app, true);
                    app.feat.split.form = None;
                    if matches!(app.ui.modal, Some(crate::state::Modal::SplitWork { .. })) {
                        app.ui.modal = None;
                    }
                }
            }
            Err(e) => app.toast_error(&e.message),
        }
    });
}

/// Switch to the new workspace (and its CLI tab when one started).
fn open_created(app: &mut App, with_session: bool) {
    let Some(out) = app.feat.split.form.as_ref().and_then(|f| f.result.clone()) else {
        return;
    };
    let Some(ws) = out.workspace else { return };
    let key = WsKey { host: HostKey::Local, id: ws.id };
    match out.session.filter(|_| with_session) {
        Some(s) => actions::open_session(app, ws.project_id, key, s.session.id, false),
        None => actions::select_workspace(app, ws.project_id, key),
    }
    app.toast_info(&format!("{} 공간을 만들었어요 · {}", ws.name, data::tilde(&ws.path)));
}

/// Start only the CLI again in the workspace that was made.
fn retry_cli(app: &mut App) {
    let Some(f) = app.feat.split.form.as_mut() else {
        return;
    };
    let Some(ws) = f.result.as_ref().and_then(|r| r.workspace.clone()) else {
        return;
    };
    let tool = f.tool.unwrap_or(Tool::Claude);
    f.running = true;
    let (cols, rows) = app.shell.center.terminal_grid();
    let p = StartSession {
        workspace_id: ws.id,
        tool,
        title: None,
        initial_prompt: None,
        parent_id: None,
        role: None,
        cols,
        rows,
    };
    app.hub.call::<SessionsStart>(HostKey::Local, p, move |app, res| {
        if let Some(f) = app.feat.split.form.as_mut() {
            f.running = false;
        }
        match res {
            Ok(info) if info.live.as_ref().and_then(|l| l.error.clone()).is_none() => {
                let key = WsKey { host: HostKey::Local, id: ws.id };
                actions::open_session(app, ws.project_id, key, info.session.id, false);
                app.feat.split.form = None;
                app.ui.modal = None;
            }
            Ok(info) => {
                let e = info.live.and_then(|l| l.error).unwrap_or_default();
                if let Some(f) = app.feat.split.form.as_mut()
                    && let Some(r) = f.result.as_mut()
                    && let Some(s) = r.steps.iter_mut().find(|s| s.kind == StepKind::StartCli)
                {
                    s.state = StepState::Failed;
                    s.message = Some(e.clone());
                }
                app.toast_error(&format!("CLI를 시작하지 못했어요: {e}"));
            }
            Err(e) => app.toast_error(&e.message),
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_checked_as_typed_and_fixed() {
        let spaced = "정산 리포트 자동화 테스트 작성 공간";
        assert_eq!(name_problem(spaced), Some("작업 이름에는 띄어쓰기를 쓸 수 없어요"));
        assert_eq!(fixed_name(spaced), "정산-리포트-자동화-테스트-작성-공간");
        assert_eq!(name_problem(&fixed_name(spaced)), None);
        assert_eq!(name_problem("work-1"), None);
        assert!(name_problem(".hidden").is_some() && name_problem("a/b").is_some() && name_problem(" ").is_some());
        assert_eq!(fixed_name(" a/b c "), "ab-c");

        assert_eq!(branch_problem("work/정산-리포트"), None);
        for bad in ["work/정산 리포트", "a..b", "-x", "x/", "x.lock", "a//b", "a:b", "x.", "@", "a@{b"] {
            assert!(branch_problem(bad).is_some(), "{bad}");
            let fixed = fixed_branch(bad);
            assert!(fixed.is_empty() || branch_problem(&fixed).is_none(), "{bad} → {fixed}");
        }
        assert_eq!(fixed_branch("work/정산 리포트"), "work/정산-리포트");
    }
}
