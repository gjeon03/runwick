//! Change proposals (from AI jobs) and the "결정 확인 필요" check.
//!
//! A proposal stores the page text it was written against (`base_body`) so it can be re-compared
//! with whatever the page says now. Nothing here edits a page; applying goes through the normal
//! save path in `wiki::ops`.

use super::links::tokens;
use super::merge;
use crate::store::{opt_ts, ts_from_sql, ts_to_sql};
use runwick_core::api::wiki::*;
use runwick_core::josa::{self, Particle};
use rusqlite::{Connection, OptionalExtension, Row, params};
use std::collections::HashSet;
use uuid::Uuid;

pub struct Stored {
    pub proposal: Proposal,
    pub page_id: Option<Uuid>,
    pub base_body: String,
    pub body: String,
}

fn status_sql(s: ProposalStatus) -> &'static str {
    match s {
        ProposalStatus::Open => "open",
        ProposalStatus::Held => "held",
        ProposalStatus::Applied => "applied",
        ProposalStatus::Partial => "partial",
        ProposalStatus::Rejected => "rejected",
    }
}

fn status_from(s: &str) -> ProposalStatus {
    match s {
        "held" => ProposalStatus::Held,
        "applied" => ProposalStatus::Applied,
        "partial" => ProposalStatus::Partial,
        "rejected" => ProposalStatus::Rejected,
        _ => ProposalStatus::Open,
    }
}

const SELECT: &str = "SELECT pr.*, pg.slug AS page_slug, pg.version AS page_version
    FROM wiki_proposals pr LEFT JOIN wiki_pages pg ON pg.id = pr.page_id AND pg.deleted = 0";

fn from_row(r: &Row) -> rusqlite::Result<Stored> {
    let base_version: Option<i64> = r.get("base_version")?;
    let page_version: Option<i64> = r.get("page_version")?;
    let page_id: Option<Uuid> = r.get("page_id")?;
    let status = status_from(&r.get::<_, String>("status")?);
    Ok(Stored {
        proposal: Proposal {
            id: r.get("id")?,
            project_id: r.get("project_id")?,
            slug: r.get("page_slug")?,
            title: r.get("title")?,
            rationale: r.get("rationale")?,
            sources: serde_json::from_str(&r.get::<_, String>("sources")?).unwrap_or_default(),
            decision_flag: r.get("decision_flag")?,
            status,
            base_version: base_version.map(|v| v as u32),
            needs_rebase: !status.is_closed() && page_id.is_some() && base_version != page_version,
            created_at: ts_from_sql(&r.get::<_, String>("created_at")?),
            decided_at: opt_ts(r.get("decided_at")?),
            job_id: r.get("job_id")?,
        },
        page_id,
        base_body: r.get::<_, Option<String>>("base_body")?.unwrap_or_default(),
        body: r.get("body")?,
    })
}

pub fn get(c: &Connection, id: Uuid) -> rusqlite::Result<Option<Stored>> {
    c.query_row(&format!("{SELECT} WHERE pr.id = ?1"), [id], from_row).optional()
}

pub fn list(c: &Connection, q: &ProposalQuery) -> rusqlite::Result<Vec<Proposal>> {
    let mut st = c.prepare(&format!(
        "{SELECT} WHERE (?1 IS NULL OR pr.project_id = ?1) AND (?2 IS NULL OR pg.slug = ?2)
           AND (?3 OR pr.status IN ('open', 'held'))
           AND (pr.page_id IS NULL OR pg.id IS NOT NULL)
         ORDER BY pr.created_at DESC"
    ))?;
    let rows = st.query_map(params![q.project_id, q.slug, q.include_closed], |r| from_row(r).map(|s| s.proposal))?;
    rows.collect()
}

pub fn open_count(c: &Connection, project: Uuid) -> rusqlite::Result<u32> {
    c.query_row("SELECT COUNT(*) FROM wiki_proposals WHERE project_id = ?1 AND status = 'open'", [project], |r| {
        r.get::<_, i64>(0).map(|n| n as u32)
    })
}

pub struct New<'a> {
    pub project_id: Uuid,
    pub page: Option<(Uuid, u32, &'a str)>,
    pub title: &'a str,
    pub body: &'a str,
    pub rationale: &'a str,
    pub sources: &'a [SourceRef],
    pub decision_flag: Option<String>,
    pub job_id: Option<Uuid>,
}

pub fn insert(c: &Connection, n: &New) -> rusqlite::Result<Uuid> {
    let id = Uuid::now_v7();
    c.execute(
        "INSERT INTO wiki_proposals (id, project_id, page_id, base_version, base_body, title, body, rationale,
             sources, decision_flag, status, job_id, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 'open', ?11, ?12)",
        params![
            id,
            n.project_id,
            n.page.map(|p| p.0),
            n.page.map(|p| p.1 as i64),
            n.page.map(|p| p.2),
            n.title,
            n.body,
            n.rationale,
            serde_json::to_string(n.sources).unwrap_or_else(|_| "[]".into()),
            n.decision_flag,
            n.job_id,
            ts_to_sql(&runwick_core::now()),
        ],
    )?;
    Ok(id)
}

pub fn set_status(c: &Connection, id: Uuid, status: ProposalStatus) -> rusqlite::Result<()> {
    let decided = status.is_closed().then(|| ts_to_sql(&runwick_core::now()));
    c.execute(
        "UPDATE wiki_proposals SET status = ?2, decided_at = ?3 WHERE id = ?1",
        params![id, status_sql(status), decided],
    )?;
    Ok(())
}

