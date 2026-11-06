//! The wiki index in SQLite (pages, versions) kept in step with the Markdown files.
//!
//! The files are the person's: [`reconcile`] notices files that changed, appeared or vanished
//! outside Runwick and records them as the person's edits (a new version) before anything
//! else reads or writes the page.

use super::files::{self, Meta};
use crate::store::{ts_from_sql, ts_to_sql};
use runwick_core::Ts;
use runwick_core::api::wiki::{Author, PageStatus, SavedBy};
use rusqlite::{Connection, OptionalExtension, Row, params};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq)]
pub struct PageRow {
    pub id: Uuid,
    pub project_id: Uuid,
    pub slug: String,
    pub title: String,
    pub created_by: Author,
    pub status: PageStatus,
    pub sources: Vec<Uuid>,
    pub tags: Vec<String>,
    pub version: u32,
    pub hash: String,
    pub mtime: i64,
    pub size: i64,
    pub created_at: Ts,
    pub updated_at: Ts,
}

impl PageRow {
    pub fn meta(&self) -> Meta {
        Meta {
            title: Some(self.title.clone()),
            created_by: Some(self.created_by),
            status: Some(self.status),
            tags: self.tags.clone(),
            sources: self.sources.clone(),
        }
    }
}

pub fn author_sql(a: Author) -> &'static str {
    match a {
        Author::User => "user",
        Author::Ai => "ai",
        Author::Import => "import",
    }
}

fn author_from(s: &str) -> Author {
    match s {
        "ai" => Author::Ai,
        "import" => Author::Import,
        _ => Author::User,
    }
}

pub fn status_sql(s: PageStatus) -> &'static str {
    match s {
        PageStatus::Draft => "draft",
        PageStatus::Reviewed => "reviewed",
    }
}

pub fn saved_by_sql(s: SavedBy) -> &'static str {
    match s {
        SavedBy::User => "user",
        SavedBy::External => "external",
        SavedBy::Ai => "ai",
        SavedBy::Import => "import",
        SavedBy::Proposal => "proposal",
    }
}

pub fn saved_by_from(s: &str) -> SavedBy {
    match s {
        "external" => SavedBy::External,
        "ai" => SavedBy::Ai,
        "import" => SavedBy::Import,
        "proposal" => SavedBy::Proposal,
        _ => SavedBy::User,
    }
}

fn json_list<T: serde::de::DeserializeOwned>(s: &str) -> Vec<T> {
    serde_json::from_str(s).unwrap_or_default()
}

fn row_from(r: &Row) -> rusqlite::Result<PageRow> {
    Ok(PageRow {
        id: r.get("id")?,
        project_id: r.get("project_id")?,
        slug: r.get("slug")?,
        title: r.get("title")?,
        created_by: author_from(&r.get::<_, String>("created_by")?),
        status: if r.get::<_, String>("status")? == "draft" { PageStatus::Draft } else { PageStatus::Reviewed },
        sources: json_list(&r.get::<_, String>("sources")?),
        tags: json_list(&r.get::<_, String>("tags")?),
        version: r.get::<_, i64>("version")? as u32,
        hash: r.get("hash")?,
        mtime: r.get("file_mtime")?,
        size: r.get("file_size")?,
        created_at: ts_from_sql(&r.get::<_, String>("created_at")?),
        updated_at: ts_from_sql(&r.get::<_, String>("updated_at")?),
    })
}

pub fn get(c: &Connection, project: Uuid, slug: &str) -> rusqlite::Result<Option<PageRow>> {
    c.query_row(
        "SELECT * FROM wiki_pages WHERE project_id = ?1 AND slug = ?2 AND deleted = 0",
        params![project, slug],
        row_from,
    )
    .optional()
}

pub fn get_by_id(c: &Connection, id: Uuid) -> rusqlite::Result<Option<PageRow>> {
    c.query_row("SELECT * FROM wiki_pages WHERE id = ?1 AND deleted = 0", [id], row_from).optional()
}

pub fn list(c: &Connection, project: Option<Uuid>) -> rusqlite::Result<Vec<PageRow>> {
    let mut st = c.prepare(
        "SELECT * FROM wiki_pages WHERE deleted = 0 AND (?1 IS NULL OR project_id = ?1)
         ORDER BY updated_at DESC, title",
    )?;
    let rows = st.query_map([project], row_from)?;
    rows.collect()
}

