//! Wiki home (WikiHome): pages grouped by project with search, scope and topic filters, the
//! review queue (proposals and AI drafts) and recent wiki changes from the timeline.

use super::{Ctx, TabState, dialogs, page_state, when};
use crate::App;
use crate::icons;
use crate::shell::{actions, widgets};
use crate::state::TabKind;
use crate::theme::{self, Seg, Tone, c, font, icon, medium, semibold, ty};
use crate::views::history::fmt;
use egui::{Align2, CornerRadius, Pos2, Rect, RichText, Sense, Vec2};
use runwick_core::api::jobs::{AutomationList, JobsOverview, PowerSource};
use runwick_core::api::projects::{ProjectId, ProjectsList};
use runwick_core::api::timeline::{EventKind, RangeQuery, TimelineRange};
use runwick_core::api::wiki::*;
use std::collections::BTreeSet;

const ASIDE_W: f32 = 300.0;

pub(super) fn show(app: &mut App, ui: &mut egui::Ui, cx: &Ctx, st: &mut TabState) {
    egui::ScrollArea::vertical().id_salt(("wiki-home", cx.tab)).auto_shrink([false, false]).show(ui, |ui| {
        egui::Frame::new().inner_margin(egui::Margin { left: 28, right: 28, top: 22, bottom: 28 }).show(ui, |ui| {
            let w = ui.available_width().min(1120.0);
            let pad = ((ui.available_width() - w) / 2.0).max(0.0);
            ui.horizontal(|ui| {
                ui.add_space(pad);
                ui.vertical(|ui| {
                    ui.set_width(w);
                    body(app, ui, cx, st);
                });
            });
        });
    });
}

fn body(app: &mut App, ui: &mut egui::Ui, cx: &Ctx, st: &mut TabState) {
    let scope = if st.all_projects { None } else { Some(cx.project) };
    let pages = app.hub.query::<WikiList>(cx.host, &WikiQuery { project_id: scope });
    let proposals = app.hub.query::<WikiProposals>(cx.host, &ProposalQuery { project_id: scope, ..Default::default() });
    header(app, ui, cx, st, pages.data.as_ref().map(|p| p.len()));
    ui.add_space(14.0);
    super::offline_banner(ui, cx);
    filters(app, ui, cx, st, pages.data.as_deref().map(Vec::as_slice).unwrap_or(&[]));
    ui.add_space(12.0);
    let wide = ui.available_width() >= 860.0;
    let list_w = if wide { ui.available_width() - ASIDE_W - 20.0 } else { ui.available_width() };
    ui.horizontal_top(|ui| {
        ui.vertical(|ui| {
            ui.set_width(list_w);
            match (&pages.data, &pages.error) {
                (Some(list), _) => list_view(app, ui, cx, st, list),
                (None, Some(e)) => error_text(ui, &format!("위키를 불러오지 못했어요: {}", e.message)),
                (None, None) => loading(ui, "위키를 불러오는 중…"),
            }
        });
        if wide {
            ui.add_space(20.0);
            ui.vertical(|ui| {
                ui.set_width(ASIDE_W);
                aside(
                    app,
                    ui,
                    cx,
                    st,
                    pages.data.as_deref().map(Vec::as_slice),
                    proposals.data.as_deref().map(Vec::as_slice),
                );
            });
        }
    });
    if !wide {
        ui.add_space(20.0);
        aside(app, ui, cx, st, pages.data.as_deref().map(Vec::as_slice), proposals.data.as_deref().map(Vec::as_slice));
    }
}

