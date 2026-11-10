//! Project search/filter bar and the project → workspace tree.

use super::{Act, filter_index, filter_segments};
use crate::App;
use crate::client::HostKey;
use crate::icons;
use crate::shell::{data, widgets};
use crate::state::{ProjectFilter, RenameTarget, WsKey};
use crate::theme::{self, Seg, c, font, icon_font, medium, mono};
use egui::{Align2, Pos2, Rect, Sense, Vec2};
use runwick_core::api::projects::{Project, Workspace, WorkspaceKind};
use runwick_core::api::sessions::SessionInfo;
use std::collections::HashMap;

pub fn filter_bar(app: &mut App, ui: &mut egui::Ui) {
    // The device filter appears only once a device is registered.
    let has_remote = !crate::remote::devices(app).is_empty();
    let filter_name = device_filter_name(app, app.ui.device_filter);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        let seg_w = if has_remote { 104.0 } else { 74.0 };
        let field_w = (ui.available_width() - seg_w - 6.0).max(60.0);
        ui.allocate_ui(Vec2::new(field_w, 30.0), |ui| {
            let id = ui.scope_id().with("project-search");
            let resp = theme::search_field(ui, &mut app.ui.project_search, "프로젝트 검색", id, 30.0);
            resp.widget_info(|| {
                egui::WidgetInfo::labeled(egui::Role::SearchInput, true, "프로젝트 이름 또는 경로 검색")
            });
            if resp.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                app.ui.project_search.clear();
            }
        });
        let mut segs: Vec<Seg> = filter_segments().into();
        if has_remote {
            segs.push(Seg::icon(icons::MONITOR, &filter_name));
        }
        let selected = filter_index(app.ui.project_filter);
        if let Some(i) = theme::segmented(ui, &segs, selected, 26.0) {
            match i {
                0 => app.ui.project_filter = ProjectFilter::All,
                1 => {
                    app.ui.project_filter = if app.ui.project_filter == ProjectFilter::Favorites {
                        ProjectFilter::All
                    } else {
                        ProjectFilter::Favorites
                    }
                }
                _ => {
                    app.ui.device_filter = match app.ui.device_filter {
                        None => Some(HostKey::Local),
                        Some(HostKey::Local) => remote_devices(app).first().map(|d| HostKey::Device(*d)),
                        Some(HostKey::Device(d)) => {
                            let devs = remote_devices(app);
                            devs.iter().position(|x| *x == d).and_then(|i| devs.get(i + 1)).map(|d| HostKey::Device(*d))
                        }
                    };
                }
            }
        }
    });
}

fn device_filter_name(app: &mut App, f: Option<HostKey>) -> String {
    match f {
        None => "기기 필터: 모든 기기".into(),
        Some(HostKey::Local) => "기기 필터: 이 컴퓨터".into(),
        Some(HostKey::Device(d)) => format!("기기 필터: {}", crate::remote::device_name(app, d)),
    }
}

fn remote_devices(app: &mut App) -> Vec<uuid::Uuid> {
    let mut v: Vec<uuid::Uuid> = data::workspaces(app)
        .data
        .map(|l| l.iter().filter_map(|w| w.remote.as_ref().map(|r| r.device_id)).collect())
        .unwrap_or_default();
    v.sort();
    v.dedup();
    v
}

fn device_matches(filter: Option<HostKey>, w: &Workspace) -> bool {
    match filter {
        None => true,
        Some(HostKey::Local) => w.remote.is_none(),
        Some(HostKey::Device(d)) => w.remote.as_ref().is_some_and(|r| r.device_id == d),
    }
}

