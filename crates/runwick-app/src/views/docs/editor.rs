//! The document body: an editor with line numbers, syntax colours and find (Markdown, text,
//! and code once 편집 is on; code is otherwise selectable read-only), the Markdown preview, a
//! light read-only view for big files, and the states of files that are not text.

use super::model::{Body, Kind};
use super::{Act, Entry, Place};
use crate::icons;
use crate::shell::widgets;
use crate::theme::{self, c, font, medium, mono, mono_bold_family, ty};
use egui::text::{ByteIndex, CCursor, LayoutJob, LayoutSection, TextFormat};
use egui::{Align, Align2, Color32, FontId, Id, Margin, Pos2, RichText, Sense, Vec2};
use std::ops::Range;

/// Bigger text is shown read-only in a light view (an editor re-lays out all of it per edit).
pub const EDIT_MAX: usize = 256 * 1024;
/// Syntax colours for code up to this size, and a smaller limit while typing.
const HIGHLIGHT_MAX: usize = 256 * 1024;
const HIGHLIGHT_EDIT_MAX: usize = 64 * 1024;
const MAX_MATCHES: usize = 10_000;
const LINE_H: f32 = 22.0;
const GUTTER_GAP: f32 = 12.0;
const MATCH_BG: Color32 = Color32::from_rgba_premultiplied(68, 58, 32, 150);
const CURRENT_BG: Color32 = Color32::from_rgba_premultiplied(140, 76, 34, 210);

#[derive(Default)]
pub struct Find {
    pub open: bool,
    pub query: String,
    pub current: usize,
    pub count: usize,
    focus: bool,
    scroll: bool,
}

impl Find {
    pub fn open(&mut self) {
        self.open = true;
        self.focus = true;
        self.scroll = true;
    }
}

/// Line starts of a big read-only text, computed once per version.
pub struct Lines {
    key: (u64, usize),
    starts: Vec<usize>,
}

/// Case-insensitive for ASCII queries (byte offsets stay valid), exact otherwise.
pub fn find_all(hay: &str, needle: &str) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    if needle.is_empty() {
        return out;
    }
    if needle.is_ascii() {
        let (h, n) = (hay.as_bytes(), needle.as_bytes());
        let mut i = 0;
        while i + n.len() <= h.len() && out.len() < MAX_MATCHES {
            if h[i..i + n.len()].eq_ignore_ascii_case(n) {
                out.push(i..i + n.len());
                i += n.len();
            } else {
                i += 1;
            }
        }
    } else {
        out.extend(hay.match_indices(needle).take(MAX_MATCHES).map(|(i, m)| i..i + m.len()));
    }
    out
}

/// Give `ranges` (sorted, non-overlapping) a background in `job`; `current` stands out.
fn mark_ranges(job: &mut LayoutJob, ranges: &[Range<usize>], current: Option<usize>) {
    if ranges.is_empty() {
        return;
    }
    let len = job.text.len();
    let piece = |lead: f32, a: usize, b: usize, format: TextFormat| LayoutSection {
        leading_space: lead,
        byte_range: ByteIndex(a)..ByteIndex(b),
        format,
    };
    let mut out = Vec::with_capacity(job.sections.len() + ranges.len() * 2);
    for sec in std::mem::take(&mut job.sections) {
        let (s, e) = (sec.byte_range.start.0, sec.byte_range.end.0);
        let mut cur = s;
        let mut lead = sec.leading_space;
        let first = ranges.partition_point(|r| r.end <= s);
        for (idx, r) in ranges.iter().enumerate().skip(first) {
            if r.start >= e {
                break;
            }
            let (a, b) = (r.start.max(cur).min(len), r.end.min(e).min(len));
            if a >= b {
                continue;
            }
            if a > cur {
                out.push(piece(lead, cur, a, sec.format.clone()));
                lead = 0.0;
            }
            let mut format = sec.format.clone();
            format.background = if Some(idx) == current { CURRENT_BG } else { MATCH_BG };
            out.push(piece(lead, a, b, format));
            lead = 0.0;
            cur = b;
        }
        if cur < e {
            out.push(piece(lead, cur, e, sec.format));
        }
    }
    job.sections = out;
}

fn body_font(kind: Kind, proportional: bool) -> FontId {
    if kind != Kind::Code && proportional { font(14.0) } else { mono(13.0) }
}

fn fmt(font_id: FontId, color: Color32) -> TextFormat {
    TextFormat { font_id, color, line_height: Some(LINE_H), ..Default::default() }
}

