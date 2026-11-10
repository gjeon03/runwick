//! File tree of the selected workspace in the sidebar (`세션 / 파일` set to 파일): folders
//! expand lazily, a name filter searches the whole workspace with the keyboard, and each row
//! offers 열기 · 나란히 열기 · 경로 복사 · Finder에서 보기 · 외부 에디터로 열기 · 새 문서.
//! Everything goes through the host that owns the workspace (this computer or a device).

mod new_doc;
mod search;
mod tree;

use crate::App;
use crate::client::HostKey;
use crate::icons;
use crate::shell::{data, widgets};
use crate::state::WsKey;
use crate::theme::c;
use egui::Vec2;
use runwick_core::api::files::{FileRef, FilesOpenExternal, FilesReveal, OpenExternal, OpenWith};
use runwick_core::rpc::topic;
use std::collections::{BTreeSet, HashMap};
use std::time::{Duration, Instant};

/// While the tree is visible and the window focused, listings are refreshed this often (and
/// whenever the window regains focus), so files made by CLIs appear.
const REFRESH_EVERY: Duration = Duration::from_secs(10);

#[derive(Default)]
pub struct State {
    expanded: HashMap<WsKey, BTreeSet<String>>,
    show_hidden: bool,
    query: String,
    /// Selected search result (keyboard).
    sel: usize,
    new_doc: Option<new_doc::Dialog>,
    search: search::Search,
    refreshed_at: Option<Instant>,
    window_focused: bool,
    /// Active document whose folders were last opened in the tree.
    revealed: Option<(WsKey, String)>,
}

/// What the person asked for; applied after drawing.
pub(crate) enum Act {
    Toggle(String),
    Open(String, bool),
    Copy(String),
    Reveal(String),
    OpenExternal(String),
    NewDoc(String),
    Refresh,
}

/// Facts about the workspace the rows need.
pub(crate) struct Ctx {
    pub ws: WsKey,
    pub root: Option<String>,
    pub remote: bool,
    pub show_hidden: bool,
    /// Document path open in the active (or side) tab, highlighted in the tree.
    pub active: Option<String>,
}

impl Ctx {
    pub fn absolute(&self, rel: &str) -> Option<String> {
        self.root.as_ref().map(
            |r| {
                if rel.is_empty() { r.clone() } else { format!("{}/{rel}", r.trim_end_matches('/')) }
            },
        )
    }
}

/// Keep picker queries separate from the sidebar while reusing its search controls.
#[derive(Default)]
pub(crate) struct FilePicker(State);

pub(crate) fn pick_file(
    app: &mut App,
    ui: &mut egui::Ui,
    ws: WsKey,
    picker: &mut FilePicker,
    focus: bool,
) -> Option<String> {
    std::mem::swap(&mut app.feat.files, &mut picker.0);
    let cx = Ctx { ws, root: None, remote: ws.host != HostKey::Local, show_hidden: false, active: None };
    if focus {
        ui.memory_mut(|m| m.request_focus(search::field_id(ui, ws)));
    }
    search::field(app, ui, &cx);
    let mut acts = Vec::new();
    egui::ScrollArea::vertical().max_height(240.0).show(ui, |ui| {
        if app.feat.files.query.trim().is_empty() {
            widgets::empty_note(ui, "파일 이름을 입력해 주세요");
        } else {
            search::show_picker(app, ui, &cx, &mut acts);
        }
    });
    std::mem::swap(&mut app.feat.files, &mut picker.0);
    acts.into_iter().find_map(|act| match act {
        Act::Open(path, _) => Some(path),
        _ => None,
    })
}

