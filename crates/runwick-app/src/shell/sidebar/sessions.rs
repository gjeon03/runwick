//! The selected workspace's header (`세션 / 파일`) and its session list. Delegated workers are
//! nested under their parent and collapsed by default.

use super::Act;
use crate::App;
use crate::client::HostKey;
use crate::icons;
use crate::shell::{data, widgets};
use crate::state::{RenameTarget, SidebarMode, TabKind, WsKey};
use crate::theme::{self, Seg, Tone, c, font, icon_font, mono};
use egui::{Align2, Pos2, Rect, Stroke, Vec2};
use runwick_core::api::projects::ProjectId;
use runwick_core::api::sessions::{SessionInfo, SessionLive, Tool};

/// `기본 공간   [세션|파일]` with a separator line above.
pub fn space_header(app: &mut App, ui: &mut egui::Ui) {
    let ws_info =
        app.ui.selected_ws.and_then(|k| data::workspaces(app).data.and_then(|l| data::find_ws(&l, &k).cloned()));
    let w = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(Vec2::new(w, 40.0), egui::Sense::hover());
    ui.painter().hline(rect.x_range(), rect.top() + 0.5, Stroke::new(1.0, c::LINE));
    let inner = Rect::from_min_max(
        Pos2::new(rect.left() + 10.0, rect.top() + 10.0),
        Pos2::new(rect.right() - 10.0, rect.bottom() - 6.0),
    );
    let Some(ws) = ws_info else {
        ui.painter().text(
            Pos2::new(inner.left(), inner.center().y),
            Align2::LEFT_CENTER,
            "작업 공간 없음",
            font(12.5),
            c::TEXT_3,
        );
        return;
    };
    let seg_w = widgets::text_width(ui, "세션", font(12.0)) + widgets::text_width(ui, "파일", font(12.0)) + 38.0;
    let name_rect = Rect::from_min_max(inner.min, Pos2::new(inner.right() - seg_w - 8.0, inner.bottom()));
    let label = match &ws.remote {
        Some(r) => format!("{} · {}", ws.name, crate::remote::device_name(app, r.device_id)),
        None => ws.name.clone(),
    };
    widgets::paint_clipped(ui, name_rect, &label, crate::theme::medium(12.5), c::TEXT_STRONG, true);
    ui.interact(name_rect, ui.scope_id().with("space-name"), egui::Sense::hover()).on_hover_text(data::tilde(&ws.path));
    let seg_rect = Rect::from_min_max(Pos2::new(inner.right() - seg_w, inner.top()), inner.max);
    ui.scope_builder(
        egui::UiBuilder::new().max_rect(seg_rect).layout(egui::Layout::left_to_right(egui::Align::Center)),
        |ui| {
            let selected = match app.ui.sidebar_mode {
                SidebarMode::Sessions => 0,
                SidebarMode::Files => 1,
            };
            if let Some(i) = theme::segmented(ui, &[Seg::text("세션"), Seg::text("파일")], selected, 24.0) {
                app.ui.sidebar_mode = if i == 0 { SidebarMode::Sessions } else { SidebarMode::Files };
            }
        },
    );
}

pub fn list(app: &mut App, ui: &mut egui::Ui, acts: &mut Vec<Act>) {
    let Some(ws) = app.ui.selected_ws else {
        widgets::empty_note(ui, "작업 공간을 고르면 세션이 여기에 보여요.");
        return;
    };
    let Some(project) = app.ui.selected_project else { return };
    let q = data::sessions_in(app, ws);
    match (&q.data, &q.error) {
        (Some(list), _) => {
            let list = list.clone();
            sessions(app, ui, ws, project, &list, acts);
        }
        (None, Some(e)) => {
            let msg = match ws.host {
                HostKey::Device(_) if e.code == runwick_core::rpc::codes::DISCONNECTED => {
                    "이 기기에 연결되어 있지 않아 세션을 볼 수 없어요.".to_string()
                }
                _ => format!("세션 목록을 불러오지 못했어요 · {}", e.message),
            };
            widgets::empty_note(ui, &msg);
        }
        (None, None) if ws.host != HostKey::Local && !app.hub.state(ws.host).is_connected() => {
            widgets::empty_note(ui, "이 기기에 연결되어 있지 않아 세션을 볼 수 없어요. 기록은 연결되면 다시 보여요.");
        }
        (None, None) => widgets::empty_note(ui, "세션을 불러오는 중…"),
    }
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        ui.add_space(2.0);
        let w = (ui.available_width() - 6.0) / 2.0;
        if widgets::wide_pill(ui, Some(icons::PLUS), "새 CLI", w, 28.0).on_hover_text("새 CLI 세션 (⌘T)").clicked()
        {
            acts.push(Act::NewSession);
        }
        if widgets::wide_pill(ui, Some(icons::GIT_BRANCH), "분리해서 작업", w, 28.0)
            .on_hover_text("별도 worktree 공간을 만들어 따로 작업해요")
            .clicked()
        {
            acts.push(Act::SplitWork(ws));
        }
    });
}