/// Markdown source with headings, quotes and fenced code told apart.
fn md_job(text: &str, proportional: bool) -> LayoutJob {
    let body = body_font(Kind::Markdown, proportional);
    let head = if proportional { theme::semibold(14.0) } else { FontId::new(13.0, mono_bold_family()) };
    let mut job = LayoutJob::default();
    let mut fence = false;
    for line in text.split_inclusive('\n') {
        let t = line.trim_start();
        let is_fence = t.starts_with("```") || t.starts_with("~~~");
        let format = if is_fence || fence {
            fmt(body.clone(), c::TEXT_2)
        } else if t.starts_with('#') && t.trim_start_matches('#').starts_with([' ', '\n']) {
            fmt(head.clone(), c::TEXT_STRONG)
        } else if t.starts_with('>') {
            fmt(body.clone(), c::TEXT_2)
        } else {
            fmt(body.clone(), c::TEXT)
        };
        if is_fence {
            fence = !fence;
        }
        job.append(line, 0.0, format);
    }
    job
}

pub(crate) struct Look {
    pub kind: Kind,
    pub lang: String,
    pub proportional: bool,
    pub editing: bool,
}

fn build_job(ui: &egui::Ui, look: &Look, text: &str) -> LayoutJob {
    if text.is_empty() {
        return LayoutJob::single_section(String::new(), fmt(body_font(look.kind, look.proportional), c::TEXT));
    }
    let limit = if look.editing { HIGHLIGHT_EDIT_MAX } else { HIGHLIGHT_MAX };
    let mut job = match look.kind {
        Kind::Code if text.len() <= limit => {
            let theme = egui_extras::syntax_highlighting::CodeTheme::dark(13.0);
            egui_extras::syntax_highlighting::highlight(ui.ctx(), ui.style(), &theme, text, &look.lang)
        }
        Kind::Markdown => md_job(text, look.proportional),
        _ => LayoutJob::single_section(text.to_string(), fmt(body_font(look.kind, look.proportional), c::TEXT)),
    };
    for s in &mut job.sections {
        s.format.line_height = Some(LINE_H);
    }
    job
}

fn gutter_width(lines: usize) -> f32 {
    let digits = lines.max(1).to_string().len().max(2) as f32;
    (digits * 7.0 + 12.0).max(36.0)
}

fn paint_line_numbers(ui: &egui::Ui, galley: &egui::Galley, origin: Pos2, right_x: f32) {
    let clip = ui.clip_rect();
    let mut line = 1usize;
    let mut starts_line = true;
    for row in &galley.rows {
        let r = row.rect().translate(origin.to_vec2());
        if starts_line && r.bottom() >= clip.top() && r.top() <= clip.bottom() {
            ui.painter().text(
                Pos2::new(right_x, r.center().y),
                Align2::RIGHT_CENTER,
                line.to_string(),
                mono(11.0),
                c::TEXT_OFF,
            );
        }
        starts_line = row.ends_with_newline;
        if starts_line {
            line += 1;
        }
    }
}

/// Matches of the open find bar in `text`, keeping `current` in range.
fn matches_for(find: &mut Find, text: &str) -> Vec<Range<usize>> {
    if !find.open {
        return Vec::new();
    }
    let m = find_all(text, &find.query);
    find.count = m.len();
    if find.current >= m.len() {
        find.current = 0;
    }
    m
}

pub(crate) struct AreaOut {
    pub changed: bool,
}

type Layouter<'a> = dyn FnMut(&egui::Ui, &dyn egui::TextBuffer, f32) -> std::sync::Arc<egui::Galley> + 'a;

fn configure<'t>(
    te: egui::TextEdit<'t>,
    id: Id,
    width: f32,
    min: Vec2,
    editable: bool,
    layouter: &'t mut Layouter<'_>,
) -> egui::TextEdit<'t> {
    let hint = if editable { "내용을 입력하세요" } else { "" };
    te.id(id)
        .frame(egui::Frame::NONE)
        .margin(Margin { left: 0, right: 14, top: 0, bottom: 0 })
        .desired_width(width)
        .min_size(min)
        .lock_focus(true)
        .hint_text(RichText::new(hint).color(c::TEXT_OFF))
        .layouter(layouter)
}

