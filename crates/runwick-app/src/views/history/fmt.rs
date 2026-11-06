//! Formatting shared by the records and timeline views: times in Korean, highlighted text,
//! and small chips.

use crate::theme::{c, font, icon_font, medium, ty};
use chrono::{Datelike, Local, NaiveDate};
use egui::text::{LayoutJob, TextFormat};
use egui::{Color32, CornerRadius, FontId, Response, Sense, Ui, Vec2};
use runwick_core::Ts;

pub fn local(ts: Ts) -> chrono::DateTime<Local> {
    ts.with_timezone(&Local)
}

pub fn today() -> NaiveDate {
    Local::now().date_naive()
}

/// The viewer's offset from UTC in minutes (days are cut in this time zone).
pub fn utc_offset_min() -> i32 {
    Local::now().offset().local_minus_utc() / 60
}

/// "오늘", "어제", "11월 5일" or "2025년 3월 2일".
pub fn day_label(d: NaiveDate) -> String {
    let t = today();
    if d == t {
        "오늘".into()
    } else if Some(d) == t.pred_opt() {
        "어제".into()
    } else if d.year() == t.year() {
        format!("{}월 {}일", d.month(), d.day())
    } else {
        format!("{}년 {}월 {}일", d.year(), d.month(), d.day())
    }
}

pub fn weekday(d: NaiveDate) -> &'static str {
    ["월", "화", "수", "목", "금", "토", "일"][d.weekday().num_days_from_monday() as usize]
}

/// "오늘 11:04", "어제 18:42", "10월 28일" (older days without the time).
pub fn when(ts: Ts) -> String {
    let l = local(ts);
    let d = l.date_naive();
    let t = today();
    if d == t || Some(d) == t.pred_opt() { format!("{} {}", day_label(d), l.format("%H:%M")) } else { day_label(d) }
}

/// "오늘 11:04" style with the time always shown.
pub fn when_full(ts: Ts) -> String {
    let l = local(ts);
    format!("{} {}", day_label(l.date_naive()), l.format("%H:%M"))
}

/// "2026년 11월 6일 11:04": a date that still reads right later (for text that is kept).
pub fn date_time(ts: Ts) -> String {
    let l = local(ts);
    format!("{}년 {}월 {}일 {}", l.year(), l.month(), l.day(), l.format("%H:%M"))
}

pub fn hm(ts: Ts) -> String {
    local(ts).format("%H:%M").to_string()
}

/// Background of a search match (ember, lifted for contrast on dark rows).
pub fn mark_bg() -> Color32 {
    c::EMBER.gamma_multiply(0.32)
}

/// Text with byte ranges highlighted.
pub fn highlighted(text: &str, ranges: &[(u32, u32)], font: FontId, color: Color32) -> LayoutJob {
    let mut job = LayoutJob::default();
    let plain = TextFormat { font_id: font.clone(), color, ..Default::default() };
    let mark = TextFormat { font_id: font, color: c::TEXT_STRONG, background: mark_bg(), ..Default::default() };
    let mut at = 0usize;
    for &(a, b) in ranges {
        let (a, b) = (a as usize, b as usize);
        if a < at || b > text.len() || !text.is_char_boundary(a) || !text.is_char_boundary(b) {
            continue;
        }
        job.append(&text[at..a], 0.0, plain.clone());
        job.append(&text[a..b], 0.0, mark.clone());
        at = b;
    }
    job.append(&text[at..], 0.0, plain);
    job
}

/// Byte ranges of `terms` in `text` (case-insensitive), for highlighting a whole message.
pub fn find_terms(text: &str, terms: &[String]) -> Vec<(u32, u32)> {
    let folded: Vec<(usize, char)> =
        text.char_indices().map(|(i, ch)| (i, ch.to_lowercase().next().unwrap_or(ch))).collect();
    let mut out: Vec<(u32, u32)> = Vec::new();
    for t in terms {
        let n: Vec<char> = t.chars().map(|ch| ch.to_lowercase().next().unwrap_or(ch)).collect();
        if n.is_empty() || n.len() > folded.len() {
            continue;
        }
        let mut i = 0;
        while i + n.len() <= folded.len() {
            if folded[i..i + n.len()].iter().map(|(_, ch)| *ch).eq(n.iter().copied()) {
                let end = folded.get(i + n.len()).map(|(b, _)| *b).unwrap_or(text.len());
                out.push((folded[i].0 as u32, end as u32));
                i += n.len();
            } else {
                i += 1;
            }
        }
    }
    out.sort();
    let mut merged: Vec<(u32, u32)> = Vec::new();
    for (a, b) in out {
        match merged.last_mut() {
            Some(l) if a <= l.1 => l.1 = l.1.max(b),
            _ => merged.push((a, b)),
        }
    }
    merged
}

