//! Timeline (S09): observed records per day without AI, AI day summaries marked as such, and
//! the resume card of a project (last work, own memo, remaining items, handover to a new
//! conversation).
//!
//! - `home`: all projects — week grid and day cards
//! - `project`: one project — resume card and its days
//! - `handover`: compose the first message of a new conversation from chosen material

mod handover;
mod home;
mod jobs;
mod project;

use super::history::fmt;
use crate::App;
use crate::client::HostKey;
use crate::icons;
use crate::state::{RightPanel, TabId, TabKind, WsKey};
use crate::theme::{c, font, icon_font, mono};
use chrono::NaiveDate;
use egui::{Align2, CornerRadius, Pos2, Rect, Sense, Vec2};
use runwick_core::api::projects::ProjectId;
use runwick_core::api::records::PinId;
use runwick_core::api::timeline::{Event, EventKind, EventLink};
use std::collections::{BTreeSet, HashSet};

#[derive(Default)]
pub struct State {
    /// Pins chosen for the next handover ("인계에 담기" in the pins list).
    pub handover_pins: BTreeSet<PinId>,
    /// Monday of the week shown on the home screen (None: this week).
    week: Option<NaiveDate>,
    /// Day expanded under the grid (None: today).
    day: Option<NaiveDate>,
    /// Kinds hidden by the filter chips.
    kinds_off: HashSet<EventKind>,
    hide_summaries: bool,
    /// Days shown in a project timeline (0: 7).
    span: i64,
    note: project::NoteState,
    handover: Option<handover::Draft>,
}

pub fn show_tab(app: &mut App, ui: &mut egui::Ui, project: Option<ProjectId>, tab: TabId) {
    let rect = ui.max_rect();
    ui.painter().rect_filled(rect, CornerRadius::ZERO, c::DOC);
    match project {
        None => home::show(app, ui, tab),
        Some(p) => project::show(app, ui, p, tab),
    }
}

/// Switch this tab between all projects and one project.
fn retarget(app: &mut App, tab: TabId, project: Option<ProjectId>) {
    for set in app.ui.tabsets.values_mut() {
        if let Some(t) = set.tabs.iter_mut().find(|t| t.id == tab) {
            t.kind = TabKind::Timeline { project };
        }
    }
}

pub fn kind_icon(k: EventKind) -> &'static str {
    match k {
        EventKind::Session => icons::TERMINAL,
        EventKind::Messages => icons::MESSAGES_SQUARE,
        EventKind::Pin => icons::PIN,
        EventKind::Commit => icons::GIT_COMMIT_HORIZONTAL,
        EventKind::File => icons::FILE_TEXT,
        EventKind::Wiki => icons::BOOK_OPEN,
        EventKind::Collab => icons::USERS,
    }
}

/// One timeline row: time, kind icon, title (and the kind in words when `kind_label`).
fn event_row(app: &mut App, ui: &mut egui::Ui, ev: &Event, project: ProjectId, kind_label: bool) {
    let w = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, 30.0), Sense::click());
    let name = format!("{} {} {}", fmt::hm(ev.at), ev.kind.label(), ev.title);
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Link, true, &name));
    if resp.hovered() {
        ui.painter().rect_filled(rect, CornerRadius::same(6), c::ROW);
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let cy = rect.center().y;
    let p = ui.painter();
    p.text(Pos2::new(rect.left() + 6.0, cy), Align2::LEFT_CENTER, fmt::hm(ev.at), mono(11.0), c::TEXT_3);
    p.text(Pos2::new(rect.left() + 56.0, cy), Align2::CENTER_CENTER, kind_icon(ev.kind), icon_font(13.0), c::TEXT_2);
    let mut right = rect.right() - 6.0;
    if kind_label {
        let lw = crate::shell::widgets::text_width(ui, ev.kind.label(), font(11.5));
        ui.painter().text(Pos2::new(right, cy), Align2::RIGHT_CENTER, ev.kind.label(), font(11.5), c::TEXT_3);
        right -= lw + 10.0;
    }
    let text_rect = Rect::from_min_max(Pos2::new(rect.left() + 70.0, rect.top()), Pos2::new(right, rect.bottom()));
    let cut = crate::shell::widgets::paint_clipped(ui, text_rect, &ev.title, font(13.0), c::TEXT_STRONG, true);
    crate::theme::focus_ring(ui, &resp, CornerRadius::same(6));
    let resp = if cut { resp.on_hover_text(&ev.title) } else { resp };
    let resp = match &ev.link {
        Some(EventLink::Commit { sha }) => resp.on_hover_text(format!("커밋 {sha} · 눌러서 해시 복사")),
        _ => resp,
    };
    if resp.clicked() {
        follow(app, ui.ctx(), ev, project);
    }
}

