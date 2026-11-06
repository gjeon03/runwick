//! File-name search over a per-workspace index, built lazily with the ignore walker (respecting
//! `.gitignore`) and rebuilt when it gets old. Building runs on the blocking pool.

use super::fuzzy::{Target, prepare_query, score};
use runwick_core::api::files::{FileHit, FindResult, compose_hangul};
use runwick_core::rpc::RpcError;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Files indexed at most per workspace.
const INDEX_MAX: usize = 200_000;
/// An index older than this is rebuilt on the next search.
const STALE_AFTER: Duration = Duration::from_secs(15);
const DEFAULT_LIMIT: usize = 50;
const MAX_LIMIT: usize = 200;

pub struct Index {
    built: Instant,
    files: Vec<(String, Target)>,
    truncated: bool,
}

impl Index {
    pub fn build(root: &Path) -> Index {
        let walker =
            ignore::WalkBuilder::new(root).follow_links(false).filter_entry(|e| e.file_name() != ".git").build();
        let mut files = Vec::new();
        let mut truncated = false;
        for item in walker {
            let Ok(e) = item else { continue };
            if !e.file_type().is_some_and(|t| t.is_file() || t.is_symlink()) {
                continue;
            }
            if files.len() >= INDEX_MAX {
                truncated = true;
                break;
            }
            let Some(rel) = super::paths::to_rel(root, e.path()) else {
                continue;
            };
            let target = Target::new(&compose_hangul(&rel));
            files.push((rel, target));
        }
        Index { built: Instant::now(), files, truncated }
    }

    pub fn search(&self, query: &str, limit: usize) -> FindResult {
        let q = prepare_query(query);
        let limit = if limit == 0 { DEFAULT_LIMIT } else { limit.min(MAX_LIMIT) };
        let mut hits: Vec<FileHit> = if q.is_empty() {
            Vec::new()
        } else {
            self.files
                .iter()
                .filter_map(|(rel, t)| {
                    score(&q, t).map(|m| FileHit { path: rel.clone(), matched: m.positions, score: m.score })
                })
                .collect()
        };
        hits.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.path.len().cmp(&b.path.len())));
        hits.truncate(limit);
        FindResult { hits, indexed: self.files.len(), truncated: self.truncated }
    }
}

type Slot = Arc<tokio::sync::Mutex<Option<Arc<Index>>>>;

fn slots() -> &'static Mutex<HashMap<PathBuf, Slot>> {
    static SLOTS: OnceLock<Mutex<HashMap<PathBuf, Slot>>> = OnceLock::new();
    SLOTS.get_or_init(Default::default)
}

/// Drop the index for a workspace so the next search sees new files.
pub fn invalidate(root: &Path) {
    if let Some(slot) = slots().lock().unwrap().get(root).cloned()
        && let Ok(mut g) = slot.try_lock()
    {
        *g = None;
    }
}

/// Search, rebuilding the index first when it is missing or stale. Concurrent searches of the
/// same workspace wait for one build instead of walking the folder twice.
pub async fn find(root: PathBuf, query: String, limit: usize) -> Result<FindResult, RpcError> {
    let slot = slots().lock().unwrap().entry(root.clone()).or_default().clone();
    let mut guard = slot.lock().await;
    let fresh = guard.as_ref().filter(|i| i.built.elapsed() < STALE_AFTER).cloned();
    let index = match fresh {
        Some(i) => i,
        None => {
            let r = root.clone();
            let built = tokio::task::spawn_blocking(move || Index::build(&r)).await.map_err(RpcError::internal)?;
            let built = Arc::new(built);
            *guard = Some(built.clone());
            built
        }
    };
    drop(guard);
    tokio::task::spawn_blocking(move || index.search(&query, limit)).await.map_err(RpcError::internal)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indexes_respecting_gitignore_and_ranks() {
        let d = tempfile::tempdir().unwrap();
        let r = std::fs::canonicalize(d.path()).unwrap();
        std::fs::create_dir_all(r.join(".git")).unwrap();
        std::fs::write(r.join(".gitignore"), "node_modules/\n").unwrap();
        std::fs::create_dir_all(r.join("node_modules/pkg")).unwrap();
        std::fs::write(r.join("node_modules/pkg/auth.js"), "").unwrap();
        std::fs::create_dir_all(r.join("src/auth")).unwrap();
        std::fs::write(r.join("src/auth/session.ts"), "").unwrap();
        std::fs::write(r.join("src/auth.ts"), "").unwrap();
        std::fs::create_dir_all(r.join("docs")).unwrap();
        std::fs::write(r.join("docs/회의 메모.md"), "").unwrap();
        let idx = Index::build(&r);
        assert_eq!(idx.files.len(), 3, "{:?}", idx.files.iter().map(|f| &f.0).collect::<Vec<_>>());
        let res = idx.search("auth", 10);
        assert_eq!(res.hits[0].path, "src/auth.ts");
        assert!(res.hits.iter().all(|h| !h.path.starts_with("node_modules")));
        assert_eq!(idx.search("ㅎㅇ", 10).hits[0].path, "docs/회의 메모.md");
        assert!(idx.search("", 10).hits.is_empty());
        assert_eq!(res.indexed, 3);
    }

    #[tokio::test]
    async fn find_sees_new_files_after_invalidate() {
        let d = tempfile::tempdir().unwrap();
        let r = std::fs::canonicalize(d.path()).unwrap();
        std::fs::write(r.join("one.md"), "").unwrap();
        assert_eq!(find(r.clone(), "one".into(), 5).await.unwrap().hits.len(), 1);
        std::fs::write(r.join("two.md"), "").unwrap();
        invalidate(&r);
        assert_eq!(find(r.clone(), "two".into(), 5).await.unwrap().hits.len(), 1);
    }
}
