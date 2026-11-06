//! Command palette (⌘K): fuzzy search over commands, projects, workspaces and sessions.
//! ↑/↓ moves, Enter runs, Esc closes and returns focus.

use super::{actions, data, widgets};
use crate::App;
use crate::client::HostKey;
use crate::icons;
use crate::state::{Modal, RightPanel, SidebarMode, TabKind, WsKey};
use crate::theme::{self, c, font, icon_font, medium, size};
use egui::{Align2, CornerRadius, Key, Pos2, Rect, RichText, Sense, Vec2};
use runwick_core::api::projects::ProjectId;
use runwick_core::api::sessions::SessionId;

#[derive(Default)]
pub struct State {
    query: String,
    sel: usize,
    focus: bool,
    scroll_to_sel: bool,
    return_focus: Option<egui::Id>,
}

pub fn open(app: &mut App, ctx: &egui::Context) {
    let st = &mut app.shell.palette;
    st.query.clear();
    st.sel = 0;
    st.focus = true;
    st.return_focus = ctx.memory(|m| m.focused());
    app.ui.palette_open = true;
}

pub fn close(app: &mut App, ctx: &egui::Context) {
    app.ui.palette_open = false;
    if let Some(id) = app.shell.palette.return_focus.take() {
        ctx.memory_mut(|m| m.request_focus(id));
    }
}

#[derive(Clone)]
enum Action {
    NewSession,
    OpenFolder,
    Settings,
    History,
    Wiki,
    Timeline,
    SplitWork,
    Quit,
    Activity,
    ToggleSidebar,
    SideBySide,
    Files,
    Project(ProjectId),
    Workspace(ProjectId, WsKey),
    Session { project: ProjectId, ws: WsKey, session: SessionId, hidden: bool },
}