pub fn list(app: &mut App, ui: &mut egui::Ui, acts: &mut Vec<Act>) {
    let pq = data::projects(app);
    let wq = data::workspaces(app);
    let (projects, workspaces) = match (pq.data, wq.data) {
        (Some(p), Some(w)) => (p, w),
        _ => {
            if let Some(e) = pq.error.or(wq.error) {
                widgets::empty_note(ui, &format!("프로젝트 목록을 불러오지 못했어요 · {}", e.message));
            } else {
                widgets::empty_note(ui, "프로젝트를 불러오는 중…");
            }
            return;
        }
    };
    let sessions = data::all_sessions(app, HostKey::Local).data;
    let all: Vec<&Project> = data::visible_projects(&projects).collect();
    if all.is_empty() {
        widgets::empty_note(ui, "아직 프로젝트가 없어요. 폴더를 열면 기본 공간이 바로 준비돼요.");
        open_folder_row(ui, acts);
        return;
    }
    let q = app.ui.project_search.trim().to_lowercase();
    let fav = app.ui.project_filter == ProjectFilter::Favorites;
    let dev = app.ui.device_filter;
    let shown: Vec<&Project> = all
        .iter()
        .copied()
        .filter(|p| !fav || p.favorite)
        .filter(|p| q.is_empty() || p.name.to_lowercase().contains(&q) || p.root.to_lowercase().contains(&q))
        .filter(|p| dev.is_none() || workspaces.iter().any(|w| w.project_id == p.id && device_matches(dev, w)))
        .collect();
    if shown.is_empty() {
        if fav && !all.iter().any(|p| p.favorite) {
            widgets::empty_note(
                ui,
                "즐겨찾기한 프로젝트가 없어요. 프로젝트 이름에 마우스를 올려 ☆을 누르면 여기에 모여요.",
            );
            if theme::link_button(ui, "전체 보기").clicked() {
                app.ui.project_filter = ProjectFilter::All;
            }
        } else {
            let msg = if q.is_empty() {
                "조건에 맞는 프로젝트가 없어요.".to_string()
            } else {
                format!("‘{}’와 맞는 프로젝트가 없어요.", app.ui.project_search.trim())
            };
            widgets::empty_note(ui, &msg);
            if theme::link_button(ui, "필터 지우기").clicked() {
                app.ui.project_search.clear();
                app.ui.project_filter = ProjectFilter::All;
                app.ui.device_filter = None;
            }
        }
        return;
    }
    let attention_counts = crate::views::activity::workspace_counts(app);
    // Live state for local workspaces; remote workspaces use their connected host.
    let mut lives: HashMap<uuid::Uuid, Vec<SessionInfo>> = HashMap::new();
    if let Some(list) = &sessions {
        for s in list.iter() {
            lives.entry(s.session.workspace_id).or_default().push(s.clone());
        }
    }
    for p in shown {
        let spaces: Vec<&Workspace> =
            workspaces.iter().filter(|w| w.project_id == p.id && device_matches(dev, w)).collect();
        let expanded = app.ui.expanded_projects.contains(&p.id);
        let badge: usize =
            spaces.iter().map(|w| attention_counts.get(&data::ws_key(w)).copied().unwrap_or_default()).sum();
        project_row(ui, p, spaces.len(), badge, expanded, acts);
        if expanded {
            for w in spaces {
                let key = data::ws_key(w);
                let device = w.remote.as_ref().map(|r| crate::remote::device_name(app, r.device_id));
                let state = if let Some(r) = &w.remote {
                    remote_ws_state(app, r.device_id, r.remote_workspace_id)
                } else {
                    let ls: Vec<_> = lives
                        .get(&w.id)
                        .map(|v| v.iter().map(|s| data::live_of(app, HostKey::Local, s)).collect())
                        .unwrap_or_default();
                    data::ws_state(ls.iter().map(|l| l.as_ref()))
                };
                let selected = app.ui.selected_ws == Some(key);
                workspace_row(ui, p, w, key, state, selected, device.as_deref(), acts);
            }
        }
    }
    open_folder_row(ui, acts);
}