pub fn show_sidebar(app: &mut App, ui: &mut egui::Ui, ws: WsKey) {
    let ectx = ui.ctx().clone();
    auto_refresh(app, &ectx, ws);
    let w = data::workspaces(app).data.and_then(|l| data::find_ws(&l, &ws).cloned());
    let active = app.ui.active_tab().and_then(|t| match &t.kind {
        crate::state::TabKind::Doc { ws: w, path } if *w == ws => Some(path.clone()),
        _ => None,
    });
    let cx = Ctx {
        ws,
        root: w.as_ref().map(|w| w.path.clone()),
        remote: ws.host != HostKey::Local,
        show_hidden: app.feat.files.show_hidden,
        active,
    };
    reveal_active(app, &cx);
    if crate::views::docs::take_requested("new-doc").is_some() {
        app.feat.files.new_doc = Some(new_doc::Dialog::new(ws, String::new()));
    }
    if let Some(q) = crate::views::docs::take_requested("search") {
        app.feat.files.query = q;
    }
    let mut acts = Vec::new();
    toolbar(app, ui, &cx, &mut acts);
    ui.add_space(2.0);
    if let Some(w) = w.as_ref().filter(|w| w.remote.is_none() && !w.path_exists) {
        widgets::empty_note(
            ui,
            &format!(
                "작업 공간 폴더를 찾을 수 없어요 · {}\n폴더를 옮겼거나 지웠다면 프로젝트를 다시 연결해 주세요. 열린 문서와 기록은 그대로예요.",
                data::tilde(&w.path)
            ),
        );
    } else if app.feat.files.query.trim().is_empty() {
        tree::show(app, ui, &cx, &mut acts);
    } else {
        search::show(app, ui, &cx, &mut acts);
    }
    for a in acts {
        apply(app, &ectx, &cx, a);
    }
    new_doc::show(app, &ectx);
    crate::views::docs::housekeeping(app, &ectx);
}

/// Open the folders of the document in the active tab so it is visible in the tree.
fn reveal_active(app: &mut App, cx: &Ctx) {
    let Some(path) = cx.active.clone() else {
        return;
    };
    let key = (cx.ws, path);
    if app.feat.files.revealed.as_ref() == Some(&key) {
        return;
    }
    let set = app.feat.files.expanded.entry(cx.ws).or_default();
    let mut dir = runwick_core::api::files::parent_dir(&key.1);
    while !dir.is_empty() {
        set.insert(dir.to_string());
        dir = runwick_core::api::files::parent_dir(dir);
    }
    app.feat.files.revealed = Some(key);
}

fn toolbar(app: &mut App, ui: &mut egui::Ui, cx: &Ctx, acts: &mut Vec<Act>) {
    ui.add_space(2.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        let field_w = (ui.available_width() - 3.0 * 26.0 - 8.0).max(80.0);
        ui.allocate_ui(Vec2::new(field_w, 28.0), |ui| search::field(app, ui, cx));
        ui.add_space(4.0);
        if widgets::icon_btn(ui, icons::FILE_PLUS, "새 문서", 26.0, 26.0, 13.0).clicked() {
            acts.push(Act::NewDoc(String::new()));
        }
        let hidden = app.feat.files.show_hidden;
        let (glyph, tip) = if hidden {
            (icons::EYE, "숨긴 파일 보는 중 — 누르면 .gitignore 대상과 숨김 파일을 감춰요")
        } else {
            (icons::EYE_OFF, "숨긴 파일 보기 (.gitignore 대상, 점으로 시작하는 파일)")
        };
        let resp = widgets::icon_btn(ui, glyph, tip, 26.0, 26.0, 13.0);
        if hidden {
            ui.painter().rect_stroke(
                resp.rect,
                egui::CornerRadius::same(6),
                egui::Stroke::new(1.0, c::LINE_STRONG),
                egui::StrokeKind::Inside,
            );
        }
        if resp.clicked() {
            app.feat.files.show_hidden = !hidden;
        }
        if widgets::icon_btn(ui, icons::REFRESH_CW, "파일 목록 새로 고침", 26.0, 26.0, 13.0).clicked() {
            acts.push(Act::Refresh);
        }
    });
}

fn auto_refresh(app: &mut App, ctx: &egui::Context, ws: WsKey) {
    let focused = ctx.input(|i| i.focused);
    let st = &mut app.feat.files;
    let regained = focused && !st.window_focused;
    st.window_focused = focused;
    if !focused {
        return;
    }
    let due = st.refreshed_at.is_none_or(|t| t.elapsed() >= REFRESH_EVERY);
    if regained || due {
        let first = st.refreshed_at.is_none();
        st.refreshed_at = Some(Instant::now());
        if !first {
            app.hub.invalidate(ws.host, &[topic::FILES]);
        }
    }
    ctx.request_repaint_after(REFRESH_EVERY);
}

