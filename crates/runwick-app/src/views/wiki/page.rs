//! One wiki page (Wiki.dc.html, RemoteWiki.dc.html): the page list on the left, the page with
//! its header (수정 시각, 근거 세션, 수동/AI, 검토 상태), open proposals, Markdown preview or
//! editor, and the change history.
//!
//! Edit buffers are kept per page (not per tab) in [`super::State::edits`], so they survive
//! switching tabs and, as drafts on this computer, an app restart. Saves name the version the
//! edit started from; a conflict keeps the person's text and shows both versions. A draft made
//! while the wiki's computer was unreachable is compared with the current page before it can be
//! saved.

use super::{Ctx, TabState, author_label, dialogs, drafts, page_state, saved_by_label, when};
use crate::App;
use crate::client::HostKey;
use crate::icons;
use crate::shell::widgets;
use crate::state::{TabKind, ToastKind};
use crate::theme::{self, Tone, c, font, medium, semibold, ty};
use egui::{CornerRadius, Key, KeyboardShortcut, Modifiers, RichText, Vec2};
use runwick_core::Ts;
use runwick_core::api::projects::ProjectId;
use runwick_core::api::sessions::{SessionQuery, SessionsList};
use runwick_core::api::wiki::*;
use std::collections::HashMap;

const NAV_W: f32 = 232.0;
const LINK_SCHEME: &str = "runwick-wiki:";

/// Which page an edit belongs to: the wiki's host, project and page slug.
pub(super) type EditKey = (HostKey, ProjectId, String);

pub(super) fn key(cx: &Ctx, slug: &str) -> EditKey {
    (cx.host, cx.project, slug.to_string())
}

pub(super) struct Edit {
    pub base_version: u32,
    pub title: String,
    pub body: String,
    pub tags: String,
    /// What the edit started from (to tell whether anything changed).
    pub orig_title: String,
    pub orig_body: String,
    pub orig_tags: String,
    /// Kept as a draft on this computer (restored after a restart): when it was last written.
    pub draft_at: Option<Ts>,
    /// Closed without saving: the page shows its saved text with a note about the draft.
    parked: bool,
    preview: bool,
    saving: bool,
    /// The page as it is now, when it moved on from `base_version`.
    conflict: Option<Box<Page>>,
    show_diff: bool,
    focus: bool,
}

impl Edit {
    pub(super) fn from(page: &Page) -> Self {
        let tags = page.summary.tags.join(", ");
        Edit {
            base_version: page.summary.version,
            title: page.summary.title.clone(),
            body: page.body.clone(),
            tags: tags.clone(),
            orig_title: page.summary.title.clone(),
            orig_body: page.body.clone(),
            orig_tags: tags,
            draft_at: None,
            parked: false,
            preview: false,
            saving: false,
            conflict: None,
            show_diff: false,
            focus: true,
        }
    }

    /// Rebuild an edit from a stored draft.
    pub(super) fn restored(d: &drafts::Draft) -> Self {
        Edit {
            base_version: d.base_version,
            title: d.title.clone(),
            body: d.body.clone(),
            tags: d.tags.clone(),
            orig_title: d.orig_title.clone(),
            orig_body: d.orig_body.clone(),
            orig_tags: d.orig_tags.clone(),
            draft_at: Some(d.saved_at),
            parked: false,
            preview: false,
            saving: false,
            conflict: None,
            show_diff: false,
            focus: false,
        }
    }

    pub(super) fn dirty(&self) -> bool {
        self.title != self.orig_title || self.body != self.orig_body || self.tags != self.orig_tags
    }

    fn tags(&self) -> Vec<String> {
        self.tags
            .split([',', ' '])
            .map(|t| t.trim().trim_start_matches('#').to_string())
            .filter(|t| !t.is_empty())
            .collect()
    }
}

/// The page has unsaved text on this computer.
pub(super) fn has_draft(app: &App, cx: &Ctx, slug: &str) -> bool {
    app.feat.wiki.edits.get(&key(cx, slug)).is_some_and(Edit::dirty)
}

