//! The relationship graph: real links, page → session sources, and local similarity
//! suggestions that are not linked yet and were not dismissed.

use super::{index, links, ops};
use crate::store::{Store, db_err, opt_ts};
use runwick_core::api::wiki::*;
use runwick_core::rpc::RpcError;
use rusqlite::OptionalExtension;
use std::collections::HashSet;
use std::path::Path;
use uuid::Uuid;

/// Similarity needed before a pair is suggested, and how many suggestions to show.
const SIMILAR_THRESHOLD: f32 = 0.12;
const MAX_SIMILAR: usize = 24;

pub fn build(store: &Store, root: &Path, project: Option<Uuid>) -> Result<Graph, RpcError> {
    let pages = ops::list(store, root, project)?;
    let bodies: Vec<String> = store
        .with(|c| {
            pages
                .iter()
                .map(|p| {
                    let row = index::get(c, p.project_id, &p.slug)?;
                    Ok(match row {
                        Some(r) => index::body(c, r.id, r.version)?.map(|(_, b)| b).unwrap_or_default(),
                        None => String::new(),
                    })
                })
                .collect::<rusqlite::Result<Vec<String>>>()
        })
        .map_err(db_err)?;
    let mut edges = Vec::new();
    let mut broken = Vec::new();
    let mut linked: HashSet<(usize, usize)> = HashSet::new();
    // Links resolve inside the page's own project.
    for (i, (p, body)) in pages.iter().zip(&bodies).enumerate() {
        let same: Vec<(String, String)> =
            pages.iter().filter(|q| q.project_id == p.project_id).map(|q| (q.slug.clone(), q.title.clone())).collect();
        for target in links::extract(body) {
            match links::resolve(&target, &same) {
                Some(slug) if slug != p.slug => {
                    let j = pages.iter().position(|q| q.project_id == p.project_id && q.slug == slug).unwrap_or(i);
                    linked.insert((i.min(j), i.max(j)));
                    edges.push(Edge {
                        kind: EdgeKind::Link,
                        from: p.slug.clone(),
                        to: slug.to_string(),
                        score: None,
                        shared: vec![],
                    });
                }
                Some(_) => {}
                None => broken.push((p.slug.clone(), target)),
            }
        }
        for s in &p.sources {
            edges.push(Edge {
                kind: EdgeKind::Source,
                from: p.slug.clone(),
                to: s.to_string(),
                score: None,
                shared: vec![],
            });
        }
    }
    let dismissed: HashSet<(Uuid, String, String)> = store
        .with(|c| {
            let mut st = c.prepare("SELECT project_id, a, b FROM wiki_dismissed")?;
            let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
            rows.collect()
        })
        .map_err(db_err)?;
    let texts: Vec<String> = pages.iter().zip(&bodies).map(|(p, b)| format!("{}\n{}", p.title, b)).collect();
    for s in links::similar(&texts, SIMILAR_THRESHOLD, MAX_SIMILAR * 2) {
        let (a, b) = (&pages[s.a], &pages[s.b]);
        if a.project_id != b.project_id || linked.contains(&(s.a, s.b)) {
            continue;
        }
        let key = if a.slug <= b.slug {
            (a.project_id, a.slug.clone(), b.slug.clone())
        } else {
            (a.project_id, b.slug.clone(), a.slug.clone())
        };
        if dismissed.contains(&key) {
            continue;
        }
        edges.push(Edge {
            kind: EdgeKind::Similar,
            from: a.slug.clone(),
            to: b.slug.clone(),
            score: Some(s.score),
            shared: s.shared,
        });
        if edges.iter().filter(|e| e.kind == EdgeKind::Similar).count() >= MAX_SIMILAR {
            break;
        }
    }
    let mut ids: Vec<Uuid> = pages.iter().flat_map(|p| p.sources.iter().copied()).collect();
    ids.sort();
    ids.dedup();
    let sessions = store
        .with(|c| {
            ids.iter()
                .map(|id| {
                    let row = c
                        .query_row("SELECT title, created_at FROM sessions WHERE id = ?1", [id], |r| {
                            Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
                        })
                        .optional()?;
                    Ok(match row {
                        Some((title, created)) => SessionNode {
                            id: *id,
                            title: crate::sessions::visible(&title),
                            created_at: opt_ts(created),
                            exists: true,
                        },
                        None => {
                            SessionNode {
                                id: *id, title: "기록 없는 세션".into(), created_at: None, exists: false
                            }
                        }
                    })
                })
                .collect::<rusqlite::Result<Vec<_>>>()
        })
        .map_err(db_err)?;
    Ok(Graph { pages, sessions, edges, broken })
}
