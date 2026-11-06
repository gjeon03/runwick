//! Small painted widgets used across the shell (rows, dots, compact buttons, menu items).

use crate::theme::{Tone, c, font, icon_font, medium, reduce_motion, size, ty};
use egui::{Align2, Color32, CornerRadius, Pos2, Rect, Response, RichText, Sense, Stroke, Ui, Vec2};

/// A full-width clickable row with hover and selected grounds.
pub fn row(ui: &mut Ui, height: f32, selected: bool, name: &str) -> Response {
    row_inset(ui, height, selected, name, 0.0)
}

/// Like [`row`], indented from the left by `inset` (nested rows).
pub fn row_inset(ui: &mut Ui, height: f32, selected: bool, name: &str, inset: f32) -> Response {
    let w = ui.available_width();
    let (full, _) = ui.allocate_exact_size(Vec2::new(w, height), Sense::hover());
    let rect = Rect::from_min_max(Pos2::new(full.left() + inset, full.top()), full.max);
    let resp = ui.interact(rect, ui.scope_id().with(("row", name, full.top() as i32)), Sense::click());
    let name = name.to_string();
    resp.widget_info(move || egui::WidgetInfo::selected(egui::Role::ListItem, true, selected, &name));
    let bg = if selected {
        Some(c::CONTROL)
    } else if resp.hovered() {
        Some(c::ROW)
    } else {
        None
    };
    if let Some(bg) = bg {
        ui.painter().rect_filled(rect, CornerRadius::same(6), bg);
    }
    crate::theme::focus_ring(ui, &resp, CornerRadius::same(6));
    resp
}

/// Status dot. Running dots breathe (throttled; still with reduced motion).
pub fn dot(ui: &Ui, center: Pos2, color: Color32, pulse: bool, radius: f32) {
    let mut color = color;
    if pulse && !reduce_motion() {
        let t = ui.input(|i| i.time) as f32;
        let a = 0.55 + 0.45 * (t * std::f32::consts::PI / 1.8).cos().abs();
        color = color.gamma_multiply(a);
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(80));
    }
    ui.painter().circle_filled(center, radius, color);
}

pub fn tone_dot(ui: &Ui, center: Pos2, tone: Tone, pulse: bool) {
    let color = match tone {
        Tone::Neutral => c::TEXT_2,
        Tone::Waiting => c::TEXT_OFF,
        t => t.fg(),
    };
    dot(ui, center, color, pulse, 3.5);
}

/// Compact icon button (`w`×`h`). The tooltip is also the accessible name.
pub fn icon_btn(ui: &mut Ui, glyph: &str, tooltip: &str, w: f32, h: f32, icon_size: f32) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, h), Sense::click());
    let tip = tooltip.to_string();
    resp.widget_info(move || egui::WidgetInfo::labeled(egui::Role::Button, true, &tip));
    let hovered = resp.hovered() || resp.has_focus();
    if hovered {
        ui.painter().rect_filled(rect, CornerRadius::same(6), c::CONTROL);
    }
    let color = if hovered { c::TEXT_STRONG } else { c::TEXT_2 };
    ui.painter().text(rect.center(), Align2::CENTER_CENTER, glyph, icon_font(icon_size), color);
    crate::theme::focus_ring(ui, &resp, CornerRadius::same(6));
    resp.on_hover_text(tooltip)
}

/// Filled pill button used for small secondary actions (`새 CLI`, `기본 공간으로 돌아가기`).
pub fn pill_button(ui: &mut Ui, glyph: Option<&str>, text: &str, h: f32, fill: Color32) -> Response {
    let galley = ui.painter().layout_no_wrap(text.to_string(), font(ty::SMALL), c::TEXT);
    let icon_w = if glyph.is_some() { 17.0 } else { 0.0 };
    let w = galley.size().x + icon_w + 20.0;
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, h), Sense::click());
    paint_pill(ui, rect, &resp, glyph, galley, fill);
    resp
}

/// Pill button that fills the given width (side-by-side pairs).
pub fn wide_pill(ui: &mut Ui, glyph: Option<&str>, text: &str, w: f32, h: f32) -> Response {
    let galley = ui.painter().layout_no_wrap(text.to_string(), font(ty::SMALL), c::TEXT);
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, h), Sense::click());
    paint_pill(ui, rect, &resp, glyph, galley, c::ROW);
    resp
}

fn paint_pill(
    ui: &Ui,
    rect: Rect,
    resp: &Response,
    glyph: Option<&str>,
    galley: std::sync::Arc<egui::Galley>,
    fill: Color32,
) {
    let text = galley.text().to_string();
    resp.widget_info(move || egui::WidgetInfo::labeled(egui::Role::Button, true, &text));
    let bg = if resp.hovered() { c::CONTROL_HOVER } else { fill };
    ui.painter().rect_filled(rect, CornerRadius::same(6), bg);
    let icon_w = if glyph.is_some() { 17.0 } else { 0.0 };
    let total = galley.size().x + icon_w;
    let mut x = rect.center().x - total / 2.0;
    if let Some(g) = glyph {
        ui.painter().text(Pos2::new(x + 6.0, rect.center().y), Align2::CENTER_CENTER, g, icon_font(12.0), c::TEXT);
        x += icon_w;
    }
    let y = rect.center().y - galley.size().y / 2.0;
    ui.painter().galley(Pos2::new(x, y), galley, c::TEXT);
    crate::theme::focus_ring(ui, resp, CornerRadius::same(6));
}