pub(super) fn show(app: &mut App, ui: &mut egui::Ui, cx: &Ctx, st: &mut TabState) {
    let Some(slug) = st.page.clone() else { return };
    let pages = app.hub.query::<WikiList>(cx.host, &WikiQuery { project_id: Some(cx.project) });
    let rect = ui.available_rect_before_wrap();
    let with_nav = rect.width() >= 760.0;
    let (nav_rect, main_rect) = if with_nav {
        let (l, r) = rect.split_left_right_at_x(rect.left() + NAV_W);
        (Some(l), r)
    } else {
        (None, rect)
    };
    if let Some(nr) = nav_rect {
        ui.painter().vline(nr.right(), nr.y_range(), egui::Stroke::new(1.0, c::LINE));
        ui.scope_builder(egui::UiBuilder::new().max_rect(nr), |ui| {
            nav(app, ui, cx, st, &slug, pages.data.as_deref().map(Vec::as_slice));
        });
    }
    ui.scope_builder(egui::UiBuilder::new().max_rect(main_rect), |ui| {
        egui::ScrollArea::vertical().id_salt(("wiki-page", cx.tab, slug.as_str())).auto_shrink([false, false]).show(
            ui,
            |ui| {
                egui::Frame::new().inner_margin(egui::Margin { left: 28, right: 28, top: 24, bottom: 32 }).show(
                    ui,
                    |ui| {
                        let w = ui.available_width().min(720.0);
                        let pad = ((ui.available_width() - w) / 2.0).max(0.0);
                        ui.horizontal(|ui| {
                            ui.add_space(pad);
                            ui.vertical(|ui| {
                                ui.set_width(w);
                                if nav_rect.is_none()
                                    && crate::views::history::fmt::quiet_link(ui, "← 모든 문서").clicked()
                                {
                                    st.page = None;
                                    return;
                                }
                                article(
                                    app,
                                    ui,
                                    cx,
                                    st,
                                    &slug,
                                    pages.data.as_deref().map(Vec::as_slice).unwrap_or(&[]),
                                );
                            });
                        });
                    },
                );
            },
        );
    });
    ui.allocate_rect(rect, egui::Sense::hover());
}

fn nav(app: &mut App, ui: &mut egui::Ui, cx: &Ctx, st: &mut TabState, current: &str, pages: Option<&[PageSummary]>) {
    egui::Frame::new().inner_margin(egui::Margin::symmetric(8, 12)).show(ui, |ui| {
        ui.set_width(NAV_W - 16.0);
        ui.spacing_mut().item_spacing.y = 2.0;
        let back = widgets::row(ui, 28.0, false, "모든 문서");
        ui.painter().text(
            back.rect.left_center() + Vec2::new(14.0, 0.0),
            egui::Align2::CENTER_CENTER,
            icons::ARROW_LEFT,
            theme::icon_font(12.0),
            c::TEXT_2,
        );
        ui.painter().text(
            back.rect.left_center() + Vec2::new(26.0, 0.0),
            egui::Align2::LEFT_CENTER,
            "모든 문서",
            font(12.5),
            c::TEXT_2,
        );
        if back.clicked() {
            st.page = None;
            st.history = None;
        }
        ui.add_space(4.0);
        if widgets::wide_pill(ui, Some(icons::PLUS), "새 문서", ui.available_width(), 30.0).clicked() {
            dialogs::open_new(app, cx);
        }
        ui.add_space(8.0);
        egui::ScrollArea::vertical().id_salt(("wiki-nav", cx.tab)).auto_shrink([false, false]).show(ui, |ui| {
            let Some(pages) = pages else {
                super::home::loading(ui, "불러오는 중…");
                return;
            };
            for p in pages {
                let on = p.slug == current;
                let draft = has_draft(app, cx, &p.slug);
                let (tone, _, state) = page_state(p);
                let resp = widgets::row(ui, 44.0, on, &p.title);
                let r = resp.rect.shrink2(Vec2::new(9.0, 6.0));
                let badge = if draft {
                    Some(("초안".to_string(), c::VIOLET))
                } else {
                    (p.open_proposals > 0).then(|| (format!("제안 {}", p.open_proposals), c::SKY))
                };
                let badge_w = badge
                    .as_ref()
                    .map(|(b, col)| ui.painter().layout_no_wrap(b.clone(), font(11.0), *col).size().x + 6.0)
                    .unwrap_or(0.0);
                let mut job = egui::text::LayoutJob::simple_singleline(
                    p.title.clone(),
                    font(13.0),
                    if on { c::TEXT_STRONG } else { c::TEXT },
                );
                job.wrap = egui::text::TextWrapping::truncate_at_width(r.width() - badge_w);
                let g = ui.painter().layout_job(job);
                ui.painter().galley(r.left_top(), g, c::TEXT);
                if let Some((b, col)) = badge {
                    ui.painter().text(r.right_top(), egui::Align2::RIGHT_TOP, b, font(11.0), col);
                }
                let (meta, fg) = if draft {
                    ("초안 · 저장 대기".to_string(), c::VIOLET)
                } else if p.status == PageStatus::Draft {
                    (state, c::TEXT_3)
                } else if p.open_proposals > 0 {
                    (format!("{} · {state}", author_label(p)), if tone == Tone::Check { c::YELLOW } else { c::TEXT_3 })
                } else {
                    (format!("{} · {}", author_label(p), when(p.updated_at)), c::TEXT_3)
                };
                let mut job = egui::text::LayoutJob::simple_singleline(meta, font(11.5), fg);
                job.wrap = egui::text::TextWrapping::truncate_at_width(r.width());
                let g = ui.painter().layout_job(job);
                ui.painter().galley(r.left_bottom() - Vec2::new(0.0, g.size().y), g, fg);
                if resp.on_hover_text(&p.title).clicked() && !on {
                    st.page = Some(p.slug.clone());
                    st.history = None;
                }
            }
        });
    });
}

