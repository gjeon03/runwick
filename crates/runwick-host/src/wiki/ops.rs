//! Wiki operations over the store and the page folder. Every write goes through [`save`] or
//! [`create`], which record a version and write the file atomically; all writes are serialized.

use super::files::{self, slugify};
use super::index::{self, PageRow};
use super::{merge, proposals};
use crate::store::{Store, db_err};
use runwick_core::api::wiki::*;
use runwick_core::rpc::{RpcError, codes};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;
use uuid::Uuid;

static WRITE: Mutex<()> = Mutex::new(());

fn lock() -> std::sync::MutexGuard<'static, ()> {
    WRITE.lock().unwrap_or_else(|e| e.into_inner())
}

fn io_err(e: std::io::Error) -> RpcError {
    RpcError::internal(format!("위키 파일을 쓰지 못했어요: {e}"))
}

fn not_found() -> RpcError {
    RpcError::not_found("위키 문서를 찾을 수 없어요")
}

pub fn reconcile(store: &Store, root: &Path, project: Uuid) -> Result<bool, RpcError> {
    let _g = lock();
    store.tx(|c| index::reconcile(c, root, project)).map_err(db_err)
}

/// Projects that have pages in the index or a folder on disk.
fn known_projects(store: &Store, root: &Path) -> Result<Vec<Uuid>, RpcError> {
    let mut ids: Vec<Uuid> = store
        .with(|c| {
            let mut st = c.prepare("SELECT DISTINCT project_id FROM wiki_pages")?;
            let rows = st.query_map([], |r| r.get(0))?;
            rows.collect()
        })
        .map_err(db_err)?;
    if let Ok(entries) = std::fs::read_dir(root) {
        for e in entries.flatten() {
            if let Some(id) = e.file_name().to_str().and_then(|s| s.parse::<Uuid>().ok())
                && !ids.contains(&id)
            {
                ids.push(id);
            }
        }
    }
    // Only folders of projects this host knows (a stray folder is not a project).
    let projects: Vec<Uuid> = store
        .with(|c| {
            let mut st = c.prepare("SELECT id FROM projects")?;
            let rows = st.query_map([], |r| r.get(0))?;
            rows.collect()
        })
        .map_err(db_err)?;
    ids.retain(|id| projects.contains(id));
    Ok(ids)
}

/// Reconcile one project or all; returns true when files changed outside Runwick.
pub fn refresh(store: &Store, root: &Path, project: Option<Uuid>) -> Result<bool, RpcError> {
    let ids = match project {
        Some(p) => vec![p],
        None => known_projects(store, root)?,
    };
    let mut changed = false;
    for id in ids {
        changed |= reconcile(store, root, id)?;
    }
    Ok(changed)
}

fn summary(row: &PageRow, body: &str, counts: &HashMap<Uuid, (u32, u32)>) -> PageSummary {
    let (open, flagged) = counts.get(&row.id).copied().unwrap_or((0, 0));
    PageSummary {
        project_id: row.project_id,
        slug: row.slug.clone(),
        title: row.title.clone(),
        created_by: row.created_by,
        status: row.status,
        tags: row.tags.clone(),
        sources: row.sources.clone(),
        version: row.version,
        updated_at: row.updated_at,
        excerpt: files::excerpt(body),
        open_proposals: open,
        flagged_proposals: flagged,
    }
}

fn current_body(c: &rusqlite::Connection, row: &PageRow) -> rusqlite::Result<String> {
    Ok(index::body(c, row.id, row.version)?.map(|(_, b)| b).unwrap_or_default())
}

pub fn list(store: &Store, root: &Path, project: Option<Uuid>) -> Result<Vec<PageSummary>, RpcError> {
    refresh(store, root, project)?;
    store
        .with(|c| {
            let counts = index::proposal_counts(c, project)?;
            index::list(c, project)?.iter().map(|row| Ok(summary(row, &current_body(c, row)?, &counts))).collect()
        })
        .map_err(db_err)
}

fn page_of(c: &rusqlite::Connection, root: &Path, row: &PageRow) -> rusqlite::Result<Page> {
    let counts = index::proposal_counts(c, Some(row.project_id))?;
    let body = current_body(c, row)?;
    Ok(Page {
        summary: summary(row, &body, &counts),
        body,
        path: runwick_core::paths::display_path(&files::page_path(root, row.project_id, &row.slug)),
    })
}

