//! Conversation records (S07): search with a visible scope, a message with its context, and
//! pins. The right panel holds search and pins; a chosen message opens in the center
//! (`TabKind::History`), which also shows everything at full width when the panel is closed.
//!
//! Other modules call [`pin_terminal_excerpt`] (terminal selection), [`pin_from_session_bar`]
//! (the session bar's 핀 button) and [`open_session_records`].

mod context;
pub(crate) mod fmt;
mod pins;
mod search;

pub use pins::source_line as pins_source_line;

use crate::App;
use crate::client::HostKey;
use crate::shell::{actions, data};
use crate::state::{RightPanel, TabKind};
use crate::theme::{Seg, c, font, ty};
use egui::{Margin, RichText};
use runwick_core::api::projects::{ById, ProjectId, WorkspaceQuery, WorkspacesList};
use runwick_core::api::records::{Anchor, Context, MessageId, NewPin, PinId, PinSource, PinsCreate};
use runwick_core::api::sessions::{ScreenQuery, SessionId, SessionsGet, SessionsScreenText, Tool};
use std::sync::Arc;
use std::time::Instant;

/// Where search looks. The chosen scope is remembered ("마지막 범위").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ScopeKind {
    Session,
    #[default]
    Project,
    All,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DateRange {
    #[default]
    All,
    Today,
    Week,
    Month,
}

impl DateRange {
    fn label(self) -> &'static str {
        match self {
            DateRange::All => "전체",
            DateRange::Today => "오늘",
            DateRange::Week => "최근 7일",
            DateRange::Month => "최근 30일",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    Search,
    Pins,
}

/// The message (or session end) being read, with how much context is loaded.
#[derive(Debug, Clone, PartialEq)]
pub struct Opened {
    pub host: HostKey,
    pub anchor: Anchor,
    pub before: u32,
    pub after: u32,
    /// Words to highlight (from the search that led here).
    pub terms: Vec<String>,
}

/// Title and memo being edited for a pin.
#[derive(Debug, Clone)]
pub struct PinDraft {
    pub host: HostKey,
    pub id: PinId,
    pub title: String,
    pub note: String,
    /// Move keyboard focus to the title once.
    pub focus: bool,
}

#[derive(Default)]
pub struct State {
    pub mode: Mode,
    pub text: String,
    /// Text the last search was made with (debounced).
    sent: String,
    edited: Option<Instant>,
    pub scope: ScopeKind,
    pub date: DateRange,
    pub tool: Option<Tool>,
    pub pinned_only: bool,
    /// Pages of results asked for ("더 보기").
    pub pages: u32,
    /// Show only results from one computer.
    pub host_filter: Option<HostKey>,
    pub open: Option<Opened>,
    /// Last context shown, kept while a longer read loads.
    shown: Option<Arc<Context>>,
    /// Anchor the reader last scrolled to (scroll once per newly opened anchor).
    scrolled: Option<Anchor>,
    pub edit_pin: Option<PinDraft>,
    pub confirm_unpin: Option<PinId>,
    /// Message being cut down to an excerpt pin.
    pub excerpt: Option<MessageId>,
    /// Message under the pointer last frame (its actions are shown).
    hover_msg: Option<MessageId>,
    /// Message whose row or buttons had keyboard focus last frame (its actions are shown).
    keyboard_msg: Option<MessageId>,
    /// Message chosen with Enter: its actions sit under it instead of the anchor's.
    select_msg: Option<MessageId>,
    /// Put keyboard focus on this message's first action button once.
    focus_actions: Option<MessageId>,
    pub focus_search: bool,
    /// Pin to scroll to and outline once.
    pub highlight_pin: Option<PinId>,
    /// The panel was drawn last frame (focus the field when it opens).
    panel_seen: u64,
    /// `RUNWICK_HISTORY` was applied (verification aid, see [`apply_verification_state`]).
    env_applied: bool,
}

/// Verification aid: `RUNWICK_HISTORY='{"text":"만료","mode":"pins","open":"<message id>"}'`
/// starts with that query, list and opened message (feature state is not part of `UiState`).
fn apply_verification_state(st: &mut State) {
    if std::mem::replace(&mut st.env_applied, true) {
        return;
    }
    let Some(v) = hook_env!("RUNWICK_HISTORY").and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()) else {
        return;
    };
    if let Some(t) = v.get("text").and_then(|t| t.as_str()) {
        st.text = t.to_string();
        st.sent = t.trim().to_string();
        st.pages = 1;
    }
    if v.get("mode").and_then(|m| m.as_str()) == Some("pins") {
        st.mode = Mode::Pins;
    }
    match v.get("scope").and_then(|m| m.as_str()) {
        Some("session") => st.scope = ScopeKind::Session,
        Some("all") => st.scope = ScopeKind::All,
        _ => {}
    }
    if let Some(id) = v.get("open").and_then(|o| o.as_str()).and_then(|o| o.parse().ok()) {
        let terms = fmt::query_terms(&st.sent);
        st.open = Some(Opened { host: HostKey::Local, anchor: Anchor::Message { id }, before: 6, after: 6, terms });
    }
}

