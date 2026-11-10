//! Record search, session pins and independent panel/center readers.

mod context;
pub(crate) mod fmt;
mod pins;
mod search;

pub use pins::source_line as pins_source_line;
pub(crate) use pins::toggle_button as pin_toggle_button;
pub(crate) use pins::{commit_title, finish_title_frame};

use crate::App;
use crate::client::HostKey;
use crate::shell::{actions, data};
use crate::state::{PanelDetail, PanelTarget, RightPanel};
use crate::theme::{c, font, ty};
use egui::RichText;
use runwick_core::api::projects::{ById, ProjectId};
use runwick_core::api::records::{
    Anchor, Context, MessageId, NewPin, Pin, PinId, PinKind, PinQuery, PinSource, PinsCreate, PinsList, RecordsTurn,
};
use runwick_core::api::sessions::{SessionId, SessionsGet, Tool};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

/// Where search looks. The chosen scope is remembered ("마지막 범위").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ScopeKind {
    #[default]
    Session,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinField {
    Title,
    Note,
}

/// Only the field explicitly chosen by the person is edited.
#[derive(Debug, Clone)]
pub struct PinDraft {
    pub host: HostKey,
    pub id: PinId,
    pub field: PinField,
    pub text: String,
    pub original: String,
    /// Move keyboard focus to the field once.
    pub focus: bool,
}

#[derive(Default)]
struct Reader {
    pub open: Option<Opened>,
    /// Last context shown, kept while a longer read loads.
    shown: Option<Arc<Context>>,
    /// Anchor the reader last scrolled to (scroll once per newly opened anchor).
    scrolled: Option<Anchor>,
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
}