fn header(app: &mut App, ui: &mut egui::Ui, cx: &Ctx, st: &mut TabState, count: Option<usize>) {
    let wide = ui.available_width() >= 1080.0;
    let scope =
        if st.all_projects { "이 컴퓨터의 모든 프로젝트".to_string() } else { cx.project_name.clone() };
    let n = count.map(|n| format!(" 문서 {n}개.")).unwrap_or_default();
    let title = |ui: &mut egui::Ui| {
        ui.label(RichText::new("위키").font(semibold(ty::TITLE)).color(c::TEXT_STRONG));
        ui.add(
            egui::Label::new(
                RichText::new(format!("{scope}{n} 직접 쓴 글은 AI가 바로 고치지 않고 제안으로 남겨요."))
                    .font(font(ty::BODY))
                    .color(c::TEXT_2),
            )
            .wrap(),
        );
    };
    if wide {
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.set_width(ui.available_width() - 680.0);
                title(ui);
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| head_actions(app, ui, cx, st, true));
        });
    } else {
        title(ui);
        ui.add_space(8.0);
        ui.horizontal_wrapped(|ui| head_actions(app, ui, cx, st, false));
    }
}

#[derive(Clone, Copy)]
enum Head {
    Search,
    New,
    Draft,
    Tidy,
    More,
}

/// Header actions, in reading order (or reversed for a right-to-left layout).
fn head_actions(app: &mut App, ui: &mut egui::Ui, cx: &Ctx, st: &mut TabState, rtl: bool) {
    let mut order = [Head::Search, Head::New, Head::Draft, Head::Tidy, Head::More];
    if rtl {
        order.reverse();
    }
    for a in order {
        match a {
            Head::Search => {
                let id = egui::Id::unique(("wiki-search", cx.tab));
                ui.add_sized(Vec2::new(220.0, 30.0), |ui: &mut egui::Ui| {
                    theme::search_field(ui, &mut st.search, "제목, 본문, 태그 검색", id, 30.0)
                });
            }
            Head::New => {
                if widgets::pill_button(ui, Some(icons::PLUS), "새 문서", 30.0, c::CONTROL).clicked() {
                    dialogs::open_new(app, cx);
                }
            }
            Head::Draft => {
                if widgets::pill_button(ui, Some(icons::SPARKLES), "세션으로 초안", 30.0, c::CONTROL)
                    .on_hover_text("고른 세션의 대화로 AI 초안을 만들어요 · 구독 사용량을 씁니다")
                    .clicked()
                {
                    dialogs::open_draft(app, cx);
                }
            }
            Head::Tidy => super::tidy_control(app, ui, cx),
            Head::More => more_menu(app, ui, cx),
        }
    }
}

fn more_menu(app: &mut App, ui: &mut egui::Ui, cx: &Ctx) {
    let resp = theme::icon_button(ui, icons::ELLIPSIS, "더 보기: 가져오기, 폴더 열기");
    egui::Popup::menu(&resp).show(|ui| {
        ui.set_min_width(220.0);
        if widgets::menu_item(ui, icons::IMPORT, "기존 Markdown 가져오기").clicked() {
            dialogs::import_files(app, cx);
        }
        if cx.host == crate::client::HostKey::Local
            && widgets::menu_item(ui, icons::FOLDER_OPEN, "위키 폴더 열기").clicked()
        {
            let dir = runwick_core::paths::wiki_dir().join(cx.project.to_string());
            let _ = std::fs::create_dir_all(&dir);
            actions::reveal(app, &dir.to_string_lossy());
        }
    });
}

fn filters(app: &mut App, ui: &mut egui::Ui, cx: &Ctx, st: &mut TabState, pages: &[PageSummary]) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        let views = [
            Seg { text: Some("목록"), glyph: Some(icons::LIST), name: "목록" },
            Seg { text: Some("관계"), glyph: Some(icons::WAYPOINTS), name: "관계" },
        ];
        if theme::segmented(ui, &views, 0, 26.0) == Some(1) {
            actions::open_center_tab(app, TabKind::WikiGraph { project: cx.project });
        }
        ui.add_space(6.0);
        if fmt::chip_button(ui, "모든 프로젝트", st.all_projects, false).clicked() {
            st.all_projects = true;
        }
        if fmt::chip_button(ui, &cx.project_name, !st.all_projects, false).on_hover_text(&cx.project_name).clicked() {
            st.all_projects = false;
        }
        ui.add_space(6.0);
        let mut tags: BTreeSet<&str> = BTreeSet::new();
        for p in pages {
            tags.extend(p.tags.iter().map(String::as_str));
        }
        if !tags.is_empty() {
            if fmt::chip_button(ui, "전체", st.topic.is_none(), false).clicked() {
                st.topic = None;
            }
            for t in tags.into_iter().take(12) {
                let on = st.topic.as_deref() == Some(t);
                if fmt::chip_button(ui, &format!("#{t}"), on, false).clicked() {
                    st.topic = if on { None } else { Some(t.to_string()) };
                }
            }
        }
        ui.label(RichText::new("최근 수정순").font(font(ty::SMALL)).color(c::TEXT_3));
    });
}

