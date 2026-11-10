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
    /// A dialog/editor that owned Escape at frame start may close while it handles the key.
    escape_owned: bool,
    /// egui surrenders ordinary editor focus before widgets run on an Escape frame.
    text_edit_focused: bool,
}

impl State {
    fn begin_frame(&mut self, ctx: &egui::Context, overlay: bool) {
        self.escape_owned = self.text_edit_focused || ctx.text_edit_focused() || overlay;
    }

    fn unhandled_escape(&mut self, ctx: &egui::Context, overlay: bool) -> bool {
        self.text_edit_focused = ctx.text_edit_focused();
        !self.composing
            && !self.escape_owned
            && !overlay
            && !self.text_edit_focused
            && ctx.input(|i| i.key_pressed(Key::Escape) && i.modifiers.is_none())
    }
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
    let overlay = overlay_owns_escape(app);
    app.shell.keys.begin_frame(ctx, overlay);
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
        app.ui.show_panel_root(RightPanel::History, None);
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
    }
}

fn overlay_owns_escape(app: &App) -> bool {
    app.ui.palette_open
        || app.ui.modal.is_some()
        || app.shell.modal.stop.is_some()
        || app.shell.center.picker.is_some()
        || crate::views::collab::peek_is_open(app)
}

/// Widgets see Escape first. A terminal consumes it after encoding it for the CLI.
pub fn handle_unhandled_escape(app: &mut App, ctx: &egui::Context) {
    let overlay = overlay_owns_escape(app);
    if app.shell.keys.unhandled_escape(ctx, overlay) {
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
    if app.ui.right.is_some() && !crate::views::collab::peek_is_open(app) {
        super::right::close(app, ctx);
        consume_esc(ctx);
    }
}

fn consume_esc(ctx: &egui::Context) {
    ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn escape_key() -> Event {
        Event::Key { key: Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE }
    }

    #[test]
    fn editor_handles_escape_before_panel_and_next_unhandled_escape_can_close_it() {
        let ctx = egui::Context::default();
        let mut keys = State::default();
        let mut text = "Draft".to_owned();
        for frame in 0..3 {
            let input =
                egui::RawInput { events: if frame == 0 { vec![] } else { vec![escape_key()] }, ..Default::default() };
            let mut output = ctx.run_ui(input, |ui| {
                keys.begin_frame(&ctx, false);
                let editor = ui.text_edit_singleline(&mut text);
                if frame == 0 {
                    editor.request_focus();
                }
                assert_eq!(keys.unhandled_escape(&ctx, false), frame == 2);
            });
            output.textures_delta.clear();
        }
    }

    #[test]
    fn consumed_escape_and_dismissed_overlays_do_not_close_the_panel() {
        let ctx = egui::Context::default();
        for overlay in [false, true] {
            let mut keys = State::default();
            let input = egui::RawInput { events: vec![escape_key()], ..Default::default() };
            let mut output = ctx.run_ui(input, |_| {
                keys.begin_frame(&ctx, overlay);
                if !overlay {
                    consume_esc(&ctx);
                }
                assert!(!keys.unhandled_escape(&ctx, false));
            });
            output.textures_delta.clear();
        }
    }
}