pub fn get(store: &Store, root: &Path, project: Uuid, slug: &str) -> Result<Page, RpcError> {
    reconcile(store, root, project)?;
    store
        .with(|c| match index::get(c, project, slug)? {
            Some(row) => page_of(c, root, &row).map(Some),
            None => Ok(None),
        })
        .map_err(db_err)?
        .ok_or_else(not_found)
}

pub struct NewPage {
    pub project_id: Uuid,
    pub title: String,
    pub body: String,
    pub tags: Vec<String>,
    pub sources: Vec<Uuid>,
    pub created_by: Author,
    pub status: PageStatus,
    pub saved_by: SavedBy,
    pub note: Option<String>,
    /// Preferred slug (file name); the title's slug otherwise.
    pub slug: Option<String>,
}

fn free_slug(c: &rusqlite::Connection, root: &Path, project: Uuid, wanted: &str) -> rusqlite::Result<String> {
    let base = slugify(wanted);
    let mut slug = base.clone();
    let mut n = 2;
    while index::slug_taken(c, project, &slug)? || files::page_path(root, project, &slug).exists() {
        slug = format!("{base}-{n}");
        n += 1;
    }
    Ok(slug)
}

fn clean_title(t: &str) -> Result<String, RpcError> {
    let t = t.trim();
    if t.is_empty() {
        return Err(RpcError::invalid("문서 제목을 입력해 주세요"));
    }
    Ok(t.chars().take(200).collect())
}

fn clean_tags(tags: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for t in tags {
        let t = t.trim().trim_start_matches('#').trim().to_string();
        if !t.is_empty() && !out.contains(&t) {
            out.push(t);
        }
    }
    out
}

pub fn create(store: &Store, root: &Path, n: NewPage) -> Result<Page, RpcError> {
    let title = clean_title(&n.title)?;
    reconcile(store, root, n.project_id)?;
    let _g = lock();
    let now = runwick_core::now();
    let slug = store.with(|c| free_slug(c, root, n.project_id, n.slug.as_deref().unwrap_or(&title))).map_err(db_err)?;
    let mut row = PageRow {
        id: Uuid::now_v7(),
        project_id: n.project_id,
        slug,
        title,
        created_by: n.created_by,
        status: n.status,
        sources: n.sources,
        tags: clean_tags(&n.tags),
        version: 1,
        hash: String::new(),
        mtime: 0,
        size: 0,
        created_at: now,
        updated_at: now,
    };
    let w = index::write_file(root, &row, &n.body).map_err(io_err)?;
    row.hash = w.hash;
    (row.mtime, row.size) = w.stamp;
    store
        .tx(|c| {
            index::put(c, &row, &n.body, n.saved_by, n.note.as_deref())?;
            page_of(c, root, &row)
        })
        .map_err(db_err)
}

pub fn import(store: &Store, root: &Path, p: &ImportPages) -> Result<ImportReport, RpcError> {
    let mut report = ImportReport::default();
    for f in &p.files {
        if f.text.len() > 2 * 1024 * 1024 {
            report.skipped.push((f.name.clone(), "2MB보다 커서 가져오지 않았어요".into()));
            continue;
        }
        let doc = files::parse(&f.text);
        let stem = Path::new(&f.name).file_stem().and_then(|s| s.to_str()).unwrap_or("문서").to_string();
        if doc.body.trim().is_empty() {
            report.skipped.push((f.name.clone(), "내용이 비어 있어요".into()));
            continue;
        }
        let title = doc.meta.title.clone().unwrap_or_else(|| files::title_from(&doc.body, &stem));
        let page = create(
            store,
            root,
            NewPage {
                project_id: p.project_id,
                title,
                body: doc.body,
                tags: doc.meta.tags,
                sources: doc.meta.sources,
                created_by: Author::Import,
                status: PageStatus::Reviewed,
                saved_by: SavedBy::Import,
                note: Some(format!("가져옴: {}", f.name)),
                slug: Some(stem),
            },
        )?;
        report.created.push(page.summary.slug);
    }
    Ok(report)
}

pub struct Save<'a> {
    pub project_id: Uuid,
    pub slug: &'a str,
    pub base_version: u32,
    pub title: &'a str,
    pub body: &'a str,
    pub tags: Option<&'a [String]>,
    pub mark_reviewed: bool,
    pub add_sources: &'a [Uuid],
    pub saved_by: SavedBy,
    pub note: Option<&'a str>,
}

