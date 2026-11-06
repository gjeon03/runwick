//! Document tabs (S03). Markdown and plain text are edited in place (Markdown also previews);
//! code is shown read-only with syntax highlighting until 편집 is turned on. Every save is
//! conditional on the disk still holding the version the edit started from, and "저장됨" is
//! shown only after the host confirms. While a document is visible its file is checked every
//! 1.5 s: without local edits a newer disk version is loaded silently, with local edits it is
//! kept aside for a three-way comparison. Buffers live here (not in the tab), so they survive
//! workspace switches and closing the tab, and unsaved ones are kept as drafts across restarts.

mod compare;
mod drafts;
mod editor;
mod header;
pub mod model;

use crate::App;
use crate::client::HostKey;
use crate::shell::data;
use crate::state::{TabId, TabKind, WsKey};
use crate::theme::Tone;
use egui::{Key, KeyboardShortcut, Modifiers};
use model::{Body, Doc, DocKey, SaveState};
use runwick_core::api::files::*;
use runwick_core::rpc::{RpcError, codes};
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// How often a visible document's file is checked for outside changes.
const POLL_EVERY: Duration = Duration::from_millis(1500);
const GC_EVERY: Duration = Duration::from_secs(5);

#[derive(Default)]
pub struct State {
    pub(crate) entries: HashMap<DocKey, Entry>,
    drafts: drafts::State,
    md: egui_commonmark::CommonMarkCache,
    gc_at: Option<Instant>,
    /// Body of the last drawn document (input verification clicks here).
    last_area: Option<egui::Rect>,
}

pub(crate) struct Entry {
    pub doc: Doc,
    pub ui: DocUi,
}