#[derive(Clone)]
enum Mark {
    Icon(&'static str),
    Tool(&'static str),
}

#[derive(Clone)]
struct Entry {
    title: String,
    sub: String,
    mark: Mark,
    hint: &'static str,
    action: Action,
}

/// Subsequence match with bonuses for consecutive letters and word starts. `None` if the
/// query letters do not all appear in order.
pub fn fuzzy(query: &str, text: &str) -> Option<i32> {
    let q: Vec<char> = query.to_lowercase().chars().filter(|c| !c.is_whitespace()).collect();
    if q.is_empty() {
        return Some(0);
    }
    let t: Vec<char> = text.to_lowercase().chars().collect();
    let mut score = 0;
    let mut qi = 0;
    let mut prev_match: Option<usize> = None;
    let mut first = None;
    for (i, ch) in t.iter().enumerate() {
        if qi < q.len() && *ch == q[qi] {
            score += 10;
            if prev_match == Some(i.wrapping_sub(1)) {
                score += 8;
            }
            if i == 0 || matches!(t[i - 1], ' ' | '/' | '-' | '_' | '.' | '·') {
                score += 6;
            }
            first.get_or_insert(i);
            prev_match = Some(i);
            qi += 1;
        }
    }
    if qi < q.len() {
        return None;
    }
    Some(score - first.unwrap_or(0) as i32 - (t.len() as i32 / 8))
}

fn commands(app: &App) -> Vec<Entry> {
    let cmd = |title: &str, sub: &str, glyph: &'static str, hint: &'static str, action: Action| Entry {
        title: title.into(),
        sub: sub.into(),
        mark: Mark::Icon(glyph),
        hint,
        action,
    };
    let collapsed = app.ui.sidebar_collapsed;
    vec![
        cmd("새 세션", "선택한 작업 공간에서 CLI 시작", icons::PLUS, "⌘T", Action::NewSession),
        cmd("폴더 열기", "폴더를 프로젝트로 열기", icons::FOLDER_PLUS, "⌘O", Action::OpenFolder),
        cmd("대화 검색", "기록과 핀", icons::HISTORY, "⌘⇧F", Action::History),
        cmd("전체 활동", "확인할 일과 실행 중인 작업", icons::ACTIVITY, "", Action::Activity),
        cmd("위키", "선택한 프로젝트의 위키", icons::BOOK_OPEN, "", Action::Wiki),
        cmd("타임라인", "선택한 프로젝트의 타임라인", icons::CHART_GANTT, "", Action::Timeline),
        cmd("분리해서 작업", "worktree 공간을 만들어 따로 작업", icons::GIT_BRANCH, "", Action::SplitWork),
        cmd("나란히 보기", "두 탭을 좌우로", icons::COLUMNS_2, "", Action::SideBySide),
        cmd("파일 보기", "사이드바를 파일 목록으로", icons::FOLDER_OPEN, "", Action::Files),
        cmd(
            if collapsed { "사이드바 펼치기" } else { "사이드바 접기" },
            "탐색 영역",
            icons::PANEL_LEFT,
            "",
            Action::ToggleSidebar,
        ),
        cmd("설정", "CLI 도구, 환경변수, 기기, 백업", icons::SLIDERS_HORIZONTAL, "⌘,", Action::Settings),
        cmd("작업까지 종료…", "실행 중인 작업을 확인하고 멈춘 뒤 종료", icons::POWER, "", Action::Quit),
    ]
}

fn entries(app: &mut App) -> Vec<Entry> {
    let mut v = commands(app);
    let (Some(projects), Some(workspaces)) = (data::projects(app).data, data::workspaces(app).data) else { return v };
    let visible: Vec<_> = data::visible_projects(&projects).collect();
    for p in &visible {
        v.push(Entry {
            title: p.name.clone(),
            sub: data::tilde(&p.root),
            mark: Mark::Icon(icons::FOLDER),
            hint: "프로젝트",
            action: Action::Project(p.id),
        });
    }
    for w in workspaces.iter().filter(|w| visible.iter().any(|p| p.id == w.project_id)) {
        let pname = data::project_of(&projects, w.project_id).map(|p| p.name.as_str()).unwrap_or("");
        let sub = match &w.branch {
            Some(b) => format!("{} · {b}", data::tilde(&w.path)),
            None => data::tilde(&w.path),
        };
        v.push(Entry {
            title: format!("{pname} / {}", w.name),
            sub,
            mark: Mark::Icon(if w.kind == runwick_core::api::projects::WorkspaceKind::Worktree {
                icons::GIT_BRANCH
            } else {
                icons::FOLDER_OPEN
            }),
            hint: "작업 공간",
            action: Action::Workspace(w.project_id, data::ws_key(w)),
        });
    }
    if let Some(sessions) = data::all_sessions(app, HostKey::Local).data {
        for s in sessions.iter().filter(|s| !s.session.archived) {
            let Some(w) = workspaces.iter().find(|w| w.id == s.session.workspace_id) else { continue };
            let pname = data::project_of(&projects, w.project_id).map(|p| p.name.as_str()).unwrap_or("");
            let live = data::live_of(app, HostKey::Local, s);
            let st = data::status(&s.session, live.as_ref());
            v.push(Entry {
                title: s.session.title.clone(),
                sub: format!("{pname} / {} · {} · {}", w.name, s.session.tool.label(), st.label),
                mark: Mark::Tool(data::tool_mark(s.session.tool)),
                hint: "세션",
                action: Action::Session {
                    project: w.project_id,
                    ws: data::ws_key(w),
                    session: s.session.id,
                    hidden: s.session.hidden,
                },
            });
        }
    }
    v
}

pub fn show(app: &mut App, ctx: &egui::Context) {
    if !app.ui.palette_open {
        return;
    }
    let all = entries(app);
    let q = app.shell.palette.query.trim().to_string();
    let mut ranked: Vec<(i32, usize)> = all
        .iter()
        .enumerate()
        .filter_map(|(i, e)| {
            let s = fuzzy(&q, &e.title).map(|s| s + 20).or_else(|| fuzzy(&q, &e.sub));
            s.map(|s| (s, i))
        })
        .collect();
    if !q.is_empty() {
        ranked.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    }
    ranked.truncate(60);
    let n = ranked.len();
    let st = &mut app.shell.palette;
    if st.sel >= n {
        st.sel = n.saturating_sub(1);
    }
    let composing = app.shell.keys.composing;
    let (down, up, enter) = if composing {
        (false, false, false)
    } else {
        ctx.input_mut(|i| {
            (
                i.consume_key(egui::Modifiers::NONE, Key::ArrowDown),
                i.consume_key(egui::Modifiers::NONE, Key::ArrowUp),
                i.consume_key(egui::Modifiers::NONE, Key::Enter),
            )
        })
    };
    if down && n > 0 {
        st.sel = (st.sel + 1) % n;
        st.scroll_to_sel = true;
    }
    if up && n > 0 {
        st.sel = (st.sel + n - 1) % n;
        st.scroll_to_sel = true;
    }
    let mut run = enter.then_some(st.sel).filter(|_| n > 0);
    let width = 620.0_f32.min(ctx.content_rect().width() - 32.0);
    let area = egui::Modal::default_area(egui::Id::unique("palette"))
        .anchor(Align2::CENTER_TOP, Vec2::new(0.0, size::TITLEBAR + 12.0));
    let frame = egui::Frame::new()
        .fill(c::PANEL_2)
        .stroke(egui::Stroke::new(1.0, c::LINE_STRONG))
        .corner_radius(CornerRadius::same(size::RADIUS_LG))
        .inner_margin(egui::Margin::same(8))
        .shadow(egui::epaint::Shadow {
            offset: [0, 18],
            blur: 56,
            spread: 0,
            color: egui::Color32::from_black_alpha(130),
        });
    let resp = egui::Modal::new(egui::Id::unique("palette"))
        .area(area)
        .backdrop_color(egui::Color32::from_black_alpha(90))
        .frame(frame)
        .show(ctx, |ui| {
            ui.set_width(width);
            ui.spacing_mut().item_spacing = Vec2::new(0.0, 2.0);
            let st = &mut app.shell.palette;
            let id = ui.scope_id().with("palette-input");
            let field = ui
                .scope(|ui| {
                    ui.set_height(36.0);
                    theme::search_field(ui, &mut st.query, "프로젝트, 작업 공간, 세션, 명령 검색", id, 36.0)
                })
                .inner;
            field.widget_info(|| egui::WidgetInfo::labeled(egui::Role::SearchInput, true, "명령 검색"));
            if st.focus {
                field.request_focus();
                st.focus = false;
            }
            if field.changed() {
                st.sel = 0;
            }
            ui.add_space(6.0);
            if n == 0 {
                ui.add_space(8.0);
                ui.label(RichText::new(format!("‘{q}’와 맞는 항목이 없어요")).font(font(12.5)).color(c::TEXT_3));
                ui.add_space(8.0);
                return;
            }
            let sel = st.sel;
            let scroll = std::mem::take(&mut st.scroll_to_sel);
            egui::ScrollArea::vertical().max_height(420.0).auto_shrink([false, true]).show(ui, |ui| {
                for (row, (_, idx)) in ranked.iter().enumerate() {
                    let e = &all[*idx];
                    let w = ui.available_width();
                    let (r, resp) = ui.allocate_exact_size(Vec2::new(w, 44.0), Sense::click());
                    let label = format!("{} — {}", e.title, e.sub);
                    resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::ListItem, true, row == sel, &label));
                    if resp.hovered() && ui.input(|i| i.pointer.delta() != Vec2::ZERO) {
                        app.shell.palette.sel = row;
                    }
                    let on = row == app.shell.palette.sel;
                    if on {
                        ui.painter().rect_filled(r, CornerRadius::same(7), c::CONTROL);
                        if scroll {
                            ui.scroll_to_rect(r, None);
                        }
                    }
                    let badge = Rect::from_center_size(Pos2::new(r.left() + 22.0, r.center().y), Vec2::splat(24.0));
                    match e.mark {
                        Mark::Tool(m) => theme::paint_tool_badge(ui.painter(), badge, m, c::CONTROL_HOVER),
                        Mark::Icon(g) => {
                            ui.painter().rect_filled(
                                badge,
                                CornerRadius::same(5),
                                if on { c::CONTROL_HOVER } else { c::ROW },
                            );
                            ui.painter().text(badge.center(), Align2::CENTER_CENTER, g, icon_font(13.0), c::TEXT);
                        }
                    }
                    let hint_w =
                        if e.hint.is_empty() { 0.0 } else { widgets::text_width(ui, e.hint, font(11.5)) + 22.0 };
                    let tr = Rect::from_min_max(
                        Pos2::new(badge.right() + 12.0, r.top() + 5.0),
                        Pos2::new(r.right() - hint_w - 8.0, r.top() + 23.0),
                    );
                    widgets::paint_clipped(ui, tr, &e.title, medium(13.0), c::TEXT_STRONG, true);
                    let sr = Rect::from_min_max(
                        Pos2::new(badge.right() + 12.0, r.top() + 23.0),
                        Pos2::new(r.right() - hint_w - 8.0, r.top() + 39.0),
                    );
                    widgets::paint_clipped(ui, sr, &e.sub, font(12.0), c::TEXT_3, true);
                    if !e.hint.is_empty() {
                        let f = font(11.5);
                        ui.painter().text(
                            Pos2::new(r.right() - 12.0, r.center().y),
                            Align2::RIGHT_CENTER,
                            e.hint,
                            f,
                            c::TEXT_3,
                        );
                    }
                    if resp.clicked() {
                        run = Some(row);
                    }
                }
            });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 12.0;
                ui.add_space(6.0);
                for t in ["↑↓ 이동", "Enter 열기", "Esc 닫기"] {
                    ui.label(RichText::new(t).font(font(11.5)).color(c::TEXT_OFF));
                }
            });
        });
    if let Some((_, idx)) = run.and_then(|row| ranked.get(row)) {
        let action = all[*idx].action.clone();
        close(app, ctx);
        perform(app, action);
        return;
    }
    if resp.should_close() {
        close(app, ctx);
    }
}

