//! Project wiki (S08). `TabKind::Wiki` shows the page list ([`home`]) or one page with its
//! proposals and history ([`page`], [`review`]); `TabKind::WikiGraph` shows relationships
//! ([`graph`]). Pages live on the host of the tab's workspace. The selected page lives here per
//! tab; edit buffers live per page and are kept as drafts on this computer ([`drafts`]), so
//! switching tabs, losing the connection or restarting the app keeps unsaved text.

mod dialogs;
mod drafts;
mod graph;
mod home;
mod page;
mod review;

use crate::App;
use crate::client::HostKey;
use crate::icons;
use crate::shell::{actions, data};
use crate::state::{TabId, TabKind};
use crate::theme::{Tone, c, font};
use runwick_core::api::jobs::{JobKind, JobQuery, JobRef, JobStatus, JobsList, JobsRequest, JobsStop, RequestJob};
use runwick_core::api::projects::ProjectId;
use runwick_core::api::wiki::*;
use std::collections::{BTreeSet, HashMap, HashSet};

#[derive(Default)]
pub struct State {
    tabs: HashMap<TabId, TabState>,
    graphs: HashMap<TabId, graph::GraphState>,
    md: egui_commonmark::CommonMarkCache,
    dialog: Option<dialogs::Dialog>,
    edits: HashMap<page::EditKey, page::Edit>,
    drafts: drafts::State,
}

#[derive(Default)]
pub(crate) struct TabState {
    init: bool,
    /// Open page (None: the list).
    page: Option<String>,
    /// A new-page proposal being reviewed from the list.
    review: Option<ProposalId>,
    search: String,
    all_projects: bool,
    topic: Option<String>,
    /// Title of the open page, for the tab.
    title: Option<String>,
    /// Change history is open; with a version selected to compare.
    history: Option<Option<u32>>,
    /// Hunks chosen for 부분 적용, per proposal.
    partial: HashMap<ProposalId, BTreeSet<u32>>,
    /// Proposal actions waiting for the host.
    busy: HashSet<ProposalId>,
}

/// Where a wiki tab reads from.
#[derive(Clone)]
pub(crate) struct Ctx {
    host: HostKey,
    project: ProjectId,
    tab: TabId,
    project_name: String,
    connected: bool,
    /// Device name when the project lives on another computer.
    device: Option<String>,
}

fn ctx_for(app: &mut App, project: ProjectId, tab: TabId) -> Ctx {
    let host = app.ui.find_tab(tab).map(|(ws, _)| ws.host).unwrap_or(HostKey::Local);
    let project_name = data::projects(app)
        .data
        .and_then(|l| data::project_of(&l, project).map(|p| p.name.clone()))
        .unwrap_or_else(|| "프로젝트".into());
    let device = match host {
        HostKey::Local => None,
        HostKey::Device(d) => Some(crate::remote::device_name(app, d)),
    };
    Ctx { host, project, tab, project_name, connected: app.hub.state(host).is_connected(), device }
}

pub fn show_tab(app: &mut App, ui: &mut egui::Ui, project: ProjectId, page: Option<&str>, tab: TabId) {
    let cx = ctx_for(app, project, tab);
    drafts::restore(app);
    drafts::tick(app, ui.ctx());
    let mut st = app.feat.wiki.tabs.remove(&tab).unwrap_or_default();
    if !st.init {
        st.init = true;
        st.page = page.map(str::to_string);
    }
    egui::Frame::new().fill(c::DOC).show(ui, |ui| {
        ui.set_min_size(ui.available_size());
        if st.review.is_some() {
            review::show_new_page(app, ui, &cx, &mut st);
        } else if st.page.is_some() {
            page::show(app, ui, &cx, &mut st);
        } else {
            st.title = None;
            home::show(app, ui, &cx, &mut st);
        }
    });
    app.feat.wiki.tabs.insert(tab, st);
    dialogs::show(app, ui.ctx());
}

pub fn show_graph_tab(app: &mut App, ui: &mut egui::Ui, project: ProjectId, tab: TabId) {
    let cx = ctx_for(app, project, tab);
    let mut gs = app.feat.wiki.graphs.remove(&tab).unwrap_or_default();
    egui::Frame::new().fill(c::DOC).show(ui, |ui| {
        ui.set_min_size(ui.available_size());
        graph::show(app, ui, &cx, &mut gs);
    });
    app.feat.wiki.graphs.insert(tab, gs);
    dialogs::show(app, ui.ctx());
}

/// Host events: bring back wiki drafts once this computer's host is connected.
pub fn on_event(app: &mut App, host: HostKey, topic: &str, _data: &serde_json::Value) {
    if host == HostKey::Local && topic == crate::client::EVENT_CONN_READY {
        drafts::restore(app);
    }
}