fn article(app: &mut App, ui: &mut egui::Ui, cx: &Ctx, st: &mut TabState, slug: &str, pages: &[PageSummary]) {
    let q = app.hub.query::<WikiGet>(cx.host, &PageRef { project_id: cx.project, slug: slug.to_string() });
    let k = key(cx, slug);
    let parked = app.feat.wiki.edits.get(&k).map(|e| e.parked);
    let editing = parked == Some(false);
    let page = match (&q.data, &q.error) {
        (Some(p), _) if p.summary.slug == slug => p.clone(),
        // Nothing received yet (for example the wiki's computer is not connected): an existing
        // draft can still be read and kept.
        (_, err) if parked.is_some() => {
            if let Some(e) = err {
                super::home::error_text(ui, &format!("문서를 불러오지 못했어요: {}", e.message));
            }
            super::offline_banner(ui, cx);
            offline_editor(app, ui, cx, &k);
            return;
        }
        (_, Some(e)) => {
            super::home::error_text(ui, &format!("문서를 열지 못했어요: {}", e.message));
            if e.code == runwick_core::rpc::codes::NOT_FOUND {
                ui.label(
                    RichText::new("파일이 지워졌거나 이름이 바뀌었을 수 있어요.").font(font(12.5)).color(c::TEXT_3),
                );
            }
            return;
        }
        _ => {
            super::home::loading(ui, "문서를 불러오는 중…");
            return;
        }
    };
    st.title = Some(page.summary.title.clone());
    header(app, ui, cx, st, &page, editing);
    ui.add_space(10.0);
    super::offline_banner(ui, cx);
    if parked == Some(true) {
        parked_banner(app, ui, &k);
        ui.add_space(8.0);
    }
    if page.summary.status == PageStatus::Draft && !editing {
        draft_banner(app, ui, cx, &page);
        ui.add_space(8.0);
    }
    if !editing {
        let props = app.hub.query::<WikiProposals>(
            cx.host,
            &ProposalQuery { project_id: Some(cx.project), slug: Some(slug.to_string()), include_closed: false },
        );
        for p in props.data.as_deref().map(Vec::as_slice).unwrap_or(&[]) {
            super::review::card(app, ui, cx, st, p);
            ui.add_space(12.0);
        }
    }
    if st.history.is_some() && !editing {
        history(app, ui, cx, st, &page);
        ui.add_space(12.0);
    }
    if editing {
        editor(app, ui, cx, &k, Some(&page));
    } else {
        preview(app, ui, cx, &page.body, pages);
    }
}

fn header(app: &mut App, ui: &mut egui::Ui, cx: &Ctx, st: &mut TabState, page: &Page, editing: bool) {
    let p = &page.summary;
    if editing {
        return;
    }
    ui.horizontal_top(|ui| {
        let title_w = (ui.available_width() - 190.0).max(160.0);
        ui.vertical(|ui| {
            ui.set_width(title_w);
            ui.add(egui::Label::new(RichText::new(&p.title).font(semibold(24.0)).color(c::TEXT_STRONG)).wrap());
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let label = if app.feat.wiki.edits.contains_key(&key(cx, &p.slug)) { "이어서 편집" } else { "편집" };
            if theme::secondary_button(ui, label).clicked() {
                resume_or_edit(app, cx, page);
                st.history = None;
            }
        });
    });
    let sessions = session_titles(app, cx);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 12.0;
        ui.label(
            RichText::new(format!("{} · {} 수정 · v{}", author_label(p), when(p.updated_at), p.version))
                .font(font(12.0))
                .color(c::TEXT_3),
        );
        if !p.sources.is_empty() {
            let resp = link(ui, &format!("근거 세션 {}", p.sources.len()));
            egui::Popup::menu(&resp).show(|ui| {
                ui.set_min_width(260.0);
                for id in &p.sources {
                    let title = sessions.get(id).cloned().unwrap_or_else(|| "기록 없는 세션".into());
                    if widgets::menu_item(ui, icons::MESSAGES_SQUARE, &title)
                        .on_hover_text("이 세션 대화 기록 보기")
                        .clicked()
                    {
                        crate::views::history::open_session_records(app, cx.host, *id);
                    }
                }
            });
        }
        if link(ui, "관계 보기").clicked() {
            crate::shell::actions::open_center_tab(app, TabKind::WikiGraph { project: cx.project });
        }
        let hist_label = if st.history.is_some() { "변경 이력 닫기" } else { "변경 이력" };
        if link(ui, hist_label).clicked() {
            st.history = if st.history.is_some() { None } else { Some(None) };
        }
        if link(ui, "인계에 담기").on_hover_text("이 문서를 가리키는 짧은 인계 문구를 복사해요").clicked()
        {
            ui.ctx().copy_text(super::handover(page, &cx.project_name, &sessions));
            app.toast_info("인계 문구를 복사했어요");
        }
    });
    if !p.tags.is_empty() {
        ui.horizontal_wrapped(|ui| {
            for t in &p.tags {
                ui.label(RichText::new(format!("#{t}")).font(font(12.0)).color(c::TEXT_2));
            }
        });
    }
}

