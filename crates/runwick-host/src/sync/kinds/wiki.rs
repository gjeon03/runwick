//! `wiki/<project>/<slug>.md`: a page's current text with a small frontmatter (title, status,
//! tags, sources). Version numbers stay per computer; each computer keeps its own history.
//!
//! A page changed on both computers since the last sync keeps this computer's text and stores
//! the other text as a proposal ("다른 컴퓨터의 변경") to review on the page. Everything goes
//! through the wiki's own save path, so every change is a version here too.

use super::{Cx, Tally};
use crate::store::db_err;
use crate::wiki::files::{self, Meta};
use crate::wiki::{index, ops};
use runwick_core::api::sync::SyncKind;
use runwick_core::api::wiki::{Author, PageStatus, SaveOutcome, SavedBy};
use runwick_core::rpc::{RpcError, topic};
use rusqlite::OptionalExtension;
use std::collections::{BTreeMap, HashSet};
use uuid::Uuid;

pub const DIR: &str = "wiki";
const NOTE: &str = "다른 컴퓨터에서 동기화";

pub fn path_of(project: Uuid, slug: &str) -> String {
    format!("{DIR}/{project}/{slug}.md")
}

/// `(project, slug)` of a page path.
pub fn parse_path(path: &str) -> Option<(Uuid, String)> {
    let mut parts = path.split('/');
    let (Some(DIR), Some(p), Some(file), None) = (parts.next(), parts.next(), parts.next(), parts.next()) else {
        return None;
    };
    // Composed (NFC): a name written decomposed elsewhere is the same page.
    let slug = runwick_core::nfc::hangul(file.strip_suffix(".md")?);
    if !valid_slug(&slug) {
        return None;
    }
    Some((p.parse().ok()?, slug))
}

/// A page name that is safe as a single file name: no separators, no `..`, not hidden.
pub fn valid_slug(slug: &str) -> bool {
    !slug.is_empty() && !slug.starts_with('.') && !slug.contains(['/', '\\', '\0']) && slug.len() <= 255
}

/// The file text for a page.
pub fn render_page(title: &str, status: PageStatus, tags: &[String], sources: &[Uuid], body: &str) -> String {
    let js = |v: &str| serde_json::to_string(v).unwrap_or_else(|_| "\"\"".into());
    let status = match status {
        PageStatus::Draft => "draft",
        PageStatus::Reviewed => "reviewed",
    };
    let sources: Vec<String> = sources.iter().map(Uuid::to_string).collect();
    let body = body.trim_start_matches(['\n', '\r']).trim_end_matches(['\n', '\r']);
    format!(
        "---\ntitle: {}\nstatus: {status}\ntags: {}\nsources: {}\n---\n\n{body}\n",
        js(title),
        serde_json::to_string(tags).unwrap_or_else(|_| "[]".into()),
        serde_json::to_string(&sources).unwrap_or_else(|_| "[]".into()),
    )
}

struct Doc {
    title: String,
    status: PageStatus,
    tags: Vec<String>,
    sources: Vec<Uuid>,
    body: String,
}

fn read(text: &str, slug: &str) -> Doc {
    let d = files::parse(text);
    let body = d.body.trim_start_matches(['\n', '\r']).trim_end_matches(['\n', '\r']).to_string() + "\n";
    Doc {
        title: d
            .meta
            .title
            .clone()
            .filter(|t| !t.trim().is_empty())
            .unwrap_or_else(|| files::title_from(&d.body, slug)),
        status: d.meta.status.unwrap_or(PageStatus::Reviewed),
        tags: d.meta.tags,
        sources: d.meta.sources,
        body,
    }
}

/// Same text, the way this module writes it (so formatting never looks like a change).
fn normalized(text: &str, slug: &str) -> String {
    let d = read(text, slug);
    render_page(&d.title, d.status, &d.tags, &d.sources, &d.body)
}

/// Every page of this computer's projects, by path.
pub fn render(cx: &Cx) -> Result<BTreeMap<String, String>, RpcError> {
    ops::refresh(cx.store, cx.wiki_root, None)?;
    cx.store
        .with(|c| {
            let synced: HashSet<Uuid> = {
                let mut st = c.prepare(&format!("SELECT id FROM projects p WHERE {}", super::projects::SYNCED))?;
                st.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?
            };
            let mut out = BTreeMap::new();
            for row in index::list(c, None)? {
                if !synced.contains(&row.project_id) {
                    continue;
                }
                let body = index::body(c, row.id, row.version)?.map(|(_, b)| b).unwrap_or_default();
                out.insert(
                    path_of(row.project_id, &row.slug),
                    render_page(&row.title, row.status, &row.tags, &row.sources, &body),
                );
            }
            Ok(out)
        })
        .map_err(db_err)
}