/// Every slug ever used in the project (deleted pages keep theirs for history).
pub fn slug_taken(c: &Connection, project: Uuid, slug: &str) -> rusqlite::Result<bool> {
    c.query_row("SELECT 1 FROM wiki_pages WHERE project_id = ?1 AND slug = ?2", params![project, slug], |_| Ok(()))
        .optional()
        .map(|o| o.is_some())
}

pub fn body(c: &Connection, page: Uuid, version: u32) -> rusqlite::Result<Option<(String, String)>> {
    c.query_row(
        "SELECT title, body FROM wiki_versions WHERE page_id = ?1 AND version = ?2",
        params![page, version as i64],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .optional()
}

pub struct VersionRow {
    pub version: u32,
    pub title: String,
    pub body: String,
    pub saved_by: SavedBy,
    pub note: Option<String>,
    pub saved_at: Ts,
}

pub fn versions(c: &Connection, page: Uuid) -> rusqlite::Result<Vec<VersionRow>> {
    let mut st = c.prepare(
        "SELECT version, title, body, saved_by, note, saved_at FROM wiki_versions WHERE page_id = ?1 ORDER BY version",
    )?;
    let rows = st.query_map([page], |r| {
        Ok(VersionRow {
            version: r.get::<_, i64>(0)? as u32,
            title: r.get(1)?,
            body: r.get(2)?,
            saved_by: saved_by_from(&r.get::<_, String>(3)?),
            note: r.get(4)?,
            saved_at: ts_from_sql(&r.get::<_, String>(5)?),
        })
    })?;
    rows.collect()
}

/// Open proposals per page id: (open, flagged).
pub fn proposal_counts(c: &Connection, project: Option<Uuid>) -> rusqlite::Result<HashMap<Uuid, (u32, u32)>> {
    let mut st = c.prepare(
        "SELECT page_id, COUNT(*), SUM(decision_flag IS NOT NULL) FROM wiki_proposals
         WHERE status = 'open' AND page_id IS NOT NULL AND (?1 IS NULL OR project_id = ?1) GROUP BY page_id",
    )?;
    let rows = st.query_map([project], |r| {
        Ok((r.get::<_, Uuid>(0)?, (r.get::<_, i64>(1)? as u32, r.get::<_, i64>(2)? as u32)))
    })?;
    rows.collect()
}

/// Insert or update the page row and add `body` as its newest version.
pub fn put(c: &Connection, row: &PageRow, body: &str, saved_by: SavedBy, note: Option<&str>) -> rusqlite::Result<()> {
    c.execute(
        "INSERT INTO wiki_pages (id, project_id, slug, title, created_by, status, sources, tags, version, hash,
             file_mtime, file_size, created_at, updated_at, deleted)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, 0)
         ON CONFLICT(id) DO UPDATE SET slug = excluded.slug, title = excluded.title,
             created_by = excluded.created_by, status = excluded.status, sources = excluded.sources,
             tags = excluded.tags, version = excluded.version, hash = excluded.hash,
             file_mtime = excluded.file_mtime, file_size = excluded.file_size,
             updated_at = excluded.updated_at, deleted = 0",
        params![
            row.id,
            row.project_id,
            row.slug,
            row.title,
            author_sql(row.created_by),
            status_sql(row.status),
            serde_json::to_string(&row.sources).unwrap_or_else(|_| "[]".into()),
            serde_json::to_string(&row.tags).unwrap_or_else(|_| "[]".into()),
            row.version as i64,
            row.hash,
            row.mtime,
            row.size,
            ts_to_sql(&row.created_at),
            ts_to_sql(&row.updated_at),
        ],
    )?;
    c.execute(
        "INSERT INTO wiki_versions (page_id, version, title, body, saved_by, note, saved_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![row.id, row.version as i64, row.title, body, saved_by_sql(saved_by), note, ts_to_sql(&row.updated_at)],
    )?;
    Ok(())
}

fn set_stamp(c: &Connection, id: Uuid, (mtime, size): (i64, i64)) -> rusqlite::Result<()> {
    c.execute("UPDATE wiki_pages SET file_mtime = ?2, file_size = ?3 WHERE id = ?1", params![id, mtime, size])?;
    Ok(())
}