/// A device's workspace: its sessions' state while connected, unknown otherwise.
fn remote_ws_state(app: &mut App, device: uuid::Uuid, ws: uuid::Uuid) -> data::WsState {
    let host = HostKey::Device(device);
    if !app.hub.state(host).is_connected() {
        return data::WsState::Unknown;
    }
    let Some(list) = data::all_sessions(app, host).data else { return data::WsState::Unknown };
    let lives: Vec<_> =
        list.iter().filter(|s| s.session.workspace_id == ws).map(|s| data::live_of(app, host, s)).collect();
    data::ws_state(lives.iter().map(|l| l.as_ref()))
}

fn open_folder_row(ui: &mut egui::Ui, acts: &mut Vec<Act>) {
    ui.add_space(2.0);
    let resp = widgets::row(ui, 28.0, false, "폴더 열기");
    let r = resp.rect;
    let fg = if resp.hovered() { c::TEXT } else { c::TEXT_3 };
    ui.painter().text(
        Pos2::new(r.left() + 10.0, r.center().y),
        Align2::CENTER_CENTER,
        icons::FOLDER_PLUS,
        icon_font(12.5),
        fg,
    );
    ui.painter().text(Pos2::new(r.left() + 21.0, r.center().y), Align2::LEFT_CENTER, "폴더 열기", font(12.5), fg);
    ui.painter().text(Pos2::new(r.right() - 8.0, r.center().y), Align2::RIGHT_CENTER, "⌘O", font(11.5), c::TEXT_OFF);
    if resp.on_hover_text("폴더를 프로젝트로 열기 — 창으로 끌어다 놓아도 돼요").clicked() {
        acts.push(Act::OpenFolder);
    }
}