impl Entry {
    pub fn new(doc: Doc) -> Self {
        Self { doc, ui: DocUi::default() }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum View {
    #[default]
    Doc,
    Compare,
    Merge,
}

#[derive(Default)]
pub(crate) struct DocUi {
    pub view: View,
    pub preview: bool,
    /// Code: the person turned on 편집.
    pub edit_code: bool,
    /// Proportional text instead of the monospace default.
    pub proportional: bool,
    pub find: editor::Find,
    /// The last pointer press was inside this document.
    pub focused: bool,
    pub focus_editor: bool,
    pub reading: bool,
    pub polling: bool,
    pub last_poll: Option<Instant>,
    pub conn_gen: u64,
    pub merge: Option<String>,
    pub confirm: Option<header::Confirm>,
    /// Close this tab once the save the person chose while closing succeeds.
    pub close_after_save: Option<(WsKey, TabId)>,
    pub compare: Option<compare::Cache>,
    pub lines: Option<editor::Lines>,
}

/// Requests from the drawing code, applied once the frame's borrows end.
pub(crate) enum Act {
    Save,
    /// 내 편집으로 저장: over the disk version that was compared.
    SaveOver,
    SaveMerge,
    TakeDisk,
    /// Drop local edits (after confirmation).
    Discard,
    Compare,
    /// From the close dialog: save, then close the tab if the save succeeds.
    SaveAndClose(WsKey, TabId),
    /// From the close dialog: discard edits, then close the tab.
    DiscardAndClose(WsKey, TabId),
    Retry,
    Reveal,
    OpenExternal(OpenWith),
    Copy(String),
    Edited,
}

/// What the header and banners need to know about where the document lives.
pub(crate) struct Place {
    pub ws_name: String,
    /// Absolute folder of the workspace on its host.
    pub root: Option<String>,
    /// Device name for documents of another computer.
    pub device: Option<String>,
    pub connected: bool,
}

impl Place {
    pub fn absolute(&self, rel: &str) -> Option<String> {
        self.root.as_ref().map(|r| format!("{}/{rel}", r.trim_end_matches('/')))
    }
    pub fn remote(&self) -> bool {
        self.device.is_some()
    }
    /// "집 Mac mini" or "이 컴퓨터".
    pub fn where_(&self) -> &str {
        self.device.as_deref().unwrap_or("이 컴퓨터")
    }
}

fn place(app: &mut App, ws: WsKey) -> Place {
    let w = data::workspaces(app).data.and_then(|l| data::find_ws(&l, &ws).cloned());
    let device = match ws.host {
        HostKey::Local => None,
        HostKey::Device(d) => Some(crate::remote::device_name(app, d)),
    };
    Place {
        ws_name: w.as_ref().map(|w| w.name.clone()).unwrap_or_else(|| "작업 공간".to_string()),
        root: w.map(|w| w.path),
        device,
        connected: app.hub.state(ws.host).is_connected(),
    }
}

/// Open a document of a workspace (or focus its tab). `side`: show it next to the current tab.
pub fn open(app: &mut App, ws: WsKey, path: &str, side: bool) {
    app.ui.page = crate::state::Page::Work;
    let before = app.ui.tabset(&ws).and_then(|s| s.active);
    let id = app.ui.open_tab(ws, TabKind::Doc { ws, path: path.to_string() });
    let key = (ws, path.to_string());
    app.feat.docs.entries.entry(key.clone()).or_insert_with(|| Entry::new(Doc::new(ws, path)));
    if let Some(e) = app.feat.docs.entries.get_mut(&key) {
        e.ui.focus_editor = true;
    }
    if side && let Some(prev) = before.filter(|p| *p != id) {
        let set = app.ui.tabset_mut(&ws);
        set.active = Some(prev);
        set.split = Some(id);
        app.shell.center.focused_pane = 1;
    } else {
        app.shell.center.focused_pane = 0;
    }
}

/// The document has unsaved edits (for the file tree).
pub fn is_dirty(app: &App, ws: WsKey, path: &str) -> bool {
    app.feat.docs.entries.get(&(ws, path.to_string())).is_some_and(|e| e.doc.dirty())
}

/// Where the last drawn document body is (for `RUNWICK_VERIFY_INPUT` `click:doc`).
pub fn last_area(app: &App) -> Option<egui::Rect> {
    app.feat.docs.last_area
}

/// Called by the shell before closing a document tab. With unsaved edits the tab stays open,
/// is brought into view and asks 저장 / 편집 유지 / 버리기; returns true when it took over.
pub fn ask_before_close(app: &mut App, tab_ws: WsKey, tab: TabId, doc_ws: WsKey, path: &str) -> bool {
    let Some(e) = app.feat.docs.entries.get_mut(&(doc_ws, path.to_string())) else {
        return false;
    };
    if !e.doc.dirty() {
        return false;
    }
    e.ui.confirm = Some(header::Confirm::Close { ws: tab_ws, tab });
    let set = app.ui.tabset_mut(&tab_ws);
    if set.split != Some(tab) {
        set.active = Some(tab);
    }
    true
}

/// Dot for a document tab: unsaved, waiting on a decision, or kept as a local draft.
pub fn tab_dot(app: &App, ws: WsKey, path: &str) -> Option<(Tone, &'static str, bool)> {
    let e = app.feat.docs.entries.get(&(ws, path.to_string()))?;
    let d = &e.doc;
    if d.external.is_some() || d.save == SaveState::Conflict {
        Some((Tone::Attention, "디스크 변경 확인 필요", false))
    } else if d.save == SaveState::Queued {
        Some((Tone::Direct, "이 컴퓨터의 초안 · 연결되면 확인 후 저장", false))
    } else if d.dirty() && ws.host != HostKey::Local && !app.hub.state(ws.host).is_connected() {
        Some((Tone::Direct, "이 컴퓨터의 초안", false))
    } else if d.saving() {
        Some((Tone::Running, "저장 중", true))
    } else if d.dirty() {
        Some((Tone::Neutral, "미저장", false))
    } else {
        None
    }
}

/// Keep unsaved buffers for the next start (the window is closing).
pub fn save_drafts(app: &mut App) {
    drafts::flush(app);
}

/// Host events: restore drafts once this computer's host is connected.
pub fn on_event(app: &mut App, host: HostKey, topic: &str, _data: &serde_json::Value) {
    if host == HostKey::Local && topic == crate::client::EVENT_CONN_READY {
        drafts::restore(app);
    }
}

/// Verification aid, like the shell's `RUNWICK_UI_OVERLAY`: `RUNWICK_DOC_VIEW=preview`,
/// `find:<text>`, `compare`, `merge`, `new-doc` or `search:<text>` puts the first document (or
/// the file tree) into
/// that state once its data allows it. Returns the argument after `:` when `what` is requested,
/// and forgets the request.
pub(crate) fn take_requested(what: &str) -> Option<String> {
    static REQUEST: std::sync::OnceLock<std::sync::Mutex<Option<String>>> = std::sync::OnceLock::new();
    let cell = REQUEST.get_or_init(|| std::sync::Mutex::new(hook_env!("RUNWICK_DOC_VIEW")));
    let mut r = cell.lock().unwrap();
    let v = r.as_deref()?;
    let (name, arg) = v.split_once(':').unwrap_or((v, ""));
    if name != what {
        return None;
    }
    let arg = arg.to_string();
    *r = None;
    Some(arg)
}

fn apply_requested(e: &mut Entry) {
    if !e.doc.is_text() {
        return;
    }
    if e.doc.kind == model::Kind::Markdown && take_requested("preview").is_some() {
        e.ui.preview = true;
    }
    if let Some(q) = take_requested("find") {
        e.ui.find.open();
        e.ui.find.query = q;
    }
    let has_text = e.doc.external.as_ref().is_some_and(|x| x.text.is_some());
    if has_text && take_requested("compare").is_some() {
        e.ui.view = View::Compare;
    }
    if has_text && take_requested("merge").is_some() {
        e.ui.merge = e.doc.merge().map(|m| m.0);
        e.ui.view = View::Merge;
    }
}

pub fn show_tab(app: &mut App, ui: &mut egui::Ui, ws: WsKey, path: &str, _tab: TabId) {
    drafts::restore(app);
    let key: DocKey = (ws, path.to_string());
    let mut e = app.feat.docs.entries.remove(&key).unwrap_or_else(|| Entry::new(Doc::new(ws, path)));
    let ctx = ui.ctx().clone();
    let place = place(app, ws);
    track_connection(app, &mut e);
    apply_requested(&mut e);
    let rect = ui.max_rect();
    let mut acts = Vec::new();
    keys(app, &ctx, rect, &mut e, &mut acts);
    let mut md = std::mem::take(&mut app.feat.docs.md);
    match e.ui.view {
        View::Doc => {
            header::show(ui, &mut e, &place, &mut acts);
            app.feat.docs.last_area = Some(ui.available_rect_before_wrap());
            editor::show(ui, &mut e, &place, &mut md, &mut acts);
        }
        View::Compare => compare::show_compare(ui, &mut e, &place, &mut acts),
        View::Merge => compare::show_merge(ui, &mut e, &place, &mut acts),
    }
    app.feat.docs.md = md;
    if e.ui.confirm.is_some() {
        header::confirm(&ctx, &mut e, &mut acts);
    }
    app.feat.docs.entries.insert(key.clone(), e);
    load_or_verify(app, &key);
    poll(app, &key, &ctx);
    for a in acts {
        apply(app, &ctx, &key, a);
    }
    housekeeping(app, &ctx);
}

/// Drafts and dropping buffers nobody needs. Also called by the file tree.
pub(crate) fn housekeeping(app: &mut App, ctx: &egui::Context) {
    drafts::tick(app, ctx);
    let st = &mut app.feat.docs;
    if st.gc_at.is_some_and(|t| t.elapsed() < GC_EVERY) {
        return;
    }
    st.gc_at = Some(Instant::now());
    let open: std::collections::HashSet<DocKey> = app
        .ui
        .tabsets
        .values()
        .flat_map(|s| s.tabs.iter())
        .filter_map(|t| match &t.kind {
            TabKind::Doc { ws, path } => Some((*ws, path.clone())),
            _ => None,
        })
        .collect();
    app.feat.docs.entries.retain(|k, e| open.contains(k) || e.doc.needs_person() || e.doc.saving());
}

/// After a reconnect the disk may have changed: check again before trusting the base.
fn track_connection(app: &App, e: &mut Entry) {
    let generation = app.hub.conn(e.doc.ws.host).map(|c| c.generation()).unwrap_or(0);
    if generation != e.ui.conn_gen {
        if e.ui.conn_gen != 0 && e.doc.is_text() {
            e.doc.verified = false;
        }
        e.ui.conn_gen = generation;
    }
}

/// ⌘S and ⌘F belong to the document while it has the keyboard (or was clicked last).
fn keys(app: &App, ctx: &egui::Context, rect: egui::Rect, e: &mut Entry, acts: &mut Vec<Act>) {
    let (pressed, pos) = ctx.input(|i| (i.pointer.any_pressed(), i.pointer.interact_pos()));
    if pressed {
        e.ui.focused = pos.is_some_and(|p| rect.contains(p));
    }
    let focus_inside =
        ctx.memory(|m| m.focused()).and_then(|id| ctx.read_response(id)).map(|r| rect.contains(r.rect.center()));
    let active = focus_inside.unwrap_or(e.ui.focused);
    if !active || app.shell.keys.composing || app.ui.modal.is_some() || app.ui.palette_open {
        return;
    }
    let take = |key| ctx.input_mut(|i| i.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, key)));
    if take(Key::S) {
        match e.ui.view {
            View::Merge => acts.push(Act::SaveMerge),
            _ => acts.push(Act::Save),
        }
    }
    if take(Key::F) && e.ui.view == View::Doc {
        e.ui.find.open();
    }
}