/// A text area with line numbers. Editable when `editable`, otherwise selectable only.
pub(crate) fn text_area(
    ui: &mut egui::Ui,
    id: Id,
    text: &mut String,
    editable: bool,
    look: &Look,
    find: &mut Find,
    focus: &mut bool,
) -> AreaOut {
    let wrap = look.kind != Kind::Code;
    let ranges = matches_for(find, text);
    let current = (!ranges.is_empty()).then_some(find.current);
    let target = current.and_then(|i| ranges.get(i)).map(|r| text[..r.start].chars().count());
    let scroll_to_match = std::mem::take(&mut find.scroll);
    let known_len = text.len();
    let mut layouter = |ui: &egui::Ui, buf: &dyn egui::TextBuffer, wrap_width: f32| {
        let s = buf.as_str();
        let mut job = build_job(ui, look, s);
        if s.len() == known_len {
            mark_ranges(&mut job, &ranges, current);
        }
        job.wrap.max_width = if wrap { wrap_width } else { f32::INFINITY };
        ui.fonts_mut(|f| f.layout_job(job))
    };
    // Inside a horizontally scrolling area the available width is unbounded: size from the view.
    let (view_w, view_h) = (ui.available_width(), ui.available_height());
    let lines = text.lines().count() + 1;
    let gutter = gutter_width(lines);
    let width = (view_w - gutter - GUTTER_GAP).max(80.0);
    let min = Vec2::new(width, (view_h - 26.0).max(LINE_H));
    let mut changed = false;
    let scroll = if wrap { egui::ScrollArea::vertical() } else { egui::ScrollArea::both() };
    scroll.id_salt(id.with("scroll")).auto_shrink([false, false]).show(ui, |ui| {
        ui.add_space(12.0);
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            let (g, _) = ui.allocate_exact_size(Vec2::new(gutter + GUTTER_GAP, LINE_H), Sense::hover());
            let out = if editable {
                configure(egui::TextEdit::multiline(text), id, width, min, true, &mut layouter).show(ui)
            } else {
                let mut ro: &str = text.as_str();
                configure(egui::TextEdit::multiline(&mut ro), id, width, min, false, &mut layouter).show(ui)
            };
            changed = out.response.changed();
            paint_line_numbers(ui, &out.galley, out.galley_pos, g.left() + gutter);
            if scroll_to_match && let Some(ci) = target {
                let r = out.galley.pos_from_cursor(CCursor::new(ci)).translate(out.galley_pos.to_vec2());
                ui.scroll_to_rect(r.expand(LINE_H), Some(Align::Center));
            }
            if std::mem::take(focus) {
                out.response.request_focus();
            }
        });
        ui.add_space(14.0);
    });
    AreaOut { changed }
}

/// Find bar under the header (⌘F): Enter next, ⇧Enter previous, Esc closes.
fn find_bar(ui: &mut egui::Ui, find: &mut Find, field_id: Id, editor_focus: &mut bool) {
    let w = ui.available_width();
    let bar = egui::Frame::new().fill(c::PANEL).inner_margin(Margin { left: 14, right: 10, top: 6, bottom: 6 }).show(
        ui,
        |ui| {
            ui.set_width(w - 24.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                let field_w = (ui.available_width() - 150.0).clamp(120.0, 320.0);
                let resp = ui
                    .allocate_ui(Vec2::new(field_w, 26.0), |ui| {
                        theme::search_field(ui, &mut find.query, "문서에서 찾기", field_id, 26.0)
                    })
                    .inner;
                if std::mem::take(&mut find.focus) {
                    resp.request_focus();
                }
                if resp.changed() {
                    find.current = 0;
                    find.scroll = true;
                }
                let (enter, shift, esc) = ui
                    .input(|i| (i.key_pressed(egui::Key::Enter), i.modifiers.shift, i.key_pressed(egui::Key::Escape)));
                let mut step = 0i32;
                if resp.lost_focus() && enter {
                    step = if shift { -1 } else { 1 };
                    resp.request_focus();
                }
                let count = if find.query.is_empty() {
                    String::new()
                } else if find.count == 0 {
                    "없음".to_string()
                } else {
                    format!("{}/{}", find.current + 1, find.count)
                };
                ui.label(RichText::new(count).font(mono(11.5)).color(if find.count == 0 {
                    c::TEXT_3
                } else {
                    c::TEXT_2
                }));
                if widgets::icon_btn(ui, icons::CHEVRON_UP, "이전 (⇧Enter)", 24.0, 24.0, 13.0).clicked() {
                    step = -1;
                }
                if widgets::icon_btn(ui, icons::CHEVRON_DOWN, "다음 (Enter)", 24.0, 24.0, 13.0).clicked() {
                    step = 1;
                }
                let close = widgets::icon_btn(ui, icons::X, "찾기 닫기 (Esc)", 24.0, 24.0, 12.0).clicked()
                    || (esc && (resp.has_focus() || resp.lost_focus()));
                if step != 0 && find.count > 0 {
                    let n = find.count as i32;
                    find.current = (find.current as i32 + step).rem_euclid(n) as usize;
                    find.scroll = true;
                }
                if close {
                    find.open = false;
                    *editor_focus = true;
                }
            });
        },
    );
    let r = bar.response.rect;
    ui.painter().hline(r.x_range(), r.bottom() - 0.5, egui::Stroke::new(1.0, c::LINE));
}

