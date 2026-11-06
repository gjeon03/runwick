//! The folder tree: one host listing per expanded folder, fetched when it is opened.

use super::{Act, Ctx, file_icon, menu};
use crate::App;
use crate::icons;
use crate::shell::widgets;
use crate::theme::{self, Tone, c, font, icon_font, mono};
use egui::{Align2, Color32, CornerRadius, Pos2, Rect, Sense, Vec2};
use runwick_core::api::files::{DirListing, FileEntry, FilesList, GitMark, ListDir, display_path};
use runwick_core::rpc::{RpcError, codes};
use std::collections::BTreeSet;

const ROW_H: f32 = 25.0;
const INDENT: f32 = 14.0;
/// Deeper folders still open, without more indent (no horizontal scrolling in the sidebar).
const MAX_INDENT_DEPTH: usize = 8;

pub fn list_params(cx: &Ctx, dir: &str) -> ListDir {
    ListDir { workspace_id: cx.ws.id, dir: dir.to_string(), show_hidden: cx.show_hidden }
}

fn indent_x(depth: usize) -> f32 {
    8.0 + depth.min(MAX_INDENT_DEPTH) as f32 * INDENT
}

pub fn git_color(m: GitMark) -> Color32 {
    match m {
        GitMark::Modified => Tone::Check.fg(),
        GitMark::Added | GitMark::Untracked => Tone::Running.fg(),
        GitMark::Deleted | GitMark::Conflicted => Tone::Recover.fg(),
        GitMark::Renamed => Tone::Info.fg(),
        GitMark::Contains => c::TEXT_3,
    }
}

pub fn show(app: &mut App, ui: &mut egui::Ui, cx: &Ctx, acts: &mut Vec<Act>) {
    let q = app.hub.query::<FilesList>(cx.ws.host, &list_params(cx, ""));
    let expanded = app.feat.files.expanded.get(&cx.ws).cloned().unwrap_or_default();
    match (q.data, q.error) {
        (Some(listing), _) if listing.entries.is_empty() => {
            widgets::empty_note(ui, "빈 폴더예요. 새 문서를 만들거나 CLI가 만든 파일이 생기면 여기에 보여요.");
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                if widgets::pill_button(ui, Some(icons::FILE_PLUS), "새 문서", 26.0, c::ROW).clicked() {
                    acts.push(Act::NewDoc(String::new()));
                }
            });
        }
        (Some(listing), _) => entries(app, ui, cx, &listing, 0, &expanded, acts),
        (None, Some(e)) => error_note(ui, &e, 0, acts),
        (None, None) if cx.remote && !app.hub.state(cx.ws.host).is_connected() => {
            widgets::empty_note(ui, "이 기기에 연결되어 있지 않아 파일을 볼 수 없어요. 연결되면 바로 보여요.");
        }
        (None, None) => widgets::empty_note(ui, "파일 목록을 불러오는 중…"),
    }
}

fn entries(
    app: &mut App,
    ui: &mut egui::Ui,
    cx: &Ctx,
    listing: &DirListing,
    depth: usize,
    expanded: &BTreeSet<String>,
    acts: &mut Vec<Act>,
) {
    for e in &listing.entries {
        let open = e.is_dir && expanded.contains(&e.path);
        let unsaved = !e.is_dir && crate::views::docs::is_dirty(app, cx.ws, &e.path);
        let selected = cx.active.as_deref() == Some(e.path.as_str());
        let resp = row(ui, depth, e, open, selected, unsaved);
        if resp.clicked() || (resp.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))) {
            acts.push(if e.is_dir { Act::Toggle(e.path.clone()) } else { Act::Open(e.path.clone(), false) });
        }
        resp.context_menu(|ui| menu(ui, cx, &e.path, e.is_dir, acts));
        if open {
            child(app, ui, cx, &e.path, depth + 1, expanded, acts);
        }
    }
    if listing.more > 0 {
        dim_line(ui, depth, &format!("{}개 더 있어요 · 위에서 이름으로 찾아 주세요", listing.more));
    }
}

fn child(
    app: &mut App,
    ui: &mut egui::Ui,
    cx: &Ctx,
    dir: &str,
    depth: usize,
    expanded: &BTreeSet<String>,
    acts: &mut Vec<Act>,
) {
    let q = app.hub.query::<FilesList>(cx.ws.host, &list_params(cx, dir));
    match (q.data, q.error) {
        (Some(l), _) if l.entries.is_empty() && l.more == 0 => dim_line(ui, depth, "빈 폴더"),
        (Some(l), _) => entries(app, ui, cx, &l, depth, expanded, acts),
        (None, Some(e)) => error_note(ui, &e, depth, acts),
        (None, None) => dim_line(ui, depth, "불러오는 중…"),
    }
}

