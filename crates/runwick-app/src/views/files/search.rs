//! File-name search in the sidebar: fuzzy, across the whole workspace (Korean initial
//! consonants work), with ↑/↓ to choose, Enter to open, ⌘Enter to open side by side, Esc to clear.

use super::{Act, Ctx, file_icon, menu};
use crate::App;
use crate::shell::widgets;
use crate::theme::{self, c, font, icon_font, medium, mono};
use egui::text::{LayoutJob, TextFormat};
use egui::{Align2, CornerRadius, Key, Modifiers, Pos2, Rect, Sense, Vec2};
use runwick_core::api::files::{FileHit, FilesFind, FindFiles, FindResult, display_path};
use runwick_core::josa;
use std::sync::Arc;

const ROW_H: f32 = 26.0;
const LIMIT: usize = 60;

#[derive(Default)]
pub struct Search {
    /// Last answer, shown while the next query is on its way (no flicker while typing).
    last: Option<Arc<FindResult>>,
    open_sel: Option<bool>,
    scroll_sel: bool,
}

pub fn field(app: &mut App, ui: &mut egui::Ui, cx: &Ctx) {
    let id = egui::Id::unique(("files-search", cx.ws));
    let composing = app.shell.keys.composing;
    let focused = ui.ctx().memory(|m| m.has_focus(id));
    let mut nav = 0i32;
    if focused && !composing {
        ui.input_mut(|i| {
            if i.consume_key(Modifiers::NONE, Key::ArrowDown) {
                nav = 1;
            }
            if i.consume_key(Modifiers::NONE, Key::ArrowUp) {
                nav = -1;
            }
        });
    }
    let st = &mut app.feat.files;
    let resp = theme::search_field(ui, &mut st.query, "파일 이름 검색", id, 28.0);
    if resp.changed() {
        st.sel = 0;
    }
    let (enter, cmd, esc) = ui.input(|i| (i.key_pressed(Key::Enter), i.modifiers.command, i.key_pressed(Key::Escape)));
    if resp.lost_focus() && enter && !composing && !st.query.trim().is_empty() {
        st.search.open_sel = Some(cmd);
    }
    if esc && (resp.has_focus() || resp.lost_focus()) {
        st.query.clear();
    }
    if nav != 0 {
        st.sel = (st.sel as i32 + nav).max(0) as usize;
        st.search.scroll_sel = true;
    }
}

pub fn show(app: &mut App, ui: &mut egui::Ui, cx: &Ctx, acts: &mut Vec<Act>) {
    let query = app.feat.files.query.trim().to_string();
    let params = FindFiles { workspace_id: cx.ws.id, query: query.clone(), limit: LIMIT };
    let q = app.hub.query::<FilesFind>(cx.ws.host, &params);
    if let Some(d) = &q.data {
        app.feat.files.search.last = Some(d.clone());
    }
    let res = q.data.clone().or_else(|| app.feat.files.search.last.clone());
    let Some(res) = res else {
        let text = match &q.error {
            Some(e) => format!("파일 이름을 찾지 못했어요 · {}", e.message),
            None if cx.remote && !app.hub.state(cx.ws.host).is_connected() => {
                "이 기기에 연결되어 있지 않아 찾을 수 없어요.".to_string()
            }
            None => "찾는 중…".to_string(),
        };
        widgets::empty_note(ui, &text);
        return;
    };
    let stale = q.data.is_none();
    if res.hits.is_empty() {
        let text = if stale {
            "찾는 중…".to_string()
        } else {
            format!("{} 맞는 파일 이름이 없어요", josa::gwa_wa(&format!("‘{query}’")))
        };
        widgets::empty_note(ui, &text);
        return;
    }
    let st = &mut app.feat.files;
    st.sel = st.sel.min(res.hits.len() - 1);
    let sel = st.sel;
    if !stale && let Some(side) = st.search.open_sel.take() {
        acts.push(Act::Open(res.hits[sel].path.clone(), side));
    }
    let scroll = std::mem::take(&mut st.search.scroll_sel);
    for (i, hit) in res.hits.iter().enumerate() {
        let unsaved = crate::views::docs::is_dirty(app, cx.ws, &hit.path);
        let resp = hit_row(ui, hit, i == sel, unsaved, stale);
        if i == sel && scroll {
            ui.scroll_to_rect(resp.rect, None);
        }
        if resp.clicked() {
            acts.push(Act::Open(hit.path.clone(), ui.input(|i| i.modifiers.command)));
        }
        resp.context_menu(|ui| menu(ui, cx, &hit.path, false, acts));
    }
    if res.truncated {
        widgets::empty_note(ui, &format!("파일이 너무 많아 {}개까지만 찾았어요.", res.indexed));
    } else {
        let w = ui.available_width();
        let (r, _) = ui.allocate_exact_size(Vec2::new(w, 22.0), Sense::hover());
        let cut = widgets::paint_clipped(
            ui,
            r.shrink2(Vec2::new(8.0, 0.0)),
            "↑↓ · Enter 열기 · ⌘Enter 나란히",
            font(11.5),
            c::TEXT_OFF,
            true,
        );
        let tip =
            format!("↑↓ 고르기 · Enter 열기 · ⌘Enter 나란히 열기 · Esc 지우기\n파일 {}개에서 찾았어요", res.indexed);
        let resp = ui.interact(r, ui.scope_id().with("files-search-hint"), Sense::hover());
        if cut || resp.hovered() {
            resp.on_hover_text(tip);
        }
    }
}