pub fn save(store: &Store, root: &Path, s: Save) -> Result<SaveOutcome, RpcError> {
    let title = clean_title(s.title)?;
    reconcile(store, root, s.project_id)?;
    let _g = lock();
    let row = store.with(|c| index::get(c, s.project_id, s.slug)).map_err(db_err)?.ok_or_else(not_found)?;
    if row.version != s.base_version {
        let current = store.with(|c| page_of(c, root, &row)).map_err(db_err)?;
        return Ok(SaveOutcome::Conflict { current, yours_title: title, yours_body: s.body.to_string() });
    }
    let old_body = store.with(|c| current_body(c, &row)).map_err(db_err)?;
    let tags = s.tags.map(clean_tags).unwrap_or_else(|| row.tags.clone());
    let mut sources = row.sources.clone();
    for id in s.add_sources {
        if !sources.contains(id) {
            sources.push(*id);
        }
    }
    let status = if s.mark_reviewed { PageStatus::Reviewed } else { row.status };
    let unchanged = merge::split(&old_body) == merge::split(s.body)
        && title == row.title
        && tags == row.tags
        && sources == row.sources
        && status == row.status;
    if unchanged {
        return store.with(|c| page_of(c, root, &row)).map(|page| SaveOutcome::Saved { page }).map_err(db_err);
    }
    let mut next =
        PageRow { title, tags, sources, status, version: row.version + 1, updated_at: runwick_core::now(), ..row };
    let w = index::write_file(root, &next, s.body).map_err(io_err)?;
    next.hash = w.hash;
    (next.mtime, next.size) = w.stamp;
    store
        .tx(|c| {
            index::put(c, &next, s.body, s.saved_by, s.note)?;
            page_of(c, root, &next)
        })
        .map(|page| SaveOutcome::Saved { page })
        .map_err(db_err)
}

pub fn history(store: &Store, root: &Path, project: Uuid, slug: &str) -> Result<Vec<VersionInfo>, RpcError> {
    reconcile(store, root, project)?;
    let rows = store
        .with(|c| match index::get(c, project, slug)? {
            Some(row) => index::versions(c, row.id).map(Some),
            None => Ok(None),
        })
        .map_err(db_err)?
        .ok_or_else(not_found)?;
    let mut out = Vec::with_capacity(rows.len());
    let mut prev = "";
    for v in &rows {
        let (added, removed) = merge::counts(prev, &v.body);
        out.push(VersionInfo {
            version: v.version,
            title: v.title.clone(),
            saved_by: v.saved_by,
            note: v.note.clone(),
            saved_at: v.saved_at,
            added,
            removed,
        });
        prev = &v.body;
    }
    out.reverse();
    Ok(out)
}

pub fn version(store: &Store, r: &VersionRef) -> Result<VersionText, RpcError> {
    let rows = store
        .with(|c| match index::get(c, r.project_id, &r.slug)? {
            Some(row) => index::versions(c, row.id).map(Some),
            None => Ok(None),
        })
        .map_err(db_err)?
        .ok_or_else(not_found)?;
    let i = rows.iter().position(|v| v.version == r.version).ok_or_else(|| RpcError::not_found("그 버전이 없어요"))?;
    let prev = if i == 0 { "" } else { rows[i - 1].body.as_str() };
    let v = &rows[i];
    let (added, removed) = merge::counts(prev, &v.body);
    Ok(VersionText {
        info: VersionInfo {
            version: v.version,
            title: v.title.clone(),
            saved_by: v.saved_by,
            note: v.note.clone(),
            saved_at: v.saved_at,
            added,
            removed,
        },
        body: v.body.clone(),
        diff: merge::line_diff(prev, &v.body),
    })
}

/// Insert `[[to]]` into `from` as the person's edit (a normal save on the current version).
pub fn link(store: &Store, root: &Path, p: &PagePair) -> Result<Page, RpcError> {
    let from = get(store, root, p.project_id, &p.from)?;
    let to = get(store, root, p.project_id, &p.to)?;
    let link = format!("[[{}]]", to.summary.title);
    let mut lines: Vec<String> = merge::split(&from.body).iter().map(|l| l.to_string()).collect();
    match lines.iter_mut().find(|l| l.starts_with("관련 문서:")) {
        Some(l) if l.contains(&link) => return Ok(from),
        Some(l) => l.push_str(&format!(", {link}")),
        None => {
            if lines.last().is_some_and(|l| !l.trim().is_empty()) {
                lines.push(String::new());
            }
            lines.push(format!("관련 문서: {link}"));
        }
    }
    let body = merge::join(&lines);
    match save(
        store,
        root,
        Save {
            project_id: p.project_id,
            slug: &p.from,
            base_version: from.summary.version,
            title: &from.summary.title,
            body: &body,
            tags: None,
            mark_reviewed: false,
            add_sources: &[],
            saved_by: SavedBy::User,
            note: Some("관련 문서 링크 추가"),
        },
    )? {
        SaveOutcome::Saved { page } => Ok(page),
        SaveOutcome::Conflict { .. } => {
            Err(RpcError::new(codes::CONFLICT, "문서가 방금 바뀌었어요. 다시 시도해 주세요"))
        }
    }
}