fn sessions(
    app: &mut App,
    ui: &mut egui::Ui,
    ws: WsKey,
    project: ProjectId,
    list: &[SessionInfo],
    acts: &mut Vec<Act>,
) {
    let mut parents: Vec<&SessionInfo> = list
        .iter()
        .filter(|s| !s.session.archived)
        .filter(|s| s.session.parent_id.is_none_or(|pid| !list.iter().any(|o| o.session.id == pid)))
        .collect();
    if parents.is_empty() {
        widgets::empty_note(ui, "아직 세션이 없어요. 오른쪽에서 CLI를 고르거나 ⌘T를 누르세요.");
        return;
    }
    parents.sort_by_key(|s| s.session.created_at);
    let active = match app.ui.active_tab().map(|t| t.kind.clone()) {
        Some(TabKind::Terminal { session, .. }) => Some(session),
        _ => None,
    };
    let requests = app
        .hub
        .query::<runwick_core::api::collab::CollabList>(
            ws.host,
            &runwick_core::api::collab::RequestQuery { activity: true, limit: 100, ..Default::default() },
        )
        .data;
    let index =
        runwick_core::api::activity::RequestIndex::new(requests.as_deref().map(Vec::as_slice).unwrap_or_default());
    for s in parents {
        let kids: Vec<&SessionInfo> =
            list.iter().filter(|k| k.session.parent_id == Some(s.session.id) && !k.session.archived).collect();
        let open = app.shell.sidebar.expanded_parents.contains(&s.session.id);
        let live = data::live_of(app, ws.host, s);
        let asking = kids
            .iter()
            .filter(|k| {
                index
                    .group(&k.session, data::live_of(app, ws.host, k).as_ref())
                    .is_some_and(runwick_core::api::activity::Group::needs_person)
            })
            .count();
        let workers = Workers { count: kids.len(), asking };
        session_row(ui, ws, project, s, live.as_ref(), active == Some(s.session.id), workers, open, acts);
        if open {
            for k in kids {
                let kl = data::live_of(app, ws.host, k);
                worker_row(ui, ws, k, kl.as_ref(), active == Some(k.session.id), acts);
            }
        }
    }
}

/// Delegated workers under a session: how many, and how many wait for the person.
#[derive(Clone, Copy)]
struct Workers {
    count: usize,
    asking: usize,
}

fn meta_color(st: &data::Status) -> egui::Color32 {
    match st.tone {
        Tone::Neutral => c::TEXT_2,
        Tone::Waiting => c::TEXT_3,
        t => t.fg(),
    }
}

#[allow(clippy::too_many_arguments)]
fn session_row(
    ui: &mut egui::Ui,
    ws: WsKey,
    project: ProjectId,
    s: &SessionInfo,
    live: Option<&SessionLive>,
    selected: bool,
    workers: Workers,
    open: bool,
    acts: &mut Vec<Act>,
) {
    let st = data::status(&s.session, live);
    let resp = widgets::row(ui, 44.0, selected, &s.session.title);
    let r = resp.rect;
    let badge = Rect::from_min_size(Pos2::new(r.left() + 8.0, r.top() + 7.0), Vec2::splat(20.0));
    theme::paint_tool_badge(ui.painter(), badge, data::tool_mark(s.session.tool), c::CONTROL);
    if live.is_some_and(runwick_core::api::activity::has_new_response) {
        widgets::dot(ui, badge.right_top(), Tone::Info.fg(), false, 3.0);
    }
    let right = if workers.count > 0 { r.right() - 30.0 } else { r.right() - 8.0 };
    let text_x = badge.right() + 8.0;
    let title_rect = Rect::from_min_max(Pos2::new(text_x, r.top() + 5.0), Pos2::new(right, r.top() + 24.0));
    let cut = widgets::paint_clipped(ui, title_rect, &s.session.title, font(13.0), c::TEXT_STRONG, true);
    let mut meta = format!("{} · {}", st.label, data::ago(data::last_activity(&s.session, live)));
    let mut color = meta_color(&st);
    if workers.count > 0 {
        meta.push_str(&format!(" · 작업자 {}", workers.count));
    }
    // A worker waiting for an answer must not hide behind a collapsed row (and must survive
    // truncation, so it comes first).
    if workers.asking > 0 {
        meta = format!("작업자 확인 필요 {} · {meta}", workers.asking);
        color = c::EMBER;
    }
    if let Some(device) = &s.session.origin_device {
        meta = format!("{device} · {meta}");
    }
    let meta_rect = Rect::from_min_max(Pos2::new(text_x, r.top() + 23.0), Pos2::new(right, r.top() + 40.0));
    widgets::paint_clipped(ui, meta_rect, &meta, font(11.5), color, true);
    let mut toggled = false;
    if workers.count > 0 {
        let t = Rect::from_center_size(Pos2::new(r.right() - 15.0, r.center().y), Vec2::new(22.0, 26.0));
        let tr = ui.interact(t, ui.scope_id().with(("workers", s.session.id)), egui::Sense::click());
        let name = if open {
            format!("작업자 {}개 접기", workers.count)
        } else {
            format!("작업자 {}개 펼치기", workers.count)
        };
        let n2 = name.clone();
        tr.widget_info(move || egui::WidgetInfo::labeled(egui::Role::Button, true, &n2));
        if tr.hovered() {
            ui.painter().rect_filled(t, egui::CornerRadius::same(5), c::CONTROL_HOVER);
        }
        let g = if open { icons::CHEVRON_DOWN } else { icons::CHEVRON_RIGHT };
        ui.painter().text(t.center(), Align2::CENTER_CENTER, g, icon_font(12.0), c::TEXT_3);
        if tr.on_hover_text(name).clicked() {
            acts.push(Act::ToggleWorkers(s.session.id));
            toggled = true;
        }
    }
    let mut tip = format!("{}\n{} · {}", s.session.title, s.session.tool.label(), st.label);
    if let Some(device) = &s.session.origin_device {
        tip.push_str(&format!("\n{device}에서 동기화한 기록 · 읽기 전용"));
    }
    let resp = if cut { resp.on_hover_text(tip) } else { resp.on_hover_text_at_pointer(tip) };
    if resp.clicked() && !toggled {
        acts.push(Act::OpenSession { project, ws, session: s.session.id, hidden: s.session.hidden });
    }
    session_menu(&resp, ws, s, &st, acts);
}