/// Continue a kept draft, or start editing the saved text.
fn resume_or_edit(app: &mut App, cx: &Ctx, page: &Page) {
    match app.feat.wiki.edits.get_mut(&key(cx, &page.summary.slug)) {
        Some(e) => {
            e.parked = false;
            e.focus = true;
        }
        None => {
            app.feat.wiki.edits.insert(key(cx, &page.summary.slug), Edit::from(page));
        }
    }
}

fn parked_banner(app: &mut App, ui: &mut egui::Ui, k: &EditKey) {
    let at = app.feat.wiki.edits.get(k).and_then(|e| e.draft_at).map(|t| format!(" · {}", when(t))).unwrap_or_default();
    let mut resume = false;
    let mut discard = false;
    theme::card_frame().fill(c::VIOLET_BG).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal_wrapped(|ui| {
            ui.label(theme::icon(icons::PENCIL, 13.0, c::VIOLET));
            ui.label(
                RichText::new(format!("저장하지 않은 편집이 이 컴퓨터에 있어요{at}"))
                    .font(medium(12.5))
                    .color(c::VIOLET),
            );
            ui.label(RichText::new("아래는 저장된 내용이에요.").font(font(12.5)).color(c::TEXT_2));
        });
        ui.horizontal(|ui| {
            resume = theme::secondary_button(ui, "이어서 편집").clicked();
            discard =
                theme::danger_button(ui, "초안 버리기").on_hover_text("이 컴퓨터에 남은 편집 내용을 지워요").clicked();
        });
    });
    if resume && let Some(e) = app.feat.wiki.edits.get_mut(k) {
        e.parked = false;
        e.focus = true;
    }
    if discard {
        app.feat.wiki.edits.remove(k);
        drafts::touch(app);
    }
}

fn link(ui: &mut egui::Ui, text: &str) -> egui::Response {
    let resp = ui.add(
        egui::Label::new(RichText::new(text).font(font(12.0)).color(c::TEXT_2).underline()).sense(egui::Sense::click()),
    );
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

fn session_titles(app: &mut App, cx: &Ctx) -> HashMap<uuid::Uuid, String> {
    let q =
        SessionQuery { project_id: Some(cx.project), workspace_id: None, include_hidden: true, include_archived: true };
    app.hub
        .query::<SessionsList>(cx.host, &q)
        .data
        .map(|l| l.iter().map(|s| (s.session.id, s.session.title.clone())).collect())
        .unwrap_or_default()
}

fn draft_banner(app: &mut App, ui: &mut egui::Ui, cx: &Ctx, page: &Page) {
    theme::card_frame().fill(c::SKY_BG).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal_wrapped(|ui| {
            ui.label(theme::icon(icons::SPARKLES, 13.0, c::SKY));
            ui.label(RichText::new("AI 초안 · 검토 전").font(medium(12.5)).color(c::SKY));
            let n = page.summary.sources.len();
            let basis = if n > 0 {
                format!("세션 {n}개를 근거로 만든 초안이에요.")
            } else {
                "AI가 만든 초안이에요.".into()
            };
            ui.label(
                RichText::new(format!("{basis} 내용을 확인하고 고치거나 검토 완료로 표시하세요."))
                    .font(font(12.5))
                    .color(c::TEXT_2),
            );
        });
        ui.add_space(4.0);
        if ui.add_enabled_ui(cx.connected, |ui| theme::secondary_button(ui, "검토 완료로 표시")).inner.clicked()
        {
            let s = SavePage {
                project_id: cx.project,
                slug: page.summary.slug.clone(),
                base_version: page.summary.version,
                title: page.summary.title.clone(),
                body: page.body.clone(),
                tags: None,
                mark_reviewed: true,
            };
            app.hub.call::<WikiSave>(cx.host, s, |app, res| match res {
                Ok(SaveOutcome::Saved { .. }) => app.toast(ToastKind::Success, "검토 완료로 표시했어요"),
                Ok(SaveOutcome::Conflict { .. }) => app.toast_error("문서가 방금 바뀌었어요. 다시 확인해 주세요"),
                Err(e) => app.toast_error(&e.message),
            });
        }
    });
}