/// Keep unsaved wiki edits for the next start (the window is closing).
pub fn save_drafts(app: &mut App) {
    drafts::flush(app);
}

/// A tab's state dot: tone, words, pulse.
pub type TabDot = (Tone, &'static str, bool);

/// Tab title and dot for a wiki tab: "위키: <page title>" while a page is open, and a draft
/// marker when that page has unsaved text on this computer.
pub fn tab_look(app: &App, tab: TabId) -> Option<(String, Option<TabDot>)> {
    let st = app.feat.wiki.tabs.get(&tab)?;
    let slug = st.page.as_ref()?;
    let title = format!("위키: {}", st.title.clone().unwrap_or_else(|| slug.clone()));
    let host = app.ui.find_tab(tab).map(|(ws, _)| ws.host).unwrap_or(HostKey::Local);
    let project = match app.ui.find_tab(tab).map(|(_, t)| &t.kind) {
        Some(TabKind::Wiki { project, .. }) => *project,
        _ => return Some((title, None)),
    };
    let dot = app.feat.wiki.edits.get(&(host, project, slug.clone())).filter(|e| e.dirty()).map(|e| {
        if e.draft_at.is_some() || !app.hub.state(host).is_connected() {
            (Tone::Direct, "이 컴퓨터의 초안 · 연결되면 저장", false)
        } else {
            (Tone::Neutral, "미저장", false)
        }
    });
    Some((title, dot))
}

/// Link scheme of a wiki body's link back to a conversation message (`[출처](runwick-record:<id>)`).
pub(crate) const RECORD_SCHEME: &str = "runwick-record:";

/// Words from the records to start a page with ("위키 초안으로" on a message or a pin).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Seed {
    /// Suggested page title.
    pub title: String,
    /// Markdown under the title: the quoted words and where they came from.
    pub body: String,
    /// Where the words came from, for the dialog.
    pub source: String,
    /// The session the words come from (kept as the page's 근거 세션).
    pub session: Option<uuid::Uuid>,
}

impl Seed {
    /// Quote `text` from `session`, ending with a source line that links back to `message` when
    /// there is one.
    pub(crate) fn quote(
        title: &str,
        text: &str,
        source: &str,
        session: Option<uuid::Uuid>,
        message: Option<uuid::Uuid>,
    ) -> Seed {
        let quoted: Vec<String> =
            text.trim().lines().map(|l| if l.trim().is_empty() { ">".to_string() } else { format!("> {l}") }).collect();
        let line = match message {
            Some(id) => {
                let label = source.replace('[', "\\[").replace(']', "\\]");
                format!("출처: [{label}]({RECORD_SCHEME}{id})")
            }
            None => format!("출처: {source}"),
        };
        Seed {
            title: title_hint(title),
            body: format!("{}\n\n{line}\n", quoted.join("\n")),
            source: source.to_string(),
            session,
        }
    }
}

/// A page title from the first line of some text (like a pin's default title): Markdown
/// markers dropped, at most 40 characters, cut between words.
fn title_hint(text: &str) -> String {
    const MAX: usize = 40;
    let line = text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    let line = line.trim_start_matches(['#', '>', '-', '*', ' ']).replace("**", "").replace('`', "");
    let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() <= MAX {
        return line;
    }
    let cut: String = line.chars().take(MAX).collect();
    match cut.rfind(' ') {
        Some(i) if cut[..i].chars().count() > MAX / 2 => cut[..i].trim_end_matches([',', '.', '·']).to_string(),
        _ => cut,
    }
}

/// "위키 초안으로" (S07 → S08): open the project's wiki with "새 문서" filled from records.
/// The page is made only when the person presses 만들기 in the dialog.
pub(crate) fn new_page_from(app: &mut App, project: ProjectId, ws: crate::state::WsKey, seed: Seed) {
    let same_place = app.ui.selected_project == Some(project) && app.ui.selected_ws.is_some_and(|w| w.host == ws.host);
    if !same_place {
        actions::select_workspace(app, project, ws);
    }
    actions::open_center_tab(app, TabKind::Wiki { project, page: None });
    let Some(tab) = app.ui.current_tabset().and_then(|s| s.active) else {
        return;
    };
    let cx = ctx_for(app, project, tab);
    dialogs::open_seeded(app, &cx, seed);
}

/// Open a page in the wiki tab of the selected workspace (creating the tab if needed).
pub(crate) fn open_page(app: &mut App, project: ProjectId, slug: &str) {
    actions::open_center_tab(app, TabKind::Wiki { project, page: None });
    let Some(id) = app.ui.current_tabset().and_then(|s| s.active) else {
        return;
    };
    let st = app.feat.wiki.tabs.entry(id).or_default();
    st.init = true;
    st.page = Some(slug.to_string());
    st.review = None;
    st.history = None;
}

