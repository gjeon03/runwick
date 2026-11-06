//! Bringing a worktree's branch into the project's default workspace (S11 통합): preview,
//! `git merge --no-ff --no-commit` (the person or a CLI commits; nothing is pushed), conflicts
//! with their file text, and `git merge --abort` for "보류".

use super::{dirty, git_message, local_workspace};
use crate::git::git;
use crate::host::{Ctx, Host};
use crate::router::Router;
use runwick_core::api::collab::ChangedFile;
use runwick_core::api::projects::{ById, Workspace, WorkspaceId, WorkspaceKind, WorkspaceQuery};
use runwick_core::api::worktree::*;
use runwick_core::rpc::{RpcError, codes, topic};
use std::path::Path;
use std::sync::Arc;

/// Conflicted files shown at most this large.
const FILE_LIMIT: u64 = 400 * 1024;

pub fn register(r: &mut Router) {
    r.add::<WorktreeMergePreview, _, _>(|ctx: Ctx, p: ById| async move { preview(&ctx.host, p.id).await });
    r.add::<WorktreeMerge, _, _>(|ctx: Ctx, p: ById| async move { merge(&ctx.host, p.id).await });
    r.add::<WorktreeMergeAbort, _, _>(|ctx: Ctx, p: ById| async move {
        let (_, target) = pair(&ctx.host, p.id)?;
        git(Path::new(&target.path), &["merge", "--abort"])
            .await
            .map_err(|e| RpcError::new(codes::CONFLICT, format!("통합을 취소하지 못했어요: {}", git_message(&e))))?;
        ctx.host.bus.changed(&[topic::WORKSPACES, topic::FILES]);
        Ok(())
    });
    r.add::<WorktreeConflict, _, _>(|ctx: Ctx, q: ConflictQuery| async move {
        let (_, target) = pair(&ctx.host, q.workspace_id)?;
        if q.path.contains("..") || Path::new(&q.path).is_absolute() {
            return Err(RpcError::invalid("작업 폴더 밖의 파일은 열 수 없어요"));
        }
        let full = Path::new(&target.path).join(&q.path);
        match std::fs::metadata(&full) {
            Ok(m) if m.len() > FILE_LIMIT => {
                Err(RpcError::invalid("파일이 커서 여기서는 보여 주지 않아요. 직접 열어 주세요"))
            }
            Ok(_) => std::fs::read(&full)
                .map(|b| String::from_utf8_lossy(&b).into_owned())
                .map_err(|e| RpcError::internal(format!("파일을 읽지 못했어요: {e}"))),
            Err(_) => Err(RpcError::not_found("충돌 파일을 찾을 수 없어요")),
        }
    });
}

/// The worktree workspace and its project's default workspace (the merge target).
fn pair(host: &Host, id: WorkspaceId) -> Result<(Workspace, Workspace), RpcError> {
    let ws = local_workspace(host, id)?;
    let q = WorkspaceQuery { project_id: Some(ws.project_id), include_archived: true };
    let all = crate::projects::list_workspaces(&host.store, &q).map_err(crate::store::db_err)?;
    let target = all
        .into_iter()
        .find(|w| w.kind == WorkspaceKind::Default && w.remote.is_none())
        .ok_or_else(|| RpcError::not_found("통합할 기본 공간을 찾을 수 없어요"))?;
    Ok((ws, target))
}

async fn rev(dir: &Path, what: &str) -> Option<String> {
    git(dir, &["rev-parse", "--short", what]).await.ok().filter(|s| !s.is_empty())
}

async fn conflicts(dir: &Path) -> Vec<String> {
    git(dir, &["diff", "--name-only", "--diff-filter=U"])
        .await
        .map(|s| s.lines().map(str::to_string).collect())
        .unwrap_or_default()
}

async fn in_progress(dir: &Path) -> bool {
    git(dir, &["rev-parse", "-q", "--verify", "MERGE_HEAD"]).await.is_ok()
}

async fn files_between(dir: &Path, from: &str, to: &str) -> Vec<ChangedFile> {
    let mut out: Vec<ChangedFile> = git(dir, &["diff", "--name-status", "--no-renames", from, to])
        .await
        .map(|s| {
            s.lines()
                .filter_map(|l| l.split_once('\t'))
                .map(|(st, p)| ChangedFile {
                    path: p.to_string(),
                    status: st.chars().take(1).collect(),
                    added: None,
                    removed: None,
                })
                .collect()
        })
        .unwrap_or_default();
    if let Ok(ns) = git(dir, &["diff", "--numstat", "--no-renames", from, to]).await {
        for l in ns.lines() {
            let mut c = l.splitn(3, '\t');
            if let (Some(a), Some(d), Some(p)) = (c.next(), c.next(), c.next())
                && let Some(f) = out.iter_mut().find(|f| f.path == p)
            {
                f.added = a.parse().ok();
                f.removed = d.parse().ok();
            }
        }
    }
    out
}

