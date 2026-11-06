//! Keyword search over collected messages.
//!
//! Words of three or more characters use the trigram index (substring match, so Korean and code
//! fragments work); shorter words are matched by scanning (`LIKE`). All words must appear.
//! When the index fails, the search falls back to scanning and says so, and a rebuild starts in
//! the background: the new index is filled next to the old one in small steps and swapped in at
//! the end, so searching keeps working meanwhile.

use super::db::{MSG_COL_COUNT, MSG_COLS, message_from_row, tool_to_sql};
use crate::store::{Store, ts_to_sql};
use runwick_core::api::records::{Hit, Scope, SearchQuery, SearchResult};
use runwick_core::api::sessions::SessionId;
use rusqlite::Connection;
use rusqlite::types::Value as SqlValue;
use std::sync::atomic::{AtomicBool, Ordering};

pub const DEFAULT_LIMIT: u32 = 50;
pub const MAX_LIMIT: u32 = 500;
const MAX_TERMS: usize = 8;
/// Characters of context kept before the first match in a snippet.
const SNIPPET_BEFORE: usize = 48;
const SNIPPET_LEN: usize = 180;

/// Words of a query: whitespace separated, `"quoted text"` kept together, duplicates dropped.
pub fn terms(q: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut rest = q.trim();
    while !rest.is_empty() && out.len() < MAX_TERMS {
        let (term, next) = if let Some(r) = rest.strip_prefix('"') {
            match r.find('"') {
                Some(end) => (&r[..end], &r[end + 1..]),
                None => (r, ""),
            }
        } else {
            match rest.find(char::is_whitespace) {
                Some(end) => (&rest[..end], &rest[end..]),
                None => (rest, ""),
            }
        };
        let term = term.trim();
        if !term.is_empty() && !out.iter().any(|t| t.to_lowercase() == term.to_lowercase()) {
            out.push(term.to_string());
        }
        rest = next.trim_start();
    }
    out
}

fn uses_index(term: &str) -> bool {
    term.chars().count() >= 3
}

/// FTS5 expression: every indexable word as a quoted substring.
fn match_expr(terms: &[String]) -> Option<String> {
    let parts: Vec<String> =
        terms.iter().filter(|t| uses_index(t)).map(|t| format!("\"{}\"", t.replace('"', "\"\""))).collect();
    (!parts.is_empty()).then(|| parts.join(" AND "))
}

fn like_pattern(term: &str) -> String {
    let mut s = String::with_capacity(term.len() + 2);
    s.push('%');
    for c in term.chars() {
        if matches!(c, '%' | '_' | '\\') {
            s.push('\\');
        }
        s.push(c);
    }
    s.push('%');
    s
}

struct Built {
    where_sql: String,
    params: Vec<SqlValue>,
}

/// WHERE clause for the query; `index` false matches every word by scanning.
fn build(q: &SearchQuery, terms: &[String], index: bool) -> Built {
    let mut conds: Vec<String> = Vec::new();
    let mut params: Vec<SqlValue> = Vec::new();
    let mut push = |cond: &str, v: SqlValue, conds: &mut Vec<String>| {
        params.push(v);
        conds.push(cond.replace('?', &format!("?{}", params.len())));
    };
    match (index, match_expr(terms)) {
        (true, Some(expr)) => {
            push(
                "m.rid IN (SELECT rowid FROM messages_fts WHERE messages_fts MATCH ?)",
                SqlValue::Text(expr),
                &mut conds,
            );
            for t in terms.iter().filter(|t| !uses_index(t)) {
                push("m.text LIKE ? ESCAPE '\\'", SqlValue::Text(like_pattern(t)), &mut conds);
            }
        }
        _ => {
            for t in terms {
                push("m.text LIKE ? ESCAPE '\\'", SqlValue::Text(like_pattern(t)), &mut conds);
            }
        }
    }
    match q.scope {
        Scope::Session { id } => push("m.session_id = ?", SqlValue::Blob(id.as_bytes().to_vec()), &mut conds),
        Scope::Project { id } => push("m.project_id = ?", SqlValue::Blob(id.as_bytes().to_vec()), &mut conds),
        Scope::All => {}
    }
    if let Some(from) = &q.from {
        push("m.at >= ?", SqlValue::Text(ts_to_sql(from)), &mut conds);
    }
    if let Some(to) = &q.to {
        push("m.at < ?", SqlValue::Text(ts_to_sql(to)), &mut conds);
    }
    if let Some(tool) = q.tool {
        push("s.tool = ?", SqlValue::Text(tool_to_sql(tool).into()), &mut conds);
    }
    if q.pinned_only {
        conds.push("EXISTS (SELECT 1 FROM pins WHERE pins.message_id = m.id)".into());
    }
    let where_sql = if conds.is_empty() { String::new() } else { format!("WHERE {}", conds.join(" AND ")) };
    Built { where_sql, params }
}