pub(crate) fn look_of(e: &Entry) -> Look {
    let mut lang = super::model::Kind::language(&e.doc.path);
    if lang == "ts" {
        // The bundled grammars have no TypeScript; JavaScript colours it reasonably.
        lang = "js".into();
    }
    Look { kind: e.doc.kind, lang, proportional: e.ui.proportional, editing: editable(e) }
}

pub(crate) fn editable(e: &Entry) -> bool {
    e.doc.is_text()
        && !e.doc.lossy
        && !e.doc.readonly_file
        && e.doc.text.len() <= EDIT_MAX
        && (e.doc.kind != Kind::Code || e.ui.edit_code)
}

pub fn show(
    ui: &mut egui::Ui,
    e: &mut Entry,
    place: &Place,
    md: &mut egui_commonmark::CommonMarkCache,
    acts: &mut Vec<Act>,
) {
    match &e.doc.body {
        Body::Text => {}
        _ => return not_text(ui, e, place, acts),
    }
    let id = Id::unique(("doc", e.doc.ws, e.doc.path.as_str()));
    if e.ui.find.open && !(e.doc.kind == Kind::Markdown && e.ui.preview) {
        find_bar(ui, &mut e.ui.find, id.with("find"), &mut e.ui.focus_editor);
    }
    if e.doc.kind == Kind::Markdown && e.ui.preview {
        return preview(ui, e, md);
    }
    if e.doc.text.len() > EDIT_MAX {
        return big_view(ui, e);
    }
    let look = look_of(e);
    let editable = editable(e);
    let out = text_area(ui, id, &mut e.doc.text, editable, &look, &mut e.ui.find, &mut e.ui.focus_editor);
    if out.changed {
        acts.push(Act::Edited);
        if e.doc.save != super::model::SaveState::Queued && !e.doc.saving() {
            e.doc.save = super::model::SaveState::Idle;
        }
        e.doc.reloaded_at = None;
    }
}

/// Markdown images would be read from this computer's disk (or the web) by the preview; show
/// their text instead so a remote document never mixes in local files.
fn strip_images(text: &str) -> std::borrow::Cow<'_, str> {
    if !text.contains("![") {
        return std::borrow::Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find("![") {
        out.push_str(&rest[..i]);
        let after = &rest[i + 2..];
        let parsed = after.find("](").and_then(|close| {
            let alt = &after[..close];
            let tail = &after[close + 2..];
            (!alt.contains('\n')).then_some(())?;
            tail.find(')').filter(|end| !tail[..*end].contains('\n')).map(|end| (alt, close + 2 + end + 1))
        });
        match parsed {
            Some((alt, used)) => {
                let alt = if alt.trim().is_empty() { "이미지" } else { alt.trim() };
                out.push_str(&format!("`이미지: {alt}`"));
                rest = &after[used..];
            }
            None => {
                out.push_str("![");
                rest = after;
            }
        }
    }
    out.push_str(rest);
    std::borrow::Cow::Owned(out)
}

fn preview(ui: &mut egui::Ui, e: &mut Entry, md: &mut egui_commonmark::CommonMarkCache) {
    let id = Id::unique(("doc-preview", e.doc.ws, e.doc.path.as_str()));
    egui::ScrollArea::vertical().id_salt(id).auto_shrink([false, false]).show(ui, |ui| {
        egui::Frame::new().inner_margin(Margin { left: 28, right: 28, top: 18, bottom: 28 }).show(ui, |ui| {
            ui.set_max_width(ui.available_width().min(780.0));
            ui.style_mut().override_text_style = Some(egui::TextStyle::Body);
            // The default text color only, so links keep the link color.
            ui.visuals_mut().widgets.noninteractive.fg_stroke.color = c::TEXT;
            let text = strip_images(&e.doc.text);
            if text.trim().is_empty() {
                ui.label(
                    RichText::new("빈 문서예요. 편집에서 내용을 쓰면 여기에 보여요.")
                        .font(font(ty::BODY))
                        .color(c::TEXT_3),
                );
            } else {
                egui_commonmark::CommonMarkViewer::new().max_image_width(Some(640)).show(ui, md, &text);
            }
        });
    });
}