fn matches(p: &PageSummary, st: &TabState) -> bool {
    if let Some(t) = &st.topic
        && !p.tags.iter().any(|x| x == t)
    {
        return false;
    }
    let q = st.search.trim().to_lowercase();
    q.is_empty()
        || p.title.to_lowercase().contains(&q)
        || p.excerpt.to_lowercase().contains(&q)
        || p.tags.iter().any(|t| t.to_lowercase().contains(&q))
}

fn list_view(app: &mut App, ui: &mut egui::Ui, cx: &Ctx, st: &mut TabState, all: &[PageSummary]) {
    if all.is_empty() && !st.all_projects {
        empty_wiki(app, ui, cx);
        return;
    }
    let shown: Vec<&PageSummary> = all.iter().filter(|p| matches(p, st)).collect();
    if shown.is_empty() {
        no_match(app, ui, cx, st);
        return;
    }
    let names = app.hub.query::<ProjectsList>(cx.host, &()).data;
    let automation = app.hub.query::<AutomationList>(cx.host, &()).data;
    let mut groups: Vec<(ProjectId, Vec<&PageSummary>)> = Vec::new();
    for p in shown {
        match groups.iter_mut().find(|(id, _)| *id == p.project_id) {
            Some((_, v)) => v.push(p),
            None => groups.push((p.project_id, vec![p])),
        }
    }
    // The tab's own project first.
    groups.sort_by_key(|(id, _)| *id != cx.project);
    for (project, list) in groups {
        let name = names
            .as_ref()
            .and_then(|n| n.iter().find(|p| p.id == project).map(|p| p.name.clone()))
            .unwrap_or_else(|| "프로젝트".into());
        let total = all.iter().filter(|p| p.project_id == project).count();
        let auto = automation.as_ref().and_then(|a| a.iter().find(|r| r.project_id == project)).map(|r| r.wiki);
        theme::card_frame().inner_margin(egui::Margin::same(0)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            group_header(ui, &name, list.len(), total, auto);
            for p in list {
                let draft = app
                    .feat
                    .wiki
                    .edits
                    .get(&(cx.host, p.project_id, p.slug.clone()))
                    .is_some_and(super::page::Edit::dirty);
                if page_row(ui, p, draft) {
                    if project == cx.project {
                        st.page = Some(p.slug.clone());
                        st.history = None;
                    } else {
                        super::open_page(app, project, &p.slug);
                    }
                }
            }
        });
        ui.add_space(12.0);
    }
}

fn group_header(ui: &mut egui::Ui, name: &str, shown: usize, total: usize, auto: Option<bool>) {
    egui::Frame::new().inner_margin(egui::Margin { left: 14, right: 14, top: 10, bottom: 8 }).show(ui, |ui| {
        ui.horizontal(|ui| {
            theme::truncated(ui, name, medium(13.0), c::TEXT_STRONG);
            let n = if shown == total { format!("문서 {total}개") } else { format!("{shown}개 일치") };
            ui.label(RichText::new(n).font(font(ty::SMALL)).color(c::TEXT_3));
            match auto {
                Some(true) => ui.label(RichText::new("AI 정리 켜짐").font(font(ty::SMALL)).color(c::SKY)),
                Some(false) => ui.label(RichText::new("AI 정리 꺼짐").font(font(ty::SMALL)).color(c::TEXT_3)),
                None => ui.label(""),
            };
        });
    });
}

