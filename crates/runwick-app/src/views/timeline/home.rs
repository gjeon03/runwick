//! All projects: a week grid (project × weekday, observed record counts, AI summary dot) and
//! the chosen day's cards.

use super::{event_row, fmt, kind_chips, retarget, summary_box, summary_switch};
use crate::App;
use crate::client::HostKey;
use crate::icons;
use crate::state::TabId;
use crate::theme::{self, Seg, c, font, medium, mono, semibold, ty};
use chrono::{Datelike, Duration, NaiveDate};
use egui::{Align2, CornerRadius, Margin, Pos2, Rect, RichText, Sense, Stroke, Vec2};
use runwick_core::api::jobs::AutomationList;
use runwick_core::api::timeline::{ProjectDay, Range, RangeQuery, TimelineRange};

fn monday(d: NaiveDate) -> NaiveDate {
    d - Duration::days(d.weekday().num_days_from_monday() as i64)
}

fn shade(n: u32) -> egui::Color32 {
    match n {
        0 => egui::Color32::TRANSPARENT,
        1..=5 => c::ROW,
        6..=15 => c::CONTROL,
        16..=30 => c::CONTROL_HOVER,
        _ => c::TEXT_OFF.gamma_multiply(0.6),
    }
}

pub fn show(app: &mut App, ui: &mut egui::Ui, tab: TabId) {
    let today = fmt::today();
    let week = app.feat.timeline.week.unwrap_or_else(|| monday(today));
    let day = app
        .feat
        .timeline
        .day
        .filter(|d| *d >= week && *d < week + Duration::days(7))
        .unwrap_or(if week == monday(today) { today } else { week });
    let q = RangeQuery {
        from: week,
        to: week + Duration::days(6),
        project_id: None,
        utc_offset_min: fmt::utc_offset_min(),
    };
    let res = app.hub.query::<TimelineRange>(HostKey::Local, &q);
    egui::ScrollArea::vertical().id_salt(("timeline-home", tab)).auto_shrink([false, false]).show(ui, |ui| {
        let w = ui.available_width();
        let col = (w - 56.0).clamp(300.0, 1120.0);
        let left = ((w - col) / 2.0).max(16.0);
        egui::Frame::new().inner_margin(Margin { left: left as i8, right: 0, top: 22, bottom: 28 }).show(ui, |ui| {
            ui.set_max_width(col);
            ui.set_min_width(col);
            ui.spacing_mut().item_spacing.y = 16.0;
            header(app, ui, week, today);
            controls(app, ui, tab);
            match (&res.data, &res.error) {
                (Some(range), _) => {
                    grid(app, ui, range, week, day, today);
                    day_section(app, ui, range, day, today, tab);
                }
                (None, Some(e)) => {
                    theme::chip(ui, theme::Tone::Recover, "타임라인을 읽지 못했어요");
                    ui.label(RichText::new(&e.message).font(font(ty::SMALL)).color(c::TEXT_2));
                }
                (None, None) => {
                    ui.label(RichText::new("기록을 모으는 중…").font(font(ty::BODY)).color(c::TEXT_3));
                }
            }
        });
    });
}

fn header(app: &mut App, ui: &mut egui::Ui, week: NaiveDate, today: NaiveDate) {
    ui.horizontal_wrapped(|ui| {
        ui.vertical(|ui| {
            ui.set_max_width((ui.available_width() - 260.0).max(280.0));
            ui.label(RichText::new("타임라인").font(semibold(ty::TITLE)).color(c::TEXT_STRONG));
            ui.add(
                egui::Label::new(
                    RichText::new("세션, 핀, 커밋, 위키, 협업 기록을 날짜별로 모아요. AI 요약이 없어도 실제 기록은 그대로 보여요.")
                        .font(font(ty::BODY))
                        .color(c::TEXT_2),
                )
                .wrap(),
            );
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            let this_week = monday(today);
            if ui.add_enabled(week != this_week, egui::Button::new(RichText::new("오늘").font(font(12.5)))).clicked() {
                app.feat.timeline.week = None;
                app.feat.timeline.day = None;
            }
            let next = ui.add_enabled(week < this_week, egui::Button::new(theme::icon(icons::CHEVRON_RIGHT, 13.0, c::TEXT)));
            if next.on_hover_text("다음 주").clicked() {
                app.feat.timeline.week = Some(week + Duration::days(7));
                app.feat.timeline.day = None;
            }
            let end = week + Duration::days(6);
            let label = if week.month() == end.month() {
                format!("{}월 {}일 – {}일", week.month(), week.day(), end.day())
            } else {
                format!("{}월 {}일 – {}월 {}일", week.month(), week.day(), end.month(), end.day())
            };
            ui.add_sized(Vec2::new(124.0, 30.0), egui::Label::new(RichText::new(label).font(medium(13.0)).color(c::TEXT_STRONG)));
            if ui.button(theme::icon(icons::CHEVRON_LEFT, 13.0, c::TEXT)).on_hover_text("지난 주").clicked() {
                app.feat.timeline.week = Some(week - Duration::days(7));
                app.feat.timeline.day = None;
            }
        });
    });
}