fn run(
    c: &Connection,
    q: &SearchQuery,
    terms: &[String],
    index: bool,
    limit: u32,
) -> rusqlite::Result<(Vec<Hit>, u64)> {
    let b = build(q, terms, index);
    let from = "FROM messages m JOIN sessions s ON s.id = m.session_id LEFT JOIN projects p ON p.id = m.project_id";
    let total: i64 = c.query_row(
        &format!("SELECT COUNT(*) {from} {}", b.where_sql),
        rusqlite::params_from_iter(b.params.iter()),
        |r| r.get(0),
    )?;
    let sql = format!(
        "SELECT {MSG_COLS}, s.title, s.tool, COALESCE(p.name, ''),
                EXISTS (SELECT 1 FROM pins WHERE pins.message_id = m.id)
         {from} {} ORDER BY m.at DESC, m.rid DESC LIMIT {limit}",
        b.where_sql
    );
    let mut st = c.prepare(&sql)?;
    let rows = st.query_map(rusqlite::params_from_iter(b.params.iter()), |r| {
        let message = message_from_row(r, 0)?;
        let n = MSG_COL_COUNT;
        let (snippet, highlights) = snippet(&message.text, terms);
        Ok(Hit {
            session_title: crate::sessions::visible(&r.get::<_, String>(n)?),
            tool: super::db::tool_from_sql(&r.get::<_, String>(n + 1)?),
            project_name: r.get(n + 2)?,
            pinned: r.get(n + 3)?,
            message,
            snippet,
            highlights,
        })
    })?;
    Ok((rows.collect::<rusqlite::Result<_>>()?, total.max(0) as u64))
}

/// Sessions in scope whose collection is still running / incomplete.
fn scope_counts(c: &Connection, scope: &Scope) -> rusqlite::Result<(u32, u32)> {
    let (cond, param): (&str, Option<SessionId>) = match scope {
        Scope::Session { id } => ("s.id = ?1", Some(*id)),
        Scope::Project { id } => ("s.project_id = ?1", Some(*id)),
        Scope::All => ("?1 IS NULL", None),
    };
    c.query_row(
        &format!(
            "SELECT COALESCE(SUM(rs.state = 'collecting'), 0), COALESCE(SUM(rs.state IN ('error', 'no_file')), 0)
             FROM sessions s JOIN record_sources rs ON rs.session_id = s.id WHERE {cond}"
        ),
        [param],
        |r| Ok((r.get::<_, i64>(0)? as u32, r.get::<_, i64>(1)? as u32)),
    )
}

static REINDEXING: AtomicBool = AtomicBool::new(false);

pub fn search(store: &Store, q: &SearchQuery) -> rusqlite::Result<SearchResult> {
    let terms = terms(&q.text);
    let limit = if q.limit == 0 { DEFAULT_LIMIT } else { q.limit.min(MAX_LIMIT) };
    let (collecting, incomplete) = store.with(|c| scope_counts(c, &q.scope))?;
    let mut out = SearchResult { collecting, incomplete, ..Default::default() };
    if terms.is_empty() && !q.pinned_only {
        return Ok(out);
    }
    out.scanned = !terms.is_empty() && match_expr(&terms).is_none();
    match store.with(|c| run(c, q, &terms, true, limit)) {
        Ok((hits, total)) => {
            out.hits = hits;
            out.total = total;
        }
        Err(e) if match_expr(&terms).is_some() => {
            tracing::warn!("search index failed, scanning instead: {e}");
            out.index_error =
                Some(format!("검색 색인을 쓰지 못해 전체를 훑어 찾았어요 ({e}). 색인을 다시 만드는 중이에요."));
            let (hits, total) = store.with(|c| run(c, q, &terms, false, limit))?;
            out.hits = hits;
            out.total = total;
            out.scanned = true;
        }
        Err(e) => return Err(e),
    }
    Ok(out)
}

/// Whether a rebuild should start (the caller runs [`reindex`] on a blocking thread).
pub fn begin_reindex() -> bool {
    !REINDEXING.swap(true, Ordering::SeqCst)
}