/// One page row. Returns true when clicked.
fn page_row(ui: &mut egui::Ui, p: &PageSummary, draft: bool) -> bool {
    let w = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, 54.0), Sense::click());
    let title = p.title.clone();
    resp.widget_info(move || egui::WidgetInfo::labeled(egui::Role::Link, true, &title));
    let painter = ui.painter_at(rect);
    painter.hline(rect.x_range(), rect.top(), egui::Stroke::new(1.0, c::LINE));
    if resp.hovered() {
        painter.rect_filled(rect, CornerRadius::ZERO, c::PANEL_2);
    }
    let (tone, glyph, state) =
        if draft { (Tone::Direct, icons::PENCIL, "초안 · 저장 대기".to_string()) } else { page_state(p) };
    let right_w = 250.0_f32.min(w * 0.45);
    let text_w = (w - right_w - 28.0).max(80.0);
    let x = rect.left() + 14.0;
    let title = painter.layout(p.title.clone(), medium(13.5), c::TEXT_STRONG, f32::INFINITY);
    let tw = title.size().x.min(text_w);
    painter.with_clip_rect(Rect::from_min_size(Pos2::new(x, rect.top()), Vec2::new(text_w, rect.height()))).galley(
        Pos2::new(x, rect.top() + 9.0),
        title,
        c::TEXT_STRONG,
    );
    let _ = tw;
    let tags: Vec<String> = p.tags.iter().map(|t| format!("#{t}")).collect();
    let sub = match (tags.is_empty(), p.excerpt.is_empty()) {
        (false, false) => format!("{} · {}", tags.join(" "), p.excerpt),
        (false, true) => tags.join(" "),
        (true, false) => p.excerpt.clone(),
        (true, true) => "내용 없음".into(),
    };
    let mut job = egui::text::LayoutJob::simple_singleline(sub.clone(), font(12.0), c::TEXT_3);
    job.wrap = egui::text::TextWrapping::truncate_at_width(text_w);
    let g = painter.layout_job(job);
    painter.galley(Pos2::new(x, rect.top() + 30.0), g, c::TEXT_3);
    // Right side: state, sources, time.
    let mut rx = rect.right() - 14.0;
    let when_g = painter.layout_no_wrap(when(p.updated_at), font(12.0), c::TEXT_3);
    rx -= when_g.size().x;
    painter.galley(Pos2::new(rx, rect.center().y - when_g.size().y / 2.0), when_g, c::TEXT_3);
    if !p.sources.is_empty() {
        let src = painter.layout_no_wrap(format!("세션 {}", p.sources.len()), font(12.0), c::TEXT_2);
        rx -= src.size().x + 14.0;
        painter.galley(Pos2::new(rx, rect.center().y - src.size().y / 2.0), src, c::TEXT_2);
    }
    let fg = if tone == Tone::Neutral { c::TEXT_3 } else { tone.fg() };
    let st_g = painter.layout_no_wrap(state, font(12.0), fg);
    rx -= st_g.size().x + 14.0;
    painter.galley(Pos2::new(rx, rect.center().y - st_g.size().y / 2.0), st_g, fg);
    painter.text(Pos2::new(rx - 9.0, rect.center().y), Align2::CENTER_CENTER, glyph, crate::theme::icon_font(12.0), fg);
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        if p.title.chars().count() > 30 || sub.chars().count() > 60 {
            resp.clone().on_hover_text(format!("{}\n{sub}", p.title));
        }
    }
    theme::focus_ring(ui, &resp, CornerRadius::same(6));
    resp.clicked()
}

