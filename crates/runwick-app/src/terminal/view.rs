//! One session shown in the app: attach/re-attach, size, input, and drawing.

use super::ask::{self, Ask};
use super::banner::{self, Act};
use super::find::{Find, Outcome};
use super::input::{self, Button, MouseAction};
use super::model::{GridSize, Shared};
use super::render::{self, Layout, Metrics, Overlay, PAD};
use crate::App;
use crate::client::HostKey;
use crate::icons;
use crate::shell::widgets;
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::TermMode;
use egui::{Event, EventFilter, Key, Modifiers, PointerButton, Rect, Sense};
use runwick_core::api::projects::ById;
use runwick_core::api::records::{RecordsStatus, StatusQuery};
use runwick_core::api::sessions::{
    Attach, Phase, Resize, SessionId, SessionLive, SessionsAttach, SessionsGet, SessionsResize, SessionsTakeInput,
};
use std::time::{Duration, Instant};

const RESIZE_DEBOUNCE: Duration = Duration::from_millis(100);
const BLINK: Duration = Duration::from_millis(530);
/// The cursor stops blinking (stays on) after this long without input or output.
const BLINK_FOR: Duration = Duration::from_secs(10);
const ATTACH_RETRY: Duration = Duration::from_secs(2);
const MENU_WIDTH: f32 = 200.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Interactive,
    ReadOnly,
}

pub struct View {
    pub shared: Shared,
    /// Terminal area of the last interactive draw.
    pub area: Option<Rect>,
    /// Connection generation the current attach belongs to.
    pub attached: Option<u64>,
    pub attached_interactive: bool,
    attaching: Option<(u64, Instant)>,
    retry_at: Option<Instant>,
    pub last_frame: u64,
    pub last_shown: Instant,
    pub viewing_sent: Option<bool>,
    pending_resize: Option<(GridSize, Instant)>,
    /// Last size requested from the host, to avoid repeating it while the answer is on its way.
    sent_size: Option<(GridSize, Instant)>,
    pub want_focus: bool,
    pub has_focus: bool,
    preedit: String,
    deferred: Vec<u8>,
    suppress_text: bool,
    wheel: f32,
    drag: Option<Button>,
    last_mouse_cell: Option<(usize, usize)>,
    /// The primary button went down with Ctrl (a menu click, kept from the program).
    ctrl_press: bool,
    /// Focus last reported to the program (DECSET 1004), or seen while it did not ask.
    focus_sent: Option<bool>,
    selecting: bool,
    pub find: Find,
    blink_epoch: Instant,
    starting_since: Option<Instant>,
    /// Since when the session is shown without a break (requests from before are dropped).
    visible_since: Instant,
    /// A question waiting for the person (clipboard write, multi-line paste).
    ask: Option<Ask>,
}

impl View {
    pub fn new(size: GridSize) -> Self {
        Self {
            shared: Shared::new(size),
            area: None,
            attached: None,
            attached_interactive: false,
            attaching: None,
            retry_at: None,
            last_frame: 0,
            last_shown: Instant::now(),
            viewing_sent: None,
            pending_resize: None,
            sent_size: None,
            want_focus: true,
            has_focus: false,
            preedit: String::new(),
            deferred: Vec::new(),
            suppress_text: false,
            wheel: 0.0,
            drag: None,
            last_mouse_cell: None,
            ctrl_press: false,
            focus_sent: None,
            selecting: false,
            find: Find::default(),
            blink_epoch: Instant::now(),
            starting_since: None,
            visible_since: Instant::now(),
            ask: None,
        }
    }

    /// Forget the attachment (connection lost): keep showing the last screen.
    pub fn lost(&mut self) {
        self.attached = None;
        self.attaching = None;
        self.viewing_sent = None;
        self.pending_resize = None;
        self.shared.lock().release();
    }
}