/// Rebuild the index next to the current one and swap it in. Each step holds the store only
/// briefly, so searches (on the old index, or by scanning) continue meanwhile.
pub fn reindex(store: &Store) -> rusqlite::Result<()> {
    let result = (|| {
        store.with(|c| {
            c.execute_batch(
                "DROP TABLE IF EXISTS messages_fts_new;
                 CREATE VIRTUAL TABLE messages_fts_new USING fts5(text, content='messages', content_rowid='rid',
                     tokenize='trigram case_sensitive 0');",
            )
        })?;
        let mut last: i64 = 0;
        loop {
            let next: Option<i64> = store.with(|c| {
                c.execute(
                    "INSERT INTO messages_fts_new (rowid, text)
                     SELECT rid, text FROM messages WHERE rid > ?1 ORDER BY rid LIMIT 2000",
                    [last],
                )?;
                c.query_row("SELECT MAX(rowid) FROM messages_fts_new", [], |r| r.get(0))
            })?;
            match next {
                Some(n) if n > last => last = n,
                _ => break,
            }
        }
        store.tx(|tx| {
            tx.execute(
                "INSERT INTO messages_fts_new (rowid, text) SELECT rid, text FROM messages WHERE rid > ?1",
                [last],
            )?;
            // Triggers go first: renaming checks every trigger against the schema.
            tx.execute_batch(&format!(
                "DROP TRIGGER IF EXISTS messages_ai;
                 DROP TRIGGER IF EXISTS messages_ad;
                 DROP TRIGGER IF EXISTS messages_au;
                 DROP TABLE messages_fts;
                 ALTER TABLE messages_fts_new RENAME TO messages_fts;
                 {}",
                super::FTS_TRIGGERS
            ))
        })
    })();
    REINDEXING.store(false, Ordering::SeqCst);
    result
}

/// Lowercased characters with their byte offsets in the original text.
fn folded(s: &str) -> Vec<(usize, char)> {
    s.char_indices().map(|(i, c)| (i, c.to_lowercase().next().unwrap_or(c))).collect()
}

/// Byte ranges where `needle` occurs in `hay` (case-insensitive, non-overlapping).
fn find_all(hay: &[(usize, char)], hay_len: usize, needle: &str) -> Vec<(usize, usize)> {
    let n: Vec<char> = needle.chars().map(|c| c.to_lowercase().next().unwrap_or(c)).collect();
    let mut out = Vec::new();
    if n.is_empty() || n.len() > hay.len() {
        return out;
    }
    let mut i = 0;
    while i + n.len() <= hay.len() {
        if hay[i..i + n.len()].iter().map(|(_, c)| *c).eq(n.iter().copied()) {
            let end = hay.get(i + n.len()).map(|(b, _)| *b).unwrap_or(hay_len);
            out.push((hay[i].0, end));
            i += n.len();
        } else {
            i += 1;
        }
    }
    out
}

/// One line around the first match with the byte ranges of every match in it.
pub fn snippet(text: &str, terms: &[String]) -> (String, Vec<(u32, u32)>) {
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let chars = folded(&flat);
    let first = terms.iter().filter_map(|t| find_all(&chars, flat.len(), t).first().copied()).min_by_key(|r| r.0);
    let start_char = match first {
        Some((byte, _)) => chars.iter().position(|(b, _)| *b == byte).unwrap_or(0).saturating_sub(SNIPPET_BEFORE),
        None => 0,
    };
    let end_char = (start_char + SNIPPET_LEN).min(chars.len());
    let start_byte = chars.get(start_char).map(|(b, _)| *b).unwrap_or(flat.len());
    let end_byte = chars.get(end_char).map(|(b, _)| *b).unwrap_or(flat.len());
    let mut snip = String::new();
    if start_char > 0 {
        snip.push('…');
    }
    snip.push_str(&flat[start_byte..end_byte]);
    if end_char < chars.len() {
        snip.push('…');
    }
    let sc = folded(&snip);
    let mut ranges: Vec<(usize, usize)> = terms.iter().flat_map(|t| find_all(&sc, snip.len(), t)).collect();
    ranges.sort();
    let mut merged: Vec<(u32, u32)> = Vec::new();
    for (a, b) in ranges {
        match merged.last_mut() {
            Some(last) if a as u32 <= last.1 => last.1 = last.1.max(b as u32),
            _ => merged.push((a as u32, b as u32)),
        }
    }
    (snip, merged)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terms_split_quote_and_dedupe() {
        assert_eq!(terms("  만료 시각 "), vec!["만료", "시각"]);
        assert_eq!(terms("\"만료 시각\" retry Retry"), vec!["만료 시각", "retry"]);
        assert_eq!(terms("\"unterminated phrase"), vec!["unterminated phrase"]);
        assert!(terms("   ").is_empty());
        assert_eq!(match_expr(&terms("ab 만료시각 \"x\"y\"")).as_deref(), Some("\"만료시각\""));
        assert_eq!(like_pattern("50%_a\\"), "%50\\%\\_a\\\\%");
    }

    #[test]
    fn snippet_centers_on_the_match_and_marks_all_terms() {
        let text =
            format!("{}\n원인을 찾았습니다. 만료 시각은 초 단위인데 Expires 값과 비교합니다.", "앞 문장 ".repeat(40));
        let (s, h) = snippet(&text, &["만료 시각".into(), "expires".into()]);
        assert!(s.starts_with('…'));
        assert!(!s.contains('\n'));
        let marked: Vec<&str> = h.iter().map(|(a, b)| &s[*a as usize..*b as usize]).collect();
        assert_eq!(marked, vec!["만료 시각", "Expires"]);
        let (s, h) = snippet("짧은 글", &["없음".into()]);
        assert_eq!((s.as_str(), h.len()), ("짧은 글", 0));
    }
}