fn empty_wiki(app: &mut App, ui: &mut egui::Ui, cx: &Ctx) {
    theme::card_frame().inner_margin(egui::Margin::same(22)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(icon(icons::BOOK_OPEN, 22.0, c::TEXT_3));
        ui.add_space(4.0);
        ui.label(RichText::new("아직 위키 문서가 없어요").font(semibold(ty::PANEL_TITLE)).color(c::TEXT_STRONG));
        ui.label(
            RichText::new("직접 쓰거나, 갖고 있던 Markdown을 가져오거나, 세션 대화로 초안을 만들 수 있어요.")
                .font(font(ty::BODY))
                .color(c::TEXT_2),
        );
        ui.add_space(10.0);
        ui.horizontal_wrapped(|ui| {
            if theme::primary_button(ui, "새 문서").clicked() {
                dialogs::open_new(app, cx);
            }
            if theme::secondary_button(ui, "기존 Markdown 가져오기").clicked() {
                dialogs::import_files(app, cx);
            }
            if theme::secondary_button(ui, "선택한 세션으로 초안 만들기")
                .on_hover_text("구독 사용량을 씁니다")
                .clicked()
            {
                dialogs::open_draft(app, cx);
            }
        });
        ui.add_space(8.0);
        ui.label(
            RichText::new("AI가 알아서 정리하는 자동 작업은 따로 켜야 해요. 직접 만든 문서는 AI가 바로 고치지 않아요.")
                .font(font(ty::SMALL))
                .color(c::TEXT_3),
        );
    });
}

fn no_match(app: &mut App, ui: &mut egui::Ui, cx: &Ctx, st: &mut TabState) {
    theme::card_frame().inner_margin(egui::Margin::same(18)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        let what = match (&st.topic, st.search.trim()) {
            (Some(t), _) => format!("‘#{t}’ 문서"),
            (None, q) if !q.is_empty() => format!("‘{q}’에 맞는 문서"),
            _ => "문서".into(),
        };
        ui.label(RichText::new(format!("이 범위에는 {what}가 없어요.")).font(medium(13.0)).color(c::TEXT_STRONG));
        ui.label(
            RichText::new("다른 프로젝트 문서까지 찾아보거나 새로 만들 수 있어요.").font(font(12.5)).color(c::TEXT_2),
        );
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if !st.all_projects && theme::secondary_button(ui, "모든 프로젝트에서 보기").clicked() {
                st.all_projects = true;
            }
            if theme::secondary_button(ui, "새 문서").clicked() {
                dialogs::open_new(app, cx);
            }
        });
    });
}

fn aside(
    app: &mut App,
    ui: &mut egui::Ui,
    cx: &Ctx,
    st: &mut TabState,
    pages: Option<&[PageSummary]>,
    proposals: Option<&[Proposal]>,
) {
    review_queue(app, ui, cx, st, pages, proposals);
    ui.add_space(14.0);
    recent_changes(app, ui, cx, st);
    ui.add_space(12.0);
    automation_note(app, ui, cx);
}

