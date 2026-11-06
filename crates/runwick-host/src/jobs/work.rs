//! What each job kind does around the AI call: gather the project's allowed records into a
//! prompt ([`prepare`]) and turn the answer into proposals, a draft page or a day summary
//! ([`complete`]). A failure at any step leaves existing pages untouched.

use super::{prompts, records as rec, store};
use crate::host::Host;
use crate::store::db_err;
use crate::wiki::ops;
use runwick_core::Ts;
use runwick_core::api::jobs::{Job, JobKind};
use runwick_core::api::records::Message;
use runwick_core::api::timeline::Summary;
use runwick_core::api::wiki::{Author, PageStatus, SavedBy, SourceRef};
use std::collections::HashMap;
use std::path::Path;
use uuid::Uuid;

/// How far back the first wiki run (or a request with no new records) reads.
const FIRST_LOOKBACK: chrono::Duration = chrono::Duration::days(14);
const READ_LIMIT: usize = 2_000;
const SESSION_LIMIT: usize = 400;

pub enum Prepared {
    /// Call the AI with this prompt.
    Ask { prompt: String, basis: Basis },
    /// Nothing to send; finish with this result.
    Nothing(String),
}

pub enum Basis {
    Wiki { messages: Vec<Message>, read_to: Ts },
    Draft { messages: Vec<Message>, sessions: Vec<Uuid> },
    Summary { day: chrono::NaiveDate, count: u32, fingerprint: String },
}

fn project_name(host: &Host, id: Uuid) -> Result<String, String> {
    crate::projects::get_project(&host.store, id).map(|p| p.name).map_err(|e| e.message)
}

fn session_titles(host: &Host, ids: impl Iterator<Item = Uuid>) -> HashMap<Uuid, String> {
    let ids: Vec<Uuid> = ids.collect();
    host.store
        .with(|c| {
            let mut out = HashMap::new();
            for id in ids {
                if out.contains_key(&id) {
                    continue;
                }
                let t: Option<String> = rusqlite::OptionalExtension::optional(c.query_row(
                    "SELECT title FROM sessions WHERE id = ?1",
                    [id],
                    |r| r.get(0),
                ))?;
                out.insert(id, t.map_or_else(|| "세션".into(), |t| crate::sessions::visible(&t)));
            }
            Ok(out)
        })
        .unwrap_or_default()
}

/// Earliest start of the given sessions of this host.
fn sessions_started(host: &Host, ids: &[Uuid]) -> Option<Ts> {
    host.store
        .with(|c| {
            let mut first: Option<Ts> = None;
            for id in ids {
                let t: Option<String> = rusqlite::OptionalExtension::optional(c.query_row(
                    "SELECT created_at FROM sessions WHERE id = ?1",
                    [id],
                    |r| r.get(0),
                ))?;
                if let Some(t) = t.map(|s| crate::store::ts_from_sql(&s)) {
                    first = Some(first.map_or(t, |f| f.min(t)));
                }
            }
            Ok(first)
        })
        .ok()
        .flatten()
}

fn pages(host: &Host, root: &Path, project: Uuid) -> Result<Vec<(String, String, String)>, String> {
    ops::pages_with_bodies(&host.store, root, project)
        .map(|v| v.into_iter().map(|(r, b)| (r.slug, r.title, b)).collect())
        .map_err(|e| e.message)
}

pub fn wiki_cursor(host: &Host, project: Uuid) -> Option<Ts> {
    host.store
        .with(|c| store::cursor(c, project, JobKind::WikiProposals, ""))
        .ok()
        .flatten()
        .map(|s| crate::store::ts_from_sql(&s))
}

