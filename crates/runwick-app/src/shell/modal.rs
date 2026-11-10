//! Modal dialogs. [`dialog`] is the shared frame (dim backdrop, Esc closes, focus stays inside);
//! feature modules use it for their own flows. The shell owns the CLI chooser, rename and the
//! stop confirmation.

use super::{actions, data};
use crate::App;
use crate::client::HostKey;
use crate::state::{RenameTarget, WsKey};
use crate::theme::{self, Tone, c, font, medium, semibold, size, ty};
use egui::{Align2, CornerRadius, Pos2, Rect, RichText, Sense, Vec2};
use runwick_core::api::projects::{ProjectUpdate, ProjectsUpdate, WorkspaceUpdate, WorkspacesUpdate};
use runwick_core::api::sessions::{SessionId, SessionUpdate, SessionsUpdate, Tool};

#[derive(Default)]
pub struct State {
    /// Stop confirmation target (drawn over everything, owned by the shell).
    pub stop: Option<StopAsk>,
    pub last_tool: Option<Tool>,
    new_title: String,
    new_tool: Option<Tool>,
    rename_for: Option<RenameTarget>,
    rename_text: String,
    focus_next: bool,
}

pub struct StopAsk {
    pub host: HostKey,
    pub session: SessionId,
    /// Workers chosen to stop together with the target.
    pub with_workers: Vec<(SessionId, bool)>,
    initialized: bool,
    archive_after: bool,
}

impl State {
    pub fn ask_stop(&mut self, host: HostKey, session: SessionId) {
        self.stop = Some(StopAsk { host, session, with_workers: Vec::new(), initialized: false, archive_after: false });
    }

    pub fn ask_stop_and_archive(&mut self, host: HostKey, session: SessionId) {
        self.ask_stop(host, session);
        self.stop.as_mut().expect("just created").archive_after = true;
    }
}

/// Show a modal dialog of `width`. Returns the content's result and whether it should close
/// (backdrop click or Esc).
pub fn dialog<R>(ctx: &egui::Context, id: &str, width: f32, add: impl FnOnce(&mut egui::Ui) -> R) -> (R, bool) {
    let frame = egui::Frame::new()
        .fill(c::PANEL)
        .stroke(egui::Stroke::new(1.0, c::LINE_STRONG))
        .corner_radius(CornerRadius::same(size::RADIUS_LG))
        .inner_margin(egui::Margin::same(20))
        .shadow(egui::epaint::Shadow {
            offset: [0, 18],
            blur: 56,
            spread: 0,
            color: egui::Color32::from_black_alpha(130),
        });
    let resp = egui::Modal::new(egui::Id::unique(id))
        .backdrop_color(egui::Color32::from_black_alpha(150))
        .frame(frame)
        .show(ctx, |ui| {
            let w = width.min(ctx.content_rect().width() - 48.0);
            ui.set_width(w);
            ui.spacing_mut().item_spacing = Vec2::new(8.0, 8.0);
            add(ui)
        });
    let close = resp.should_close();
    (resp.inner, close)
}

/// Title + optional description at the top of a dialog.
pub fn dialog_title(ui: &mut egui::Ui, title: &str, desc: Option<&str>) {
    ui.label(RichText::new(title).font(semibold(ty::PANEL_TITLE + 1.0)).color(c::TEXT_STRONG));
    if let Some(d) = desc {
        ui.add(egui::Label::new(RichText::new(d).font(font(12.5)).color(c::TEXT_2)).wrap());
    }
    ui.add_space(4.0);
}

/// Draw `ui.modal` through its owner, then the shell's own stop confirmation.
pub fn show_container(app: &mut App, ctx: &egui::Context) {
    if let Some(m) = app.ui.modal.clone() {
        let keep = crate::views::show_modal(app, ctx, &m);
        if !keep && app.ui.modal.as_ref() == Some(&m) {
            app.ui.modal = None;
        }
    }
    super::sidebar::show_dialogs(app, ctx);
    if let Some(ask) = app.shell.modal.stop.as_ref() {
        if ask.archive_after || crate::views::settings::general::confirm_stop() {
            stop_confirm(app, ctx);
        } else {
            // 일반 › "실행을 멈추기 전에 확인" is off: stop only the chosen session, never its workers.
            let (host, session) = (ask.host, ask.session);
            app.shell.modal.stop = None;
            actions::stop_session(app, host, session);
        }
    }
}