fn dim_line(ui: &mut egui::Ui, depth: usize, text: &str) {
    let w = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(Vec2::new(w, ROW_H - 3.0), Sense::hover());
    let r = Rect::from_min_max(Pos2::new(rect.left() + indent_x(depth) + 16.0, rect.top()), rect.max);
    widgets::paint_clipped(ui, r, text, font(12.0), c::TEXT_3, true);
}

fn error_note(ui: &mut egui::Ui, e: &RpcError, depth: usize, acts: &mut Vec<Act>) {
    let msg = match e.code.as_str() {
        codes::DISCONNECTED => "기기에 연결되어 있지 않아 파일을 볼 수 없어요. 연결되면 바로 보여요.".to_string(),
        codes::NOT_FOUND | codes::UNSUPPORTED | codes::DENIED => e.message.clone(),
        _ => format!("파일 목록을 불러오지 못했어요 · {}", e.message),
    };
    if depth == 0 {
        widgets::empty_note(ui, &msg);
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            if widgets::pill_button(ui, Some(icons::REFRESH_CW), "다시 시도", 26.0, c::ROW).clicked() {
                acts.push(Act::Refresh);
            }
        });
    } else {
        let w = ui.available_width();
        let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, ROW_H - 3.0), Sense::hover());
        let r = Rect::from_min_max(Pos2::new(rect.left() + indent_x(depth) + 16.0, rect.top()), rect.max);
        widgets::paint_clipped(ui, r, &msg, font(12.0), Tone::Recover.fg(), true);
        resp.on_hover_text(msg);
    }
}

fn row(ui: &mut egui::Ui, depth: usize, e: &FileEntry, open: bool, selected: bool, unsaved: bool) -> egui::Response {
    let w = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, ROW_H), Sense::click());
    let name = display_path(&e.name).to_string();
    let label = name.clone();
    resp.widget_info(move || egui::WidgetInfo::selected(egui::Role::ListItem, true, selected, &label));
    let bg = if selected {
        Some(c::CONTROL)
    } else if resp.hovered() {
        Some(c::ROW)
    } else {
        None
    };
    if let Some(bg) = bg {
        ui.painter().rect_filled(rect, CornerRadius::same(5), bg);
    }
    let cy = rect.center().y;
    let x = rect.left() + indent_x(depth);
    if e.is_dir {
        let chevron = if open { icons::CHEVRON_DOWN } else { icons::CHEVRON_RIGHT };
        ui.painter().text(Pos2::new(x + 5.0, cy), Align2::CENTER_CENTER, chevron, icon_font(11.0), c::TEXT_3);
    }
    let icon_x = x + 16.0;
    let glyph = file_icon(&e.path, e.is_dir, open);
    ui.painter().text(Pos2::new(icon_x + 6.5, cy), Align2::CENTER_CENTER, glyph, icon_font(13.0), c::TEXT_3);
    let mut right = rect.right() - 8.0;
    if let Some(m) = e.git {
        ui.painter().text(Pos2::new(right, cy), Align2::RIGHT_CENTER, m.letter(), mono(10.5), git_color(m));
        right -= 14.0;
    }
    if unsaved {
        ui.painter().circle_filled(Pos2::new(right - 3.0, cy), 3.0, c::TEXT_STRONG);
        right -= 12.0;
    }
    let fg = if selected { c::TEXT_STRONG } else { c::TEXT_2 };
    let name_rect = Rect::from_min_max(Pos2::new(icon_x + 19.0, rect.top()), Pos2::new(right - 2.0, rect.bottom()));
    widgets::paint_clipped(ui, name_rect, &name, font(12.5), fg, true);
    theme::focus_ring(ui, &resp, CornerRadius::same(5));
    let mut tip = display_path(&e.path).to_string();
    if e.symlink {
        tip.push_str("\n링크");
    }
    if unsaved {
        tip.push_str("\n미저장 편집이 있어요");
    }
    if let Some(m) = e.git {
        tip.push_str(&format!("\ngit: {}", m.label()));
    }
    resp.on_hover_text(tip)
}