fn project_row(ui: &mut egui::Ui, p: &Project, spaces: usize, badge: usize, expanded: bool, acts: &mut Vec<Act>) {
    let resp = widgets::row(ui, 28.0, false, &p.name);
    let r = resp.rect;
    let cy = r.center().y;
    let painter = ui.painter();
    // Chevron (its own hit area toggles without selecting).
    let chev = Rect::from_center_size(Pos2::new(r.left() + 10.0, cy), Vec2::new(18.0, 24.0));
    let chev_resp = ui.interact(chev, ui.scope_id().with(("chev", p.id)), Sense::click());
    chev_resp
        .widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, if expanded { "접기" } else { "펼치기" }));
    theme::focus_ring(ui, &chev_resp, egui::CornerRadius::same(4));
    let chev_glyph = if expanded { icons::CHEVRON_DOWN } else { icons::CHEVRON_RIGHT };
    let chev_fg = if chev_resp.hovered() { c::TEXT_STRONG } else { c::TEXT_3 };
    painter.text(chev.center(), Align2::CENTER_CENTER, chev_glyph, icon_font(12.0), chev_fg);
    // Right side: count, star, badge.
    let count = format!("공간 {spaces}");
    let count_w = widgets::text_width(ui, &count, font(11.5));
    let mut right = r.right() - 8.0;
    painter.text(Pos2::new(right, cy), Align2::RIGHT_CENTER, &count, font(11.5), c::TEXT_3);
    right -= count_w + 5.0;
    // The star's focus ring (2.5 px outside) must stay clear of the badge to its left.
    let star_rect = Rect::from_center_size(Pos2::new(right - 8.0, cy), Vec2::splat(16.0));
    let star_resp = ui.interact(star_rect, ui.scope_id().with(("star", p.id)), Sense::click());
    let fav = p.favorite;
    star_resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::Switch, true, fav, "즐겨찾기"));
    let show_star = fav || resp.hovered() || star_resp.hovered() || star_resp.has_focus();
    if fav {
        theme::paint_star(painter, star_rect.center(), 5.5, if star_resp.hovered() { c::TEXT_2 } else { c::TEXT_3 });
    } else if show_star {
        let fg = if star_resp.hovered() { c::TEXT_STRONG } else { c::TEXT_3 };
        painter.text(star_rect.center(), Align2::CENTER_CENTER, icons::STAR, icon_font(11.0), fg);
    }
    theme::focus_ring(ui, &star_resp, egui::CornerRadius::same(4));
    if show_star {
        right -= 22.0;
    }
    if badge > 0 {
        right -= widgets::count_badge(ui, Pos2::new(right, cy), badge) + 5.0;
    }
    let name_rect = Rect::from_min_max(Pos2::new(r.left() + 21.0, r.top()), Pos2::new(right - 2.0, r.bottom()));
    let fg = if expanded { c::TEXT_STRONG } else { c::TEXT_2 };
    let cut = widgets::paint_clipped(ui, name_rect, &p.name, medium(13.0), fg, true);
    let tip = if badge > 0 { format!("{}\n확인할 일 {badge}개", p.name) } else { p.name.clone() };
    let star_tip = if fav { "즐겨찾기 해제" } else { "즐겨찾기에 추가" };
    if star_resp.on_hover_text(star_tip).clicked() {
        acts.push(Act::Favorite(p.id, !fav));
    } else if chev_resp.clicked() {
        acts.push(Act::ToggleExpand(p.id));
    } else {
        let resp = if cut || badge > 0 { resp.on_hover_text(tip) } else { resp };
        if resp.clicked() {
            acts.push(if expanded { Act::ToggleExpand(p.id) } else { Act::SelectProject(p.id) });
        }
        resp.context_menu(|ui| {
            if widgets::menu_item(ui, icons::PENCIL, "이름 바꾸기").clicked() {
                acts.push(Act::Rename(RenameTarget::Project { host: HostKey::Local, id: p.id }));
            }
            let (g, t) =
                if fav { (icons::STAR, "즐겨찾기 해제") } else { (icons::STAR, "즐겨찾기에 추가") };
            if widgets::menu_item(ui, g, t).clicked() {
                acts.push(Act::Favorite(p.id, !fav));
            }
            widgets::menu_sep(ui);
            if widgets::menu_item(ui, icons::FOLDER_OPEN, "Finder에서 열기").clicked() {
                acts.push(Act::Reveal(p.root.clone()));
            }
            if widgets::menu_item(ui, icons::COPY, "경로 복사").clicked() {
                acts.push(Act::CopyPath(p.root.clone()));
            }
            widgets::menu_sep(ui);
            if widgets::menu_item(ui, icons::ARCHIVE, "목록에서 보관")
                .on_hover_text("목록에서만 숨겨요. 실행과 기록, 폴더는 그대로예요")
                .clicked()
            {
                acts.push(Act::ArchiveProject(p.id, p.name.clone()));
            }
        });
    }
}