/// S01 chooser as a dialog (⌘T, `+`).
pub fn show_new_session_modal(app: &mut App, ctx: &egui::Context, ws: WsKey) -> bool {
    show_session_chooser(app, ctx, ws, None)
}

pub(super) fn show_session_chooser(
    app: &mut App,
    ctx: &egui::Context,
    ws: WsKey,
    target: Option<(crate::state::Pane, Tool)>,
) -> bool {
    let wq = data::workspaces(app);
    let Some(list) = wq.data else {
        // Still loading: keep the dialog requested; it draws as soon as the list arrives.
        return wq.error.is_none();
    };
    let Some(w) = data::find_ws(&list, &ws).cloned() else {
        app.toast_error("작업 공간을 찾을 수 없어요");
        return false;
    };
    let project = data::projects(app)
        .data
        .and_then(|l| data::project_of(&l, w.project_id).map(|p| p.name.clone()))
        .unwrap_or_default();
    let lines = data::tool_lines(app, ws.host, true);
    let default_tool = app
        .shell
        .modal
        .last_tool
        .or_else(|| {
            lines.iter().find(|(t, _, i)| *t != Tool::Shell && i.as_ref().is_some_and(|i| i.installed)).map(|l| l.0)
        })
        .unwrap_or(Tool::Claude);
    let st = &mut app.shell.modal;
    if st.new_tool.is_none() {
        st.new_tool = Some(target.map(|(_, tool)| tool).unwrap_or(default_tool));
        st.new_title.clear();
        st.focus_next = true;
    }
    let mut chosen = st.new_tool.unwrap_or(default_tool);
    let mut title = std::mem::take(&mut st.new_title);
    let focus = std::mem::take(&mut st.focus_next);
    let starting = app.shell.center.starting;
    let mut start = false;
    let mut cancel = false;
    let ((), close) = dialog(ctx, "new-session", 520.0, |ui| {
        dialog_title(ui, "새 CLI 세션", Some(&format!("{project} / {} · {}", w.name, data::tilde(&w.path))));
        for (tool, line, _) in &lines {
            let tool = *tool;
            let on = chosen == tool;
            let w = ui.available_width();
            let (r, resp) = ui
                .allocate_exact_size(Vec2::new(w, 52.0), if line.startable { Sense::click() } else { Sense::hover() });
            let label = format!("{} — {}", tool.label(), line.text);
            resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::RadioButton, line.startable, on, &label));
            let bg = if on {
                c::CONTROL
            } else if resp.hovered() && line.startable {
                c::PANEL_2
            } else {
                c::ROW
            };
            ui.painter().rect_filled(r, CornerRadius::same(8), bg);
            if on {
                ui.painter().rect_stroke(
                    r,
                    CornerRadius::same(8),
                    egui::Stroke::new(1.0, c::TEXT_3),
                    egui::StrokeKind::Inside,
                );
            }
            let badge = Rect::from_center_size(Pos2::new(r.left() + 26.0, r.center().y), Vec2::splat(28.0));
            theme::paint_tool_badge(ui.painter(), badge, data::tool_mark(tool), c::CONTROL_HOVER);
            let fg = if line.startable { c::TEXT_STRONG } else { c::TEXT_2 };
            ui.painter().text(
                Pos2::new(badge.right() + 12.0, r.center().y - 9.0),
                Align2::LEFT_CENTER,
                tool.label(),
                medium(ty::BODY),
                fg,
            );
            let col = match line.tone {
                Tone::Neutral => c::TEXT_2,
                Tone::Waiting => c::TEXT_3,
                t => t.fg(),
            };
            ui.painter().text(
                Pos2::new(badge.right() + 12.0, r.center().y + 9.0),
                Align2::LEFT_CENTER,
                &line.text,
                font(12.0),
                col,
            );
            theme::focus_ring(ui, &resp, CornerRadius::same(8));
            if resp.clicked() {
                chosen = tool;
            }
            if resp.double_clicked() {
                start = true;
            }
        }
        ui.add_space(4.0);
        ui.label(RichText::new("제목 (선택)").font(medium(12.0)).color(c::TEXT_3));
        let te = ui.add(
            egui::TextEdit::singleline(&mut title)
                .hint_text(RichText::new("비워 두면 자동으로 정해요").color(c::TEXT_3))
                .desired_width(f32::INFINITY)
                .margin(egui::Margin::symmetric(10, 7)),
        );
        if focus {
            te.request_focus();
        }
        if te.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            start = true;
        }
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let label = if starting.is_some() { "시작하는 중…" } else { "시작" };
                let b = ui.add_enabled(
                    starting.is_none(),
                    egui::Button::new(RichText::new(label).font(medium(ty::BODY)).color(c::ON_EMBER))
                        .fill(c::EMBER)
                        .corner_radius(CornerRadius::same(6))
                        .min_size(Vec2::new(72.0, 30.0)),
                );
                if b.clicked() {
                    start = true;
                }
                if theme::secondary_button(ui, "취소").clicked() {
                    cancel = true;
                }
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    ui.label(RichText::new("탭을 닫아도 실행은 계속돼요").font(font(12.0)).color(c::TEXT_3));
                });
            });
        });
    });
    let st = &mut app.shell.modal;
    st.new_tool = Some(chosen);
    st.new_title = title;
    if start && starting.is_none() {
        let t = st.new_title.trim().to_string();
        st.new_tool = None;
        st.new_title.clear();
        actions::start_session_in_pane(
            app,
            ws,
            w.project_id,
            chosen,
            (!t.is_empty()).then_some(t),
            target.map(|(pane, _)| pane),
        );
        return false;
    }
    if close || cancel {
        st.new_tool = None;
        st.new_title.clear();
        return false;
    }
    true
}