pub fn dismiss(store: &Store, p: &PagePair) -> Result<(), RpcError> {
    let (a, b) = if p.from <= p.to { (&p.from, &p.to) } else { (&p.to, &p.from) };
    store
        .with(|c| {
            c.execute(
                "INSERT OR IGNORE INTO wiki_dismissed (project_id, a, b, created_at) VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![p.project_id, a, b, crate::store::ts_to_sql(&runwick_core::now())],
            )
        })
        .map(|_| ())
        .map_err(db_err)
}

// ---------------------------------------------------------------------------------------------
// Proposals

pub fn proposal_detail(store: &Store, root: &Path, id: Uuid) -> Result<ProposalDetail, RpcError> {
    let s = store
        .with(|c| proposals::get(c, id))
        .map_err(db_err)?
        .ok_or_else(|| RpcError::not_found("제안을 찾을 수 없어요"))?;
    let page = match &s.proposal.slug {
        Some(slug) => Some(get(store, root, s.proposal.project_id, slug)?),
        None => None,
    };
    // Re-read after reconcile: the page may have moved on.
    let s = store
        .with(|c| proposals::get(c, id))
        .map_err(db_err)?
        .ok_or_else(|| RpcError::not_found("제안을 찾을 수 없어요"))?;
    let placed = proposals::hunks(&s, page.as_ref().map(|p| p.body.as_str()));
    Ok(ProposalDetail {
        proposal: s.proposal,
        against_version: page.as_ref().map(|p| p.summary.version),
        hunks: placed.into_iter().map(|p| p.hunk).collect(),
        proposed_body: s.body,
    })
}

pub fn set_proposal_status(store: &Store, id: Uuid, status: ProposalStatus) -> Result<Proposal, RpcError> {
    let s = store
        .with(|c| proposals::get(c, id))
        .map_err(db_err)?
        .ok_or_else(|| RpcError::not_found("제안을 찾을 수 없어요"))?;
    if s.proposal.status.is_closed() {
        return Err(RpcError::invalid("이미 처리한 제안이에요"));
    }
    store
        .with(|c| {
            proposals::set_status(c, id, status)?;
            proposals::get(c, id)
        })
        .map_err(db_err)?
        .map(|s| s.proposal)
        .ok_or_else(|| RpcError::not_found("제안을 찾을 수 없어요"))
}

/// Apply all or some clean hunks of a proposal. Refuses when the page changed after the
/// version the person reviewed; conflict hunks are never applied.
pub fn apply(store: &Store, root: &Path, a: &ApplyProposal) -> Result<(Page, Proposal), RpcError> {
    let s = store
        .with(|c| proposals::get(c, a.id))
        .map_err(db_err)?
        .ok_or_else(|| RpcError::not_found("제안을 찾을 수 없어요"))?;
    if s.proposal.status.is_closed() {
        return Err(RpcError::invalid("이미 처리한 제안이에요"));
    }
    let sessions: Vec<Uuid> = {
        let mut v: Vec<Uuid> = Vec::new();
        for src in &s.proposal.sources {
            if !v.contains(&src.session_id) {
                v.push(src.session_id);
            }
        }
        v
    };
    let Some(slug) = s.proposal.slug.clone() else {
        if s.page_id.is_some() {
            return Err(RpcError::not_found("제안 대상 문서가 사라졌어요"));
        }
        let page = create(
            store,
            root,
            NewPage {
                project_id: s.proposal.project_id,
                title: s.proposal.title.clone(),
                body: s.body.clone(),
                tags: Vec::new(),
                sources: sessions,
                created_by: Author::Ai,
                status: PageStatus::Reviewed,
                saved_by: SavedBy::Proposal,
                note: Some("새 문서 제안 적용".into()),
                slug: None,
            },
        )?;
        let p = set_proposal_status(store, a.id, ProposalStatus::Applied)?;
        return Ok((page, p));
    };
    let page = get(store, root, s.proposal.project_id, &slug)?;
    let reviewed = a.against_version.or(s.proposal.base_version);
    if reviewed != Some(page.summary.version) {
        return Err(RpcError::new(
            codes::CONFLICT,
            "검토한 뒤 문서가 바뀌었어요. 지금 문서와 다시 비교한 내용을 확인해 주세요",
        ));
    }
    let placed = proposals::hunks(&s, Some(&page.body));
    let clean: Vec<u32> = placed.iter().filter(|p| p.hunk.kind == HunkKind::Clean).map(|p| p.hunk.index).collect();
    let chosen: Vec<u32> = match &a.hunks {
        Some(h) => h.iter().copied().filter(|i| clean.contains(i)).collect(),
        None => clean.clone(),
    };
    let (body, n) = merge::apply(&page.body, &placed, &chosen);
    if n == 0 {
        return Err(RpcError::invalid("적용할 수 있는 변경을 고르지 않았어요"));
    }
    let complete = n == placed.len();
    let note =
        if complete { "제안 적용".to_string() } else { format!("제안 일부 적용 ({n}/{})", placed.len()) };
    let out = save(
        store,
        root,
        Save {
            project_id: s.proposal.project_id,
            slug: &slug,
            base_version: page.summary.version,
            title: &page.summary.title,
            body: &body,
            tags: None,
            mark_reviewed: false,
            add_sources: &sessions,
            saved_by: SavedBy::Proposal,
            note: Some(&note),
        },
    )?;
    let page = match out {
        SaveOutcome::Saved { page } => page,
        SaveOutcome::Conflict { .. } => {
            return Err(RpcError::new(codes::CONFLICT, "적용하는 사이 문서가 바뀌었어요. 다시 비교해 주세요"));
        }
    };
    let status = if complete { ProposalStatus::Applied } else { ProposalStatus::Partial };
    let p = set_proposal_status(store, a.id, status)?;
    Ok((page, p))
}