// ---------------------------------------------------------------------------------------------
// Preview

/// Markdown for the viewer: `[[링크]]` become clickable wiki links; images become their alt text
/// (pages never load remote images).
pub(super) fn preview_markdown(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    let mut in_code = false;
    for line in body.split_inclusive('\n') {
        if line.trim_start().starts_with("```") {
            in_code = !in_code;
        }
        if in_code || !line.contains("[[") && !line.contains("![") {
            out.push_str(line);
            continue;
        }
        let mut rest = line;
        while !rest.is_empty() {
            let wiki = rest.find("[[");
            let img = rest.find("![");
            match (wiki, img) {
                (Some(i), j) if j.is_none_or(|j| i < j) => {
                    let after = &rest[i + 2..];
                    let Some(end) = after.find("]]") else {
                        out.push_str(rest);
                        break;
                    };
                    let inner = &after[..end];
                    let (target, label) = match inner.split_once('|') {
                        Some((t, l)) => (t, l),
                        None => (inner, inner),
                    };
                    out.push_str(&rest[..i]);
                    out.push_str(&format!("[{}](<{LINK_SCHEME}{}>)", label.trim(), target.trim().replace('>', "")));
                    rest = &after[end + 2..];
                }
                (_, Some(j)) => {
                    let after = &rest[j + 2..];
                    match (after.find("]("), after.find(')')) {
                        (Some(a), Some(b)) if a < b => {
                            out.push_str(&rest[..j]);
                            out.push_str(&format!("[그림: {}]", &after[..a]));
                            rest = &after[b + 1..];
                        }
                        _ => {
                            out.push_str(rest);
                            break;
                        }
                    }
                }
                _ => {
                    out.push_str(rest);
                    break;
                }
            }
        }
    }
    out
}