/// Rename a project, workspace or session.
pub fn show_rename_modal(app: &mut App, ctx: &egui::Context, target: &RenameTarget) -> bool {
    let Some(current) = current_name(app, target) else {
        // Not loaded yet (keep the dialog requested) or the item is gone.
        return current_loading(app, target);
    };
    let current = Some(current);
    let st = &mut app.shell.modal;
    if st.rename_for.as_ref() != Some(target) {
        st.rename_for = Some(target.clone());
        st.rename_text = current.clone().unwrap_or_default();
        st.focus_next = true;
    }
    let mut text = std::mem::take(&mut st.rename_text);
    let focus = std::mem::take(&mut st.focus_next);
    let what = match target {
        RenameTarget::Project { .. } => "프로젝트",
        RenameTarget::Workspace(_) => "작업 공간",
        RenameTarget::Session { .. } => "세션",
    };
    let mut save = false;
    let mut cancel = false;
    let ((), close) = dialog(ctx, "rename", 420.0, |ui| {
        let desc = match target {
            RenameTarget::Session { .. } => "직접 정한 이름은 자동 제목으로 덮어쓰지 않아요.",
            RenameTarget::Workspace(_) => "이름만 바뀌어요. 폴더와 branch는 그대로예요.",
            RenameTarget::Project { .. } => "Runwick에서 보이는 이름만 바뀌어요. 폴더 이름은 그대로예요.",
        };
        dialog_title(ui, &format!("{what} 이름 바꾸기"), Some(desc));
        let te = ui.add(
            egui::TextEdit::singleline(&mut text).desired_width(f32::INFINITY).margin(egui::Margin::symmetric(10, 7)),
        );
        if focus {
            te.request_focus();
        }
        if te.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            save = true;
        }
        ui.add_space(6.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let ok = !text.trim().is_empty() && Some(text.trim()) != current.as_deref();
            let b = ui.add_enabled(
                ok,
                egui::Button::new(RichText::new("저장").font(medium(ty::BODY)).color(c::ON_EMBER))
                    .fill(c::EMBER)
                    .corner_radius(CornerRadius::same(6))
                    .min_size(Vec2::new(64.0, 30.0)),
            );
            if b.clicked() {
                save = true;
            }
            if theme::secondary_button(ui, "취소").clicked() {
                cancel = true;
            }
        });
    });
    let name = text.trim().to_string();
    app.shell.modal.rename_text = text;
    if save && !name.is_empty() && Some(name.as_str()) != current.as_deref() {
        apply_rename(app, target, name);
        app.shell.modal.rename_for = None;
        return false;
    }
    if close || cancel {
        app.shell.modal.rename_for = None;
        return false;
    }
    true
}