fn controls(app: &mut App, ui: &mut egui::Ui, tab: TabId) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = Vec2::new(6.0, 8.0);
        if let Some(p) = app.ui.selected_project {
            let name = crate::shell::data::projects(app)
                .data
                .and_then(|l| crate::shell::data::project_of(&l, p).map(|x| x.name.clone()))
                .unwrap_or_else(|| "선택한 프로젝트".into());
            let short: String = name.chars().take(24).collect();
            if theme::segmented(ui, &[Seg::text("모든 프로젝트"), Seg::text(&short)], 0, 26.0) == Some(1) {
                retarget(app, tab, Some(p));
            }
            ui.add_space(8.0);
        } else {
            theme::segmented(ui, &[Seg::text("모든 프로젝트")], 0, 26.0);
        }
        kind_chips(app, ui);
        ui.add_space(6.0);
        summary_switch(app, ui);
    });
}

fn grid(app: &mut App, ui: &mut egui::Ui, range: &Range, week: NaiveDate, day: NaiveDate, today: NaiveDate) {
    let auto = app.hub.query::<AutomationList>(HostKey::Local, &()).data;
    let show_sum = !app.feat.timeline.hide_summaries;
    egui::Frame::new()
        .fill(c::PANEL)
        .corner_radius(CornerRadius::same(12))
        .inner_margin(Margin { left: 12, right: 12, top: 10, bottom: 12 })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            let w = ui.available_width();
            let name_w = (w * 0.24).clamp(140.0, 230.0);
            let cell_w = ((w - name_w - 4.0 * 7.0) / 7.0).max(36.0);
            // Heads.
            let (head, _) = ui.allocate_exact_size(Vec2::new(w, 40.0), Sense::hover());
            ui.painter().text(
                Pos2::new(head.left() + 6.0, head.center().y),
                Align2::LEFT_CENTER,
                "프로젝트",
                font(12.0),
                c::TEXT_3,
            );
            for i in 0..7 {
                let d = week + Duration::days(i);
                let r = Rect::from_min_size(
                    Pos2::new(head.left() + name_w + 4.0 + i as f32 * (cell_w + 4.0), head.top()),
                    Vec2::new(cell_w, 40.0),
                );
                let future = d > today;
                let resp = ui.interact(
                    r,
                    ui.scope_id().with(("head", d)),
                    if future { Sense::hover() } else { Sense::click() },
                );
                let label = format!(
                    "{}월 {}일 {}요일{}",
                    d.month(),
                    d.day(),
                    fmt::weekday(d),
                    if future { ", 아직 오지 않은 날" } else { "" }
                );
                resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, !future, d == day, &label));
                if d == day {
                    ui.painter().rect_filled(r, CornerRadius::same(7), c::CONTROL_HOVER);
                } else if resp.hovered() && !future {
                    ui.painter().rect_filled(r, CornerRadius::same(7), c::ROW);
                }
                let fg = if future {
                    c::TEXT_OFF
                } else if d == day {
                    c::TEXT_STRONG
                } else {
                    c::TEXT_2
                };
                ui.painter().text(
                    Pos2::new(r.center().x, r.center().y - 6.0),
                    Align2::CENTER_CENTER,
                    format!("{} {}", fmt::weekday(d), d.day()),
                    medium(12.0),
                    fg,
                );
                if d == today {
                    ui.painter().text(
                        Pos2::new(r.center().x, r.center().y + 9.0),
                        Align2::CENTER_CENTER,
                        "오늘",
                        font(10.5),
                        c::TEXT_3,
                    );
                }
                if resp.clicked() {
                    app.feat.timeline.day = Some(d);
                }
            }
            if range.projects.is_empty() {
                ui.add_space(6.0);
                ui.label(RichText::new("이 주에는 남은 기록이 없어요.").font(font(ty::SMALL)).color(c::TEXT_3));
            }
            for p in &range.projects {
                let (row, _) = ui.allocate_exact_size(Vec2::new(w, 38.0), Sense::hover());
                let nr = Rect::from_min_size(row.min, Vec2::new(name_w, 38.0));
                let nresp = ui.interact(nr, ui.scope_id().with(("proj", p.project_id)), Sense::click());
                nresp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Link, true, &p.name));
                if nresp.hovered() {
                    ui.painter().rect_filled(nr, CornerRadius::same(6), c::ROW);
                }
                let sub = auto
                    .as_ref()
                    .and_then(|l| l.iter().find(|a| a.project_id == p.project_id))
                    .map(|a| if a.timeline { "AI 요약 켜짐" } else { "AI 요약 꺼짐" })
                    .or((!p.root_exists).then_some("폴더 없음"));
                let name_rect = Rect::from_min_max(
                    Pos2::new(nr.left() + 6.0, nr.top() + if sub.is_some() { 3.0 } else { 0.0 }),
                    Pos2::new(nr.right() - 4.0, if sub.is_some() { nr.center().y + 2.0 } else { nr.bottom() }),
                );
                let cut =
                    crate::shell::widgets::paint_clipped(ui, name_rect, &p.name, medium(13.0), c::TEXT_STRONG, true);
                if let Some(s) = sub {
                    ui.painter().text(
                        Pos2::new(nr.left() + 6.0, nr.bottom() - 9.0),
                        Align2::LEFT_CENTER,
                        s,
                        font(11.5),
                        c::TEXT_3,
                    );
                }
                let nresp = if cut { nresp.on_hover_text(&p.name) } else { nresp };
                if nresp.clicked() {
                    app.feat.timeline.day = None;
                    crate::shell::actions::open_center_tab(
                        app,
                        crate::state::TabKind::Timeline { project: Some(p.project_id) },
                    );
                }
                for i in 0..7 {
                    let d = week + Duration::days(i);
                    let r = Rect::from_min_size(
                        Pos2::new(row.left() + name_w + 4.0 + i as f32 * (cell_w + 4.0), row.top()),
                        Vec2::new(cell_w, 38.0),
                    );
                    let pd = range.days.iter().find(|x| x.project_id == p.project_id && x.day == d);
                    cell(app, ui, r, &p.name, d, pd, d > today, d == day, show_sum);
                }
            }
            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 14.0;
                ui.label(RichText::new("숫자는 그날 실제로 남은 기록 수예요.").font(font(12.0)).color(c::TEXT_3));
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    let (r, _) = ui.allocate_exact_size(Vec2::splat(6.0), Sense::hover());
                    ui.painter().circle_filled(r.center(), 2.5, c::SKY);
                    ui.label(RichText::new("AI 요약 있음").font(font(12.0)).color(c::TEXT_3));
                });
                ui.label(
                    RichText::new("칸이나 요일을 누르면 아래에 그날 기록을 펼쳐요.").font(font(12.0)).color(c::TEXT_3),
                );
            });
        });
}