fn preview(app: &mut App, ui: &mut egui::Ui, cx: &Ctx, body: &str, pages: &[PageSummary]) {
    if body.trim().is_empty() {
        ui.label(RichText::new("빈 문서예요. 편집을 눌러 내용을 써 보세요.").font(font(ty::BODY)).color(c::TEXT_3));
        return;
    }
    let md = preview_markdown(body);
    ui.scope(|ui| {
        ui.style_mut().override_text_style = Some(egui::TextStyle::Body);
        ui.style_mut().text_styles.insert(egui::TextStyle::Body, font(14.5));
        // The default text color only, so links keep the link color.
        ui.visuals_mut().widgets.noninteractive.fg_stroke.color = c::TEXT;
        egui_commonmark::CommonMarkViewer::new().default_width(Some(ui.available_width() as usize)).show(
            ui,
            &mut app.feat.wiki.md,
            &md,
        );
    });
    // Wiki links open pages here instead of a browser; source links open the message.
    let mut target = None;
    let mut record = None;
    ui.ctx().output_mut(|o| {
        o.commands.retain(|cmd| match cmd {
            egui::OutputCommand::OpenUrl(u) if u.url.starts_with(LINK_SCHEME) => {
                target = Some(u.url[LINK_SCHEME.len()..].to_string());
                false
            }
            egui::OutputCommand::OpenUrl(u) if u.url.starts_with(super::RECORD_SCHEME) => {
                record = Some(u.url[super::RECORD_SCHEME.len()..].to_string());
                false
            }
            egui::OutputCommand::OpenUrl(u) if u.url.ends_with(".md") && !u.url.contains("://") => {
                target = Some(u.url.clone());
                false
            }
            _ => true,
        })
    });
    if let Some(r) = record {
        match r.trim().parse() {
            Ok(id) => crate::views::history::open_message(app, cx.host, id, Vec::new()),
            Err(_) => app.toast_error("출처 링크가 올바르지 않아요"),
        }
    }
    if let Some(t) = target {
        match super::resolve(&t, pages) {
            Some(p) => {
                if let Some(st) = app.feat.wiki.tabs.get_mut(&cx.tab) {
                    st.page = Some(p.slug.clone());
                } else {
                    super::open_page(app, cx.project, &p.slug);
                }
            }
            None => app.toast_info(&format!("‘{t}’ 문서가 아직 없어요. 새 문서로 만들 수 있어요")),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Editor

/// A draft whose page could not be loaded (the wiki's computer is not reachable).
fn offline_editor(app: &mut App, ui: &mut egui::Ui, cx: &Ctx, k: &EditKey) {
    editor(app, ui, cx, k, None);
}

fn editor(app: &mut App, ui: &mut egui::Ui, cx: &Ctx, k: &EditKey, page: Option<&Page>) {
    let Some(mut e) = app.feat.wiki.edits.remove(k) else { return };
    // The page moved on (saved elsewhere, or while this draft waited offline): compare first.
    if let Some(p) = page
        && cx.connected
        && e.conflict.is_none()
        && p.summary.version != e.base_version
        && e.dirty()
    {
        e.conflict = Some(Box::new(p.clone()));
        e.show_diff = true;
    }
    let save_key = ui.input_mut(|i| i.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, Key::S)));
    let mut save = save_key;
    let mut close = false;
    let mut changed = false;
    let can_save = cx.connected && page.is_some();
    ui.horizontal(|ui| {
        changed |= ui
            .add(
                egui::TextEdit::singleline(&mut e.title)
                    .font(semibold(22.0))
                    .text_color(c::TEXT_STRONG)
                    .hint_text("문서 제목")
                    .desired_width(ui.available_width() - 270.0)
                    .frame(egui::Frame::NONE),
            )
            .changed();
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let label = if e.saving {
                "저장 중…"
            } else if can_save {
                "저장"
            } else {
                "연결되면 저장"
            };
            if ui
                .add_enabled_ui(!e.saving && can_save, |ui| theme::primary_button(ui, label))
                .inner
                .on_hover_text("⌘S")
                .clicked()
            {
                save = true;
            }
            let leave = if e.dirty() { "닫기 (초안 유지)" } else { "취소" };
            if theme::secondary_button(ui, leave)
                .on_hover_text("편집 내용은 이 컴퓨터에 초안으로 남아요. 다시 열면 이어서 고칠 수 있어요")
                .clicked()
            {
                close = true;
            }
            let modes = [theme::Seg::text("편집"), theme::Seg::text("미리보기")];
            if let Some(i) = theme::segmented(ui, &modes, e.preview as usize, 26.0) {
                e.preview = i == 1;
            }
        });
    });
    changed |= ui
        .add(
            egui::TextEdit::singleline(&mut e.tags)
                .font(font(12.5))
                .hint_text("태그 (쉼표로 구분, 예: 인증, 결정)")
                .desired_width(f32::INFINITY),
        )
        .changed();
    ui.add_space(6.0);
    let mut discard = false;
    if e.dirty() && (!cx.connected || e.draft_at.is_some()) {
        local_draft_banner(ui, cx, &e, &mut discard);
        ui.add_space(6.0);
    }
    if let Some(cur) = e.conflict.clone() {
        conflict_banner(ui, &mut e, &cur, &mut save);
        ui.add_space(6.0);
    }
    if e.preview {
        let pages = app.hub.query::<WikiList>(cx.host, &WikiQuery { project_id: Some(cx.project) }).data;
        let body = e.body.clone();
        preview(app, ui, cx, &body, pages.as_deref().map(Vec::as_slice).unwrap_or(&[]));
    } else {
        let resp = egui::Frame::new()
            .fill(c::PANEL)
            .corner_radius(CornerRadius::same(8))
            .inner_margin(egui::Margin::same(12))
            .show(ui, |ui| {
                ui.add(
                    egui::TextEdit::multiline(&mut e.body)
                        .id(egui::Id::unique(("wiki-editor", k.0, k.1, k.2.as_str())))
                        .font(font(14.0))
                        .text_color(c::TEXT)
                        .frame(egui::Frame::NONE)
                        .lock_focus(true)
                        .desired_width(f32::INFINITY)
                        .desired_rows(18)
                        .hint_text("Markdown으로 써요. [[다른 문서 제목]]으로 링크를 걸 수 있어요."),
                )
            });
        changed |= resp.inner.changed();
        if std::mem::take(&mut e.focus) {
            resp.inner.request_focus();
        }
    }
    if changed {
        drafts::touch(app);
    }
    if discard || (close && !e.dirty()) {
        drafts::touch(app);
        return; // dropped
    }
    if close {
        // Keep the text as a draft on this computer; the page offers to continue it.
        e.parked = true;
        e.draft_at.get_or_insert_with(runwick_core::now);
        app.feat.wiki.edits.insert(k.clone(), e);
        drafts::touch(app);
        return;
    }
    if save && !e.saving && can_save {
        if !e.dirty() && e.conflict.is_none() {
            drafts::touch(app);
            return; // nothing to save
        }
        submit(app, cx, k, &mut e);
    }
    app.feat.wiki.edits.insert(k.clone(), e);
}