/// Draw a session in `ui.max_rect()` and handle its input. `conn_chip`: say connection
/// problems in a chip over the screen (off when a banner above already says them).
pub fn show(app: &mut App, ui: &mut egui::Ui, host: HostKey, session: SessionId, mode: Mode, conn_chip: bool) {
    let ctx = ui.ctx().clone();
    let area = ui.max_rect();
    ui.painter().rect_filled(area, 0.0, crate::theme::c::TERMINAL);
    let m = Metrics::new(&ctx);
    let key = (host, session);
    app.terms.ctx = Some(ctx.clone());

    let conn_state = app.hub.state(host);
    let connected = conn_state.is_connected();
    let me = conn_state.info().map(|i| i.client_id);
    let generation = app.hub.conn(host).map(|c| c.generation());
    let record = app.hub.query::<SessionsGet>(host, &ById { id: session }).data;
    // Resume is offered only for a conversation the records hold (as the quit dialog decides).
    let saved_conversation = app
        .hub
        .query::<RecordsStatus>(host, &StatusQuery::default())
        .data
        .map(|list| list.iter().any(|c| c.session_id == session && c.message_count > 0));
    let live = app.terms.live.get(&key).cloned().or_else(|| record.as_ref().and_then(|r| r.live.clone()));

    // Which states to show, and how much room the bottom bar takes.
    let starting_secs = {
        let view = app.terms.views.entry(key).or_insert_with(|| View::new(initial_size(&m, area)));
        match live.as_ref().map(|l| l.phase) {
            Some(Phase::Starting) => view.starting_since.get_or_insert_with(Instant::now).elapsed().as_secs(),
            _ => {
                view.starting_since = None;
                0
            }
        }
    };
    let states = banner::decide(&banner::Inputs {
        record: record.as_deref(),
        live: live.as_ref(),
        connected,
        ever_connected: generation.is_some_and(|g| g > 0),
        disconnect_reason: match &conn_state {
            crate::client::ConnState::Disconnected { reason, .. } => Some(reason.clone()),
            _ => None,
        },
        disconnected_on_purpose: match host {
            HostKey::Device(id) => matches!(crate::remote::status(app, id), crate::remote::Status::Off),
            HostKey::Local => false,
        },
        me,
        interactive: mode == Mode::Interactive,
        starting_secs,
        saved_conversation,
    });
    let bar_state = states.iter().find(|s| s.is_bar()).cloned();
    let chip_state = states.iter().find(|s| !s.is_bar() && (conn_chip || !s.is_connection())).cloned();
    let term_area = match bar_state {
        Some(_) => Rect::from_min_max(area.min, egui::pos2(area.right(), area.bottom() - banner::BAR_HEIGHT)),
        None => area,
    };

    let running = live.as_ref().is_some_and(|l| matches!(l.phase, Phase::Starting | Phase::Running));
    let owner = live.as_ref().and_then(|l| l.input_owner_client);
    let may_control = mode == Mode::Interactive && connected && (owner.is_none() || owner == me);
    let desired = {
        let (cols, rows) = m.grid_for(term_area.size());
        GridSize { cols, rows }
    };

    // Attach (again) when the connection is new or the mode needs an interactive attach.
    if connected && let (Some(generation), Some(conn)) = (generation, app.hub.conn(host)) {
        let view = app.terms.views.get_mut(&key).expect("view exists");
        let wants_interactive = mode == Mode::Interactive;
        let up_to_date = view.attached == Some(generation) && (view.attached_interactive || !wants_interactive);
        let in_flight = view.attaching.is_some_and(|(g, at)| g == generation && at.elapsed() < Duration::from_secs(15));
        let waiting = view.retry_at.is_some_and(|t| Instant::now() < t);
        if !up_to_date && !in_flight && !waiting {
            view.shared.lock().hold();
            conn.set_output_sink(session, view.shared.sink());
            view.attaching = Some((generation, Instant::now()));
            let interactive = wants_interactive || view.attached_interactive;
            let size = if interactive { desired } else { view.shared.lock().size() };
            app.hub.call::<SessionsAttach>(
                host,
                Attach { id: session, cols: size.cols, rows: size.rows, interactive },
                move |app, res| attached(app, host, session, generation, interactive, res),
            );
        }
    }

    let view = app.terms.views.get_mut(&key).expect("view exists");
    let pass = ctx.cumulative_pass_nr();
    if view.last_frame + 1 < pass {
        view.visible_since = Instant::now(); // shown again after being hidden
    }
    view.last_frame = pass;
    view.last_shown = Instant::now();
    if mode == Mode::Interactive {
        view.area = Some(term_area);
    }

    // Follow the area: the input holder resizes the run (debounced); others show the run's size.
    if may_control && running && view.attached.is_some() {
        let current = view.shared.lock().size();
        // The run may have been sized elsewhere (for example a restart from another view).
        let host_size = live.as_ref().map(|l| GridSize { cols: l.cols, rows: l.rows });
        let host_differs = host_size.is_some_and(|h| h != desired)
            && view.sent_size.is_none_or(|(s, at)| s != desired || at.elapsed() > Duration::from_secs(2));
        if desired != current || host_differs {
            let since = match view.pending_resize {
                Some((d, at)) if d == desired => at,
                _ => {
                    view.pending_resize = Some((desired, Instant::now()));
                    Instant::now()
                }
            };
            if since.elapsed() >= RESIZE_DEBOUNCE {
                view.pending_resize = None;
                view.sent_size = Some((desired, Instant::now()));
                view.shared.lock().resize(desired);
                app.hub.call::<SessionsResize>(
                    host,
                    Resize { id: session, cols: desired.cols, rows: desired.rows },
                    |_, _| {}, // refused when another client took input meanwhile; its size wins
                );
            } else {
                ctx.request_repaint_after(RESIZE_DEBOUNCE - since.elapsed());
            }
        } else {
            view.pending_resize = None;
        }
    }

    // Keyboard focus.
    let view = app.terms.views.get_mut(&key).expect("view exists");
    let id = ui.scope_id().with(("terminal", host, session));
    let resp = ui.interact(term_area, id, Sense::click_and_drag());
    let overlay_open = app.ui.modal.is_some() || app.ui.palette_open;
    if mode == Mode::Interactive {
        let nothing_focused = ctx.memory(|mem| mem.focused().is_none());
        if resp.clicked() || resp.drag_started() || (view.want_focus && !overlay_open && nothing_focused) {
            resp.request_focus();
            view.want_focus = false;
        }
    }
    let focused = resp.has_focus();
    view.has_focus = focused;
    if focused {
        ctx.memory_mut(|mem| {
            mem.set_focus_lock_filter(
                id,
                EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: true },
            )
        });
    }

    let lay = Layout { origin: (term_area.min + PAD).round(), m: m.clone() };
    let mut sent = Vec::new();
    if mode == Mode::Interactive && may_control && running {
        let in_focus = focused && ctx.input(|i| i.focused);
        let wants = view.shared.lock().term.mode().contains(TermMode::FOCUS_IN_OUT);
        if let Some(report) = focus_report(&mut view.focus_sent, in_focus, wants) {
            sent.extend_from_slice(report);
        }
    }
    if mode == Mode::Interactive {
        handle_keys(&ctx, view, focused && may_control && running, &mut sent);
    }
    handle_pointer(ui, &resp, &lay, view, may_control && running, &mut sent);
    if !sent.is_empty() {
        super::verify::record("sent", &String::from_utf8_lossy(&sent));
        if let Some(conn) = app.hub.conn(host) {
            conn.send_input(session, &sent);
        }
        let mut ct = view.shared.lock();
        ct.term.scroll_display(Scroll::Bottom);
        view.blink_epoch = Instant::now();
    }

    // Draw.
    let clip = ui.painter().clip_rect().intersect(term_area);
    let painter = ui.painter().with_clip_rect(clip);
    let mut next_repaint: Option<Instant> = None;
    let copy_request;
    {
        let mut ct = view.shared.lock();
        copy_request = ct.events.take();
        if let Some(t) = ct.flush_sync() {
            next_repaint = Some(t);
        }
        if let Some(t) = view.find.refresh(&ct) {
            next_repaint = Some(next_repaint.map_or(t, |n| n.min(t)));
        }
        let blinking = ct.term.cursor_style().blinking && focused;
        let idle = view.blink_epoch.elapsed();
        let cursor_on = if blinking && idle < BLINK_FOR {
            let phase = idle.as_millis() / BLINK.as_millis();
            let next = view.blink_epoch + BLINK * (phase as u32 + 1);
            next_repaint = Some(next_repaint.map_or(next, |n| n.min(next)));
            phase.is_multiple_of(2)
        } else {
            true
        };
        let ov = Overlay {
            focused,
            cursor_on,
            // An ended run has no cursor to type at.
            cursor_hidden: !running,
            matches: &view.find.matches,
            current_match: view.find.current,
            preedit: &view.preedit,
        };
        render::paint(&painter, &lay, &ct.term, &ov);

        if focused && mode == Mode::Interactive {
            // Place the IME candidate window at the cursor.
            let cur = ct.term.grid().cursor.point;
            let row = (cur.line.0 + ct.term.grid().display_offset() as i32).max(0) as usize;
            let cursor_rect = lay.cell_rect(row, cur.column.0, 1);
            ctx.output_mut(|o| {
                o.ime = Some(egui::output::IMEOutput {
                    purpose: egui::IMEPurpose::Normal,
                    rect: term_area,
                    cursor_rect,
                    should_interrupt_composition: false,
                })
            });
        }
    }
    if let Some(t) = next_repaint {
        ctx.request_repaint_after(t.saturating_duration_since(Instant::now()));
    }

    // A program's clipboard write (OSC 52): only from a session being shown, never silently.
    let mut clip_write = None;
    if let Some(req) = copy_request.filter(|r| mode == Mode::Interactive && r.at >= view.visible_since) {
        match app.terms.clipboard.get(&key) {
            Some(true) => clip_write = Some(req.text),
            Some(false) => {}
            None => view.ask = Some(Ask::Copy(req.text)),
        }
    }
    let mut paste_now = None;
    let answer = view.ask.as_ref().and_then(|a| ask::show(ui, term_area, a));
    if let Some(yes) = answer
        && let Some(asked) = view.ask.take()
    {
        view.want_focus = true;
        match asked {
            // Remembered for the session while the app runs.
            Ask::Copy(text) => {
                app.terms.clipboard.insert(key, yes);
                clip_write = yes.then_some(text);
            }
            Ask::Paste(text) => paste_now = yes.then_some(text),
        }
    }

    // Find bar (Cmd+F while focused).
    if view.find.open {
        let (outcome, moved) = view.find.show(ui, term_area);
        if moved && let Some(mtch) = view.find.current_match().cloned() {
            view.shared.lock().term.scroll_to_point(*mtch.start());
        }
        if matches!(outcome, Outcome::Closed) {
            view.want_focus = true;
        }
    }

    // Context menu: right-click, or Shift+right-click / Ctrl+click while the program has the mouse.
    let mut menu_act = None;
    if mode == Mode::Interactive {
        let program_has_mouse =
            may_control && running && view.shared.lock().term.mode().intersects(TermMode::MOUSE_MODE);
        let click = if resp.secondary_clicked() {
            Click::Secondary
        } else if resp.clicked() {
            Click::Primary
        } else {
            Click::None
        };
        let set = menu_click(click, ctx.input(|i| i.modifiers), program_has_mouse);
        if set == Some(true) {
            super::verify::record("menu", "opened");
        }
        let has_sel = view.shared.lock().term.selection.as_ref().is_some_and(|s| !s.is_empty());
        let set = set.map(egui::SetOpenCommand::Bool);
        // Without a width the popup area is 600 points wide and menu items fill it.
        egui::Popup::context_menu(&resp).open_memory(set).width(MENU_WIDTH).show(|ui| {
            let entries = [
                (MenuAct::Copy, has_sel, icons::COPY, "복사"),
                (MenuAct::Paste, may_control && running, icons::CLIPBOARD, "붙여넣기"),
                (MenuAct::SelectAll, true, icons::TYPE, "모두 선택"),
                (MenuAct::Find, true, icons::TEXT_SEARCH, "화면에서 찾기"),
                (MenuAct::Pin, has_sel, icons::PIN, "핀으로 고정"),
            ];
            for (act, enabled, glyph, label) in entries {
                if act == MenuAct::Pin {
                    widgets::menu_sep(ui);
                }
                let r = ui.add_enabled_ui(enabled, |ui| widgets::menu_item(ui, glyph, label)).inner;
                super::verify::target(label, r.rect);
                if r.clicked() {
                    menu_act = Some(act);
                }
            }
        });
    }
    let paste =
        paste_now.filter(|_| may_control && running).map(|t| input::paste_bytes(&t, *view.shared.lock().term.mode()));
    let mut pin_text = None;
    match menu_act {
        Some(MenuAct::Copy) => copy_selection(&ctx, view),
        Some(MenuAct::Paste) => ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaste),
        Some(MenuAct::SelectAll) => select_all(view),
        Some(MenuAct::Find) => view.find.open(),
        Some(MenuAct::Pin) => pin_text = view.shared.lock().term.selection_to_string().filter(|t| !t.trim().is_empty()),
        None => {}
    }
    if let Some(text) = pin_text {
        crate::views::history::pin_terminal_excerpt(app, host, session, text);
    }
    if let Some(bytes) = paste {
        super::verify::record("sent", &String::from_utf8_lossy(&bytes));
        if let Some(conn) = app.hub.conn(host) {
            conn.send_input(session, &bytes);
        }
    }
    if let Some(text) = clip_write {
        write_clipboard(app, &ctx, text);
    }

    // States.
    let mut act = chip_state.as_ref().and_then(|s| banner::chip(ui, term_area, s));
    if let Some(s) = &bar_state {
        let bar = Rect::from_min_max(egui::pos2(area.left(), term_area.bottom()), area.max);
        act = act.or(banner::bar(ui, bar, s));
    }
    if let Some(act) = act {
        run_act(app, host, session, act);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MenuAct {
    Copy,
    Paste,
    SelectAll,
    Find,
    Pin,
}

/// Focus reporting (DECSET 1004): CSI I / CSI O when the terminal gains or loses focus, only
/// while the program asked for it. A change seen while it did not ask is remembered, not sent.
fn focus_report(last: &mut Option<bool>, in_focus: bool, program_wants: bool) -> Option<&'static [u8]> {
    let changed = last.is_some_and(|l| l != in_focus);
    *last = Some(in_focus);
    (changed && program_wants).then_some(if in_focus { b"\x1b[I" } else { b"\x1b[O" })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Click {
    None,
    Primary,
    Secondary,
}

/// Open (`Some(true)`) or close (`Some(false)`) the context menu after a click. A right-click
/// belongs to a program that captures the mouse (it is reported to it); Shift+right-click and
/// Ctrl+click open the menu anyway. Any other click closes it.
fn menu_click(click: Click, mods: Modifiers, program_has_mouse: bool) -> Option<bool> {
    match click {
        Click::Secondary => (mods.shift || !program_has_mouse).then_some(true),
        Click::Primary => Some(mods.ctrl),
        Click::None => None,
    }
}

fn initial_size(m: &Metrics, area: Rect) -> GridSize {
    let (cols, rows) = m.grid_for(area.size());
    GridSize { cols, rows }
}

fn attached(
    app: &mut App,
    host: HostKey,
    session: SessionId,
    generation: u64,
    interactive: bool,
    res: Result<SessionLive, runwick_core::RpcError>,
) {
    let current = app.hub.conn(host).map(|c| c.generation());
    let Some(view) = app.terms.views.get_mut(&(host, session)) else { return };
    if view.attaching.is_some_and(|(g, _)| g == generation) {
        view.attaching = None;
    }
    if current != Some(generation) {
        return; // the connection changed meanwhile; the next frame attaches again
    }
    match res {
        Ok(live) => {
            view.shared.lock().replace(GridSize { cols: live.cols, rows: live.rows });
            view.attached = Some(generation);
            view.attached_interactive = interactive;
            view.retry_at = None;
            view.viewing_sent = None;
            app.terms.live.insert((host, session), live);
        }
        Err(e) => {
            tracing::debug!(%session, "attach failed: {}", e.message);
            view.shared.lock().release();
            view.retry_at = Some(Instant::now() + ATTACH_RETRY);
        }
    }
}

fn run_act(app: &mut App, host: HostKey, session: SessionId, act: Act) {
    use crate::shell::actions;
    match act {
        Act::Cancel => actions::stop_session(app, host, session),
        Act::Resume => actions::restart_session(app, host, session, true),
        Act::Rerun | Act::NewConversation => actions::restart_session(app, host, session, false),
        Act::TakeInput => app.hub.send::<SessionsTakeInput>(host, ById { id: session }),
        Act::ToolSettings => actions::open_settings(app, Some(crate::state::SettingsSection::Tools)),
        Act::Reconnect => {
            if let HostKey::Device(id) = host {
                crate::remote::reconnect(app, id);
            }
        }
    }
    if let Some(v) = app.terms.views.get_mut(&(host, session)) {
        v.want_focus = true;
    }
}

fn copy_selection(ctx: &egui::Context, view: &View) {
    if let Some(text) = view.shared.lock().term.selection_to_string().filter(|t| !t.is_empty()) {
        // Verification runs record the copy instead of replacing the person's clipboard.
        if !super::verify::record("copied", &text) {
            ctx.copy_text(text);
        }
    }
}

/// Put a program's text on the clipboard (the person allowed it) and say so.
fn write_clipboard(app: &mut App, ctx: &egui::Context, text: String) {
    let n = text.chars().count();
    // Verification runs record the copy instead of replacing the person's clipboard.
    if !super::verify::record("copied", &text) {
        ctx.copy_text(text);
    }
    app.toast(crate::state::ToastKind::Success, format!("프로그램이 클립보드에 복사했어요 · {n}자"));
}

fn select_all(view: &View) {
    let mut ct = view.shared.lock();
    let top = Point::new(ct.term.topmost_line(), Column(0));
    let bottom = Point::new(ct.term.bottommost_line(), ct.term.last_column());
    let mut sel = Selection::new(SelectionType::Lines, top, Side::Left);
    sel.update(bottom, Side::Right);
    ct.term.selection = Some(sel);
}

/// Keyboard, IME and clipboard events for a focused terminal.
fn handle_keys(ctx: &egui::Context, view: &mut View, can_type: bool, sent: &mut Vec<u8>) {
    if !view.has_focus {
        view.preedit.clear();
        return;
    }
    let cmd = Modifiers::COMMAND;
    if ctx.input_mut(|i| i.consume_key(cmd, Key::F)) {
        view.find.open();
        return;
    }
    if ctx.input_mut(|i| i.consume_key(cmd, Key::A)) {
        select_all(view);
    }
    let events = ctx.input(|i| i.events.clone());
    // Keep Escape in the focused CLI, even when input is temporarily unavailable.
    ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape));
    let mode = *view.shared.lock().term.mode();
    let alt_screen = mode.contains(TermMode::ALT_SCREEN);
    let option_as_meta = crate::views::settings::general::option_as_meta();
    for ev in events {
        match ev {
            Event::Copy => copy_selection(ctx, view),
            // Line breaks without bracketed paste would run each line: ask first.
            Event::Paste(text) if can_type && input::paste_needs_confirm(&text, mode) => {
                view.ask = Some(Ask::Paste(text));
            }
            Event::Paste(text) if can_type => sent.extend(input::paste_bytes(&text, mode)),
            Event::Text(text) if can_type => {
                if std::mem::take(&mut view.suppress_text) {
                    continue; // the character Option produced; Meta already sent ESC + key
                }
                sent.extend_from_slice(text.as_bytes());
            }
            Event::Ime(egui::ImeEvent::Preedit { text, .. }) => {
                super::verify::record("preedit", &text);
                view.preedit = text;
            }
            Event::Ime(egui::ImeEvent::Commit(text)) => {
                view.preedit.clear();
                if can_type && !text.is_empty() && text != "\n" && text != "\r" {
                    sent.extend_from_slice(text.as_bytes());
                }
                // Keys that came with the commit (Korean: Enter/Space/arrows finish a syllable)
                // go after the committed text.
                sent.append(&mut view.deferred);
            }
            Event::Key { key, pressed: true, modifiers, .. } => {
                if modifiers.shift && !alt_screen && matches!(key, Key::PageUp | Key::PageDown) && !modifiers.ctrl {
                    let mut ct = view.shared.lock();
                    ct.term.scroll_display(if key == Key::PageUp { Scroll::PageUp } else { Scroll::PageDown });
                    continue;
                }
                if !can_type {
                    continue;
                }
                let modifiers = input::key_mods(key, modifiers, option_as_meta);
                let Some(bytes) = input::key_bytes(key, modifiers, mode) else { continue };
                if modifiers.alt && !modifiers.ctrl && bytes.first() == Some(&0x1b) && bytes.len() <= 5 {
                    view.suppress_text = true;
                }
                if view.preedit.is_empty() {
                    sent.extend(bytes);
                } else {
                    view.deferred.extend(bytes);
                }
            }
            _ => {}
        }
    }
    view.suppress_text = false;
    if view.preedit.is_empty() {
        sent.append(&mut view.deferred);
    } else {
        // Still composing: those keys belonged to the composition (candidate selection).
        view.deferred.clear();
    }
}

