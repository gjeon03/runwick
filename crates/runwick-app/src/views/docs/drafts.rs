//! Unsaved buffers survive closing the app: they are kept in this computer's host (kv
//! `docs.drafts`, also for documents of remote devices) shortly after each edit and on exit, and
//! restored on the next start. A restored draft is checked against the disk before saving.

use super::Entry;
use super::model::{Doc, Draft};
use crate::App;
use crate::client::HostKey;
use runwick_core::api::host::{KvGet, KvKey, KvPut, KvSet};
use std::time::{Duration, Instant};

const KEY: &str = "docs.drafts";
const DEBOUNCE: Duration = Duration::from_millis(1500);
/// Larger buffers are not kept as drafts (editing is limited well below this).
const MAX_DRAFT_BYTES: usize = 2 * 1024 * 1024;

#[derive(Default)]
pub struct State {
    requested: bool,
    /// Drafts were read (or there were none); only then may they be written.
    restored: bool,
    changed_at: Option<Instant>,
    last_written: Option<String>,
}

/// Something about unsaved buffers changed; write the drafts soon.
pub fn touch(app: &mut App) {
    app.feat.docs.drafts.changed_at = Some(Instant::now());
}

/// Read the drafts once this computer's host is connected.
pub fn restore(app: &mut App) {
    let st = &mut app.feat.docs.drafts;
    if st.requested || !app.hub.state(HostKey::Local).is_connected() {
        return;
    }
    st.requested = true;
    app.hub.call::<KvGet>(HostKey::Local, KvKey { key: KEY.into() }, |app, res| {
        let drafts = match res {
            Ok(Some(v)) => serde_json::from_value::<Vec<Draft>>(v).unwrap_or_else(|e| {
                tracing::warn!("drafts ignored: {e}");
                Vec::new()
            }),
            Ok(None) => Vec::new(),
            Err(e) => {
                // Keep trying later; never overwrite drafts we could not read.
                tracing::warn!("drafts not restored: {}", e.message);
                app.feat.docs.drafts.requested = false;
                return;
            }
        };
        app.feat.docs.drafts.restored = true;
        let n = drafts.len();
        for d in drafts {
            let key = (d.ws, d.path.clone());
            match app.feat.docs.entries.get_mut(&key) {
                // Typed into before the drafts arrived: keep that buffer, it is newer.
                Some(e) if e.doc.dirty() => {}
                // Opened (maybe still loading) before the drafts arrived: the draft replaces the
                // untouched buffer and is checked against the disk like any restored draft.
                Some(e) => {
                    e.doc = Doc::from_draft(&d);
                    e.ui.reading = false;
                    e.ui.polling = false;
                }
                None => {
                    app.feat.docs.entries.insert(key, Entry::new(Doc::from_draft(&d)));
                }
            }
        }
        if n > 0 {
            app.feat.docs.drafts.last_written = None;
            tracing::info!("restored {n} unsaved document(s)");
        }
    });
}

fn collect(app: &App) -> Vec<Draft> {
    let mut v: Vec<Draft> = app
        .feat
        .docs
        .entries
        .values()
        .filter_map(|e| e.doc.draft())
        .filter(|d| d.text.len() <= MAX_DRAFT_BYTES)
        .collect();
    v.sort_by(|a, b| (a.ws, &a.path).cmp(&(b.ws, &b.path)));
    v
}

fn write(app: &mut App) {
    if !app.feat.docs.drafts.restored || !app.hub.state(HostKey::Local).is_connected() {
        return;
    }
    let drafts = collect(app);
    // Compare without the save time so unchanged drafts are not rewritten.
    let sig = drafts.iter().map(|d| format!("{:?}{}{}\u{1}", d.ws, d.path, d.text)).collect::<String>();
    app.feat.docs.drafts.changed_at = None;
    if app.feat.docs.drafts.last_written.as_deref() == Some(sig.as_str()) {
        return;
    }
    let Ok(value) = serde_json::to_value(&drafts) else {
        return;
    };
    app.feat.docs.drafts.last_written = Some(sig);
    app.hub.call::<KvPut>(HostKey::Local, KvSet { key: KEY.into(), value }, |app, res| {
        if let Err(e) = res {
            tracing::warn!("drafts not saved: {}", e.message);
            app.feat.docs.drafts.last_written = None;
            touch(app);
        }
    });
}

/// Write the drafts a moment after the last change.
pub fn tick(app: &mut App, ctx: &egui::Context) {
    let Some(at) = app.feat.docs.drafts.changed_at else {
        return;
    };
    let waited = at.elapsed();
    if waited >= DEBOUNCE {
        write(app);
    } else {
        ctx.request_repaint_after(DEBOUNCE - waited);
    }
}

/// Write now (the window is closing).
pub fn flush(app: &mut App) {
    if app.feat.docs.drafts.changed_at.is_some() || app.feat.docs.drafts.last_written.is_none() {
        write(app);
    }
}