/// Words of a query the way the host splits them (whitespace; `"quoted"` kept together).
pub fn query_terms(q: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = q.trim();
    while !rest.is_empty() {
        let (term, next) = if let Some(r) = rest.strip_prefix('"') {
            match r.find('"') {
                Some(e) => (&r[..e], &r[e + 1..]),
                None => (r, ""),
            }
        } else {
            match rest.find(char::is_whitespace) {
                Some(e) => (&rest[..e], &rest[e..]),
                None => (rest, ""),
            }
        };
        if !term.trim().is_empty() {
            out.push(term.trim().to_string());
        }
        rest = next.trim_start();
    }
    out
}

/// Filter chip like `날짜: 전체 ▾` or a toggle (`핀만 보기`). `on` draws it as selected.
pub fn chip_button(ui: &mut Ui, text: &str, on: bool, dropdown: bool) -> Response {
    let f = font(ty::SMALL);
    let galley = ui.painter().layout_no_wrap(text.to_string(), f, c::TEXT);
    let extra = if dropdown { 14.0 } else { 0.0 };
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(galley.size().x + 16.0 + extra, 24.0), Sense::click());
    let label = text.to_string();
    resp.widget_info(move || egui::WidgetInfo::selected(egui::Role::Button, true, on, &label));
    let bg = if on {
        c::CONTROL_HOVER
    } else if resp.hovered() {
        c::CONTROL
    } else {
        c::ROW
    };
    ui.painter().rect_filled(rect, CornerRadius::same(6), bg);
    let fg = if on { c::TEXT_STRONG } else { c::TEXT };
    ui.painter().galley(egui::pos2(rect.left() + 8.0, rect.center().y - galley.size().y / 2.0), galley, fg);
    if dropdown {
        ui.painter().text(
            egui::pos2(rect.right() - 11.0, rect.center().y),
            egui::Align2::CENTER_CENTER,
            crate::icons::CHEVRON_DOWN,
            icon_font(11.0),
            c::TEXT_3,
        );
    }
    crate::theme::focus_ring(ui, &resp, CornerRadius::same(6));
    resp
}

/// Small text button with a grey ground (`원문`, `핀 해제`).
pub fn small_button(ui: &mut Ui, text: &str) -> Response {
    let galley = ui.painter().layout_no_wrap(text.to_string(), font(11.5), c::TEXT_STRONG);
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(galley.size().x + 16.0, 24.0), Sense::click());
    let label = text.to_string();
    resp.widget_info(move || egui::WidgetInfo::labeled(egui::Role::Button, true, &label));
    let bg = if resp.hovered() { c::LINE_STRONG.gamma_multiply(1.3) } else { c::CONTROL_HOVER };
    ui.painter().rect_filled(rect, CornerRadius::same(6), bg);
    ui.painter().galley(rect.center() - galley.size() / 2.0, galley, c::TEXT_STRONG);
    crate::theme::focus_ring(ui, &resp, CornerRadius::same(6));
    resp
}

/// A muted link-like text button ("이전 메시지 더 읽기").
pub fn quiet_link(ui: &mut Ui, text: &str) -> Response {
    let resp = ui.add(egui::Button::new(egui::RichText::new(text).font(font(ty::SMALL)).color(c::TEXT_3)).frame(false));
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

/// Grey badge with a device name (results from another computer).
pub fn device_badge(ui: &mut Ui, label: &str, stale: bool) -> Response {
    let fg = if stale { c::YELLOW } else { c::TEXT };
    egui::Frame::new()
        .fill(c::CONTROL)
        .corner_radius(CornerRadius::same(4))
        .inner_margin(egui::Margin::symmetric(5, 1))
        .show(ui, |ui| ui.label(egui::RichText::new(label).font(font(10.5)).color(fg)))
        .response
}

/// Section label inside a card ("내 메모").
pub fn label(ui: &mut Ui, text: &str) -> Response {
    ui.label(egui::RichText::new(text).font(medium(12.0)).color(c::TEXT_3))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terms_and_ranges() {
        assert_eq!(query_terms("\"만료 시각\" Retry"), vec!["만료 시각", "Retry"]);
        let text = "Retry 간격과 retry 상한, 만료 시각";
        let r = find_terms(text, &query_terms("retry \"만료 시각\""));
        let words: Vec<&str> = r.iter().map(|(a, b)| &text[*a as usize..*b as usize]).collect();
        assert_eq!(words, vec!["Retry", "retry", "만료 시각"]);
        let job = highlighted(text, &r, font(12.0), c::TEXT);
        assert_eq!(job.text, text);
        // Broken ranges are ignored instead of panicking.
        let job = highlighted("가나", &[(1, 2)], font(12.0), c::TEXT);
        assert_eq!(job.text, "가나");
    }
}