/// Bring one page in line with the other computer's version.
pub fn apply(cx: &Cx, path: &str, base: Option<&str>, local: Option<&str>, remote: Option<&str>, tally: &mut Tally) {
    let Some((project, slug)) = parse_path(path) else {
        tally.skip(path, "위키 파일 이름이 아니에요");
        return;
    };
    match cx.store.with(|c| super::project_exists(c, project)) {
        Ok(true) => {}
        Ok(false) => return tally.skip(path, "프로젝트가 아직 없어요"),
        Err(e) => return tally.skip(path, e.to_string()),
    }
    let norm = |t: Option<&str>| t.map(|t| normalized(t, &slug));
    let (b, l, r) = (norm(base), norm(local), norm(remote));
    if l == r {
        return;
    }
    let result = if b == l {
        take(cx, path, project, &slug, r.as_deref(), tally)
    } else {
        match (l.as_deref(), r.as_deref()) {
            (Some(_), None) => {
                tally.conflict(
                    SyncKind::Wiki,
                    title_of(local, &slug),
                    "다른 컴퓨터에서 지웠지만 이 컴퓨터에서 고쳐서 남겼어요",
                );
                Ok(false)
            }
            (None, Some(_)) => {
                tally.conflict(
                    SyncKind::Wiki,
                    title_of(remote, &slug),
                    "이 컴퓨터에서 지웠지만 다른 컴퓨터에서 고쳐서 되살렸어요",
                );
                take(cx, path, project, &slug, r.as_deref(), tally)
            }
            (Some(_), Some(text)) => propose(cx, project, &slug, text).map(|added| {
                tally.found.push(super::conflicts::Found {
                    kind: SyncKind::Wiki,
                    path: path.to_string(),
                    title: title_of(remote, &slug),
                    lost: super::conflicts::Side::Remote,
                    content: text.to_string(),
                });
                tally.conflict(
                    SyncKind::Wiki,
                    title_of(local, &slug),
                    if added {
                        "두 컴퓨터에서 고쳐서 이 컴퓨터 내용을 두고, 다른 컴퓨터의 변경을 제안으로 남겼어요"
                    } else {
                        "두 컴퓨터에서 고쳤어요. 다른 컴퓨터의 변경은 이미 제안으로 남아 있어요"
                    },
                );
                false
            }),
            (None, None) => Ok(false),
        }
    };
    match result {
        Ok(true) => tally.imported(SyncKind::Wiki, &[topic::WIKI]),
        Ok(false) => {
            tally.topics.insert(topic::WIKI);
        }
        Err(e) => tally.skip(path, e.message),
    }
}

fn title_of(text: Option<&str>, slug: &str) -> String {
    text.map(|t| read(t, slug).title).unwrap_or_else(|| slug.to_string())
}

/// Make the page say `text` (None removes it). Returns true when something changed.
fn take(
    cx: &Cx,
    path: &str,
    project: Uuid,
    slug: &str,
    text: Option<&str>,
    tally: &mut Tally,
) -> Result<bool, RpcError> {
    let file = files::page_path(cx.wiki_root, project, slug);
    let Some(text) = text else {
        if file.exists() {
            std::fs::remove_file(&file).map_err(|e| RpcError::internal(format!("위키 파일을 지우지 못했어요: {e}")))?;
        }
        ops::reconcile(cx.store, cx.wiki_root, project)?;
        return Ok(true);
    };
    let d = read(text, slug);
    let row = cx.store.with(|c| index::get(c, project, slug)).map_err(db_err)?;
    if let Some(row) = row {
        let out = ops::save(
            cx.store,
            cx.wiki_root,
            ops::Save {
                project_id: project,
                slug,
                base_version: row.version,
                title: &d.title,
                body: &d.body,
                tags: Some(&d.tags),
                mark_reviewed: d.status == PageStatus::Reviewed,
                add_sources: &d.sources,
                saved_by: SavedBy::Import,
                note: Some(NOTE),
            },
        )?;
        return match out {
            SaveOutcome::Saved { .. } => Ok(true),
            // Edited here in the meantime: keep that, offer the other text.
            SaveOutcome::Conflict { .. } => propose(cx, project, slug, text).map(|_| {
                tally.found.push(super::conflicts::Found {
                    kind: SyncKind::Wiki,
                    path: path.to_string(),
                    title: d.title.clone(),
                    lost: super::conflicts::Side::Remote,
                    content: text.to_string(),
                });
                false
            }),
        };
    }
    let taken = cx.store.with(|c| index::slug_taken(c, project, slug)).map_err(db_err)?;
    if taken || file.exists() {
        // A page with this name was removed here before: bring the file back so the wiki
        // revives the same page (and its history) instead of making `slug-2`.
        let meta = Meta {
            title: Some(d.title),
            created_by: Some(Author::Import),
            status: Some(d.status),
            tags: d.tags,
            sources: d.sources,
        };
        files::write_atomic(&file, &files::render(&meta, &runwick_core::now(), &d.body))
            .map_err(|e| RpcError::internal(format!("위키 파일을 쓰지 못했어요: {e}")))?;
        ops::reconcile(cx.store, cx.wiki_root, project)?;
        return Ok(true);
    }
    ops::create(
        cx.store,
        cx.wiki_root,
        ops::NewPage {
            project_id: project,
            title: d.title,
            body: d.body,
            tags: d.tags,
            sources: d.sources,
            created_by: Author::Import,
            status: d.status,
            saved_by: SavedBy::Import,
            note: Some(NOTE.into()),
            slug: Some(slug.to_string()),
        },
    )?;
    Ok(true)
}