/// Selection, wheel and mouse reporting.
fn handle_pointer(
    ui: &egui::Ui,
    resp: &egui::Response,
    lay: &Layout,
    view: &mut View,
    can_report: bool,
    sent: &mut Vec<u8>,
) {
    let ctx = ui.ctx();
    let mut ct = view.shared.lock();
    let mode = *ct.term.mode();
    let (cols, rows) = (ct.term.columns(), ct.term.screen_lines());
    let offset = ct.term.grid().display_offset() as i32;
    let modifiers = ctx.input(|i| i.modifiers);
    // Shift keeps selection available while a program captures the mouse.
    let reporting = can_report && mode.intersects(TermMode::MOUSE_MODE) && !modifiers.shift;
    let to_point = |row: usize, col: usize| Point::new(Line(row as i32 - offset), Column(col));

    if resp.hovered() || resp.dragged() {
        for ev in ctx.input(|i| i.events.clone()) {
            match ev {
                Event::MouseWheel { unit, delta, modifiers, .. } if resp.hovered() => {
                    let lines = match unit {
                        egui::MouseWheelUnit::Point => delta.y / lay.m.cell.y,
                        egui::MouseWheelUnit::Line => delta.y,
                        egui::MouseWheelUnit::Page => delta.y * rows as f32,
                    };
                    view.wheel += lines;
                    let whole = view.wheel.trunc() as i32;
                    if whole == 0 {
                        continue;
                    }
                    view.wheel -= whole as f32;
                    let pos = ctx.input(|i| i.pointer.hover_pos()).unwrap_or(resp.rect.center());
                    let (row, col, _) = lay.cell_at(pos, cols, rows);
                    if reporting {
                        let button = if whole > 0 { Button::WheelUp } else { Button::WheelDown };
                        for _ in 0..whole.unsigned_abs().min(10) {
                            if let Some(b) = input::mouse_report(MouseAction::Press, button, col, row, modifiers, mode)
                            {
                                sent.extend(b);
                            }
                        }
                    } else if mode.contains(TermMode::ALT_SCREEN) && mode.contains(TermMode::ALTERNATE_SCROLL) {
                        if can_report {
                            let key = if whole > 0 { Key::ArrowUp } else { Key::ArrowDown };
                            for _ in 0..whole.unsigned_abs().min(10) {
                                if let Some(b) = input::key_bytes(key, Modifiers::NONE, mode) {
                                    sent.extend(b);
                                }
                            }
                        }
                    } else {
                        ct.term.scroll_display(Scroll::Delta(whole));
                    }
                }
                Event::PointerButton { pos, button, pressed, modifiers } if reporting && resp.rect.contains(pos) => {
                    if button == PointerButton::Primary {
                        // Ctrl+click opens the menu; its press and release stay out of the program.
                        let menu = if pressed { modifiers.ctrl } else { view.ctrl_press };
                        view.ctrl_press = pressed && modifiers.ctrl;
                        if menu {
                            continue;
                        }
                    }
                    let b = match button {
                        PointerButton::Primary => Button::Left,
                        PointerButton::Middle => Button::Middle,
                        PointerButton::Secondary => Button::Right,
                        _ => continue,
                    };
                    let (row, col, _) = lay.cell_at(pos, cols, rows);
                    let action = if pressed { MouseAction::Press } else { MouseAction::Release };
                    view.drag = pressed.then_some(b);
                    view.last_mouse_cell = Some((row, col));
                    if let Some(bytes) = input::mouse_report(action, b, col, row, modifiers, mode) {
                        sent.extend(bytes);
                    }
                }
                Event::PointerMoved(pos) if reporting && resp.rect.contains(pos) => {
                    let (row, col, _) = lay.cell_at(pos, cols, rows);
                    if view.last_mouse_cell != Some((row, col)) {
                        view.last_mouse_cell = Some((row, col));
                        let b = view.drag.unwrap_or(Button::Left);
                        if let Some(bytes) =
                            input::mouse_report(MouseAction::Motion(view.drag), b, col, row, modifiers, mode)
                        {
                            sent.extend(bytes);
                        }
                    }
                }
                _ => {}
            }
        }
    }
    if reporting {
        return;
    }

    // Selection with the primary button.
    let Some(pos) = resp.interact_pointer_pos() else {
        view.selecting = false;
        return;
    };
    let (row, col, right) = lay.cell_at(pos, cols, rows);
    let point = to_point(row, col);
    let side = if right { Side::Right } else { Side::Left };
    if resp.triple_clicked() {
        ct.term.selection = Some(Selection::new(SelectionType::Lines, point, side));
    } else if resp.double_clicked() {
        let mut sel = Selection::new(SelectionType::Semantic, point, side);
        sel.update(point, side);
        ct.term.selection = Some(sel);
    } else if resp.drag_started_by(PointerButton::Primary) {
        // Start where the button went down, not where the drag was recognised.
        let origin = ctx.input(|i| i.pointer.press_origin()).unwrap_or(pos);
        let (r0, c0, right0) = lay.cell_at(origin, cols, rows);
        let side0 = if right0 { Side::Right } else { Side::Left };
        let mut sel = Selection::new(SelectionType::Simple, to_point(r0, c0), side0);
        sel.update(point, side);
        ct.term.selection = Some(sel);
        view.selecting = true;
    } else if resp.dragged_by(PointerButton::Primary) && view.selecting {
        if let Some(sel) = ct.term.selection.as_mut() {
            sel.update(point, side);
        }
        // Dragging past the edge scrolls the history.
        if pos.y < resp.rect.top() {
            ct.term.scroll_display(Scroll::Delta(1));
        } else if pos.y > resp.rect.bottom() {
            ct.term.scroll_display(Scroll::Delta(-1));
        }
        ctx.request_repaint();
    } else if resp.clicked_by(PointerButton::Primary) && !modifiers.ctrl {
        // Ctrl+click opens the menu for the selection; keep it.
        ct.term.selection = None;
    }
    if resp.drag_stopped() {
        view.selecting = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn focused_terminal_owns_escape_before_panel_shortcuts() {
        for focused in [false, true] {
            for can_type in [false, true] {
                let ctx = egui::Context::default();
                let mut view = View::new(GridSize { cols: 80, rows: 24 });
                view.has_focus = focused;
                let mut sent = Vec::new();
                let input = egui::RawInput {
                    events: vec![Event::Key {
                        key: Key::Escape,
                        physical_key: None,
                        pressed: true,
                        repeat: false,
                        modifiers: Modifiers::NONE,
                    }],
                    ..Default::default()
                };
                let mut output = ctx.run_ui(input, |_| {
                    handle_keys(&ctx, &mut view, can_type, &mut sent);
                    assert_eq!(ctx.input(|i| i.key_pressed(Key::Escape)), !focused);
                });
                output.textures_delta.clear();
                assert_eq!(sent, if focused && can_type { vec![0x1b] } else { vec![] });
            }
        }
    }

    #[test]
    fn focus_changes_are_reported_only_when_the_program_asks() {
        let mut last = None;
        assert_eq!(focus_report(&mut last, true, true), None, "the first state is not a change");
        assert_eq!(focus_report(&mut last, true, true), None);
        assert_eq!(focus_report(&mut last, false, true), Some(&b"\x1b[O"[..]));
        assert_eq!(focus_report(&mut last, true, true), Some(&b"\x1b[I"[..]));
        assert_eq!(focus_report(&mut last, false, false), None, "mode off: nothing is sent");
        assert_eq!(focus_report(&mut last, false, true), None, "and no stale change later");
    }

    #[test]
    fn right_click_belongs_to_a_program_that_has_the_mouse() {
        let none = Modifiers::NONE;
        let shift = Modifiers { shift: true, ..Default::default() };
        let ctrl = Modifiers { ctrl: true, ..Default::default() };
        // Without mouse reporting a right-click opens the menu (and is never sent).
        assert_eq!(menu_click(Click::Secondary, none, false), Some(true));
        // With mouse reporting the program gets it; Shift+right-click and Ctrl+click open the menu.
        assert_eq!(menu_click(Click::Secondary, none, true), None);
        assert_eq!(menu_click(Click::Secondary, shift, true), Some(true));
        assert_eq!(menu_click(Click::Primary, ctrl, true), Some(true));
        assert_eq!(menu_click(Click::Primary, ctrl, false), Some(true));
        // A plain click closes it; no click leaves it as it is.
        assert_eq!(menu_click(Click::Primary, none, true), Some(false));
        assert_eq!(menu_click(Click::Primary, shift, false), Some(false));
        assert_eq!(menu_click(Click::None, ctrl, true), None);
    }
}
