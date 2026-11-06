//! General preferences of the person on this computer (S13 일반), stored on the host.

use crate::method;
use crate::rpc::topic;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Prefs {
    /// Scale of the whole interface; 1.0 is the design size.
    pub ui_scale: f32,
    /// Terminal text size in points.
    pub terminal_font_size: f32,
    pub reduce_motion: bool,
    /// The Option key sends Meta (Esc prefix) in terminals. Off lets macOS compose characters.
    pub option_as_meta: bool,
    /// Ask before stopping a run.
    pub confirm_stop: bool,
}

impl Default for Prefs {
    fn default() -> Self {
        Self { ui_scale: 1.0, terminal_font_size: 12.5, reduce_motion: false, option_as_meta: true, confirm_stop: true }
    }
}

pub const UI_SCALES: [f32; 4] = [0.9, 1.0, 1.1, 1.25];
pub const TERMINAL_FONT_MIN: f32 = 10.0;
pub const TERMINAL_FONT_MAX: f32 = 20.0;

impl Prefs {
    /// Keep values within supported ranges (older or hand-edited data).
    pub fn sanitized(mut self) -> Self {
        if !self.ui_scale.is_finite() {
            self.ui_scale = 1.0;
        }
        self.ui_scale = self.ui_scale.clamp(UI_SCALES[0], UI_SCALES[UI_SCALES.len() - 1]);
        if !self.terminal_font_size.is_finite() {
            self.terminal_font_size = 12.5;
        }
        self.terminal_font_size =
            ((self.terminal_font_size * 2.0).round() / 2.0).clamp(TERMINAL_FONT_MIN, TERMINAL_FONT_MAX);
        self
    }
}

method!(PrefsGet, "prefs.get", () => Prefs, [topic::SETTINGS]);
method!(PrefsSet, "prefs.set", Prefs => Prefs);

/// The command "외부 에디터로 열기" runs (host setting [`crate::api::files::EDITOR_SETTING`]).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct EditorSetting {
    /// Saved command line; empty means the system's text editor.
    pub command: String,
    /// Why the saved command would not start now (the program moved or was removed).
    #[serde(default)]
    pub problem: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SetEditor {
    /// As typed: a program name looked up on the login shell's `PATH` or an absolute path,
    /// then arguments (`code --wait`). Empty clears it.
    pub command: String,
}

method!(PrefsEditor, "prefs.editor", () => EditorSetting, [topic::SETTINGS]);
// Saved with the program resolved to its full path, so the host finds it whatever PATH it has.
method!(PrefsSetEditor, "prefs.set_editor", SetEditor => EditorSetting);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_out_of_range_values_and_reads_partial_json() {
        let p = Prefs { ui_scale: 9.0, terminal_font_size: 13.3, ..Default::default() }.sanitized();
        assert_eq!(p.ui_scale, 1.25);
        assert_eq!(p.terminal_font_size, 13.5);
        let p: Prefs = serde_json::from_str(r#"{"reduce_motion":true}"#).unwrap();
        assert!(p.reduce_motion && p.option_as_meta && p.confirm_stop);
        assert_eq!(p.terminal_font_size, 12.5);
    }
}