/// Read-only view for big text: only visible lines are laid out.
fn big_view(ui: &mut egui::Ui, e: &mut Entry) {
    let key = (e.doc.seq, e.doc.text.len());
    if e.ui.lines.as_ref().map(|l| l.key) != Some(key) {
        let mut starts = vec![0];
        starts.extend(e.doc.text.match_indices('\n').map(|(i, _)| i + 1));
        e.ui.lines = Some(Lines { key, starts });
    }
    let Some(lines) = e.ui.lines.as_ref() else {
        return;
    };
    let text = e.doc.text.as_str();
    let starts = &lines.starts;
    let n = starts.len();
    egui::Frame::new().fill(c::PANEL).inner_margin(Margin::symmetric(14, 5)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(
            RichText::new(format!(
                "{} 넘는 파일이라 가벼운 읽기 전용 보기로 열었어요 · {}줄",
                human_size(EDIT_MAX as u64),
                n
            ))
            .font(font(ty::SMALL))
            .color(c::TEXT_3),
        );
    });
    let ranges = matches_for(&mut e.ui.find, text);
    let current = e.ui.find.current;
    let gutter = gutter_width(n);
    let view_w = ui.available_width();
    let mut area =
        egui::ScrollArea::both().id_salt(("doc-big", e.doc.ws, e.doc.path.as_str())).auto_shrink([false, false]);
    if std::mem::take(&mut e.ui.find.scroll)
        && let Some(r) = ranges.get(current)
    {
        let line = starts.partition_point(|s| *s <= r.start).saturating_sub(1);
        area = area.vertical_scroll_offset((line as f32 * LINE_H - ui.available_height() / 2.0).max(0.0));
    }
    area.show_rows(ui, LINE_H, n, |ui, rows| {
        for i in rows {
            let s = starts[i];
            let end = starts.get(i + 1).map(|x| x - 1).unwrap_or(text.len()).max(s);
            let line = text[s..end].trim_end_matches('\r');
            let mut job = LayoutJob::single_section(line.to_string(), fmt(mono(13.0), c::TEXT));
            let first = ranges.partition_point(|r| r.end <= s);
            let local: Vec<Range<usize>> = ranges[first..]
                .iter()
                .take_while(|r| r.start < s + line.len())
                .map(|r| r.start.saturating_sub(s)..(r.end - s).min(line.len()))
                .collect();
            let cur_local = (current >= first && current < first + local.len()).then(|| current - first);
            mark_ranges(&mut job, &local, cur_local);
            let galley = ui.fonts_mut(|f| f.layout_job(job));
            let w = gutter + GUTTER_GAP + galley.size().x + 14.0;
            let (r, _) = ui.allocate_exact_size(Vec2::new(w.max(view_w), LINE_H), Sense::hover());
            ui.painter().text(
                Pos2::new(r.left() + gutter, r.center().y),
                Align2::RIGHT_CENTER,
                (i + 1).to_string(),
                mono(11.0),
                c::TEXT_OFF,
            );
            let y = r.center().y - galley.size().y / 2.0;
            ui.painter().galley(Pos2::new(r.left() + gutter + GUTTER_GAP, y), galley, c::TEXT);
        }
    });
}