fn hit_row(ui: &mut egui::Ui, hit: &FileHit, selected: bool, unsaved: bool, stale: bool) -> egui::Response {
    let w = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, ROW_H), Sense::click());
    let shown = display_path(&hit.path).to_string();
    let chars: Vec<char> = shown.chars().collect();
    let name_start = chars.iter().rposition(|c| *c == '/').map(|i| i + 1).unwrap_or(0);
    let name: String = chars[name_start..].iter().collect();
    let dir: String = chars[..name_start.saturating_sub(1)].iter().collect();
    let label = shown.clone();
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
    let alpha = if stale { 0.55 } else { 1.0 };
    ui.painter().text(
        Pos2::new(rect.left() + 14.5, cy),
        Align2::CENTER_CENTER,
        file_icon(&hit.path, false, false),
        icon_font(13.0),
        c::TEXT_3.gamma_multiply(alpha),
    );
    let mut right = rect.right() - 8.0;
    if unsaved {
        ui.painter().circle_filled(Pos2::new(right - 3.0, cy), 3.0, c::TEXT_STRONG);
        right -= 12.0;
    }
    let x = rect.left() + 27.0;
    let mut job = LayoutJob::default();
    for (i, ch) in name.chars().enumerate() {
        let matched = hit.matched.contains(&((name_start + i) as u32));
        let (f, col) = if matched { (medium(12.5), c::TEXT_STRONG) } else { (font(12.5), c::TEXT_2) };
        let mut format = TextFormat::simple(f, col.gamma_multiply(alpha));
        if matched {
            format.underline = egui::Stroke::new(1.0, c::TEXT_3);
        }
        let mut buf = [0u8; 4];
        job.append(ch.encode_utf8(&mut buf), 0.0, format);
    }
    let name_max = (right - x) * if dir.is_empty() { 1.0 } else { 0.62 };
    job.wrap = egui::text::TextWrapping::truncate_at_width(name_max.max(20.0));
    let galley = ui.painter().layout_job(job);
    let name_w = galley.size().x;
    ui.painter().galley(Pos2::new(x, cy - galley.size().y / 2.0), galley, c::TEXT_2);
    if !dir.is_empty() {
        let r = Rect::from_min_max(Pos2::new(x + name_w + 8.0, rect.top()), Pos2::new(right - 2.0, rect.bottom()));
        if r.width() > 16.0 {
            widgets::paint_clipped(ui, r, &dir, mono(11.0), c::TEXT_3.gamma_multiply(alpha), true);
        }
    }
    theme::focus_ring(ui, &resp, CornerRadius::same(5));
    resp.on_hover_text(shown)
}