/// A proposal from an AI job. Returns None when it would not change the page.
pub struct AiProposal {
    pub project_id: Uuid,
    pub slug: Option<String>,
    pub title: String,
    pub body: String,
    pub rationale: String,
    pub sources: Vec<SourceRef>,
    pub ai_flag: Option<String>,
    pub job_id: Option<Uuid>,
}

pub fn add_proposal(store: &Store, root: &Path, p: AiProposal) -> Result<Option<Uuid>, RpcError> {
    reconcile(store, root, p.project_id)?;
    let _g = lock();
    store
        .tx(|c| {
            let target = match &p.slug {
                Some(slug) => index::get(c, p.project_id, slug)?,
                None => None,
            };
            let base = match &target {
                Some(row) => Some((row.id, row.version, current_body(c, row)?)),
                None => None,
            };
            if let Some((_, _, cur)) = &base
                && merge::split(cur) == merge::split(&p.body)
            {
                return Ok(None);
            }
            let rows = index::list(c, Some(p.project_id))?;
            let bodies: Vec<String> = rows.iter().map(|r| current_body(c, r)).collect::<rusqlite::Result<_>>()?;
            let decisions = proposals::decision_lines(
                rows.iter().zip(&bodies).map(|(r, b)| (r.title.as_str(), r.tags.as_slice(), b.as_str())),
            );
            let base_text = base.as_ref().map(|b| b.2.as_str()).unwrap_or("");
            let flag = p
                .ai_flag
                .clone()
                .filter(|f| !f.trim().is_empty())
                .or_else(|| proposals::decision_flag(base_text, &p.body, &decisions));
            let title = if p.title.trim().is_empty() {
                target.as_ref().map(|r| r.title.clone()).unwrap_or_default()
            } else {
                p.title.clone()
            };
            let id = proposals::insert(
                c,
                &proposals::New {
                    project_id: p.project_id,
                    page: base.as_ref().map(|(id, v, b)| (*id, *v, b.as_str())),
                    title: &title,
                    body: &p.body,
                    rationale: &p.rationale,
                    sources: &p.sources,
                    decision_flag: flag,
                    job_id: p.job_id,
                },
            )?;
            Ok(Some(id))
        })
        .map_err(db_err)
}

/// Pages of a project with their current text (for AI prompts), newest first.
pub fn pages_with_bodies(store: &Store, root: &Path, project: Uuid) -> Result<Vec<(PageRow, String)>, RpcError> {
    reconcile(store, root, project)?;
    store
        .with(|c| index::list(c, Some(project))?.into_iter().map(|r| current_body(c, &r).map(|b| (r, b))).collect())
        .map_err(db_err)
}

pub fn open_proposals(store: &Store, project: Uuid) -> Result<u32, RpcError> {
    store.with(|c| proposals::open_count(c, project)).map_err(db_err)
}
