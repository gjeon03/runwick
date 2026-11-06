//! The application object: owns host connections, UI state and feature states, and lays out
//! the window each frame.

use crate::client::{EVENT_CONN_READY, HostKey, Hub};
use crate::state::{Toast, ToastKind, UiState};
use crate::{shell, terminal, theme, views};
use serde_json::Value;
use std::path::PathBuf;

/// Per-feature view state. Each feature owns its `State` type in its own module.
#[derive(Default)]
pub struct Features {
    pub files: views::files::State,
    pub docs: views::docs::State,
    pub history: views::history::State,
    pub wiki: views::wiki::State,
    pub timeline: views::timeline::State,
    pub settings: views::settings::State,
    pub collab: views::collab::State,
    pub activity: views::activity::State,
    pub refresh: views::refresh::State,
    pub split: views::split::State,
    pub lifecycle: views::lifecycle::State,
    pub devices: views::devices::State,
    pub backup: views::backup::State,
}

pub struct App {
    pub hub: Hub,
    pub ui: UiState,
    pub feat: Features,
    pub terms: terminal::Terminals,
    pub shell: shell::State,
    pub toasts: Vec<Toast>,
    pub exe: PathBuf,
    #[cfg(any(debug_assertions, feature = "verify-hooks"))]
    snapshot: Option<crate::snapshot::Snapshot>,
    /// Scripted input for verification runs (`RUNWICK_VERIFY_INPUT`, see terminal/verify.rs).
    #[cfg(any(debug_assertions, feature = "verify-hooks"))]
    verify: Option<terminal::verify::Script>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, exe: PathBuf) -> Self {
        theme::install_fonts(&cc.egui_ctx);
        theme::apply_style(&cc.egui_ctx);
        let hub = Hub::new(cc.egui_ctx.clone(), exe.clone());
        // Verification runs may start from a given UI state (see snapshot.rs).
        let ui =
            hook_env!("RUNWICK_UI_STATE").and_then(|s| serde_json::from_str::<UiState>(&s).ok()).unwrap_or_default();
        Self {
            hub,
            ui,
            feat: Features::default(),
            terms: terminal::Terminals::default(),
            shell: shell::State::default(),
            toasts: Vec::new(),
            exe,
            #[cfg(any(debug_assertions, feature = "verify-hooks"))]
            snapshot: crate::snapshot::Snapshot::from_env(),
            #[cfg(any(debug_assertions, feature = "verify-hooks"))]
            verify: terminal::verify::Script::from_env(),
        }
    }

    pub fn toast(&mut self, kind: ToastKind, text: impl Into<String>) {
        self.toasts.push(Toast { kind, text: text.into(), created: std::time::Instant::now() });
    }

    pub fn toast_error(&mut self, text: &str) {
        self.toast(ToastKind::Error, text);
    }

    pub fn toast_info(&mut self, text: &str) {
        self.toast(ToastKind::Info, text);
    }

    fn on_event(&mut self, host: HostKey, topic: &str, data: &Value) {
        if topic == EVENT_CONN_READY {
            shell::on_connected(self, host);
        }
        terminal::on_event(self, host, topic, data);
        views::on_event(self, host, topic, data);
    }
}

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        #[cfg(any(debug_assertions, feature = "verify-hooks"))]
        if let Some(s) = self.snapshot.as_mut() {
            s.tick(ctx);
        }
        let (events, tasks) = self.hub.pump();
        for task in tasks {
            task(self);
        }
        for (host, topic, data) in events {
            self.on_event(host, &topic, &data);
        }
        // Saved general preferences (scale, motion, terminal keys and font) apply everywhere.
        views::settings::general::on_frame(self, ctx);
        terminal::end_frame(self);
    }

    #[cfg(any(debug_assertions, feature = "verify-hooks"))]
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        if let Some(mut script) = self.verify.take() {
            script.hook(self, ctx, raw_input);
            self.verify = Some(script);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        shell::show(self, ui, frame);
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        egui::Rgba::from(theme::c::WINDOW).to_array()
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        shell::on_exit(self);
    }
}