fn current_loading(app: &mut App, target: &RenameTarget) -> bool {
    match target {
        RenameTarget::Project { .. } => data::projects(app).is_first_load(),
        RenameTarget::Workspace(_) => data::workspaces(app).is_first_load(),
        RenameTarget::Session { host, .. } => data::all_sessions(app, *host).is_first_load(),
    }
}

fn current_name(app: &mut App, target: &RenameTarget) -> Option<String> {
    match target {
        RenameTarget::Project { id, .. } => {
            data::projects(app).data.and_then(|l| data::project_of(&l, *id).map(|p| p.name.clone()))
        }
        RenameTarget::Workspace(ws) => {
            data::workspaces(app).data.and_then(|l| data::find_ws(&l, ws).map(|w| w.name.clone()))
        }
        RenameTarget::Session { host, id } => data::all_sessions(app, *host)
            .data
            .and_then(|l| l.iter().find(|s| s.session.id == *id).map(|s| s.session.title.clone())),
    }
}

fn apply_rename(app: &mut App, target: &RenameTarget, name: String) {
    let fail =
        |app: &mut App, e: runwick_core::RpcError| app.toast_error(&format!("이름을 바꾸지 못했어요: {}", e.message));
    match target {
        RenameTarget::Project { host, id } => app.hub.call::<ProjectsUpdate>(
            *host,
            ProjectUpdate { id: *id, name: Some(name), ..Default::default() },
            move |app, r| {
                if let Err(e) = r {
                    fail(app, e)
                }
            },
        ),
        RenameTarget::Workspace(ws) => {
            // Remote workspaces are named in this computer's records.
            let id = data::workspaces(app).data.and_then(|l| data::find_ws(&l, ws).map(|w| w.id)).unwrap_or(ws.id);
            app.hub.call::<WorkspacesUpdate>(
                HostKey::Local,
                WorkspaceUpdate { id, name: Some(name), ..Default::default() },
                move |app, r| {
                    if let Err(e) = r {
                        fail(app, e)
                    }
                },
            )
        }
        RenameTarget::Session { host, id } => app.hub.call::<SessionsUpdate>(
            *host,
            SessionUpdate { id: *id, title: Some(name), ..Default::default() },
            move |app, r| {
                if let Err(e) = r {
                    fail(app, e)
                }
            },
        ),
    }
}