pub async fn preview(host: &Arc<Host>, id: WorkspaceId) -> Result<MergePreview, RpcError> {
    let (ws, target) = pair(host, id)?;
    let src = Path::new(&ws.path);
    let dst = Path::new(&target.path);
    let mut p = MergePreview {
        workspace_id: id,
        source_branch: None,
        source_rev: None,
        target_branch: None,
        target_rev: None,
        target_path: target.path.clone(),
        merge_base: None,
        commits: Vec::new(),
        files: Vec::new(),
        source_dirty: 0,
        target_dirty: Vec::new(),
        in_progress: false,
        conflicts: Vec::new(),
        reason: None,
    };
    if ws.kind != WorkspaceKind::Worktree {
        p.reason = Some("분리해서 만든 공간만 기본 공간에 통합할 수 있어요".into());
        return Ok(p);
    }
    if !src.is_dir() || !dst.is_dir() {
        p.reason = Some("공간 폴더를 찾을 수 없어요".into());
        return Ok(p);
    }
    p.source_branch = git(src, &["rev-parse", "--abbrev-ref", "HEAD"]).await.ok().filter(|b| b != "HEAD");
    p.source_rev = rev(src, "HEAD").await;
    p.target_branch = git(dst, &["rev-parse", "--abbrev-ref", "HEAD"]).await.ok().filter(|b| b != "HEAD");
    p.target_rev = rev(dst, "HEAD").await;
    let (sd, su) = dirty(src).await;
    p.source_dirty = sd.len() as u32 + su;
    p.target_dirty = dirty(dst).await.0;
    p.in_progress = in_progress(dst).await;
    p.conflicts = conflicts(dst).await;
    let Some(source) = p.source_branch.clone() else {
        p.reason = Some("분리한 공간이 branch에 있지 않아 통합할 수 없어요".into());
        return Ok(p);
    };
    p.merge_base = git(dst, &["merge-base", "HEAD", &source]).await.ok().map(|r| r.chars().take(7).collect());
    p.commits = git(dst, &["log", "--format=%h%x09%s", &format!("HEAD..{source}")])
        .await
        .map(|s| {
            s.lines()
                .filter_map(|l| l.split_once('\t'))
                .map(|(r, sub)| Commit { rev: r.to_string(), subject: sub.to_string() })
                .collect()
        })
        .unwrap_or_default();
    if let Some(base) = &p.merge_base {
        p.files = files_between(dst, base, &source).await;
    }
    Ok(p)
}

pub async fn merge(host: &Arc<Host>, id: WorkspaceId) -> Result<MergeOutcome, RpcError> {
    let p = preview(host, id).await?;
    if let Some(r) = p.reason {
        return Err(RpcError::invalid(r));
    }
    if p.in_progress {
        return Err(RpcError::new(
            codes::CONFLICT,
            "기본 공간에서 이미 통합이 진행 중이에요. 마치거나 취소한 뒤 다시 시도하세요",
        ));
    }
    if !p.target_dirty.is_empty() {
        return Err(RpcError::new(
            codes::CONFLICT,
            format!("기본 공간에 커밋하지 않은 변경 {}개가 있어 통합하지 않았어요", p.target_dirty.len()),
        ));
    }
    if p.commits.is_empty() {
        return Ok(MergeOutcome {
            merged: false,
            conflicts: vec![],
            message: "통합할 커밋이 없어요".into(),
            head: p.target_rev,
        });
    }
    let source = p.source_branch.unwrap_or_default();
    let dst = Path::new(&p.target_path);
    let res = git(dst, &["merge", "--no-ff", "--no-commit", &source]).await;
    host.bus.changed(&[topic::WORKSPACES, topic::FILES]);
    let conflicts = conflicts(dst).await;
    match res {
        Ok(_) => Ok(MergeOutcome {
            merged: true,
            conflicts,
            message: "변경을 기본 공간에 합쳤어요. 아직 커밋하지 않았어요".into(),
            head: rev(dst, "HEAD").await,
        }),
        Err(_) if !conflicts.is_empty() => Ok(MergeOutcome {
            merged: false,
            message: format!("충돌 {}개 · 내 결정이 필요해요", conflicts.len()),
            conflicts,
            head: rev(dst, "HEAD").await,
        }),
        Err(e) => Err(RpcError::new(codes::CONFLICT, refused_message(&e))),
    }
}

/// Why git refused to start a merge, in words; files git named are listed.
fn refused_message(stderr: &str) -> String {
    let files: Vec<&str> = stderr.lines().filter(|l| l.starts_with('\t')).map(str::trim).collect();
    let list = files.join(", ");
    if stderr.contains("untracked working tree files would be overwritten") {
        format!(
            "기본 공간에 같은 이름의 추적하지 않는 파일이 있어 통합하지 않았어요: {list} · 옮기거나 지운 뒤 다시 시도하세요"
        )
    } else if stderr.contains("local changes to the following files would be overwritten") {
        format!("기본 공간의 커밋하지 않은 변경과 겹쳐 통합하지 않았어요: {list}")
    } else {
        format!("통합하지 못했어요: {}", git_message(stderr))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refusals_name_the_files() {
        let e = "error: The following untracked working tree files would be overwritten by merge:\n\tfake-1.txt\n\tsrc/b.rs\nPlease move or remove them before you merge.\nAborting\n";
        let m = refused_message(e);
        assert!(m.contains("추적하지 않는 파일") && m.contains("fake-1.txt, src/b.rs"), "{m}");
        assert_eq!(refused_message("fatal: something else\n"), "통합하지 못했어요: something else");
    }
}
