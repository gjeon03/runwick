//! Project wiki (S08): Markdown files under `RUNWICK_HOME/wiki/<project>/<slug>.md`, an index
//! with every saved version, links, similarity suggestions and AI change proposals.
//!
//! - `files`: frontmatter, slugs, atomic writes; `index`: the SQLite index and reconciling it
//!   with files edited outside Runwick; `ops`: create/save/import/apply (all writes)
//! - `merge`: line diffs and re-comparing proposals with a page that moved on
//! - `proposals`: proposal rows and the "결정 확인 필요" check; `links`/`graph`: relationships

pub mod files;
mod graph;
pub mod index;
pub mod links;
pub mod merge;
pub mod ops;
pub mod proposals;

use crate::host::{Ctx, Host};
use crate::jobs::records as rec;
use crate::router::Router;
use crate::store::Migration;
use runwick_core::api::timeline::{EventKind, EventLink, NewEvent};
use runwick_core::api::wiki::*;
use runwick_core::paths;
use runwick_core::rpc::{RpcError, topic};
use std::sync::Arc;

pub const MIGRATIONS: &[Migration] = &[(
    "0101_wiki",
    "CREATE TABLE wiki_pages (
        id BLOB PRIMARY KEY,
        project_id BLOB NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
        slug TEXT NOT NULL,
        title TEXT NOT NULL,
        created_by TEXT NOT NULL,
        status TEXT NOT NULL,
        sources TEXT NOT NULL DEFAULT '[]',
        tags TEXT NOT NULL DEFAULT '[]',
        version INTEGER NOT NULL,
        hash TEXT NOT NULL,
        file_mtime INTEGER NOT NULL DEFAULT 0,
        file_size INTEGER NOT NULL DEFAULT 0,
        created_at TEXT NOT NULL,
        updated_at TEXT NOT NULL,
        -- The file was removed outside Runwick; versions stay for history.
        deleted INTEGER NOT NULL DEFAULT 0,
        UNIQUE (project_id, slug)
    );
    CREATE TABLE wiki_versions (
        page_id BLOB NOT NULL REFERENCES wiki_pages(id) ON DELETE CASCADE,
        version INTEGER NOT NULL,
        title TEXT NOT NULL,
        body TEXT NOT NULL,
        saved_by TEXT NOT NULL,
        note TEXT,
        saved_at TEXT NOT NULL,
        PRIMARY KEY (page_id, version)
    );
    CREATE TABLE wiki_proposals (
        id BLOB PRIMARY KEY,
        project_id BLOB NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
        -- NULL proposes a new page.
        page_id BLOB REFERENCES wiki_pages(id) ON DELETE CASCADE,
        base_version INTEGER,
        base_body TEXT,
        title TEXT NOT NULL,
        body TEXT NOT NULL,
        rationale TEXT NOT NULL,
        sources TEXT NOT NULL DEFAULT '[]',
        decision_flag TEXT,
        status TEXT NOT NULL,
        job_id BLOB,
        created_at TEXT NOT NULL,
        decided_at TEXT
    );
    CREATE INDEX wiki_proposals_project ON wiki_proposals(project_id, status);
    CREATE TABLE wiki_dismissed (
        project_id BLOB NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
        a TEXT NOT NULL,
        b TEXT NOT NULL,
        created_at TEXT NOT NULL,
        PRIMARY KEY (project_id, a, b)
    );",
)];

fn root() -> std::path::PathBuf {
    paths::wiki_dir()
}

/// Run blocking wiki work (file IO, hashing, similarity) off the async workers.
async fn blocking<T: Send + 'static>(
    host: &Arc<Host>,
    f: impl FnOnce(&Host) -> Result<T, RpcError> + Send + 'static,
) -> Result<T, RpcError> {
    let host = host.clone();
    tokio::task::spawn_blocking(move || f(&host)).await.map_err(RpcError::internal)?
}

/// Note a wiki change on the project's timeline (one row per page per day for edits).
fn timeline(host: &Host, page: &PageSummary, what: &str, dedupe_per_day: bool) {
    let day = chrono::Local::now().format("%Y-%m-%d");
    let event = NewEvent {
        project_id: page.project_id,
        kind: EventKind::Wiki,
        title: format!("위키 “{}” {what}", page.title),
        at: runwick_core::now(),
        session_id: None,
        link: Some(EventLink::Wiki { page: page.slug.clone() }),
        dedupe_key: dedupe_per_day.then(|| format!("wiki:{}:{}:{day}", page.project_id, page.slug)),
    };
    rec::record_event(host, event);
}