// ---------------------------------------------------------------------------------------------
// Labels

/// How a page came to be and whether it waits for review: tone, icon, words.
pub(crate) fn page_state(p: &PageSummary) -> (Tone, &'static str, String) {
    if p.flagged_proposals > 0 {
        return (Tone::Check, icons::CIRCLE_HELP, format!("제안 {}건 · 확인 필요", p.open_proposals));
    }
    if p.status == PageStatus::Draft {
        return (Tone::Info, icons::SPARKLES, "AI 초안 · 검토 전".into());
    }
    if p.open_proposals > 0 {
        return (Tone::Info, icons::FILE_DIFF, format!("제안 {}건", p.open_proposals));
    }
    match p.created_by {
        Author::Import => (Tone::Neutral, icons::IMPORT, "가져온 문서".into()),
        Author::Ai => (Tone::Neutral, icons::SPARKLES, "AI 작성 · 검토함".into()),
        Author::User => (Tone::Neutral, icons::PENCIL, "직접 작성".into()),
    }
}

pub(crate) fn author_label(p: &PageSummary) -> &'static str {
    match (p.created_by, p.status) {
        (Author::Ai, PageStatus::Draft) => "AI 초안",
        (Author::Ai, PageStatus::Reviewed) => "AI 작성 · 검토함",
        (Author::Import, _) => "가져온 문서",
        (Author::User, _) => "직접 작성",
    }
}

pub(crate) fn saved_by_label(s: SavedBy) -> &'static str {
    match s {
        SavedBy::User => "직접 편집",
        SavedBy::External => "파일에서 직접 고침",
        SavedBy::Ai => "AI 초안",
        SavedBy::Import => "가져옴",
        SavedBy::Proposal => "제안 적용",
    }
}

pub(crate) fn when(ts: runwick_core::Ts) -> String {
    crate::views::history::fmt::when(ts)
}

/// Resolve a written link target (`[[제목]]`, `slug`, `slug.md`) to a page of the list.
pub(crate) fn resolve<'a>(target: &str, pages: &'a [PageSummary]) -> Option<&'a PageSummary> {
    let t = target.trim().trim_end_matches(".md");
    let t = t.rsplit('/').next().unwrap_or(t);
    let lower = t.to_lowercase();
    let slug = slugify(t);
    pages
        .iter()
        .find(|p| p.slug == t)
        .or_else(|| pages.iter().find(|p| p.slug == slug))
        .or_else(|| pages.iter().find(|p| p.title.to_lowercase() == lower))
}

/// Short text for "인계에 담기": what the page is, where it lives and what it is based on.
pub(crate) fn handover(page: &Page, project: &str, sessions: &HashMap<uuid::Uuid, String>) -> String {
    let p = &page.summary;
    let mut s = format!("[위키] {} — {project} (v{} · {} 수정)\n", p.title, p.version, when(p.updated_at));
    s.push_str(&format!("파일: {}\n", page.path));
    if !p.excerpt.is_empty() {
        s.push_str(&format!("요약: {}\n", p.excerpt));
    }
    if !p.sources.is_empty() {
        let names: Vec<String> =
            p.sources.iter().map(|id| sessions.get(id).cloned().unwrap_or_else(|| "기록 없는 세션".into())).collect();
        s.push_str(&format!("근거 세션: {}\n", names.join(", ")));
    }
    s
}

// ---------------------------------------------------------------------------------------------
// Shared actions

fn act_error(app: &mut App, e: &runwick_core::RpcError) {
    app.toast_error(&e.message);
}

/// "지금 정리": run the project's wiki proposal job now (also on battery).
pub(crate) fn request_tidy(app: &mut App, cx: &Ctx) {
    let req = RequestJob { kind: JobKind::WikiProposals, project_id: cx.project, target: Default::default() };
    app.hub.call::<JobsRequest>(cx.host, req, |app, res| match res {
        Ok(_) => app.toast_info("위키 정리를 시작했어요. 끝나면 검토할 것에 제안이 보여요"),
        Err(e) => act_error(app, &e),
    });
}

pub(crate) fn stop_job(app: &mut App, host: HostKey, id: uuid::Uuid) {
    app.hub.call::<JobsStop>(host, JobRef { id }, |app, res| {
        if let Err(e) = res {
            act_error(app, &e);
        }
    });
}

/// The project's active wiki job (queued, running or held), if any.
pub(crate) fn active_wiki_job(app: &mut App, cx: &Ctx) -> Option<runwick_core::api::jobs::Job> {
    let q = JobQuery { project_id: Some(cx.project), active_only: true, limit: 20 };
    let jobs = app.hub.query::<JobsList>(cx.host, &q).data?;
    jobs.iter().find(|j| matches!(j.kind, JobKind::WikiProposals | JobKind::WikiDraft) && j.status.is_active()).cloned()
}