enum Review<'a> {
    Proposal(&'a Proposal),
    Draft(&'a PageSummary),
}

fn review_queue(
    app: &mut App,
    ui: &mut egui::Ui,
    cx: &Ctx,
    st: &mut TabState,
    pages: Option<&[PageSummary]>,
    proposals: Option<&[Proposal]>,
) {
    let mut items: Vec<(runwick_core::Ts, Review)> = Vec::new();
    for p in proposals.unwrap_or(&[]) {
        items.push((p.created_at, Review::Proposal(p)));
    }
    for p in pages.unwrap_or(&[]).iter().filter(|p| p.status == PageStatus::Draft) {
        items.push((p.updated_at, Review::Draft(p)));
    }
    items.sort_by_key(|i| std::cmp::Reverse(i.0));
    theme::card_frame().inner_margin(egui::Margin::same(0)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        egui::Frame::new().inner_margin(egui::Margin { left: 14, right: 14, top: 12, bottom: 8 }).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("검토할 것").font(semibold(13.5)).color(c::TEXT_STRONG));
                if !items.is_empty() {
                    widgets::count_badge(ui, ui.cursor().min + Vec2::new(18.0, 9.0), items.len());
                    ui.add_space(22.0);
                }
            });
            ui.label(RichText::new("적용하기 전에는 문서가 바뀌지 않아요").font(font(ty::SMALL)).color(c::TEXT_3));
        });
        if proposals.is_none() && pages.is_none() {
            pad(ui, |ui| loading(ui, "불러오는 중…"));
            return;
        }
        if items.is_empty() {
            pad(ui, |ui| {
                ui.label(RichText::new("검토할 제안이나 초안이 없어요.").font(font(12.5)).color(c::TEXT_3));
            });
            return;
        }
        for (_, item) in items.iter().take(8) {
            let (tone, glyph, title, kind, msg, at) = match item {
                Review::Proposal(p) => {
                    let (tone, glyph, kind) = if p.decision_flag.is_some() {
                        (Tone::Check, icons::CIRCLE_HELP, "확인 필요")
                    } else if p.slug.is_none() {
                        (Tone::Info, icons::FILE_PLUS, "새 문서 제안")
                    } else {
                        (Tone::Info, icons::FILE_DIFF, "변경 제안")
                    };
                    let kind = if p.needs_rebase { "재비교 필요" } else { kind };
                    let msg = p.decision_flag.clone().unwrap_or_else(|| p.rationale.clone());
                    (tone, glyph, p.title.clone(), kind, msg, p.created_at)
                }
                Review::Draft(p) => {
                    (Tone::Info, icons::SPARKLES, p.title.clone(), "AI 초안", p.excerpt.clone(), p.updated_at)
                }
            };
            if review_row(ui, tone, glyph, &title, kind, &msg, at) {
                match item {
                    Review::Proposal(p) => match &p.slug {
                        Some(slug) if p.project_id == cx.project => {
                            st.page = Some(slug.clone());
                            st.history = None;
                        }
                        Some(slug) => super::open_page(app, p.project_id, slug),
                        None => st.review = Some(p.id),
                    },
                    Review::Draft(p) if p.project_id == cx.project => {
                        st.page = Some(p.slug.clone());
                        st.history = None;
                    }
                    Review::Draft(p) => super::open_page(app, p.project_id, &p.slug),
                }
            }
        }
    });
}

fn review_row(
    ui: &mut egui::Ui,
    tone: Tone,
    glyph: &str,
    title: &str,
    kind: &str,
    msg: &str,
    at: runwick_core::Ts,
) -> bool {
    let w = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, 52.0), Sense::click());
    let painter = ui.painter_at(rect);
    painter.hline(rect.x_range(), rect.top(), egui::Stroke::new(1.0, c::LINE));
    if resp.hovered() {
        painter.rect_filled(rect, CornerRadius::ZERO, c::PANEL_2);
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    painter.text(
        Pos2::new(rect.left() + 22.0, rect.top() + 17.0),
        Align2::CENTER_CENTER,
        glyph,
        crate::theme::icon_font(14.0),
        tone.fg(),
    );
    let x = rect.left() + 38.0;
    let when_g = painter.layout_no_wrap(when(at), font(11.5), c::TEXT_3);
    let max = (rect.right() - 12.0 - when_g.size().x - 8.0 - x).max(40.0);
    let mut job = egui::text::LayoutJob::simple_singleline(title.to_string(), medium(13.0), c::TEXT_STRONG);
    job.wrap = egui::text::TextWrapping::truncate_at_width(max);
    painter.galley(Pos2::new(x, rect.top() + 9.0), painter.layout_job(job), c::TEXT_STRONG);
    painter.galley(Pos2::new(rect.right() - 12.0 - when_g.size().x, rect.top() + 10.0), when_g, c::TEXT_3);
    let mut job = egui::text::LayoutJob::default();
    job.append(kind, 0.0, egui::TextFormat::simple(font(12.0), tone.fg()));
    job.append(&format!(" · {msg}"), 0.0, egui::TextFormat::simple(font(12.0), c::TEXT_3));
    job.wrap = egui::text::TextWrapping::truncate_at_width(rect.right() - 12.0 - x);
    painter.galley(Pos2::new(x, rect.top() + 29.0), painter.layout_job(job), c::TEXT_3);
    let full = format!("{title}\n{kind} · {msg}");
    let resp = resp.on_hover_text(full);
    resp.clicked()
}