fn load_or_verify(app: &mut App, key: &DocKey) {
    let Some(e) = app.feat.docs.entries.get_mut(key) else {
        return;
    };
    let wants = e.doc.body == Body::Loading || (e.doc.is_text() && !e.doc.verified);
    if !wants || e.ui.reading || !app.hub.state(key.0.host).is_connected() {
        return;
    }
    e.ui.reading = true;
    let seq = e.doc.seq;
    let params = ReadFile { workspace_id: key.0.id, path: key.1.clone(), max_bytes: None };
    let k = key.clone();
    app.hub.call::<FilesRead>(key.0.host, params, move |app, res| {
        let Some(e) = app.feat.docs.entries.get_mut(&k) else {
            return;
        };
        e.ui.reading = false;
        if e.doc.seq != seq {
            return; // Saved meanwhile; check again with the new base.
        }
        match res {
            Ok(c) => e.doc.loaded(c),
            Err(err) => e.doc.load_failed(&err),
        }
        e.ui.last_poll = Some(Instant::now());
        if e.doc.save == SaveState::Queued && e.doc.verified {
            // Reconnected and the disk still holds the base: the save the person asked for.
            e.doc.save = SaveState::Idle;
            save(app, &k, SaveKind::Plain);
        }
    });
}

/// Check the file of a visible document for changes made elsewhere.
fn poll(app: &mut App, key: &DocKey, ctx: &egui::Context) {
    let Some(e) = app.feat.docs.entries.get_mut(key) else {
        return;
    };
    if !e.doc.is_text() || !e.doc.verified {
        return;
    }
    ctx.request_repaint_after(POLL_EVERY);
    let due = e.ui.last_poll.is_none_or(|t| t.elapsed() >= POLL_EVERY);
    if !due || e.ui.polling || e.ui.reading || e.doc.saving() || !app.hub.state(key.0.host).is_connected() {
        return;
    }
    e.ui.polling = true;
    e.ui.last_poll = Some(Instant::now());
    let seq = e.doc.seq;
    let k = key.clone();
    let params = StatFiles { workspace_id: key.0.id, paths: vec![key.1.clone()] };
    app.hub.call::<FilesStat>(key.0.host, params, move |app, res| {
        let Some(e) = app.feat.docs.entries.get_mut(&k) else {
            return;
        };
        e.ui.polling = false;
        let Ok(stats) = res else { return };
        let Some(st) = stats.into_iter().next() else {
            return;
        };
        if e.doc.seq != seq || !e.doc.needs_read(&st) {
            return;
        }
        e.ui.polling = true;
        let params = ReadFile { workspace_id: k.0.id, path: k.1.clone(), max_bytes: None };
        let k2 = k.clone();
        app.hub.call::<FilesRead>(k.0.host, params, move |app, res| {
            let Some(e) = app.feat.docs.entries.get_mut(&k2) else {
                return;
            };
            e.ui.polling = false;
            if e.doc.seq != seq {
                return;
            }
            if let Ok(c) = res {
                let was_dirty = e.doc.dirty();
                e.doc.disk_read(c);
                if was_dirty {
                    drafts::touch(app);
                }
            }
        });
    });
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SaveKind {
    Plain,
    Over,
    Merge,
}

fn save(app: &mut App, key: &DocKey, kind: SaveKind) {
    let Some(e) = app.feat.docs.entries.get_mut(key) else {
        return;
    };
    if kind == SaveKind::Plain && e.doc.external.is_some() {
        // The disk changed: decide in the comparison first (the banner explains other cases).
        if e.doc.external.as_ref().is_some_and(|x| x.text.is_some()) {
            e.ui.view = View::Compare;
        }
        return;
    }
    if e.doc.lossy {
        app.toast_info("UTF-8이 아닌 글자가 있어 이 앱에서는 저장하지 않아요. 외부 에디터를 써 주세요");
        return;
    }
    let (text, over) = match kind {
        SaveKind::Plain => (e.doc.text.clone(), None),
        SaveKind::Over => (e.doc.text.clone(), Some(e.doc.external.as_ref().and_then(|x| x.hash.clone()))),
        SaveKind::Merge => {
            let Some(m) = e.ui.merge.clone() else { return };
            (m, Some(e.doc.external.as_ref().and_then(|x| x.hash.clone())))
        }
    };
    if kind == SaveKind::Plain && !e.doc.dirty() && !e.doc.missing {
        return;
    }
    let Some((write_text, base_hash)) = e.doc.begin_save(&text, over) else {
        return;
    };
    let host = key.0.host;
    if !app.hub.state(host).is_connected() {
        e.doc.save_failed(&RpcError::new(codes::DISCONNECTED, "연결되어 있지 않아요"));
        drafts::touch(app);
        return;
    }
    let params = WriteFile { workspace_id: key.0.id, path: key.1.clone(), text: write_text, base_hash };
    let k = key.clone();
    app.hub.call::<FilesWrite>(host, params, move |app, res| {
        let Some(e) = app.feat.docs.entries.get_mut(&k) else {
            return;
        };
        let mut close = None;
        match res {
            Ok(out) => {
                let saved = matches!(out, WriteOutcome::Saved { .. });
                e.doc.finish_save(text, out, kind == SaveKind::Merge);
                if saved {
                    e.ui.view = View::Doc;
                    e.ui.merge = None;
                    e.ui.compare = None;
                    close = e.ui.close_after_save.take();
                } else {
                    e.ui.close_after_save = None;
                }
            }
            Err(err) => {
                e.doc.save_failed(&err);
                e.ui.close_after_save = None;
            }
        }
        drafts::touch(app);
        if let Some((ws, tab)) = close {
            // Still asks again if something was typed while saving.
            crate::shell::actions::close_tab(app, ws, tab);
        }
    });
}

fn apply(app: &mut App, ctx: &egui::Context, key: &DocKey, act: Act) {
    let host = key.0.host;
    let file = FileRef { workspace_id: key.0.id, path: key.1.clone() };
    match act {
        Act::Save => save(app, key, SaveKind::Plain),
        Act::SaveOver => save(app, key, SaveKind::Over),
        Act::SaveMerge => save(app, key, SaveKind::Merge),
        Act::TakeDisk => {
            if let Some(e) = app.feat.docs.entries.get_mut(key)
                && e.doc.take_disk()
            {
                e.ui.view = View::Doc;
                e.ui.compare = None;
                drafts::touch(app);
            }
        }
        Act::Discard => {
            if let Some(e) = app.feat.docs.entries.get_mut(key) {
                if e.doc.external.as_ref().is_some_and(|x| x.text.is_some()) {
                    e.doc.take_disk();
                } else {
                    e.doc.text = e.doc.base.clone();
                    e.doc.restored = false;
                    e.doc.save = SaveState::Idle;
                }
                e.ui.view = View::Doc;
                drafts::touch(app);
            }
        }
        Act::SaveAndClose(ws, tab) => {
            save(app, key, SaveKind::Plain);
            if let Some(e) = app.feat.docs.entries.get_mut(key)
                && e.doc.saving()
            {
                e.ui.close_after_save = Some((ws, tab));
            }
        }
        Act::DiscardAndClose(ws, tab) => {
            apply(app, ctx, key, Act::Discard);
            crate::shell::actions::close_tab(app, ws, tab);
        }
        Act::Compare => {
            if let Some(e) = app.feat.docs.entries.get_mut(key) {
                e.ui.view = View::Compare;
            }
        }
        Act::Retry => {
            if let Some(e) = app.feat.docs.entries.get_mut(key) {
                if !e.doc.is_text() {
                    e.doc.body = Body::Loading;
                }
                e.doc.verified = false;
            }
        }
        Act::Reveal => app.hub.call::<FilesReveal>(host, file, |app, res| {
            if let Err(e) = res {
                app.toast_error(&e.message);
            }
        }),
        Act::OpenExternal(with) => app.hub.call::<FilesOpenExternal>(
            host,
            OpenExternal { workspace_id: file.workspace_id, path: file.path, with },
            |app, res| {
                if let Err(e) = res {
                    app.toast_error(&e.message);
                }
            },
        ),
        Act::Copy(text) => {
            ctx.copy_text(text);
            app.toast(crate::state::ToastKind::Success, "경로를 복사했어요");
        }
        Act::Edited => drafts::touch(app),
    }
}
