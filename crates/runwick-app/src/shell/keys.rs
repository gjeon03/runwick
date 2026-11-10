//! Global shortcuts. Only Command shortcuts are taken while a terminal or text field has focus;
//! nothing is intercepted while an input method is composing (Korean, Japanese, ...).

use super::actions;
use crate::App;
use crate::state::RightPanel;
use egui::{Event, ImeEvent, Key, KeyboardShortcut, Modifiers};

#[derive(Default)]
pub struct State {
    /// An IME composition is in progress.
    pub composing: bool,
}

pub fn track_ime(app: &mut App, ctx: &egui::Context) {
    ctx.input(|i| {
        for e in &i.events {
            if let Event::Ime(ime) = e {
                match ime {
                    ImeEvent::Preedit { text, .. } => app.shell.keys.composing = !text.is_empty(),
                    ImeEvent::Commit(_) => app.shell.keys.composing = false,
                    _ => {}
                }
            }
        }
    });
}

fn take(ctx: &egui::Context, mods: Modifiers, key: Key) -> bool {
    ctx.input_mut(|i| i.consume_shortcut(&KeyboardShortcut::new(mods, key)))
}

const DIGITS: [Key; 9] =
    [Key::Num1, Key::Num2, Key::Num3, Key::Num4, Key::Num5, Key::Num6, Key::Num7, Key::Num8, Key::Num9];

/// Handle global shortcuts before any widget sees the keys.
pub fn handle(app: &mut App, ctx: &egui::Context) {
    super::center::prepare_panes(app, ctx);
    if app.shell.keys.composing {
        return;
    }
    let cmd = Modifiers::COMMAND;
    // ⌘K toggles the palette from anywhere except over a modal dialog.
    if take(ctx, cmd, Key::K)
        && app.ui.modal.is_none()
        && app.shell.modal.stop.is_none()
        && app.shell.center.picker.is_none()
    {
        if app.ui.palette_open {
            super::palette::close(app, ctx);
        } else {
            super::palette::open(app, ctx);
        }
        return;
    }
    if app.ui.palette_open
        || app.ui.modal.is_some()
        || app.shell.modal.stop.is_some()
        || app.shell.center.picker.is_some()
    {
        // The open overlay owns Esc and the remaining keys.
        return;
    }
    if take(ctx, cmd | Modifiers::SHIFT, Key::F) {
        app.ui.page = crate::state::Page::Work;
        app.ui.right = Some(RightPanel::History);
    } else if take(ctx, cmd, Key::T) {
        actions::new_session(app);
    } else if take(ctx, cmd, Key::O) {
        actions::open_folder_dialog(app);
    } else if take(ctx, cmd, Key::Comma) {
        actions::open_settings(app, None);
    } else if take(ctx, cmd, Key::W) {
        close_active_tab(app);
    } else if let Some(n) = DIGITS.iter().position(|k| take(ctx, cmd, *k)) {
        focus_tab(app, n);
    } else if ctx.input(|i| i.key_pressed(Key::Escape) && i.modifiers.is_none()) {
        escape(app, ctx);
    }
}

fn close_active_tab(app: &mut App) {
    if app.ui.page != crate::state::Page::Work {
        return;
    }
    let Some(ws) = app.ui.selected_ws else { return };
    let Some(set) = app.ui.tabset(&ws) else { return };
    let target = set.focused_tab();
    if let Some(id) = target {
        actions::close_tab(app, ws, id);
    }
}

fn focus_tab(app: &mut App, n: usize) {
    let Some(ws) = app.ui.selected_ws else { return };
    let set = app.ui.tabset_mut(&ws);
    set.select_number(n);
    app.ui.page = crate::state::Page::Work;
}

/// Esc closes the topmost app overlay. In a terminal it belongs to the CLI.
fn escape(app: &mut App, ctx: &egui::Context) {
    if app.shell.sidebar.flyout {
        app.shell.sidebar.flyout = false;
        consume_esc(ctx);
        return;
    }
    if app.ui.page == crate::state::Page::Settings {
        if !ctx.text_edit_focused() {
            app.ui.page = crate::state::Page::Work;
            consume_esc(ctx);
        }
        return;
    }
    // A focused terminal (it owns Esc, Cmd+F, ...) or a text field outside the panel (sidebar
    // search, document) keeps its own Esc.
    let terminal = crate::terminal::focused_session(app).is_some();
    let typing_elsewhere = ctx.text_edit_focused() && !focus_in(app.shell.right.rect, ctx);
    if app.ui.right.is_some() && !terminal && !focus_in(app.shell.center.rect, ctx) && !typing_elsewhere {
        super::right::close(app, ctx);
        consume_esc(ctx);
    }
}

fn consume_esc(ctx: &egui::Context) {
    ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape));
}

/// The keyboard focus is inside `area` (the center: a terminal or document gets Esc).
fn focus_in(area: Option<egui::Rect>, ctx: &egui::Context) -> bool {
    let Some(id) = ctx.memory(|m| m.focused()) else { return false };
    let Some(rect) = ctx.read_response(id).map(|r| r.rect) else { return false };
    area.is_some_and(|a| a.contains(rect.center()))
}