/// The edit lives only on this computer until it is saved to the wiki's computer.
fn local_draft_banner(ui: &mut egui::Ui, cx: &Ctx, e: &Edit, discard: &mut bool) {
    theme::card_frame().fill(c::VIOLET_BG).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal_wrapped(|ui| {
            ui.label(theme::icon(icons::PENCIL, 13.0, c::VIOLET));
            let at = e.draft_at.map(|t| format!(" · {}", super::when(t))).unwrap_or_default();
            ui.label(RichText::new(format!("초안 · 이 컴퓨터에만 있어요{at}")).font(medium(12.5)).color(c::VIOLET));
        });
        let where_ = cx.device.clone().unwrap_or_else(|| "이 컴퓨터의 Runwick".into());
        let text = if cx.connected {
            "지난번에 저장하지 못한 편집이에요. 지금 문서와 대조했어요. 바뀐 게 있으면 비교를 먼저 보여 드려요."
                .to_string()
        } else {
            format!("{where_}에 아직 저장하지 않았어요. 연결되면 그쪽 최신 버전과 대조한 뒤 저장할 수 있어요.")
        };
        ui.label(RichText::new(text).font(font(12.5)).color(c::TEXT_2));
        if theme::danger_button(ui, "초안 버리기").on_hover_text("이 컴퓨터에 남은 편집 내용을 지워요").clicked()
        {
            *discard = true;
        }
    });
}

fn conflict_banner(ui: &mut egui::Ui, e: &mut Edit, cur: &Page, save: &mut bool) {
    theme::card_frame().fill(c::YELLOW_BG).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal_wrapped(|ui| {
            ui.label(theme::icon(icons::TRIANGLE_ALERT, 13.0, c::YELLOW));
            ui.label(
                RichText::new(format!(
                    "편집을 시작한 뒤 문서가 바뀌었어요 (v{} · {}).",
                    cur.summary.version,
                    when(cur.summary.updated_at)
                ))
                .font(medium(12.5))
                .color(c::YELLOW),
            );
            ui.label(
                RichText::new("내 편집은 그대로 두었어요. 어느 쪽으로 할지 골라 주세요.")
                    .font(font(12.5))
                    .color(c::TEXT_2),
            );
        });
        ui.horizontal_wrapped(|ui| {
            let diff_label = if e.show_diff { "차이 닫기" } else { "차이 보기" };
            if theme::secondary_button(ui, diff_label).clicked() {
                e.show_diff = !e.show_diff;
            }
            if theme::secondary_button(ui, "내 편집으로 저장")
                .on_hover_text("지금 저장된 내용 위에 내 편집을 새 버전으로 저장해요. 이전 버전은 변경 이력에 남아요")
                .clicked()
            {
                e.base_version = cur.summary.version;
                *save = true;
            }
            if theme::secondary_button(ui, "저장된 내용으로 바꾸기")
                .on_hover_text("내 편집을 버리고 저장된 내용으로 편집을 이어가요")
                .clicked()
            {
                *e = Edit::from(cur);
                e.focus = false;
            }
        });
        if e.show_diff && e.conflict.is_some() {
            ui.add_space(6.0);
            ui.label(RichText::new("− 저장된 내용   + 내 편집").font(font(11.5)).color(c::TEXT_3));
            ui.spacing_mut().item_spacing.y = 0.0;
            let diff = similar::TextDiff::from_lines(cur.body.as_str(), e.body.as_str());
            for ch in diff.iter_all_changes() {
                let kind = match ch.tag() {
                    similar::ChangeTag::Equal => LineKind::Same,
                    similar::ChangeTag::Delete => LineKind::Removed,
                    similar::ChangeTag::Insert => LineKind::Added,
                };
                super::review::diff_line(ui, kind, ch.value().trim_end_matches('\n'));
            }
        }
    });
}

fn submit(app: &mut App, cx: &Ctx, k: &EditKey, e: &mut Edit) {
    e.saving = true;
    let req = SavePage {
        project_id: cx.project,
        slug: k.2.clone(),
        base_version: e.base_version,
        title: e.title.clone(),
        body: e.body.clone(),
        tags: Some(e.tags()),
        mark_reviewed: false,
    };
    let k = k.clone();
    app.hub.call::<WikiSave>(cx.host, req, move |app, res| {
        let Some(e) = app.feat.wiki.edits.get_mut(&k) else { return };
        e.saving = false;
        match res {
            Ok(SaveOutcome::Saved { page }) => {
                app.feat.wiki.edits.remove(&k);
                drafts::touch(app);
                app.toast(ToastKind::Success, format!("저장했어요 · v{}", page.summary.version));
            }
            Ok(SaveOutcome::Conflict { current, .. }) => {
                e.conflict = Some(Box::new(current));
                e.show_diff = true;
            }
            Err(err) => {
                drafts::touch(app);
                app.toast_error(&format!(
                    "저장하지 못했어요. 편집 내용은 이 컴퓨터에 초안으로 남아요 ({})",
                    err.message
                ));
            }
        }
    });
}