/// Go where a row leads (records, pins, documents, results).
fn follow(app: &mut App, ctx: &egui::Context, ev: &Event, project: ProjectId) {
    let host = HostKey::Local;
    match &ev.link {
        Some(EventLink::Session { id }) => super::history::open_session_records(app, host, *id),
        Some(EventLink::Message { id }) => super::history::open_message(app, host, *id, Vec::new()),
        Some(EventLink::Pin { id }) => {
            let h = &mut app.feat.history;
            h.mode = super::history::Mode::Pins;
            h.scope = super::history::ScopeKind::Project;
            h.highlight_pin = Some(*id);
            app.ui.right = Some(RightPanel::History);
        }
        Some(EventLink::Commit { sha }) => {
            ctx.copy_text(sha.clone());
            app.toast(crate::state::ToastKind::Success, "커밋 해시를 복사했어요");
        }
        Some(EventLink::File { workspace_id: Some(w), path }) => {
            let ws = WsKey { host, id: *w };
            crate::shell::actions::select_workspace(app, project, ws);
            app.ui.open_tab(ws, TabKind::Doc { ws, path: path.clone() });
        }
        Some(EventLink::File { workspace_id: None, path }) => app.toast_info(&format!("파일: {path}")),
        Some(EventLink::Wiki { page }) => {
            crate::shell::actions::open_center_tab(app, TabKind::Wiki { project, page: Some(page.clone()) })
        }
        Some(EventLink::Collab { request, .. }) => {
            crate::shell::actions::open_center_tab(app, TabKind::Result { host, request: *request })
        }
        None => {}
    }
}

/// Filter chips for the record kinds.
fn kind_chips(app: &mut App, ui: &mut egui::Ui) {
    for k in EventKind::ALL {
        let on = !app.feat.timeline.kinds_off.contains(&k);
        if fmt::chip_button(ui, k.label(), on, false)
            .on_hover_text(if on { "눌러서 숨기기" } else { "눌러서 보이기" })
            .clicked()
        {
            if on {
                app.feat.timeline.kinds_off.insert(k);
            } else {
                app.feat.timeline.kinds_off.remove(&k);
            }
        }
    }
}

/// "AI 요약 함께 보기" switch.
fn summary_switch(app: &mut App, ui: &mut egui::Ui) {
    let on = !app.feat.timeline.hide_summaries;
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(30.0 + 8.0 + 92.0, 28.0), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::Switch, true, on, "AI 요약 함께 보기"));
    let track = Rect::from_min_size(Pos2::new(rect.left(), rect.center().y - 9.0), Vec2::new(30.0, 18.0));
    ui.painter().rect_filled(track, CornerRadius::same(9), if on { c::EMBER } else { c::CONTROL_HOVER });
    let knob_x = if on { track.right() - 9.0 } else { track.left() + 9.0 };
    ui.painter().circle_filled(Pos2::new(knob_x, track.center().y), 7.0, if on { c::ON_EMBER } else { c::TEXT_2 });
    ui.painter().text(
        Pos2::new(track.right() + 8.0, rect.center().y),
        Align2::LEFT_CENTER,
        "AI 요약 함께 보기",
        font(12.0),
        c::TEXT,
    );
    crate::theme::focus_ring(ui, &resp, CornerRadius::same(9));
    if resp.clicked() {
        app.feat.timeline.hide_summaries = on;
    }
}

/// Blue AI summary box: generation time and basis, then the text.
fn summary_box(ui: &mut egui::Ui, s: &runwick_core::api::timeline::Summary) {
    egui::Frame::new()
        .fill(c::SKY_BG)
        .corner_radius(CornerRadius::same(8))
        .inner_margin(egui::Margin::symmetric(10, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 3.0;
            let mut head = format!("AI 요약 · {} 생성 · 기록 {}건 기준", fmt::when_full(s.generated_at), s.basis_count);
            if let Some(p) = &s.provider {
                head.push_str(&format!(" · {p}"));
            }
            ui.label(egui::RichText::new(head).font(crate::theme::medium(11.0)).color(c::SKY));
            ui.add(
                egui::Label::new(egui::RichText::new(&s.text).font(font(12.5)).color(c::SKY.gamma_multiply(1.0)))
                    .wrap(),
            );
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_has_an_icon_and_label() {
        let icons: HashSet<&str> = EventKind::ALL.iter().map(|k| kind_icon(*k)).collect();
        assert_eq!(icons.len(), EventKind::ALL.len());
        assert!(EventKind::ALL.iter().all(|k| !k.label().is_empty()));
    }
}