/// "실행 종료" confirmation: shows the target and its running workers before stopping.
fn stop_confirm(app: &mut App, ctx: &egui::Context) {
    let Some(ask) = app.shell.modal.stop.as_ref() else { return };
    let (host, session, archive_after) = (ask.host, ask.session, ask.archive_after);
    let list = data::all_sessions(app, host).data;
    let Some(info) = list.as_ref().and_then(|l| l.iter().find(|s| s.session.id == session)).cloned() else {
        if list.is_some() {
            app.shell.modal.stop = None;
        }
        return;
    };
    let workers: Vec<_> = list
        .iter()
        .flat_map(|l| l.iter())
        .filter(|s| s.session.parent_id == Some(session))
        .filter(|s| data::status(&s.session, data::live_of(app, host, s).as_ref()).alive)
        .cloned()
        .collect();
    let ws_label = {
        let wl = data::workspaces(app).data;
        let pl = data::projects(app).data;
        let w = wl.as_ref().and_then(|l| l.iter().find(|w| w.id == info.session.workspace_id));
        let p = pl.as_ref().and_then(|l| data::project_of(l, info.session.project_id));
        match (p, w) {
            (Some(p), Some(w)) => format!("{} / {}", p.name, w.name),
            _ => String::new(),
        }
    };
    let ask = app.shell.modal.stop.as_mut().expect("checked above");
    if !ask.initialized {
        ask.with_workers = workers.iter().map(|w| (w.session.id, true)).collect();
        ask.initialized = true;
    }
    let mut choices = ask.with_workers.clone();
    let mut confirm = false;
    let mut cancel = false;
    let ((), close) = dialog(ctx, "stop-session", 460.0, |ui| {
        dialog_title(
            ui,
            if archive_after { "실행을 종료하고 보관할까요?" } else { "실행을 종료할까요?" },
            Some(
                "CLI 프로세스를 멈춰요. 화면 출력과 대화 기록은 남고, 지원하는 CLI는 나중에 같은 대화를 재개할 수 있어요.",
            ),
        );
        egui::Frame::new()
            .fill(c::ROW)
            .corner_radius(CornerRadius::same(8))
            .inner_margin(egui::Margin::symmetric(12, 10))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    theme::tool_badge(ui, data::tool_mark(info.session.tool), 22.0, c::CONTROL);
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 1.0;
                        ui.label(RichText::new(&info.session.title).font(medium(ty::BODY)).color(c::TEXT_STRONG));
                        ui.label(
                            RichText::new(format!("{} · {ws_label}", info.session.tool.label()))
                                .font(font(12.0))
                                .color(c::TEXT_3),
                        );
                    });
                });
            });
        if archive_after {
            ui.label(
                egui::RichText::new("종료가 확인되면 이 세션을 보관해요. 기록은 계속 검색할 수 있어요.")
                    .font(font(12.0))
                    .color(c::TEXT_2),
            );
        }
        if !choices.is_empty() {
            ui.add_space(4.0);
            ui.label(
                RichText::new(format!("이 세션이 맡긴 작업자 {}개가 실행 중이에요", choices.len()))
                    .font(medium(12.5))
                    .color(c::TEXT),
            );
            for (id, on) in choices.iter_mut() {
                let Some(wk) = workers.iter().find(|w| w.session.id == *id) else { continue };
                let name =
                    wk.session.role.clone().filter(|r| !r.is_empty()).unwrap_or_else(|| wk.session.title.clone());
                ui.checkbox(
                    on,
                    RichText::new(format!("{name} · {} 함께 종료", wk.session.tool.label()))
                        .font(font(12.5))
                        .color(c::TEXT_2),
                );
            }
            ui.label(RichText::new("선택하지 않은 작업자는 계속 실행돼요.").font(font(12.0)).color(c::TEXT_3));
        }
        ui.add_space(8.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if theme::danger_button(ui, if archive_after { "실행 종료 후 보관" } else { "실행 종료" }).clicked()
            {
                confirm = true;
            }
            if theme::secondary_button(ui, "취소").clicked() {
                cancel = true;
            }
        });
    });
    if let Some(ask) = app.shell.modal.stop.as_mut() {
        ask.with_workers = choices.clone();
    }
    if confirm {
        if archive_after {
            stop_and_archive(app, host, session);
        } else {
            actions::stop_session(app, host, session);
        }
        for (id, on) in choices {
            if on {
                actions::stop_session(app, host, id);
            }
        }
        app.shell.modal.stop = None;
    } else if close || cancel {
        app.shell.modal.stop = None;
    }
}

fn stop_and_archive(app: &mut App, host: HostKey, session: SessionId) {
    use runwick_core::api::sessions::{SessionsStop, Stop};
    app.hub.call::<SessionsStop>(host, Stop { id: session, force: false }, move |app, result| {
        match result {
            Ok(()) => {
                // The stop response follows process cleanup; update rejects survivors or a new run.
                app.hub.send::<SessionsUpdate>(
                    host,
                    SessionUpdate { id: session, archived: Some(true), ..Default::default() },
                );
            }
            Err(e) => app.toast_error(&format!("실행을 종료하지 못해 보관하지 않았어요: {}", e.message)),
        }
    });
}