// ---------------------------------------------------------------------------------------------
// History

fn history(app: &mut App, ui: &mut egui::Ui, cx: &Ctx, st: &mut TabState, page: &Page) {
    let r = PageRef { project_id: cx.project, slug: page.summary.slug.clone() };
    let list = app.hub.query::<WikiHistory>(cx.host, &r);
    theme::card_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.label(RichText::new("변경 이력").font(semibold(13.5)).color(c::TEXT_STRONG));
            ui.label(RichText::new("저장할 때마다 이전 내용을 남겨요").font(font(ty::SMALL)).color(c::TEXT_3));
        });
        ui.add_space(4.0);
        let Some(versions) = list.data.as_deref() else {
            match &list.error {
                Some(e) => super::home::error_text(ui, &e.message),
                None => super::home::loading(ui, "불러오는 중…"),
            }
            return;
        };
        let selected = st.history.flatten();
        for v in versions {
            let on = selected == Some(v.version);
            let resp = widgets::row(ui, 30.0, on, &format!("v{}", v.version));
            let r = resp.rect.shrink2(Vec2::new(10.0, 0.0));
            let p = ui.painter();
            p.text(r.left_center(), egui::Align2::LEFT_CENTER, format!("v{}", v.version), medium(12.5), c::TEXT_STRONG);
            let what = match &v.note {
                Some(n) => format!("{} · {n}", saved_by_label(v.saved_by)),
                None => saved_by_label(v.saved_by).to_string(),
            };
            p.text(
                r.left_center() + Vec2::new(40.0, 0.0),
                egui::Align2::LEFT_CENTER,
                when(v.saved_at),
                font(12.0),
                c::TEXT_3,
            );
            p.text(r.left_center() + Vec2::new(150.0, 0.0), egui::Align2::LEFT_CENTER, what, font(12.0), c::TEXT_2);
            p.text(
                r.right_center(),
                egui::Align2::RIGHT_CENTER,
                format!("+{} −{}", v.added, v.removed),
                crate::theme::mono(11.5),
                c::TEXT_3,
            );
            if resp.clicked() {
                st.history = Some(if on { None } else { Some(v.version) });
            }
        }
        if let Some(v) = selected {
            ui.add_space(8.0);
            let vr = VersionRef { project_id: cx.project, slug: page.summary.slug.clone(), version: v };
            let text = app.hub.query::<WikiVersion>(cx.host, &vr);
            match (&text.data, &text.error) {
                (Some(t), _) => {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(format!("v{v}에서 바뀐 내용")).font(medium(12.5)).color(c::TEXT_2));
                        if v != page.summary.version
                            && theme::secondary_button(ui, "이 버전 내용으로 편집")
                                .on_hover_text("저장하면 새 버전이 돼요. 지금 내용도 이력에 남아요")
                                .clicked()
                        {
                            let mut e = Edit::from(page);
                            e.body = t.body.clone();
                            e.title = t.info.title.clone();
                            app.feat.wiki.edits.insert(key(cx, &page.summary.slug), e);
                            drafts::touch(app);
                            st.history = None;
                        }
                    });
                    egui::Frame::new().corner_radius(CornerRadius::same(6)).fill(c::PANEL_2).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.spacing_mut().item_spacing.y = 0.0;
                        for l in t.diff.iter().filter(|l| l.kind != LineKind::Same).take(400) {
                            super::review::diff_line(ui, l.kind, &l.text);
                        }
                        if t.diff.iter().all(|l| l.kind == LineKind::Same) {
                            ui.label(
                                RichText::new("본문은 그대로이고 제목·태그만 바뀌었어요.")
                                    .font(font(12.0))
                                    .color(c::TEXT_3),
                            );
                        }
                    });
                }
                (None, Some(e)) => super::home::error_text(ui, &e.message),
                (None, None) => super::home::loading(ui, "불러오는 중…"),
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::preview_markdown;

    #[test]
    fn wikilinks_become_links_and_images_text() {
        assert_eq!(
            preview_markdown("앞 [[로그인과 세션]] 뒤 [[deploy|배포]]\n"),
            "앞 [로그인과 세션](<runwick-wiki:로그인과 세션>) 뒤 [배포](<runwick-wiki:deploy>)\n"
        );
        assert_eq!(preview_markdown("![구성도](a.png) 끝\n"), "[그림: 구성도] 끝\n");
        assert_eq!(preview_markdown("```\n[[코드]]\n```\n"), "```\n[[코드]]\n```\n");
        assert_eq!(preview_markdown("[[닫히지 않음\n"), "[[닫히지 않음\n");
    }
}