/// What the current scope points at.
#[derive(Debug, Clone)]
pub struct ScopeCtx {
    /// Host of the selected workspace (Local when none).
    pub host: HostKey,
    pub session: Option<(SessionId, String)>,
    pub project: Option<(ProjectId, String)>,
}

pub fn scope_ctx(app: &mut App) -> ScopeCtx {
    let ws = app.ui.selected_ws;
    let host = ws.map(|w| w.host).unwrap_or(HostKey::Local);
    let session = match app.ui.active_tab().map(|t| t.kind.clone()) {
        Some(TabKind::Terminal { host: h, session }) if h == host => {
            let title = ws
                .and_then(|w| data::sessions_in(app, w).data)
                .and_then(|l| l.iter().find(|s| s.session.id == session).map(|s| s.session.title.clone()))
                .unwrap_or_else(|| "현재 세션".into());
            Some((session, title))
        }
        _ => None,
    };
    let project = match (host, ws) {
        (HostKey::Local, _) => app.ui.selected_project.map(|p| {
            let name = data::projects(app).data.and_then(|l| data::project_of(&l, p).map(|p| p.name.clone()));
            (p, name.unwrap_or_else(|| "프로젝트".into()))
        }),
        (device, Some(w)) => {
            // The project id inside the other computer's records.
            let remote = app.hub.query::<WorkspacesList>(device, &WorkspaceQuery::default()).data;
            let pid = remote.and_then(|l| l.iter().find(|x| x.id == w.id).map(|x| x.project_id));
            let name = data::workspaces(app)
                .data
                .and_then(|l| data::find_ws(&l, &w).map(|x| x.project_id))
                .and_then(|lp| data::projects(app).data.and_then(|l| data::project_of(&l, lp).map(|p| p.name.clone())));
            pid.map(|p| (p, name.unwrap_or_else(|| "프로젝트".into())))
        }
        _ => None,
    };
    ScopeCtx { host, session, project }
}

/// Short name of a computer for result badges.
pub fn host_label(app: &App, host: HostKey) -> String {
    match host {
        HostKey::Local => "이 컴퓨터".into(),
        HostKey::Device(id) => crate::remote::device_name_cached(app, id)
            .or_else(|| app.hub.state(host).info().map(|i| i.device_name.clone()))
            .unwrap_or_else(|| "다른 기기".into()),
    }
}

/// Hosts answering right now (this computer first).
pub fn connected_hosts(app: &App) -> Vec<HostKey> {
    let mut v: Vec<HostKey> = app.hub.hosts().filter(|c| c.state().is_connected()).map(|c| c.key).collect();
    v.sort();
    v
}

/// Right panel (S07): search or pins.
pub fn show_panel(app: &mut App, ui: &mut egui::Ui) {
    apply_verification_state(&mut app.feat.history);
    let frame = ui.ctx().cumulative_pass_nr();
    if app.feat.history.panel_seen + 1 < frame && app.feat.history.mode == Mode::Search {
        app.feat.history.focus_search = true;
    }
    app.feat.history.panel_seen = frame;
    // Without a workspace there is no center to read in: read inside the panel.
    if app.ui.selected_ws.is_none() && app.feat.history.open.is_some() {
        egui::Frame::new().inner_margin(Margin::same(12)).show(ui, |ui| {
            if fmt::quiet_link(ui, "← 검색 결과").clicked() {
                app.feat.history.open = None;
            }
        });
        context::show(app, ui, true);
        return;
    }
    column(app, ui);
}