fn recent_changes(app: &mut App, ui: &mut egui::Ui, cx: &Ctx, st: &TabState) {
    let today = fmt::today();
    let q = RangeQuery {
        from: today - chrono::Duration::days(13),
        to: today,
        project_id: (!st.all_projects).then_some(cx.project),
        utc_offset_min: fmt::utc_offset_min(),
    };
    let range = app.hub.query::<TimelineRange>(cx.host, &q);
    theme::card_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(RichText::new("최근 변경").font(semibold(13.5)).color(c::TEXT_STRONG));
        ui.add_space(4.0);
        match (&range.data, &range.error) {
            (Some(r), _) => {
                let mut events: Vec<_> =
                    r.days.iter().flat_map(|d| d.events.iter()).filter(|e| e.kind == EventKind::Wiki).collect();
                events.sort_by_key(|e| std::cmp::Reverse(e.at));
                if events.is_empty() {
                    ui.label(RichText::new("최근 2주 동안 위키 변경이 없어요.").font(font(12.5)).color(c::TEXT_3));
                }
                for e in events.iter().take(6) {
                    ui.horizontal(|ui| {
                        ui.add_sized(
                            Vec2::new(74.0, 18.0),
                            egui::Label::new(RichText::new(when(e.at)).font(font(11.5)).color(c::TEXT_3)),
                        );
                        theme::truncated(ui, &e.title, font(12.5), c::TEXT);
                    });
                }
            }
            (None, Some(e)) => {
                ui.label(
                    RichText::new(format!("타임라인을 읽지 못했어요: {}", e.message)).font(font(12.0)).color(c::ROSE),
                );
            }
            (None, None) => loading(ui, "불러오는 중…"),
        }
        ui.add_space(4.0);
        if fmt::quiet_link(ui, "타임라인에서 날짜별로 보기").clicked() {
            actions::open_center_tab(app, TabKind::Timeline { project: Some(cx.project) });
        }
    });
}

/// One honest line about automatic tidying for this host.
fn automation_note(app: &mut App, ui: &mut egui::Ui, cx: &Ctx) {
    let rows = app.hub.query::<AutomationList>(cx.host, &()).data;
    let overview = app.hub.query::<JobsOverview>(cx.host, &()).data;
    let Some(rows) = rows else { return };
    let on: Vec<&str> = rows.iter().filter(|r| r.wiki).map(|r| r.project_name.as_str()).collect();
    let mut text = match on.len() {
        0 => "AI 위키 정리는 모든 프로젝트에서 꺼져 있어요.".to_string(),
        1 => format!("AI 위키 정리는 {}만 켜져 있어요.", on[0]),
        n => format!("AI 위키 정리는 프로젝트 {n}곳에서 켜져 있어요."),
    };
    if !on.is_empty() && overview.as_ref().is_some_and(|o| o.power == PowerSource::Battery) {
        text.push_str(" 지금은 배터리라 새 분석을 보류 중이에요.");
    }
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        ui.label(RichText::new(text).font(font(12.0)).color(c::TEXT_3));
        if fmt::quiet_link(ui, "자동 작업 설정").clicked() {
            actions::open_settings(app, Some(crate::state::SettingsSection::Automation));
        }
    });
}

fn pad(ui: &mut egui::Ui, f: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::new().inner_margin(egui::Margin { left: 14, right: 14, top: 4, bottom: 12 }).show(ui, f);
}

pub(super) fn loading(ui: &mut egui::Ui, text: &str) {
    ui.horizontal(|ui| {
        ui.add(egui::Spinner::new().size(12.0).color(c::TEXT_3));
        ui.label(RichText::new(text).font(font(12.5)).color(c::TEXT_3));
    });
}

pub(super) fn error_text(ui: &mut egui::Ui, text: &str) {
    ui.horizontal_wrapped(|ui| {
        ui.label(icon(icons::CIRCLE_ALERT, 13.0, c::ROSE));
        ui.label(RichText::new(text).font(font(12.5)).color(c::ROSE));
    });
}