#[derive(Default)]
pub struct State {
    reader: Reader,
    readers: HashMap<String, Reader>,
    pub text: String,
    /// Text the last search was made with (debounced).
    sent: String,
    edited: Option<Instant>,
    pub scope: ScopeKind,
    pub date: DateRange,
    pub tool: Option<Tool>,
    /// Pages of results asked for ("더 보기").
    pub pages: u32,
    /// Show only results from one computer.
    pub host_filter: Option<HostKey>,
    pub edit_pin: Option<PinDraft>,
    pub(crate) title_drawn: bool,
    pending_pins: HashSet<(HostKey, MessageId)>,
    pub confirm_unpin: Option<PinId>,
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
fn apply_verification_state(app: &mut App) {
    let st = &mut app.feat.history;
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
    match v.get("scope").and_then(|m| m.as_str()) {
        Some("session") => st.scope = ScopeKind::Session,
        Some("all") => st.scope = ScopeKind::All,
        _ => {}
    }
    let tab =
        if v.get("mode").and_then(|m| m.as_str()) == Some("pins") { RightPanel::Pins } else { RightPanel::History };
    let open = v.get("open").and_then(|o| o.as_str()).and_then(|o| o.parse().ok()).map(|id| Opened {
        host: HostKey::Local,
        anchor: Anchor::Message { id },
        before: 6,
        after: 6,
        terms: fmt::query_terms(&st.sent),
    });
    app.ui.show_panel_root(tab, None);
    if let Some(open) = open {
        open_in(app, open, tab);
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
    let target =
        app.ui.panel_target().or_else(|| app.ui.focused_session().map(|(host, session)| PanelTarget { host, session }));
    let host = target.map(|t| t.host).or(app.ui.selected_ws.map(|w| w.host)).unwrap_or(HostKey::Local);
    if let Some(t) = target {
        let info = app.hub.query::<SessionsGet>(host, &ById { id: t.session }).data;
        let project = info.as_ref().map(|s| {
            let place = crate::views::collab::place(app, host, &s.session);
            (s.session.project_id, place.project)
        });
        return ScopeCtx {
            host,
            session: Some((
                t.session,
                info.as_ref().map(|s| s.session.title.clone()).unwrap_or_else(|| "현재 세션".into()),
            )),
            project,
        };
    }
    let project = app.ui.selected_project.map(|p| {
        let name = data::projects(app).data.and_then(|l| data::project_of(&l, p).map(|p| p.name.clone()));
        (p, name.unwrap_or_else(|| "프로젝트".into()))
    });
    ScopeCtx { host, session: None, project }
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

pub fn show_panel(app: &mut App, ui: &mut egui::Ui) {
    apply_verification_state(app);
    let frame = ui.ctx().cumulative_pass_nr();
    if app.feat.history.panel_seen + 1 < frame {
        app.feat.history.focus_search = true;
    }
    app.feat.history.panel_seen = frame;
    search::show(app, ui);
}

pub fn show_pins(app: &mut App, ui: &mut egui::Ui) {
    if app.ui.panel_target().is_none() {
        crate::shell::widgets::empty_note(ui, "세션을 고르면 고정한 핀을 볼 수 있어요");
        return;
    }
    pins::show(app, ui);
}

fn read(app: &mut App, ui: &mut egui::Ui, key: String, open: Opened, in_panel: bool) {
    let mut reader = app.feat.history.readers.remove(&key).unwrap_or_default();
    if reader.open.as_ref().is_none_or(|o| o.host != open.host || o.anchor != open.anchor) {
        reader = Reader { open: Some(open), ..Default::default() };
    }
    std::mem::swap(&mut app.feat.history.reader, &mut reader);
    ui.push_id(&key, |ui| context::show(app, ui, in_panel));
    std::mem::swap(&mut app.feat.history.reader, &mut reader);
    app.feat.history.readers.insert(key, reader);
}

pub fn show_detail(app: &mut App, ui: &mut egui::Ui, open: Opened) {
    let key = format!("panel:{:?}:{:?}:{:?}", app.ui.right, open.host, open.anchor);
    read(app, ui, key, open, true);
}

pub fn show_record_tab(app: &mut App, ui: &mut egui::Ui, host: HostKey, anchor: Anchor, tab: crate::state::TabId) {
    ui.painter().rect_filled(ui.max_rect(), egui::CornerRadius::ZERO, c::DOC);
    let (before, after) = match anchor {
        Anchor::SessionEnd { .. } => (30, 0),
        _ => (6, 6),
    };
    read(app, ui, format!("record:{tab}"), Opened { host, anchor, before, after, terms: Vec::new() }, false);
}

/// Legacy search tabs keep a stable search layout after restoring an older layout.
pub fn show_tab(app: &mut App, ui: &mut egui::Ui, _tab: crate::state::TabId) {
    show_panel(app, ui);
}

pub fn tab_title(_app: &App) -> String {
    "대화 검색".into()
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

/// Open a message with its context in the record panel.
pub fn open_message(app: &mut App, host: HostKey, id: MessageId, terms: Vec<String>) {
    open(app, Opened { host, anchor: Anchor::Message { id }, before: 6, after: 6, terms });
}

/// The session bar passes its own session, including the right pane of a split.
pub fn toggle_from_session_bar(app: &mut App, host: HostKey, session: SessionId) {
    app.feat.history.scope = ScopeKind::Session;
    actions::toggle_panel_target(app, RightPanel::History, Some(PanelTarget { host, session }));
}

pub fn toggle_pins(app: &mut App, host: HostKey, session: SessionId) {
    actions::toggle_panel_target(app, RightPanel::Pins, Some(PanelTarget { host, session }));
}

pub fn open_session_records(app: &mut App, host: HostKey, session: SessionId) {
    open(app, Opened { host, anchor: Anchor::SessionEnd { id: session }, before: 30, after: 0, terms: Vec::new() });
}

fn open(app: &mut App, o: Opened) {
    open_in(app, o, RightPanel::History);
}

fn open_in(app: &mut App, o: Opened, tab: RightPanel) {
    let target = match o.anchor {
        Anchor::SessionEnd { id } => Some(PanelTarget { host: o.host, session: id }),
        Anchor::Message { .. } => app
            .hub
            .query::<runwick_core::api::records::RecordsContext>(
                o.host,
                &runwick_core::api::records::ContextQuery { anchor: o.anchor, before: 0, after: 0 },
            )
            .data
            .map(|cx| PanelTarget { host: o.host, session: cx.session_id }),
    };
    app.ui.push_panel(tab, PanelDetail::Message(o), target);
}

pub fn open_pin_message(app: &mut App, host: HostKey, id: MessageId) {
    open_in(
        app,
        Opened { host, anchor: Anchor::Message { id }, before: 6, after: 6, terms: Vec::new() },
        RightPanel::Pins,
    );
}

pub fn open_pin(app: &mut App, host: HostKey, id: PinId, session: Option<SessionId>) {
    app.ui.push_panel(
        RightPanel::Pins,
        PanelDetail::Pin { host, id },
        session.map(|session| PanelTarget { host, session }),
    );
}

pub fn show_pin(app: &mut App, ui: &mut egui::Ui, host: HostKey, id: PinId) {
    pins::show_detail(app, ui, host, id);
}

fn create_pin(app: &mut App, host: HostKey, source: PinSource) {
    app.hub.call::<PinsCreate>(host, NewPin { source, title: None, note: None }, |app, res| match res {
        Ok(_) => app.toast(crate::state::ToastKind::Success, "핀에 고정했어요"),
        Err(e) => app.toast_error(&format!("핀을 고정하지 못했어요: {}", e.message)),
    });
}

/// Pin text selected in a terminal ("터미널 발췌": not linked to a message).
pub fn pin_terminal_excerpt(app: &mut App, host: HostKey, session: SessionId, text: String) {
    create_pin(app, host, PinSource::Terminal { session_id: session, text });
}

/// Excerpts attached to the same message are independent copies, not its toggle state.
fn message_pin(list: &[Pin], message: MessageId) -> Option<PinId> {
    list.iter().find(|p| p.kind == PinKind::Message && p.message_id == Some(message)).map(|p| p.id)
}

/// The answer shown by the session bar. There is no terminal-screen fallback.
pub fn session_pin(app: &mut App, host: HostKey, session: SessionId, tool: Tool) -> Option<(MessageId, bool)> {
    if !tool.is_agent() {
        return None;
    }
    let turn = app.hub.query::<RecordsTurn>(host, &ById { id: session }).data?;
    let answer = turn.last_answer.as_ref()?;
    let list = app.hub.query::<PinsList>(host, &PinQuery { session_id: Some(session), project_id: None }).data?;
    Some((answer.id, message_pin(&list, answer.id).is_some()))
}

/// Toggle the exact message shown when clicked, even if a newer answer arrives meanwhile.
pub fn pin_from_session_bar(app: &mut App, host: HostKey, session: SessionId, message: MessageId) {
    toggle_message_pin(app, host, session, message);
}

fn toggle_message_pin(app: &mut App, host: HostKey, session: SessionId, message: MessageId) {
    let key = (host, message);
    if !app.feat.history.pending_pins.insert(key) {
        return;
    }
    // Read current state on click so a still-refreshing UI cache cannot invert the action.
    app.hub.call::<PinsList>(
        host,
        PinQuery { session_id: Some(session), project_id: None },
        move |app, res| match res {
            Ok(list) => {
                if let Some(id) = message_pin(&list, message) {
                    pins::remove(app, host, id, Some(message));
                } else {
                    let source = PinSource::Message { message_id: message };
                    app.hub.call::<PinsCreate>(host, NewPin { source, title: None, note: None }, move |app, res| {
                        app.feat.history.pending_pins.remove(&key);
                        match res {
                            Ok(_) => app.toast(crate::state::ToastKind::Success, "핀에 고정했어요"),
                            Err(e) => app.toast_error(&format!("핀을 고정하지 못했어요: {}", e.message)),
                        }
                    });
                }
            }
            Err(e) => {
                app.feat.history.pending_pins.remove(&key);
                app.toast_error(&format!("핀 상태를 확인하지 못했어요: {}", e.message));
            }
        },
    );
}

/// Muted one-line note (empty and info states).
fn note(ui: &mut egui::Ui, text: &str) {
    ui.add(egui::Label::new(RichText::new(text).font(font(ty::SMALL)).color(c::TEXT_3)).wrap());
}