/// `TabKind::History`: the chosen message at full width, with search beside it when the
/// panel is closed.
pub fn show_tab(app: &mut App, ui: &mut egui::Ui, _tab: crate::state::TabId) {
    apply_verification_state(&mut app.feat.history);
    let rect = ui.max_rect();
    ui.painter().rect_filled(rect, egui::CornerRadius::ZERO, c::DOC);
    if app.ui.right == Some(RightPanel::History) || rect.width() < 760.0 {
        context::show(app, ui, false);
        return;
    }
    let left_w = 380.0_f32.min(rect.width() * 0.45);
    let left = egui::Rect::from_min_size(rect.min, egui::vec2(left_w, rect.height()));
    let right = egui::Rect::from_min_max(egui::pos2(left.right(), rect.top()), rect.max);
    ui.painter().rect_filled(left, egui::CornerRadius::ZERO, c::PANEL);
    ui.painter().vline(left.right(), rect.y_range(), egui::Stroke::new(1.0, c::LINE));
    ui.scope_builder(egui::UiBuilder::new().max_rect(left).id_salt("history-left"), |ui| {
        ui.set_clip_rect(left);
        column(app, ui);
    });
    ui.scope_builder(egui::UiBuilder::new().max_rect(right).id_salt("history-right"), |ui| {
        ui.set_clip_rect(right);
        context::show(app, ui, false);
    });
}

/// Mode switch plus search or pins.
fn column(app: &mut App, ui: &mut egui::Ui) {
    let pin_count = pins::count(app);
    let pins_label = match pin_count {
        Some(n) => format!("핀 {n}"),
        None => "핀".into(),
    };
    egui::Frame::new().inner_margin(Margin { left: 12, right: 12, top: 12, bottom: 0 }).show(ui, |ui| {
        let sel = if app.feat.history.mode == Mode::Search { 0 } else { 1 };
        let full = ui.available_width();
        ui.allocate_ui(egui::vec2(full, 30.0), |ui| {
            if let Some(i) = crate::theme::segmented(ui, &[Seg::text("검색"), Seg::text(&pins_label)], sel, 26.0) {
                app.feat.history.mode = if i == 0 { Mode::Search } else { Mode::Pins };
                app.feat.history.focus_search = i == 0;
            }
        });
    });
    match app.feat.history.mode {
        Mode::Search => search::show(app, ui),
        Mode::Pins => pins::show(app, ui),
    }
}

/// Title of the `TabKind::History` tab: "기록: <session>" while a message is being read.
pub fn tab_title(app: &App) -> String {
    let st = &app.feat.history;
    match (&st.open, &st.shown) {
        (Some(o), Some(cx)) if context::same_session(cx, o) => format!("기록: {}", cx.session_title),
        _ => "대화 검색".into(),
    }
}

/// "위키 초안으로" from a pin or message of `session`: the wiki of the session's project opens
/// with a new page that quotes `seed`. Without a session (or when it is gone) the selected
/// project is used.
pub(crate) fn draft_to_wiki(app: &mut App, host: HostKey, session: Option<SessionId>, seed: crate::views::wiki::Seed) {
    let fallback = move |app: &mut App, seed| match (app.ui.selected_project, app.ui.selected_ws) {
        (Some(p), Some(ws)) if ws.host == host => crate::views::wiki::new_page_from(app, p, ws, seed),
        _ => app.toast_error("이 기록의 프로젝트를 찾을 수 없어요. 프로젝트를 고른 뒤 다시 해 보세요"),
    };
    let Some(id) = session else {
        fallback(app, seed);
        return;
    };
    app.hub.call::<SessionsGet>(host, ById { id }, move |app, res| {
        let key = res.ok().map(|info| crate::state::WsKey { host, id: info.session.workspace_id });
        let place = key.and_then(|k| {
            let list = data::workspaces(app).data?;
            data::find_ws(&list, &k).map(|w| (w.project_id, k))
        });
        match place {
            Some((project, ws)) => crate::views::wiki::new_page_from(app, project, ws, seed),
            None => fallback(app, seed),
        }
    });
}