pub fn human_size(bytes: u64) -> String {
    let b = bytes as f64;
    if b >= 1024.0 * 1024.0 {
        format!("{:.1} MB", b / 1024.0 / 1024.0)
    } else if b >= 1024.0 {
        format!("{:.0} KB", b / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

/// Loading, binary, too large, unreadable: the reason, the size, and what can be done.
fn not_text(ui: &mut egui::Ui, e: &Entry, place: &Place, acts: &mut Vec<Act>) {
    let name = runwick_core::api::files::display_path(runwick_core::api::files::file_name(&e.doc.path)).to_string();
    let (glyph, title, detail, retry) = match &e.doc.body {
        Body::Loading if !place.connected => (
            icons::WIFI_OFF,
            format!("{}에 연결되어 있지 않아요", place.where_()),
            "다시 연결되면 파일을 불러와요.".to_string(),
            false,
        ),
        Body::Loading => (icons::LOADER_CIRCLE, "불러오는 중…".to_string(), String::new(), false),
        Body::Binary => (
            icons::FILE,
            "텍스트가 아닌 파일이에요".to_string(),
            format!(
                "{} · 이미지나 압축 파일처럼 텍스트가 아닌 파일은 다른 앱에서 열어 주세요.",
                human_size(e.doc.size)
            ),
            false,
        ),
        Body::TooLarge { limit } => (
            icons::FILE_SEARCH,
            "파일이 너무 커서 열지 않았어요".to_string(),
            format!("{} · 이 앱에서는 {}까지 열어요.", human_size(e.doc.size), human_size(*limit)),
            false,
        ),
        Body::Failed { missing: true, .. } => (
            icons::FILE_SEARCH,
            "파일을 찾을 수 없어요".to_string(),
            "지워졌거나 이름이 바뀌었을 수 있어요. 파일 목록에서 다시 골라 주세요.".to_string(),
            true,
        ),
        Body::Failed { message, .. } => {
            (icons::CIRCLE_ALERT, "파일을 읽지 못했어요".to_string(), message.clone(), true)
        }
        Body::Text => return,
    };
    ui.add_space((ui.available_height() * 0.28).max(24.0));
    ui.vertical_centered(|ui| {
        ui.set_max_width(ui.available_width().min(420.0));
        ui.label(theme::icon(glyph, 26.0, c::TEXT_3));
        ui.add_space(6.0);
        ui.label(RichText::new(&name).font(medium(14.0)).color(c::TEXT_STRONG));
        ui.label(RichText::new(title).font(font(13.0)).color(c::TEXT_2));
        if !detail.is_empty() {
            ui.add(egui::Label::new(RichText::new(detail).font(font(12.0)).color(c::TEXT_3)).wrap());
        }
        ui.add_space(10.0);
        let can_open = !place.remote() && matches!(e.doc.body, Body::Binary | Body::TooLarge { .. });
        ui.horizontal(|ui| {
            let mut w = 0.0;
            if retry {
                w += 90.0;
            }
            if can_open {
                w += 230.0;
            }
            ui.add_space(((ui.available_width() - w) / 2.0).max(0.0));
            if retry && theme::secondary_button(ui, "다시 시도").clicked() {
                acts.push(Act::Retry);
            }
            if can_open {
                if theme::secondary_button(ui, "외부 앱으로 열기").clicked() {
                    acts.push(Act::OpenExternal(runwick_core::api::files::OpenWith::Default));
                }
                if theme::secondary_button(ui, "Finder에서 보기").clicked() {
                    acts.push(Act::Reveal);
                }
            }
        });
        if place.remote() && matches!(e.doc.body, Body::Binary | Body::TooLarge { .. }) {
            ui.label(
                RichText::new(format!("{}에 있는 파일이라 이 컴퓨터의 앱으로는 열 수 없어요.", place.where_()))
                    .font(font(12.0))
                    .color(c::TEXT_3),
            );
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_case_insensitively_with_valid_offsets() {
        let t = "Auth 메모\nauth again AUTH";
        let m = find_all(t, "auth");
        assert_eq!(m.len(), 3);
        for r in &m {
            assert!(t.is_char_boundary(r.start) && t.is_char_boundary(r.end));
        }
        assert_eq!(find_all(t, "메모"), vec![5..11]);
        assert!(find_all(t, "").is_empty());
    }

    #[test]
    fn marks_split_sections_without_losing_text() {
        let mut job = LayoutJob::single_section("hello world".into(), TextFormat::default());
        mark_ranges(&mut job, &[0..5, 6..11], Some(1));
        let covered: usize = job.sections.iter().map(|s| s.byte_range.end.0 - s.byte_range.start.0).sum();
        assert_eq!(covered, 11);
        assert_eq!(job.sections.len(), 3);
        assert_eq!(job.sections[2].format.background, CURRENT_BG);
    }

    #[test]
    fn images_become_text_in_preview() {
        assert_eq!(strip_images("a ![logo](x.png) b"), "a `이미지: logo` b");
        assert_eq!(strip_images("[![b](u)](l)"), "[`이미지: b`](l)");
        assert_eq!(strip_images("no images"), "no images");
        assert_eq!(strip_images("odd ![x"), "odd ![x");
    }

    #[test]
    fn markdown_job_keeps_text_exact() {
        let src = "# 제목\n\n```\ncode\n```\n> quote\n- item";
        let job = md_job(src, false);
        assert_eq!(job.text, src);
    }
}