fn mark_deleted(c: &Connection, id: Uuid) -> rusqlite::Result<()> {
    c.execute("UPDATE wiki_pages SET deleted = 1 WHERE id = ?1", [id])?;
    Ok(())
}

fn deleted_row(c: &Connection, project: Uuid, slug: &str) -> rusqlite::Result<Option<PageRow>> {
    c.query_row(
        "SELECT * FROM wiki_pages WHERE project_id = ?1 AND slug = ?2 AND deleted = 1",
        params![project, slug],
        row_from,
    )
    .optional()
}

/// What a write produced on disk.
pub struct Written {
    pub hash: String,
    pub stamp: (i64, i64),
}

pub fn write_file(root: &Path, row: &PageRow, body: &str) -> std::io::Result<Written> {
    let path = files::page_path(root, row.project_id, &row.slug);
    let text = files::render(&row.meta(), &row.updated_at, body);
    files::write_atomic(&path, &text)?;
    let stamp = files::stamp(&path).unwrap_or((0, text.len() as i64));
    Ok(Written { hash: files::hash(text.as_bytes()), stamp })
}

/// Bring the index in line with the project's folder. Returns true when something changed.
pub fn reconcile(c: &Connection, root: &Path, project: Uuid) -> rusqlite::Result<bool> {
    let dir = files::project_dir(root, project);
    let mut on_disk: HashMap<String, PathBuf> = HashMap::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for e in entries.flatten() {
            let p = e.path();
            if p.is_file()
                && let Some(slug) = files::slug_of(&p)
            {
                on_disk.insert(slug, p);
            }
        }
    }
    let mut changed = false;
    for row in list(c, Some(project))? {
        let Some(path) = on_disk.remove(&row.slug) else {
            mark_deleted(c, row.id)?;
            changed = true;
            continue;
        };
        let Some(stamp) = files::stamp(&path) else {
            continue;
        };
        if stamp == (row.mtime, row.size) {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let hash = files::hash(&bytes);
        if hash == row.hash {
            set_stamp(c, row.id, stamp)?;
            continue;
        }
        let doc = files::parse(&String::from_utf8_lossy(&bytes));
        let next = PageRow {
            title: doc.meta.title.clone().unwrap_or_else(|| row.title.clone()),
            created_by: doc.meta.created_by.unwrap_or(row.created_by),
            status: doc.meta.status.unwrap_or(row.status),
            tags: if doc.has_frontmatter { doc.meta.tags.clone() } else { row.tags.clone() },
            sources: if doc.has_frontmatter { doc.meta.sources.clone() } else { row.sources.clone() },
            version: row.version + 1,
            hash,
            mtime: stamp.0,
            size: stamp.1,
            updated_at: runwick_core::now(),
            ..row
        };
        put(c, &next, &doc.body, SavedBy::External, Some("Runwick 밖에서 고친 파일"))?;
        changed = true;
    }
    for (slug, path) in on_disk {
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let Some(stamp) = files::stamp(&path) else {
            continue;
        };
        let doc = files::parse(&String::from_utf8_lossy(&bytes));
        let now = runwick_core::now();
        let revived = deleted_row(c, project, &slug)?;
        let row = PageRow {
            id: revived.as_ref().map(|r| r.id).unwrap_or_else(Uuid::now_v7),
            project_id: project,
            title: doc.meta.title.clone().unwrap_or_else(|| files::title_from(&doc.body, &slug)),
            slug,
            created_by: doc.meta.created_by.unwrap_or(Author::User),
            status: doc.meta.status.unwrap_or(PageStatus::Reviewed),
            sources: doc.meta.sources.clone(),
            tags: doc.meta.tags.clone(),
            version: revived.as_ref().map(|r| r.version + 1).unwrap_or(1),
            hash: files::hash(&bytes),
            mtime: stamp.0,
            size: stamp.1,
            created_at: revived.as_ref().map(|r| r.created_at).unwrap_or(now),
            updated_at: now,
        };
        put(c, &row, &doc.body, SavedBy::External, Some("폴더에 직접 추가한 파일"))?;
        changed = true;
    }
    Ok(changed)
}