fn apply(app: &mut App, ctx: &egui::Context, cx: &Ctx, act: Act) {
    let ws = cx.ws;
    match act {
        Act::Toggle(dir) => {
            let set = app.feat.files.expanded.entry(ws).or_default();
            if !set.remove(&dir) {
                set.insert(dir);
            }
        }
        Act::Open(path, side) => {
            crate::views::docs::open(app, ws, &path, side);
        }
        Act::Copy(text) => {
            ctx.copy_text(text);
            app.toast(crate::state::ToastKind::Success, "경로를 복사했어요");
        }
        Act::Reveal(path) => {
            app.hub.call::<FilesReveal>(ws.host, FileRef { workspace_id: ws.id, path }, |app, res| {
                if let Err(e) = res {
                    app.toast_error(&e.message);
                }
            });
        }
        Act::OpenExternal(path) => {
            let p = OpenExternal { workspace_id: ws.id, path, with: OpenWith::Editor };
            app.hub.call::<FilesOpenExternal>(ws.host, p, |app, res| {
                if let Err(e) = res {
                    app.toast_error(&e.message);
                }
            });
        }
        Act::NewDoc(dir) => app.feat.files.new_doc = Some(new_doc::Dialog::new(ws, dir)),
        Act::Refresh => {
            app.hub.invalidate(ws.host, &[topic::FILES]);
            app.feat.files.refreshed_at = Some(Instant::now());
        }
    }
}

/// Glyph for a file by its kind.
pub(crate) fn file_icon(path: &str, is_dir: bool, expanded: bool) -> &'static str {
    use crate::views::docs::model::Kind;
    if is_dir {
        return if expanded { icons::FOLDER_OPEN } else { icons::FOLDER };
    }
    match Kind::of(path) {
        Kind::Markdown => icons::FILE_TEXT,
        Kind::Code => icons::FILE_CODE,
        Kind::Text => icons::FILE,
    }
}

/// Context menu shared by tree rows and search results.
pub(crate) fn menu(ui: &mut egui::Ui, cx: &Ctx, path: &str, is_dir: bool, acts: &mut Vec<Act>) {
    ui.set_min_width(210.0);
    if is_dir {
        if widgets::menu_item(ui, icons::FILE_PLUS, "여기에 새 문서").clicked() {
            acts.push(Act::NewDoc(path.to_string()));
        }
    } else {
        if widgets::menu_item(ui, icons::FILE_TEXT, "열기").clicked() {
            acts.push(Act::Open(path.to_string(), false));
        }
        if widgets::menu_item(ui, icons::COLUMNS_2, "나란히 열기").clicked() {
            acts.push(Act::Open(path.to_string(), true));
        }
    }
    widgets::menu_sep(ui);
    if widgets::menu_item(ui, icons::COPY, "경로 복사").on_hover_text("작업 공간 기준 경로").clicked() {
        acts.push(Act::Copy(path.to_string()));
    }
    if let Some(abs) = cx.absolute(path) {
        let tip =
            if cx.remote { "그 기기 안에서만 맞는 경로예요" } else { "이 컴퓨터의 실제 위치" };
        if widgets::menu_item(ui, icons::CLIPBOARD, "실제 경로 복사").on_hover_text(tip).clicked() {
            acts.push(Act::Copy(abs));
        }
    }
    if !cx.remote {
        widgets::menu_sep(ui);
        if widgets::menu_item(ui, icons::FOLDER_OPEN, "Finder에서 보기").clicked() {
            acts.push(Act::Reveal(path.to_string()));
        }
        if !is_dir && widgets::menu_item(ui, icons::EXTERNAL_LINK, "외부 에디터로 열기").clicked() {
            acts.push(Act::OpenExternal(path.to_string()));
        }
    }
    if !is_dir {
        widgets::menu_sep(ui);
        if widgets::menu_item(ui, icons::FILE_PLUS, "같은 폴더에 새 문서").clicked() {
            acts.push(Act::NewDoc(runwick_core::api::files::parent_dir(path).to_string()));
        }
    }
}