/// Store the other computer's text as a proposal on the page (once per distinct text).
fn propose(cx: &Cx, project: Uuid, slug: &str, text: &str) -> Result<bool, RpcError> {
    let d = read(text, slug);
    let pending = cx
        .store
        .with(|c| {
            c.query_row(
                "SELECT 1 FROM wiki_proposals pr JOIN wiki_pages pg ON pg.id = pr.page_id
                 WHERE pg.project_id = ?1 AND pg.slug = ?2 AND pr.body = ?3 AND pr.status IN ('open', 'held')",
                rusqlite::params![project, slug, d.body],
                |_| Ok(()),
            )
            .optional()
        })
        .map_err(db_err)?;
    if pending.is_some() {
        return Ok(false);
    }
    let id = ops::add_proposal(
        cx.store,
        cx.wiki_root,
        ops::AiProposal {
            project_id: project,
            slug: Some(slug.to_string()),
            title: d.title,
            body: d.body,
            rationale: "다른 컴퓨터의 변경 · 두 컴퓨터에서 같은 문서를 고쳤어요. 이 컴퓨터의 내용은 그대로 두고 다른 컴퓨터에서 쓴 내용을 제안으로 남겼어요. 비교해서 필요한 부분만 반영하세요."
                .into(),
            sources: Vec::new(),
            ai_flag: None,
            job_id: None,
        },
    )?;
    Ok(id.is_some())
}

/// `updated_at` of a live page here, for tombstones.
pub fn updated_at(cx: &Cx, project: Uuid, slug: &str) -> Option<runwick_core::Ts> {
    cx.store.with(|c| index::get(c, project, slug)).ok().flatten().map(|r| r.updated_at)
}

/// Put a page text that was not kept back as the person's edit ("겹친 변경" › 되돌리기).
/// The page at `path` as this computer would write it now (`None` when there is none).
pub fn current(cx: &Cx, path: &str) -> Result<Option<String>, RpcError> {
    let (project, slug) = parse_path(path).ok_or_else(|| RpcError::invalid("위키 문서 경로가 아니에요"))?;
    ops::refresh(cx.store, cx.wiki_root, Some(project))?;
    cx.store
        .with(|c| {
            let Some(row) = index::get(c, project, &slug)? else {
                return Ok(None);
            };
            let body = index::body(c, row.id, row.version)?.map(|(_, b)| b).unwrap_or_default();
            Ok(Some(render_page(&row.title, row.status, &row.tags, &row.sources, &body)))
        })
        .map_err(db_err)
}

pub fn restore(cx: &Cx, path: &str, text: &str) -> Result<(), RpcError> {
    let (project, slug) = parse_path(path).ok_or_else(|| RpcError::invalid("위키 문서 경로가 아니에요"))?;
    let d = read(text, &slug);
    let row = cx.store.with(|c| index::get(c, project, &slug)).map_err(db_err)?;
    let note = Some("겹친 변경에서 되돌림");
    match row {
        Some(row) => {
            let out = ops::save(
                cx.store,
                cx.wiki_root,
                ops::Save {
                    project_id: project,
                    slug: &slug,
                    base_version: row.version,
                    title: &d.title,
                    body: &d.body,
                    tags: Some(&d.tags),
                    mark_reviewed: false,
                    add_sources: &d.sources,
                    saved_by: SavedBy::User,
                    note,
                },
            )?;
            match out {
                SaveOutcome::Saved { .. } => Ok(()),
                SaveOutcome::Conflict { .. } => {
                    Err(RpcError::new(runwick_core::rpc::codes::CONFLICT, "문서가 방금 바뀌었어요. 다시 시도해 주세요"))
                }
            }
        }
        None => ops::create(
            cx.store,
            cx.wiki_root,
            ops::NewPage {
                project_id: project,
                title: d.title,
                body: d.body,
                tags: d.tags,
                sources: d.sources,
                created_by: Author::User,
                status: d.status,
                saved_by: SavedBy::User,
                note: note.map(str::to_string),
                slug: Some(slug),
            },
        )
        .map(|_| ()),
    }
}