/// Ember count badge (attention), e.g. on a project row.
pub fn count_badge(ui: &Ui, right_center: Pos2, n: usize) -> f32 {
    let text = n.to_string();
    let galley = ui.painter().layout_no_wrap(text, medium(10.5), c::ON_EMBER);
    let w = (galley.size().x + 8.0).max(16.0);
    let rect = Rect::from_center_size(Pos2::new(right_center.x - w / 2.0, right_center.y), Vec2::new(w, 16.0));
    ui.painter().rect_filled(rect, CornerRadius::same(8), c::EMBER);
    ui.painter().galley(rect.center() - galley.size() / 2.0, galley, c::ON_EMBER);
    w
}

/// Small grey device badge (`🖥 집 Mac mini`) next to a remote workspace.
pub fn device_badge(ui: &Ui, left_center: Pos2, label: &str, dim: bool) -> f32 {
    let fg = if dim { c::TEXT_3 } else { c::TEXT };
    let galley = ui.painter().layout_no_wrap(label.to_string(), font(10.5), fg);
    let w = galley.size().x + 10.0 + 13.0;
    let rect = Rect::from_min_size(Pos2::new(left_center.x, left_center.y - 8.5), Vec2::new(w, 17.0));
    ui.painter().rect_filled(rect, CornerRadius::same(4), c::ROW);
    ui.painter().text(
        Pos2::new(rect.left() + 10.0, rect.center().y),
        Align2::CENTER_CENTER,
        crate::icons::MONITOR,
        icon_font(10.0),
        fg,
    );
    ui.painter().galley(Pos2::new(rect.left() + 18.0, rect.center().y - galley.size().y / 2.0), galley, fg);
    w
}

/// Menu entry with a leading icon.
pub fn menu_item(ui: &mut Ui, glyph: &str, text: &str) -> Response {
    menu_item_colored(ui, glyph, text, c::TEXT)
}

pub fn menu_item_danger(ui: &mut Ui, glyph: &str, text: &str) -> Response {
    menu_item_colored(ui, glyph, text, c::ROSE)
}

fn menu_item_colored(ui: &mut Ui, glyph: &str, text: &str, color: Color32) -> Response {
    // Popups start with a very wide available area; keep menus compact like native menus
    // while still fitting longer labels.
    let text_w = ui.fonts_mut(|f| f.layout_no_wrap(text.to_string(), font(ty::BODY), color).size().x);
    let w = ui.available_width().clamp(180.0, 260.0).max(text_w + 44.0);
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, 28.0), Sense::click());
    let t = text.to_string();
    resp.widget_info(move || egui::WidgetInfo::labeled(egui::Role::MenuItem, true, &t));
    if resp.hovered() || resp.has_focus() {
        ui.painter().rect_filled(rect, CornerRadius::same(size::RADIUS_SM), c::CONTROL);
    }
    ui.painter().text(
        Pos2::new(rect.left() + 14.0, rect.center().y),
        Align2::CENTER_CENTER,
        glyph,
        icon_font(13.0),
        color,
    );
    ui.painter().text(Pos2::new(rect.left() + 28.0, rect.center().y), Align2::LEFT_CENTER, text, font(ty::BODY), color);
    if resp.clicked() {
        ui.close();
    }
    resp
}

/// Thin separator for menus.
pub fn menu_sep(ui: &mut Ui) {
    let w = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(Vec2::new(w, 7.0), Sense::hover());
    ui.painter().hline(rect.x_range(), rect.center().y, Stroke::new(1.0, c::LINE_STRONG));
}

/// Paint a single-line text clipped with an ellipsis into `rect`. Returns whether it was cut.
pub fn paint_clipped(
    ui: &Ui,
    rect: Rect,
    text: &str,
    font: egui::FontId,
    color: Color32,
    align_center_y: bool,
) -> bool {
    let mut job = egui::text::LayoutJob::simple_singleline(text.to_string(), font, color);
    job.wrap = egui::text::TextWrapping::truncate_at_width(rect.width().max(0.0));
    let galley = ui.painter().layout_job(job);
    let cut = galley.elided;
    let y = if align_center_y { rect.center().y - galley.size().y / 2.0 } else { rect.top() };
    ui.painter().with_clip_rect(rect.expand2(Vec2::new(0.0, 4.0))).galley(Pos2::new(rect.left(), y), galley, color);
    cut
}

/// Text width in points for a given font.
pub fn text_width(ui: &Ui, text: &str, font: egui::FontId) -> f32 {
    ui.painter().layout_no_wrap(text.to_string(), font, Color32::WHITE).size().x
}

/// Muted helper text block used for empty states inside lists.
pub fn empty_note(ui: &mut Ui, text: &str) {
    egui::Frame::new().inner_margin(egui::Margin::symmetric(8, 8)).show(ui, |ui| {
        ui.add(egui::Label::new(RichText::new(text).font(font(ty::SMALL)).color(c::TEXT_3)).wrap());
    });
}