fn worker_row(
    ui: &mut egui::Ui,
    ws: WsKey,
    s: &SessionInfo,
    live: Option<&SessionLive>,
    selected: bool,
    acts: &mut Vec<Act>,
) {
    let st = data::status(&s.session, live);
    let title = match &s.session.role {
        Some(role) if !role.is_empty() => role.clone(),
        _ => s.session.title.clone(),
    };
    let resp = widgets::row_inset(ui, 26.0, selected, &title, 18.0);
    let r = resp.rect;
    let cy = r.center().y;
    ui.painter().text(
        Pos2::new(r.left() + 14.5, cy),
        Align2::CENTER_CENTER,
        st.tone.icon(),
        icon_font(13.0),
        meta_color(&st),
    );
    if live.is_some_and(runwick_core::api::activity::has_new_response) {
        widgets::dot(ui, Pos2::new(r.right() - 26.0, cy), Tone::Info.fg(), false, 3.0);
    }
    let mark = data::tool_mark(s.session.tool);
    ui.painter().text(Pos2::new(r.right() - 8.0, cy), Align2::RIGHT_CENTER, mark, mono(10.5), c::TEXT_3);
    let text_rect = Rect::from_min_max(Pos2::new(r.left() + 28.0, r.top()), Pos2::new(r.right() - 26.0, r.bottom()));
    widgets::paint_clipped(ui, text_rect, &title, font(12.5), c::TEXT, true);
    let resp = resp.on_hover_text(format!("{title}\n{} · {}", s.session.tool.label(), st.label));
    if resp.clicked() {
        acts.push(Act::OpenWorker(ws.host, s.session.id));
    }
    session_menu(&resp, ws, s, &st, acts);
}

fn session_menu(resp: &egui::Response, ws: WsKey, s: &SessionInfo, st: &data::Status, acts: &mut Vec<Act>) {
    resp.context_menu(|ui| {
        if widgets::menu_item(ui, icons::PENCIL, "이름 바꾸기").clicked() {
            acts.push(Act::Rename(RenameTarget::Session { host: ws.host, id: s.session.id }));
        }
        if widgets::menu_item(ui, icons::COPY, "작업 폴더 경로 복사").clicked() {
            acts.push(Act::CopyPath(s.session.cwd.clone()));
        }
        widgets::menu_sep(ui);
        if st.alive {
            if widgets::menu_item_danger(ui, icons::CIRCLE_STOP, "실행 종료…").clicked() {
                acts.push(Act::StopSession(ws.host, s.session.id));
            }
        } else {
            let resumable = s.session.tool != Tool::Shell && s.session.provider_conv_id.is_some();
            if resumable && widgets::menu_item(ui, icons::ROTATE_CCW, "같은 대화 재개").clicked() {
                acts.push(Act::Restart(ws.host, s.session.id, true));
            }
            if s.session.origin_device.is_none() && widgets::menu_item(ui, icons::PLAY, "새로 실행").clicked() {
                acts.push(Act::Restart(ws.host, s.session.id, false));
            }
            if widgets::menu_item(ui, icons::ARCHIVE, "목록에서 보관")
                .on_hover_text("사이드바에서만 숨겨요. 대화 기록은 검색에 남아요")
                .clicked()
            {
                acts.push(Act::ArchiveSession(ws.host, s.session.id));
            }
        }
    });
}