fn perform(app: &mut App, a: Action) {
    match a {
        Action::NewSession => actions::new_session(app),
        Action::OpenFolder => actions::open_folder_dialog(app),
        Action::Settings => actions::open_settings(app, None),
        Action::History => {
            app.ui.page = crate::state::Page::Work;
            app.ui.right = Some(RightPanel::History);
        }
        Action::Activity => {
            app.ui.page = crate::state::Page::Work;
            app.ui.right = Some(RightPanel::Activity);
        }
        Action::Wiki => match app.ui.selected_project {
            Some(project) => actions::open_center_tab(app, TabKind::Wiki { project, page: None }),
            None => app.toast_info("먼저 프로젝트를 고르세요"),
        },
        Action::Timeline => actions::open_center_tab(app, TabKind::Timeline { project: app.ui.selected_project }),
        Action::SplitWork => match app.ui.selected_ws {
            Some(ws) => app.ui.modal = Some(Modal::SplitWork { ws }),
            None => app.toast_info("먼저 작업 공간을 고르세요"),
        },
        Action::Quit => app.ui.modal = Some(Modal::Quit),
        Action::ToggleSidebar => app.ui.sidebar_collapsed = !app.ui.sidebar_collapsed,
        Action::SideBySide => {
            if let Some(ws) = app.ui.selected_ws {
                let set = app.ui.tabset_mut(&ws);
                if set.split.is_some() {
                    set.split = None;
                } else if let Some(other) = set.tabs.iter().rev().find(|t| Some(t.id) != set.active).map(|t| t.id) {
                    set.split = Some(other);
                } else {
                    app.toast_info("나란히 보려면 탭이 둘 이상 있어야 해요");
                }
            }
        }
        Action::Files => {
            app.ui.sidebar_mode = SidebarMode::Files;
            app.ui.sidebar_collapsed = false;
        }
        Action::Project(p) => actions::select_project(app, p),
        Action::Workspace(p, ws) => actions::select_workspace(app, p, ws),
        Action::Session { project, ws, session, hidden } => actions::open_session(app, project, ws, session, hidden),
    }
}

#[cfg(test)]
mod tests {
    use super::fuzzy;

    #[test]
    fn fuzzy_matches_in_order_and_prefers_word_starts() {
        assert!(fuzzy("pa", "payments-api").is_some());
        assert!(fuzzy("api", "payments-api").is_some());
        assert!(fuzzy("xyz", "payments-api").is_none());
        assert!(fuzzy("정산", "사내 정산 리포트 자동화").is_some());
        let start = fuzzy("set", "설정 settings").unwrap();
        let mid = fuzzy("set", "reset").unwrap();
        assert!(start > mid, "{start} vs {mid}");
        assert_eq!(fuzzy("", "anything"), Some(0));
    }
}