/// Header control for "지금 정리": the button, or the running job with 멈추기.
pub(crate) fn tidy_control(app: &mut App, ui: &mut egui::Ui, cx: &Ctx) {
    match active_wiki_job(app, cx) {
        Some(job) => {
            let (tone, text) = match job.status {
                JobStatus::Running => (Tone::Running, format!("{} 중", job.kind.label())),
                JobStatus::Held => {
                    (Tone::Waiting, format!("보류 · {}", job.held_reason.as_deref().unwrap_or("조건 대기")))
                }
                _ => (Tone::Waiting, "정리 대기 중".to_string()),
            };
            crate::theme::chip(ui, tone, &text);
            if job.status == JobStatus::Held {
                if crate::shell::widgets::pill_button(ui, Some(icons::SPARKLES), "지금 정리", 30.0, c::CONTROL)
                    .on_hover_text("배터리에서도 바로 실행해요 · 구독 사용량을 씁니다")
                    .clicked()
                {
                    request_tidy(app, cx);
                }
            } else if crate::theme::icon_button(ui, icons::CIRCLE_STOP, "정리 멈추기").clicked() {
                stop_job(app, cx.host, job.id);
            }
        }
        None => {
            let resp = crate::shell::widgets::pill_button(ui, Some(icons::SPARKLES), "지금 정리", 30.0, c::CONTROL)
                .on_hover_text(
                    "새 대화 기록을 읽고 위키 변경 제안을 만들어요. 문서는 바로 바뀌지 않아요 · 구독 사용량을 씁니다",
                );
            if resp.clicked() {
                request_tidy(app, cx);
            }
        }
    }
}

/// Banner when the wiki's computer is not connected: what is shown is the last copy received.
pub(crate) fn offline_banner(ui: &mut egui::Ui, cx: &Ctx) {
    if cx.connected {
        return;
    }
    let who = cx.device.clone().unwrap_or_else(|| "이 컴퓨터의 Runwick".into());
    crate::theme::card_frame().fill(c::YELLOW_BG).show(ui, |ui| {
        ui.horizontal_wrapped(|ui| {
            ui.label(crate::theme::icon(icons::TRIANGLE_ALERT, 13.0, c::YELLOW));
            ui.label(
                egui::RichText::new(format!("{who}에 연결되어 있지 않아 마지막으로 받은 내용을 보여 줘요."))
                    .font(crate::theme::medium(12.5))
                    .color(c::YELLOW),
            );
            ui.label(
                egui::RichText::new("고친 내용은 이 창에 남겨 두고, 연결되면 저장할 수 있어요.")
                    .font(font(12.0))
                    .color(c::TEXT_2),
            );
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_quotes_the_words_and_links_the_source() {
        let id = uuid::Uuid::nil();
        let s = Seed::quote(
            "세션 만료 원인",
            "만료 시각은 초 단위예요.\n\n밀리초와 비교하고 있어요.",
            "로그인 오류 [수정] · Claude Code · 2026년 11월 6일 11:04",
            Some(id),
            Some(id),
        );
        assert_eq!(s.title, "세션 만료 원인");
        assert_eq!(
            s.body,
            format!(
                "> 만료 시각은 초 단위예요.\n>\n> 밀리초와 비교하고 있어요.\n\n출처: [로그인 오류 \\[수정\\] · Claude Code · 2026년 11월 6일 11:04]({RECORD_SCHEME}{id})\n"
            )
        );
        // The link back is not a page link, so the wiki graph leaves it alone.
        assert!(!page::preview_markdown(&s.body).contains("runwick-wiki:"));
        let t =
            Seed::quote("터미널", "FAIL session.test.ts", "로그인 · 터미널 발췌 · 2026년 11월 6일 11:05", None, None);
        assert!(t.body.ends_with("출처: 로그인 · 터미널 발췌 · 2026년 11월 6일 11:05\n"), "{}", t.body);
    }

    #[test]
    fn titles_come_from_the_first_line() {
        assert_eq!(title_hint("\n## 원인을 찾았습니다\n본문"), "원인을 찾았습니다");
        assert_eq!(title_hint("- **쿠키** 만료 `exp`"), "쿠키 만료 exp");
        assert_eq!(title_hint("expiresAt * 1000  < now"), "expiresAt * 1000 < now");
        let long = "로그인 직후 세션이 만료되는 오류를 고쳐 주세요. 필요하면 Codex에게 검토도 맡겨 주세요";
        let t = title_hint(long);
        assert!(t.chars().count() <= 40, "{t}");
        assert!(long.starts_with(&t) && !t.ends_with(' '), "{t}");
    }
}
