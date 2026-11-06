//! Runwick desktop app.

/// Verification hooks (scripted input, snapshots, a UI state or screen chosen through the
/// environment) exist only in debug builds and with the `verify-hooks` feature. A release build
/// neither reads these variables nor contains their names, so no other program on the computer
/// (an agent in a managed terminal included) can drive a Runwick window through them.
macro_rules! hook_env {
    ($name:literal) => {{
        #[cfg(any(debug_assertions, feature = "verify-hooks"))]
        let value = std::env::var($name).ok().filter(|v| !v.is_empty());
        #[cfg(not(any(debug_assertions, feature = "verify-hooks")))]
        let value: Option<String> = None;
        value
    }};
}

pub mod app;
pub mod client;
pub mod icons;
pub mod presence;
pub mod remote;
pub mod shell;
pub mod snapshot;
pub mod state;
pub mod terminal;
pub mod theme;
pub mod views;

pub use app::App;
use std::path::PathBuf;

pub struct Options {
    /// Path of the running `runwick` executable (used to start the host).
    pub exe: PathBuf,
}

/// Window and Dock icon (rendered from assets/icons/runwick.svg by scripts/render-icons.sh).
const APP_ICON_PNG: &[u8] = include_bytes!("../../../assets/icons/runwick-512.png");

pub fn run(opts: Options) -> anyhow::Result<()> {
    let viewport = egui::ViewportBuilder::default()
        .with_title("Runwick")
        .with_app_id("dev.runwick.Runwick")
        .with_inner_size(snapshot::window_size_from_env().unwrap_or([1440.0, 900.0]))
        .with_min_inner_size([760.0, 520.0])
        .with_fullsize_content_view(true)
        .with_titlebar_shown(false)
        .with_title_shown(false);
    // Snapshot runs must not steal focus from whatever the person is doing.
    let viewport = if snapshot::requested() { viewport.with_active(false) } else { viewport };
    let viewport = match eframe::icon_data::from_png_bytes(APP_ICON_PNG) {
        Ok(icon) => viewport.with_icon(icon),
        Err(e) => {
            tracing::warn!("app icon not loaded: {e}");
            viewport
        }
    };
    let options = eframe::NativeOptions { viewport, ..Default::default() };
    // Held until the window closes: the menu bar process steps aside while it is held.
    let _window_lock = presence::hold_window_lock();
    eframe::run_native("Runwick", options, Box::new(move |cc| Ok(Box::new(App::new(cc, opts.exe)))))
        .map_err(|e| anyhow::anyhow!("{e}"))
}
