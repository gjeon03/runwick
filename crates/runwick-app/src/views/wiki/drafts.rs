//! Unsaved wiki edits survive closing the app: they are kept in this computer's host (kv
//! `wiki.drafts`, also for wikis on other computers) shortly after each change and on exit, and
//! restored on the next start as "이 컴퓨터의 초안". A restored draft is compared with the
//! page's current version before it can be saved (see `page::editor`).

use super::page::{Edit, EditKey};
use crate::App;
use crate::client::HostKey;
use runwick_core::Ts;
use runwick_core::api::host::{KvGet, KvKey, KvPut, KvSet};
use runwick_core::api::projects::ProjectId;
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

const KEY: &str = "wiki.drafts";
const DEBOUNCE: Duration = Duration::from_millis(1500);
/// Larger pages are not kept as drafts.
const MAX_DRAFT_BYTES: usize = 2 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(super) struct Draft {
    pub host: HostKey,
    pub project: ProjectId,
    pub slug: String,
    /// Page version the edit started from.
    pub base_version: u32,
    pub title: String,
    pub body: String,
    pub tags: String,
    pub orig_title: String,
    pub orig_body: String,
    pub orig_tags: String,
    pub saved_at: Ts,
}

impl Draft {
    fn of(k: &EditKey, e: &Edit) -> Self {
        Draft {
            host: k.0,
            project: k.1,
            slug: k.2.clone(),
            base_version: e.base_version,
            title: e.title.clone(),
            body: e.body.clone(),
            tags: e.tags.clone(),
            orig_title: e.orig_title.clone(),
            orig_body: e.orig_body.clone(),
            orig_tags: e.orig_tags.clone(),
            saved_at: e.draft_at.unwrap_or_else(runwick_core::now),
        }
    }
}

#[derive(Default)]
pub(super) struct State {
    requested: bool,
    /// Drafts were read (or there were none); only then may they be written.
    restored: bool,
    changed_at: Option<Instant>,
    last_written: Option<String>,
}

/// An edit changed; write the drafts soon.
pub(super) fn touch(app: &mut App) {
    app.feat.wiki.drafts.changed_at = Some(Instant::now());
}

/// Put stored drafts back as edits. An edit already typed into this session is newer and wins.
pub(super) fn apply(app: &mut App, drafts: Vec<Draft>) -> usize {
    let mut n = 0;
    for d in drafts {
        let k: EditKey = (d.host, d.project, d.slug.clone());
        if app.feat.wiki.edits.get(&k).is_some_and(Edit::dirty) {
            continue;
        }
        app.feat.wiki.edits.insert(k, Edit::restored(&d));
        n += 1;
    }
    n
}

/// Read the drafts once this computer's host is connected.
pub(super) fn restore(app: &mut App) {
    let st = &mut app.feat.wiki.drafts;
    if st.requested || !app.hub.state(HostKey::Local).is_connected() {
        return;
    }
    st.requested = true;
    app.hub.call::<KvGet>(HostKey::Local, KvKey { key: KEY.into() }, |app, res| {
        let drafts = match res {
            Ok(Some(v)) => serde_json::from_value::<Vec<Draft>>(v).unwrap_or_else(|e| {
                tracing::warn!("wiki drafts ignored: {e}");
                Vec::new()
            }),
            Ok(None) => Vec::new(),
            Err(e) => {
                // Try again later; never overwrite drafts that could not be read.
                tracing::warn!("wiki drafts not restored: {}", e.message);
                app.feat.wiki.drafts.requested = false;
                return;
            }
        };
        app.feat.wiki.drafts.restored = true;
        let n = apply(app, drafts);
        if n > 0 {
            app.feat.wiki.drafts.last_written = None;
            tracing::info!("restored {n} unsaved wiki page(s)");
        }
    });
}

pub(super) fn collect(app: &App) -> Vec<Draft> {
    let mut v: Vec<Draft> = app
        .feat
        .wiki
        .edits
        .iter()
        .filter(|(_, e)| e.dirty() && e.body.len() <= MAX_DRAFT_BYTES)
        .map(|(k, e)| Draft::of(k, e))
        .collect();
    v.sort_by(|a, b| (a.host, a.project, &a.slug).cmp(&(b.host, b.project, &b.slug)));
    v
}

fn write(app: &mut App) {
    if !app.feat.wiki.drafts.restored || !app.hub.state(HostKey::Local).is_connected() {
        return;
    }
    let drafts = collect(app);
    // Compare without the time so unchanged drafts are not rewritten.
    let sig: String = drafts
        .iter()
        .map(|d| format!("{:?}{}{}{}{}{}\u{1}", d.host, d.project, d.slug, d.base_version, d.title, d.body))
        .chain(drafts.iter().map(|d| d.tags.clone()))
        .collect();
    app.feat.wiki.drafts.changed_at = None;
    if app.feat.wiki.drafts.last_written.as_deref() == Some(sig.as_str()) {
        return;
    }
    let Ok(value) = serde_json::to_value(&drafts) else { return };
    app.feat.wiki.drafts.last_written = Some(sig);
    app.hub.call::<KvPut>(HostKey::Local, KvSet { key: KEY.into(), value }, |app, res| {
        if let Err(e) = res {
            tracing::warn!("wiki drafts not saved: {}", e.message);
            app.feat.wiki.drafts.last_written = None;
            touch(app);
        }
    });
}

/// Write the drafts a moment after the last change.
pub(super) fn tick(app: &mut App, ctx: &egui::Context) {
    let Some(at) = app.feat.wiki.drafts.changed_at else { return };
    let waited = at.elapsed();
    if waited >= DEBOUNCE {
        write(app);
    } else {
        ctx.request_repaint_after(DEBOUNCE - waited);
    }
}

/// Write now (the window is closing).
pub(super) fn flush(app: &mut App) {
    if app.feat.wiki.drafts.changed_at.is_some() || app.feat.wiki.drafts.last_written.is_none() {
        write(app);
    }
}