#[allow(clippy::too_many_arguments)]
fn cell(
    app: &mut App,
    ui: &mut egui::Ui,
    r: Rect,
    name: &str,
    d: NaiveDate,
    pd: Option<&ProjectDay>,
    future: bool,
    on: bool,
    show_sum: bool,
) {
    let n = pd.map(|p| p.count).unwrap_or(0);
    let has_sum = show_sum && pd.is_some_and(|p| p.summary.is_some());
    let resp =
        ui.interact(r, ui.scope_id().with(("cell", name, d)), if future { Sense::hover() } else { Sense::click() });
    let label =
        format!("{name} {}월 {}일 기록 {n}건{}", d.month(), d.day(), if has_sum { ", AI 요약 있음" } else { "" });
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, !future, &label));
    let p = ui.painter();
    if future {
        p.rect_stroke(r, CornerRadius::same(6), Stroke::new(1.0, c::ROW), egui::StrokeKind::Inside);
        return;
    }
    p.rect_filled(r, CornerRadius::same(6), shade(n));
    if on {
        p.rect_stroke(r, CornerRadius::same(6), Stroke::new(1.0, c::TEXT_3), egui::StrokeKind::Inside);
    } else if resp.hovered() {
        p.rect_stroke(r, CornerRadius::same(6), Stroke::new(1.0, c::LINE_STRONG), egui::StrokeKind::Inside);
    }
    let (txt, fg) = if n == 0 {
        ("·".to_string(), c::TEXT_OFF)
    } else {
        (n.to_string(), if n > 30 { c::TEXT_STRONG } else { c::TEXT })
    };
    p.text(r.center(), Align2::CENTER_CENTER, txt, mono(12.0), fg);
    if has_sum {
        p.circle_filled(Pos2::new(r.right() - 8.0, r.top() + 8.0), 2.5, c::SKY);
    }
    theme::focus_ring(ui, &resp, CornerRadius::same(6));
    if resp.clicked() {
        app.feat.timeline.day = Some(d);
    }
}