pub fn prepare(host: &Host, root: &Path, job: &Job) -> Result<Prepared, String> {
    let name = project_name(host, job.project_id)?;
    match job.kind {
        JobKind::WikiProposals => {
            let now = runwick_core::now();
            let since = wiki_cursor(host, job.project_id).unwrap_or(now - FIRST_LOOKBACK);
            let mut messages = rec::messages_since(host, job.project_id, &since, READ_LIMIT)?;
            if messages.is_empty() && job.trigger == runwick_core::api::jobs::Trigger::Requested {
                messages = rec::messages_since(host, job.project_id, &(now - FIRST_LOOKBACK), READ_LIMIT)?;
            }
            if messages.is_empty() {
                return Ok(Prepared::Nothing("정리할 새 대화 기록이 없어요".into()));
            }
            let titles = session_titles(host, messages.iter().map(|m| m.session_id));
            let (records, used) =
                prompts::records_block(&messages, &|m| titles.get(&m.session_id).cloned().unwrap_or_default());
            let read_to = messages.iter().map(|m| m.at).max().unwrap_or(now);
            let mut sorted = messages;
            sorted.sort_by_key(|m| std::cmp::Reverse(m.at));
            sorted.truncate(used);
            let prompt =
                prompts::wiki_prompt(&name, &prompts::pages_block(&pages(host, root, job.project_id)?), &records);
            Ok(Prepared::Ask { prompt, basis: Basis::Wiki { messages: sorted, read_to } })
        }
        JobKind::WikiDraft => {
            if job.target.sessions.is_empty() {
                return Err("초안을 만들 세션을 골라 주세요".into());
            }
            let started =
                sessions_started(host, &job.target.sessions).ok_or("고른 세션을 이 컴퓨터의 기록에서 찾지 못했어요")?;
            let messages = rec::session_messages(
                host,
                job.project_id,
                &job.target.sessions,
                started - chrono::Duration::minutes(1),
                SESSION_LIMIT,
            )?;
            if messages.is_empty() {
                return Ok(Prepared::Nothing("고른 세션에 저장된 대화 기록이 없어요".into()));
            }
            let titles = session_titles(host, job.target.sessions.iter().copied());
            let (records, _) =
                prompts::records_block(&messages, &|m| titles.get(&m.session_id).cloned().unwrap_or_default());
            let prompt =
                prompts::draft_prompt(&name, &records, &prompts::pages_block(&pages(host, root, job.project_id)?));
            Ok(Prepared::Ask { prompt, basis: Basis::Draft { messages, sessions: job.target.sessions.clone() } })
        }
        JobKind::TimelineSummary => {
            let day = job
                .target
                .day
                .as_deref()
                .and_then(|d| chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
                .ok_or("요약할 날짜가 없어요")?;
            let d = rec::day(host, job.project_id, day)?;
            if d.count == 0 {
                return Ok(Prepared::Nothing("그날 기록이 없어요".into()));
            }
            let prompt = prompts::summary_prompt(&name, &day.to_string(), &d.lines.join("\n"));
            Ok(Prepared::Ask { prompt, basis: Basis::Summary { day, count: d.count, fingerprint: d.fingerprint } })
        }
    }
}

fn source_refs(ids: &[String], messages: &[Message], titles: &HashMap<Uuid, String>) -> Vec<SourceRef> {
    let mut out: Vec<SourceRef> = Vec::new();
    for id in ids {
        let Some(m) = messages.iter().find(|m| m.id.to_string() == *id) else {
            continue;
        };
        if out.iter().any(|s| s.message_id.as_deref() == Some(id.as_str())) {
            continue;
        }
        out.push(SourceRef {
            session_id: m.session_id,
            message_id: Some(id.clone()),
            session_title: titles.get(&m.session_id).cloned(),
            at: Some(m.at),
        });
    }
    out
}

/// Apply the AI's answer. Returns the short result shown in job lists.
pub fn complete(
    host: &Host,
    root: &Path,
    job: &Job,
    basis: &Basis,
    answer: &str,
    provider: &str,
) -> Result<String, String> {
    match basis {
        Basis::Wiki { messages, read_to } => {
            let raw = prompts::parse_proposals(answer)?;
            let titles = session_titles(host, messages.iter().map(|m| m.session_id));
            let existing: Vec<String> = pages(host, root, job.project_id)?.into_iter().map(|p| p.0).collect();
            let mut made = 0;
            for p in raw {
                let slug = p.page.filter(|s| existing.contains(s));
                let added = ops::add_proposal(
                    &host.store,
                    root,
                    ops::AiProposal {
                        project_id: job.project_id,
                        slug,
                        title: p.title,
                        body: p.content,
                        rationale: p.rationale,
                        sources: source_refs(&p.sources, messages, &titles),
                        ai_flag: p.decision_conflict,
                        job_id: Some(job.id),
                    },
                )
                .map_err(|e| e.message)?;
                made += added.is_some() as u32;
            }
            host.store
                .with(|c| {
                    store::set_cursor(c, job.project_id, JobKind::WikiProposals, "", &crate::store::ts_to_sql(read_to))
                })
                .map_err(|e| db_err(e).message)?;
            Ok(if made == 0 {
                "바꿀 내용이 없다고 판단했어요".into()
            } else {
                format!("제안 {made}건")
            })
        }
        Basis::Draft { messages, sessions } => {
            let d = prompts::parse_draft(answer)?;
            let titles = session_titles(host, sessions.iter().copied());
            let cited = source_refs(&d.sources, messages, &titles);
            let mut used: Vec<Uuid> = cited.iter().map(|s| s.session_id).collect();
            if used.is_empty() {
                used = sessions.clone();
            }
            used.dedup();
            let page = ops::create(
                &host.store,
                root,
                ops::NewPage {
                    project_id: job.project_id,
                    title: d.title,
                    body: d.content,
                    tags: Vec::new(),
                    sources: used,
                    created_by: Author::Ai,
                    status: PageStatus::Draft,
                    saved_by: SavedBy::Ai,
                    note: Some(format!("AI 초안 · {provider} · 직접 요청")),
                    slug: None,
                },
            )
            .map_err(|e| e.message)?;
            Ok(format!("초안 “{}”", page.summary.title))
        }
        Basis::Summary { day, count, fingerprint } => {
            let text = prompts::parse_summary(answer)?;
            rec::store_summary(
                host,
                &Summary {
                    project_id: job.project_id,
                    day: *day,
                    text,
                    generated_at: runwick_core::now(),
                    basis_count: *count,
                    provider: Some(provider.to_string()),
                },
            )?;
            host.store
                .with(|c| store::set_cursor(c, job.project_id, JobKind::TimelineSummary, &day.to_string(), fingerprint))
                .map_err(|e| db_err(e).message)?;
            Ok(format!("요약 저장 · 기록 {count}건 기준"))
        }
    }
}