#[allow(clippy::too_many_arguments)]
fn workspace_row(
    ui: &mut egui::Ui,
    p: &Project,
    w: &Workspace,
    key: WsKey,
    state: data::WsState,
    selected: bool,
    device: Option<&str>,
    acts: &mut Vec<Act>,
) {
    let resp = widgets::row_inset(ui, 28.0, selected, &w.name, 12.0);
    let r = resp.rect;
    let cy = r.center().y;
    let fg = if selected { c::TEXT_STRONG } else { c::TEXT_2 };
    let missing = !w.path_exists && w.remote.is_none();
    let glyph = if missing {
        icons::TRIANGLE_ALERT
    } else if w.kind == WorkspaceKind::Worktree {
        icons::GIT_BRANCH
    } else {
        icons::FOLDER
    };
    let icon_fg = if missing { c::YELLOW } else { fg };
    ui.painter().text(Pos2::new(r.left() + 14.5, cy), Align2::CENTER_CENTER, glyph, icon_font(13.0), icon_fg);
    let (tone, state_name) = state.describe();
    let dot_x = r.right() - 11.5;
    let pulse = state == data::WsState::Running;
    let dot_color = match state {
        data::WsState::None => c::TEXT_OFF,
        data::WsState::Idle => c::TEXT_2,
        data::WsState::Unknown => c::GREY,
        _ => tone.fg(),
    };
    widgets::dot(ui, Pos2::new(dot_x, cy), dot_color, pulse, 3.5);
    let mut x = r.left() + 28.0;
    let right_limit = dot_x - 10.0;
    let name_w = widgets::text_width(ui, &w.name, font(13.0));
    let branch = w.branch.clone().unwrap_or_default();
    let dev_w = device.map(|d| widgets::text_width(ui, d, font(10.5)) + 30.0).unwrap_or(0.0);
    let room = (right_limit - x - dev_w).max(0.0);
    let name_room = if branch.is_empty() { room } else { name_w.min(room * 0.65).max(room.min(48.0)) };
    widgets::paint_clipped(
        ui,
        Rect::from_min_size(Pos2::new(x, r.top()), Vec2::new(name_room.min(name_w), r.height())),
        &w.name,
        font(13.0),
        fg,
        true,
    );
    x += name_room.min(name_w) + 7.0;
    if let Some(d) = device {
        x += widgets::device_badge(ui, Pos2::new(x, cy), d, matches!(state, data::WsState::Unknown)) + 7.0;
    }
    if !branch.is_empty() && x < right_limit {
        widgets::paint_clipped(
            ui,
            Rect::from_min_max(Pos2::new(x, r.top()), Pos2::new(right_limit, r.bottom())),
            &branch,
            mono(11.0),
            c::TEXT_3,
            true,
        );
    }
    let mut tip = format!("{} · {}", w.name, data::tilde(&w.path));
    if !branch.is_empty() {
        tip.push_str(&format!("\nbranch {branch}"));
    }
    tip.push_str(&format!("\n{state_name}"));
    if missing {
        tip.push_str("\n폴더를 찾을 수 없어요. 기록은 그대로 볼 수 있어요");
    }
    let resp = resp.on_hover_text(tip);
    if resp.clicked() {
        acts.push(Act::SelectWs(p.id, key));
    }
    resp.context_menu(|ui| {
        if widgets::menu_item(ui, icons::PENCIL, "이름 바꾸기").clicked() {
            acts.push(Act::Rename(RenameTarget::Workspace(key)));
        }
        if widgets::menu_item(ui, icons::SPLIT, "분리해서 작업…").clicked() {
            acts.push(Act::SplitWork(key));
        }
        widgets::menu_sep(ui);
        if w.remote.is_some() {
            // The folder is on another computer: show where and how it is reached instead.
            if widgets::menu_item(ui, icons::MONITOR, "이 공간 정보").clicked() {
                acts.push(Act::SpaceInfo(key));
            }
        } else if widgets::menu_item(ui, icons::FOLDER_OPEN, "Finder에서 열기").clicked() {
            acts.push(Act::Reveal(w.path.clone()));
        }
        if widgets::menu_item(ui, icons::COPY, "경로 복사").clicked() {
            acts.push(Act::CopyPath(w.path.clone()));
        }
        if w.kind == WorkspaceKind::Worktree {
            widgets::menu_sep(ui);
            if widgets::menu_item(ui, icons::ARCHIVE, "목록에서 보관")
                .on_hover_text("목록에서만 숨겨요. 폴더와 기록은 그대로예요")
                .clicked()
            {
                acts.push(Act::ArchiveWs(key, w.name.clone()));
            }
            if widgets::menu_item(ui, icons::GIT_MERGE, "통합 준비…").clicked() {
                acts.push(Act::Merge(p.id, key));
            }
            if widgets::menu_item_danger(ui, icons::TRASH_2, "폴더 제거…")
                .on_hover_text("실행 중 세션, 저장하지 않은 문서, 커밋하지 않은 변경을 먼저 확인해요")
                .clicked()
            {
                acts.push(Act::RemoveFolder(p.id, key));
            }
        }
    });
}
