//! Verification aid: `RUNWICK_SNAPSHOT=/path/out.png runwick` renders the window, saves a
//! screenshot once the UI has settled, and closes. Optional:
//! - `RUNWICK_SNAPSHOT_DELAY_MS` (default 2500) — wait before capturing.
//! - `RUNWICK_WINDOW=1100x760` — window size.
//! - `RUNWICK_UI_STATE='{"page":"Settings",...}'` — start from this UI state (JSON of `UiState`).
//! - `RUNWICK_SNAPSHOT_KEYS="cmd+k"` — not supported; drive state through `RUNWICK_UI_STATE`.
//!
//! Debug builds and the `verify-hooks` feature only: a release build has none of this.

#[cfg(any(debug_assertions, feature = "verify-hooks"))]
use std::{
    path::PathBuf,
    sync::Arc,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

#[cfg(any(debug_assertions, feature = "verify-hooks"))]
pub struct Snapshot {
    path: PathBuf,
    delay: Duration,
    started: Instant,
    requested: bool,
    done: Arc<AtomicBool>,
}

#[cfg(any(debug_assertions, feature = "verify-hooks"))]
impl Snapshot {
    pub fn from_env() -> Option<Self> {
        let path = hook_env!("RUNWICK_SNAPSHOT")?;
        let delay = hook_env!("RUNWICK_SNAPSHOT_DELAY_MS").and_then(|v| v.parse().ok()).unwrap_or(2500);
        Some(Self {
            path: PathBuf::from(path),
            delay: Duration::from_millis(delay),
            started: Instant::now(),
            requested: false,
            done: Arc::new(AtomicBool::new(false)),
        })
    }

    pub fn tick(&mut self, ctx: &egui::Context) {
        if self.done.load(Ordering::Relaxed) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        if !self.requested && self.started.elapsed() >= self.delay {
            self.requested = true;
            let path = self.path.clone();
            let done = self.done.clone();
            let ctx2 = ctx.clone();
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::ScreenshotCallback::new(move |img| {
                if let Err(e) = save_png(&path, &img) {
                    eprintln!("snapshot failed: {e}");
                }
                done.store(true, Ordering::Relaxed);
                ctx2.request_repaint();
            })));
        }
        ctx.request_repaint_after(Duration::from_millis(100));
    }
}

#[cfg(any(debug_assertions, feature = "verify-hooks"))]
fn save_png(path: &std::path::Path, img: &egui::ColorImage) -> anyhow::Result<()> {
    let [w, h] = img.size;
    let mut rgba = Vec::with_capacity(w * h * 4);
    for p in &img.pixels {
        rgba.extend_from_slice(&p.to_srgba_unmultiplied());
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).ok();
    }
    image::save_buffer(path, &rgba, w as u32, h as u32, image::ExtendedColorType::Rgba8)?;
    Ok(())
}

/// A snapshot run was asked for (verification builds only).
pub fn requested() -> bool {
    hook_env!("RUNWICK_SNAPSHOT").is_some()
}

/// Window size from `RUNWICK_WINDOW=WxH` (verification builds only).
pub fn window_size_from_env() -> Option<[f32; 2]> {
    let v = hook_env!("RUNWICK_WINDOW")?;
    let (w, h) = v.split_once('x')?;
    Some([w.trim().parse().ok()?, h.trim().parse().ok()?])
}