fn day_section(app: &mut App, ui: &mut egui::Ui, range: &Range, day: NaiveDate, today: NaiveDate, tab: TabId) {
    let cards: Vec<&ProjectDay> =
        range.days.iter().filter(|d| d.day == day && (d.count > 0 || d.summary.is_some())).collect();
    let total: u32 = cards.iter().map(|d| d.count).sum();
    ui.horizontal_wrapped(|ui| {
        let title = format!(
            "{}월 {}일 {}요일{}",
            day.month(),
            day.day(),
            fmt::weekday(day),
            if day == today { " · 오늘" } else { "" }
        );
        ui.label(RichText::new(title).font(semibold(16.0)).color(c::TEXT_STRONG));
        ui.label(
            RichText::new(format!("프로젝트 {}곳 · 기록 {total}건", cards.len())).font(font(12.5)).color(c::TEXT_3),
        );
    });
    if cards.is_empty() {
        ui.label(RichText::new("이 날은 남은 기록이 없어요.").font(font(ty::BODY)).color(c::TEXT_3));
        return;
    }
    let w = ui.available_width();
    let cols = ((w + 12.0) / (320.0 + 12.0)).floor().max(1.0) as usize;
    let card_w = (w - 12.0 * (cols as f32 - 1.0)) / cols as f32;
    for chunk in cards.chunks(cols) {
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 12.0;
            for pd in chunk {
                let name = range
                    .projects
                    .iter()
                    .find(|p| p.project_id == pd.project_id)
                    .map(|p| p.name.clone())
                    .unwrap_or_default();
                ui.allocate_ui_with_layout(Vec2::new(card_w, 0.0), egui::Layout::top_down(egui::Align::Min), |ui| {
                    ui.set_width(card_w);
                    day_card(app, ui, pd, &name, today, tab);
                });
            }
        });
    }
}

fn day_card(app: &mut App, ui: &mut egui::Ui, pd: &ProjectDay, name: &str, today: NaiveDate, _tab: TabId) {
    let show_sum = !app.feat.timeline.hide_summaries;
    let off = app.feat.timeline.kinds_off.clone();
    egui::Frame::new().fill(c::PANEL).corner_radius(CornerRadius::same(12)).inner_margin(Margin::same(14)).show(
        ui,
        |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 10.0;
            ui.horizontal(|ui| {
                let count = format!("기록 {}건", pd.count);
                let cw = crate::shell::widgets::text_width(ui, &count, font(12.0));
                ui.allocate_ui(Vec2::new(ui.available_width() - cw - 8.0, 20.0), |ui| {
                    theme::truncated(ui, name.to_string(), semibold(13.0), c::TEXT_STRONG);
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(RichText::new(count).font(font(12.0)).color(c::TEXT_3));
                });
            });
            if show_sum {
                match &pd.summary {
                    Some(s) => summary_box(ui, s),
                    None => super::jobs::no_summary(app, ui, pd.project_id, pd.day, today),
                }
            }
            let shown: Vec<_> = pd.events.iter().filter(|e| !off.contains(&e.kind)).take(4).collect();
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for e in &shown {
                    event_row(app, ui, e, pd.project_id, false);
                }
                if shown.is_empty() {
                    ui.label(RichText::new("선택한 종류의 기록이 없어요.").font(font(12.0)).color(c::TEXT_3));
                }
            });
            let shown_count: u32 = shown.iter().map(|e| e.count).sum();
            ui.horizontal(|ui| {
                let rest = pd.count.saturating_sub(shown_count);
                if rest > 0 {
                    ui.label(RichText::new(format!("나머지 기록 {rest}건")).font(font(12.0)).color(c::TEXT_3));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let link = ui.add(
                        egui::Button::new(RichText::new("프로젝트 타임라인 →").font(font(12.0)).color(c::TEXT))
                            .frame(false),
                    );
                    if link.clicked() {
                        crate::shell::actions::open_center_tab(
                            app,
                            crate::state::TabKind::Timeline { project: Some(pd.project_id) },
                        );
                    }
                });
            });
        },
    );
}