/// Open a message with its context (in the center when a workspace is selected).
pub fn open_message(app: &mut App, host: HostKey, id: MessageId, terms: Vec<String>) {
    open(app, Opened { host, anchor: Anchor::Message { id }, before: 6, after: 6, terms });
}

/// The session bar's 기록: search this session's records in the panel.
pub fn toggle_from_session_bar(app: &mut App) {
    if app.ui.right != Some(RightPanel::History) {
        app.feat.history.scope = ScopeKind::Session;
        app.feat.history.mode = Mode::Search;
    }
    actions::toggle_right(app, RightPanel::History);
}

/// Open the newest messages of a session ("이 세션 기록").
pub fn open_session_records(app: &mut App, host: HostKey, session: SessionId) {
    open(app, Opened { host, anchor: Anchor::SessionEnd { id: session }, before: 30, after: 0, terms: Vec::new() });
}

fn open(app: &mut App, o: Opened) {
    app.feat.history.open = Some(o);
    app.feat.history.excerpt = None;
    app.feat.history.select_msg = None;
    if app.ui.selected_ws.is_some() {
        actions::open_center_tab(app, TabKind::History);
    } else {
        app.ui.right = Some(RightPanel::History);
    }
}

/// Show the pins list with one pin outlined (and its title ready to edit when `edit`).
pub fn show_pin(app: &mut App, host: HostKey, pin: &runwick_core::api::records::Pin, edit: bool) {
    let st = &mut app.feat.history;
    st.mode = Mode::Pins;
    st.highlight_pin = Some(pin.id);
    if edit {
        st.edit_pin =
            Some(PinDraft { host, id: pin.id, title: pin.title.clone(), note: pin.note.clone(), focus: true });
    }
    app.ui.right = Some(RightPanel::History);
}

fn create_pin(app: &mut App, host: HostKey, source: PinSource, done: &'static str) {
    app.hub.call::<PinsCreate>(host, NewPin { source, title: None, note: None }, move |app, res| match res {
        Ok(pin) => {
            app.toast(crate::state::ToastKind::Success, done);
            show_pin(app, host, &pin, true);
        }
        Err(e) => app.toast_error(&format!("핀을 고정하지 못했어요: {}", e.message)),
    });
}

/// Pin text selected in a terminal ("터미널 발췌": not linked to a message).
pub fn pin_terminal_excerpt(app: &mut App, host: HostKey, session: SessionId, text: String) {
    create_pin(app, host, PinSource::Terminal { session_id: session, text }, "터미널 발췌를 핀에 고정했어요");
}

/// The session bar's 핀: the newest collected answer of an agent session; for shells (or when
/// no answer is collected yet) the visible terminal screen as a terminal excerpt.
pub fn pin_from_session_bar(app: &mut App, host: HostKey, session: SessionId, tool: Tool) {
    if tool.is_agent() {
        let src = PinSource::LatestAnswer { session_id: session };
        app.hub.call::<PinsCreate>(host, NewPin { source: src, title: None, note: None }, move |app, res| match res {
            Ok(pin) => {
                app.toast(crate::state::ToastKind::Success, "마지막 응답을 핀에 고정했어요");
                show_pin(app, host, &pin, true);
            }
            Err(e) if e.code == runwick_core::rpc::codes::NOT_FOUND => pin_screen(app, host, session),
            Err(e) => app.toast_error(&format!("핀을 고정하지 못했어요: {}", e.message)),
        });
    } else {
        pin_screen(app, host, session);
    }
}

fn pin_screen(app: &mut App, host: HostKey, session: SessionId) {
    app.hub.call::<SessionsScreenText>(host, ScreenQuery { id: session, scrollback: 0 }, move |app, res| match res {
        Ok(text) if !text.trim().is_empty() => create_pin(
            app,
            host,
            PinSource::Terminal { session_id: session, text },
            "지금 터미널 화면을 발췌해 핀에 고정했어요",
        ),
        Ok(_) => app.toast_info("고정할 터미널 내용이 없어요"),
        Err(e) => app.toast_error(&format!("터미널 화면을 읽지 못했어요: {}", e.message)),
    });
}

/// Muted one-line note (empty and info states).
fn note(ui: &mut egui::Ui, text: &str) {
    ui.add(egui::Label::new(RichText::new(text).font(font(ty::SMALL)).color(c::TEXT_3)).wrap());
}