/// Hunks of a stored proposal against the page's current body (None for a new page).
pub fn hunks(s: &Stored, current: Option<&str>) -> Vec<merge::Placed> {
    match current {
        Some(cur) => merge::rebase(&s.base_body, cur, &s.body),
        None => vec![merge::Placed {
            hunk: Hunk {
                index: 0,
                kind: HunkKind::Clean,
                old: Vec::new(),
                new: merge::split(&s.body).iter().map(|l| l.to_string()).collect(),
                context_before: Vec::new(),
            },
            at: 0..0,
        }],
    }
}

// ---------------------------------------------------------------------------------------------
// Decisions

fn strip_marker(line: &str) -> &str {
    let t = line.trim();
    let t = t.trim_start_matches(['-', '*', '+']).trim_start();
    let digits = t.chars().take_while(|c| c.is_ascii_digit()).count();
    if digits > 0 && t[digits..].starts_with(". ") { t[digits + 2..].trim() } else { t }
}

fn mentions_decision(s: &str) -> bool {
    s.contains("결정") || s.to_lowercase().contains("decision")
}

/// Lines that record decisions: every item of a page titled/tagged "결정", items under a
/// "결정" heading, and lines starting with "결정:" anywhere.
pub fn decision_lines<'a>(pages: impl IntoIterator<Item = (&'a str, &'a [String], &'a str)>) -> Vec<String> {
    let mut out = Vec::new();
    for (title, tags, body) in pages {
        let whole = mentions_decision(title) || tags.iter().any(|t| mentions_decision(t));
        let mut section = false;
        let mut in_code = false;
        for line in body.lines() {
            let t = line.trim();
            if t.starts_with("```") {
                in_code = !in_code;
                continue;
            }
            if in_code || t.is_empty() {
                continue;
            }
            if let Some(h) = t.strip_prefix('#') {
                section = mentions_decision(h);
                continue;
            }
            let item = strip_marker(t);
            let prefixed =
                item.starts_with("결정:") || item.starts_with("결정 :") || item.to_lowercase().starts_with("decision:");
            if (whole || section || prefixed) && item.chars().count() >= 4 {
                out.push(item.chars().take(200).collect());
            }
        }
    }
    out
}

fn short(s: &str) -> String {
    let s: String = s.chars().take(40).collect();
    if s.chars().count() == 40 { format!("{s}…") } else { s }
}

/// Simple contradiction check: the proposal removes or rewrites a recorded decision, or its new
/// lines talk about the same thing as a decision in different words.
pub fn decision_flag(base: &str, proposed: &str, decisions: &[String]) -> Option<String> {
    let (b, p) = (merge::split(base), merge::split(proposed));
    let bs: HashSet<&str> = b.iter().map(|l| strip_marker(l)).collect();
    let ps: HashSet<&str> = p.iter().map(|l| strip_marker(l)).collect();
    let removed: Vec<&str> = bs.difference(&ps).copied().filter(|l| !l.is_empty()).collect();
    let added: Vec<&str> = ps.difference(&bs).copied().filter(|l| !l.is_empty()).collect();
    for d in decisions {
        if removed.iter().any(|r| r == d) {
            let s = short(d);
            return Some(format!(
                "기록된 결정 “{s}”{} 바꾸거나 지우는 제안이에요. 조용히 덮지 않았어요.",
                josa::particle(&s, Particle::EulReul)
            ));
        }
    }
    for d in decisions {
        let dt: HashSet<String> = tokens(d).into_iter().collect();
        if dt.len() < 3 {
            continue;
        }
        for a in &added {
            if a == d {
                continue;
            }
            let at: HashSet<String> = tokens(a).into_iter().collect();
            let shared = dt.intersection(&at).count();
            if shared >= 3 && shared * 2 >= dt.len() {
                let s = short(d);
                return Some(format!(
                    "결정 “{s}”{} 범위가 겹쳐요. 조용히 덮지 않았어요.",
                    josa::particle(&s, Particle::GwaWa)
                ));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_decisions_by_page_heading_and_prefix() {
        let tags = vec!["결정".to_string()];
        let none: Vec<String> = Vec::new();
        let lines = decision_lines([
            ("주요 결정", none.as_slice(), "# 주요 결정\n- 쿠키 만료는 서버 기준\n- 재시도는 세 번까지\n"),
            ("메모", tags.as_slice(), "1. 배포는 화요일에만\n"),
            (
                "로그인",
                none.as_slice(),
                "본문\n## 결정\n- 세션 시간은 30분\n## 기타\n- 무관한 줄\n결정: 토큰은 초 단위로 저장\n",
            ),
        ]);
        assert_eq!(
            lines,
            vec![
                "쿠키 만료는 서버 기준",
                "재시도는 세 번까지",
                "배포는 화요일에만",
                "세션 시간은 30분",
                "결정: 토큰은 초 단위로 저장"
            ]
        );
    }

    #[test]
    fn flags_removed_or_overlapping_decisions() {
        let decisions = vec!["쿠키 만료는 서버 시각 기준으로 판단".to_string()];
        let base = "- 쿠키 만료는 서버 시각 기준으로 판단\n- 기타\n";
        let removed = decision_flag(base, "- 기타\n", &decisions).unwrap();
        assert!(removed.contains("바꾸거나 지우는"), "{removed}");
        let overlap = decision_flag("", "쿠키 만료는 클라이언트 시각 기준으로 판단한다\n", &decisions).unwrap();
        assert!(overlap.contains("범위가 겹쳐요"), "{overlap}");
        assert!(overlap.contains("”과"), "{overlap}");
        assert_eq!(decision_flag("", "배포 절차에 롤백 단계를 추가\n", &decisions), None);
    }
}