pub fn register(r: &mut Router) {
    r.add::<WikiList, _, _>(|ctx: Ctx, q: WikiQuery| async move {
        blocking(&ctx.host, move |h| {
            let changed = ops::refresh(&h.store, &root(), q.project_id)?;
            if changed {
                h.bus.changed(&[topic::WIKI]);
            }
            ops::list(&h.store, &root(), q.project_id)
        })
        .await
    });
    r.add::<WikiGet, _, _>(|ctx: Ctx, p: PageRef| async move {
        blocking(&ctx.host, move |h| ops::get(&h.store, &root(), p.project_id, &p.slug)).await
    });
    r.add::<WikiCreate, _, _>(|ctx: Ctx, p: CreatePage| async move {
        blocking(&ctx.host, move |h| {
            let page = ops::create(
                &h.store,
                &root(),
                ops::NewPage {
                    project_id: p.project_id,
                    title: p.title,
                    body: p.body,
                    tags: p.tags,
                    sources: p.sources,
                    created_by: Author::User,
                    status: PageStatus::Reviewed,
                    saved_by: SavedBy::User,
                    note: Some("새 문서".into()),
                    slug: None,
                },
            )?;
            timeline(h, &page.summary, "새 문서", false);
            h.bus.changed(&[topic::WIKI]);
            Ok(page)
        })
        .await
    });
    r.add::<WikiImport, _, _>(|ctx: Ctx, p: ImportPages| async move {
        blocking(&ctx.host, move |h| {
            let report = ops::import(&h.store, &root(), &p)?;
            if !report.created.is_empty() {
                h.bus.changed(&[topic::WIKI]);
            }
            Ok(report)
        })
        .await
    });
    r.add::<WikiSave, _, _>(|ctx: Ctx, s: SavePage| async move {
        blocking(&ctx.host, move |h| {
            let out = ops::save(
                &h.store,
                &root(),
                ops::Save {
                    project_id: s.project_id,
                    slug: &s.slug,
                    base_version: s.base_version,
                    title: &s.title,
                    body: &s.body,
                    tags: s.tags.as_deref(),
                    mark_reviewed: s.mark_reviewed,
                    add_sources: &[],
                    saved_by: SavedBy::User,
                    note: None,
                },
            )?;
            if let SaveOutcome::Saved { page } = &out {
                timeline(h, &page.summary, "수정", true);
                h.bus.changed(&[topic::WIKI]);
            }
            Ok(out)
        })
        .await
    });
    r.add::<WikiHistory, _, _>(|ctx: Ctx, p: PageRef| async move {
        blocking(&ctx.host, move |h| ops::history(&h.store, &root(), p.project_id, &p.slug)).await
    });
    r.add::<WikiVersion, _, _>(|ctx: Ctx, v: VersionRef| async move {
        blocking(&ctx.host, move |h| ops::version(&h.store, &v)).await
    });
    r.add::<WikiGraph, _, _>(|ctx: Ctx, q: WikiQuery| async move {
        blocking(&ctx.host, move |h| graph::build(&h.store, &root(), q.project_id)).await
    });
    r.add::<WikiLink, _, _>(|ctx: Ctx, p: PagePair| async move {
        blocking(&ctx.host, move |h| {
            let page = ops::link(&h.store, &root(), &p)?;
            h.bus.changed(&[topic::WIKI]);
            Ok(page)
        })
        .await
    });
    r.add::<WikiDismiss, _, _>(|ctx: Ctx, p: PagePair| async move {
        ops::dismiss(&ctx.host.store, &p)?;
        ctx.host.bus.changed(&[topic::WIKI]);
        Ok(())
    });
    r.add::<WikiProposals, _, _>(|ctx: Ctx, q: ProposalQuery| async move {
        ctx.host.store.with(|c| proposals::list(c, &q)).map_err(crate::store::db_err)
    });
    r.add::<WikiProposalGet, _, _>(|ctx: Ctx, p: ProposalAction| async move {
        blocking(&ctx.host, move |h| ops::proposal_detail(&h.store, &root(), p.id)).await
    });
    r.add::<WikiApply, _, _>(|ctx: Ctx, a: ApplyProposal| async move {
        blocking(&ctx.host, move |h| {
            let (page, p) = ops::apply(&h.store, &root(), &a)?;
            let what = if p.status == ProposalStatus::Partial { "제안 일부 적용" } else { "제안 적용" };
            timeline(h, &page.summary, what, false);
            h.bus.changed(&[topic::WIKI]);
            Ok(page)
        })
        .await
    });
    r.add::<WikiHold, _, _>(|ctx: Ctx, p: ProposalAction| async move {
        let out = ops::set_proposal_status(&ctx.host.store, p.id, ProposalStatus::Held)?;
        ctx.host.bus.changed(&[topic::WIKI]);
        Ok(out)
    });
    r.add::<WikiReject, _, _>(|ctx: Ctx, p: ProposalAction| async move {
        let out = ops::set_proposal_status(&ctx.host.store, p.id, ProposalStatus::Rejected)?;
        ctx.host.bus.changed(&[topic::WIKI, topic::JOBS]);
        Ok(out)
    });
}

#[cfg(test)]
mod tests;
