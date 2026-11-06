//! "분리해서 작업" (S10): a git worktree as its own workspace of the same project, removing it
//! later with every risk shown first, and local merging (`merge`).
//!
//! Creation runs in visible steps (git check → worktree → optional patch → link → CLI). A later
//! step's failure keeps what earlier steps made; nothing falls back to the default workspace.

mod merge;

use crate::git::git;
use crate::host::{Ctx, Host};
use crate::router::Router;
use crate::store::{db_err, ts_to_sql};
use runwick_core::api::projects::{ById, Workspace, WorkspaceId, WorkspaceKind, WorkspaceUpdate};
use runwick_core::api::sessions::StartSession;
use runwick_core::api::worktree::*;
use runwick_core::rpc::{RpcError, codes, topic};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use uuid::Uuid;

pub fn register(r: &mut Router) {
    r.add::<WorktreePlan, _, _>(|ctx: Ctx, p: ById| async move { plan(&ctx.host, p.id).await });
    r.add::<WorktreeCreate, _, _>(|ctx: Ctx, p: Create| async move { create(&ctx.host, p).await });
    r.add::<WorktreeChanges, _, _>(|ctx: Ctx, p: ById| async move {
        let ws = local_workspace(&ctx.host, p.id)?;
        Ok(git(Path::new(&ws.path), &["diff", "HEAD", "--stat", "--patch"]).await.unwrap_or_default())
    });
    r.add::<WorktreeRemoveCheck, _, _>(|ctx: Ctx, p: ById| async move { remove_check(&ctx.host, p.id).await });
    r.add::<WorktreeRemove, _, _>(|ctx: Ctx, p: Remove| async move { remove(&ctx.host, p).await });
    merge::register(r);
}

fn local_workspace(host: &Host, id: WorkspaceId) -> Result<Workspace, RpcError> {
    let ws = crate::projects::get_workspace(&host.store, id)?;
    if ws.remote.is_some() {
        return Err(RpcError::invalid("다른 기기의 작업 공간은 그 기기에서 분리해요"));
    }
    Ok(ws)
}

/// `(status, path)` of `git status --porcelain` lines; `??` marks untracked files.
async fn status(dir: &Path) -> Vec<(String, String)> {
    let Ok(out) = git(dir, &["status", "--porcelain", "-z"]).await else {
        return Vec::new();
    };
    let mut entries = out.split('\0').filter(|l| !l.is_empty());
    let mut v = Vec::new();
    while let Some(l) = entries.next() {
        if l.len() < 4 || !l.is_char_boundary(3) {
            continue;
        }
        let code = l[..2].trim().to_string();
        if code.starts_with('R') || code.starts_with('C') {
            entries.next(); // the original path of a rename or copy
        }
        v.push((code, l[3..].to_string()));
    }
    v
}

/// Tracked changes as "M src/a.rs" and the number of untracked files.
async fn dirty(dir: &Path) -> (Vec<String>, u32) {
    let all = status(dir).await;
    let untracked = all.iter().filter(|(s, _)| s == "??").count() as u32;
    let tracked = all.into_iter().filter(|(s, _)| s != "??").map(|(s, p)| format!("{s} {p}")).collect();
    (tracked, untracked)
}

/// The main repository folder (also for a linked worktree).
fn main_root(top: &Path) -> PathBuf {
    crate::git::main_worktree_root(top).unwrap_or_else(|| top.to_path_buf())
}

/// `<repo>.worktrees/<name>` next to the main repository.
fn planned_path(main: &Path, name: &str) -> PathBuf {
    let repo = main.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "repo".into());
    main.with_file_name(format!("{repo}.worktrees")).join(name)
}

/// Names usable as a folder: letters, digits, `-`, `_`, `.` (Korean letters included).
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && name.chars().all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

async fn valid_branch(dir: &Path, branch: &str) -> bool {
    git(dir, &["check-ref-format", "--branch", branch]).await.is_ok()
}

pub async fn plan(host: &Arc<Host>, id: WorkspaceId) -> Result<Plan, RpcError> {
    let ws = local_workspace(host, id)?;
    let dir = PathBuf::from(&ws.path);
    let mut plan = Plan {
        workspace_id: id,
        is_git: false,
        reason: None,
        repo_root: None,
        base_branch: None,
        base_rev: None,
        dirty: Vec::new(),
        untracked: 0,
        suggested_name: String::new(),
        suggested_branch: String::new(),
        suggested_path: String::new(),
        branches: Vec::new(),
    };
    if !dir.is_dir() {
        plan.reason = Some(format!("작업 폴더를 찾을 수 없어요: {}", ws.path));
        return Ok(plan);
    }
    let Ok(top) = git(&dir, &["rev-parse", "--show-toplevel"]).await else {
        plan.reason = Some("Git 저장소가 아니라 분리할 수 없어요. 현재 공간에서 순서대로 작업할 수 있어요".into());
        return Ok(plan);
    };
    plan.is_git = true;
    let main = main_root(Path::new(&top));
    plan.repo_root = Some(main.to_string_lossy().into_owned());
    match git(&dir, &["rev-parse", "--short", "HEAD"]).await {
        Ok(rev) => plan.base_rev = Some(rev),
        Err(_) => plan.reason = Some("아직 커밋이 없어 분리할 수 없어요. 먼저 커밋해 주세요".into()),
    }
    plan.base_branch = git(&dir, &["rev-parse", "--abbrev-ref", "HEAD"]).await.ok().filter(|b| b != "HEAD");
    (plan.dirty, plan.untracked) = dirty(&dir).await;
    plan.branches = git(&dir, &["for-each-ref", "--format=%(refname:short)", "refs/heads"])
        .await
        .map(|s| s.lines().map(str::to_string).collect())
        .unwrap_or_default();
    let name = (1..1000)
        .map(|n| format!("work-{n}"))
        .find(|n| !planned_path(&main, n).exists() && !plan.branches.contains(&format!("work/{n}")))
        .unwrap_or_else(|| format!("work-{}", &Uuid::now_v7().simple().to_string()[24..]));
    plan.suggested_path = planned_path(&main, &name).to_string_lossy().into_owned();
    plan.suggested_branch = format!("work/{name}");
    plan.suggested_name = name;
    Ok(plan)
}

fn step(kind: StepKind, state: StepState, message: Option<String>) -> Step {
    Step { kind, state, message }
}

/// Mark the steps after a failure as not run.
fn skip_rest(steps: &mut Vec<Step>, wanted: &[StepKind]) {
    for k in wanted {
        if !steps.iter().any(|s| s.kind == *k) {
            steps.push(step(*k, StepState::Skipped, None));
        }
    }
}

pub async fn create(host: &Arc<Host>, p: Create) -> Result<Created, RpcError> {
    let mut wanted = vec![StepKind::CheckGit, StepKind::CreateWorktree];
    if p.carry_changes {
        wanted.push(StepKind::CarryChanges);
    }
    wanted.push(StepKind::Link);
    if p.tool.is_some() {
        wanted.push(StepKind::StartCli);
    }
    let mut out = Created { steps: Vec::new(), workspace: None, session: None };
    let src = local_workspace(host, p.workspace_id)?;
    let plan = plan(host, p.workspace_id).await?;
    let name = p.name.trim();
    let branch = p.branch.trim();
    let target = p
        .path
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| planned_path(Path::new(plan.repo_root.as_deref().unwrap_or("")), name));
    let src_dir = PathBuf::from(&src.path);

    // 1. Git check.
    let problem = if let Some(r) = plan.reason.clone().filter(|_| !plan.is_git || plan.base_rev.is_none()) {
        Some(r)
    } else if !valid_name(name) {
        Some("작업 이름에는 글자, 숫자, -, _, . 만 쓸 수 있어요".into())
    } else if !valid_branch(&src_dir, branch).await {
        Some(format!("branch 이름으로 쓸 수 없어요: {branch}"))
    } else if !p.use_existing_branch && plan.branches.iter().any(|b| b == branch) {
        Some(format!("이미 있는 branch예요: {branch} · 기존 branch 쓰기를 고르세요"))
    } else if target.exists() {
        Some(format!("폴더가 이미 있어요: {}", target.display()))
    } else {
        None
    };
    if let Some(msg) = problem {
        out.steps.push(step(StepKind::CheckGit, StepState::Failed, Some(msg)));
        skip_rest(&mut out.steps, &wanted);
        return Ok(out);
    }
    out.steps.push(step(StepKind::CheckGit, StepState::Done, None));

    // 2. Worktree.
    let main = PathBuf::from(plan.repo_root.as_deref().unwrap_or(&src.path));
    if let Some(parent) = target.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let target_s = target.to_string_lossy().into_owned();
    let head = git(&src_dir, &["rev-parse", "HEAD"]).await.unwrap_or_default();
    let base = p.base.as_deref().map(str::trim).filter(|b| !b.is_empty()).unwrap_or(&head).to_string();
    let added = if p.use_existing_branch {
        git(&main, &["worktree", "add", &target_s, branch]).await
    } else {
        git(&main, &["worktree", "add", "-b", branch, &target_s, &base]).await
    };
    if let Err(e) = added {
        out.steps.push(step(StepKind::CreateWorktree, StepState::Failed, Some(git_message(&e))));
        skip_rest(&mut out.steps, &wanted);
        return Ok(out);
    }
    out.steps.push(step(StepKind::CreateWorktree, StepState::Done, Some(target_s.clone())));

    // 3. Uncommitted changes as a patch (the source folder is not touched).
    let mut carried_ok = true;
    if p.carry_changes {
        let msg = carry(&src_dir, &target).await;
        carried_ok = msg.is_ok();
        out.steps.push(match msg {
            Ok(m) => step(StepKind::CarryChanges, StepState::Done, Some(m)),
            Err(m) => step(StepKind::CarryChanges, StepState::Failed, Some(m)),
        });
    }

    // 4. Link as a workspace of the same project.
    let ws = match link(host, &src, &target_s, name, &base) {
        Ok(ws) => ws,
        Err(e) => {
            out.steps.push(step(StepKind::Link, StepState::Failed, Some(e.message)));
            skip_rest(&mut out.steps, &wanted);
            return Ok(out);
        }
    };
    host.bus.changed(&[topic::WORKSPACES, topic::PROJECTS]);
    out.steps.push(step(StepKind::Link, StepState::Done, None));
    out.workspace = Some(ws.clone());

    // 5. CLI (only when everything before it worked; retrying it is "CLI만 다시 시도").
    if let Some(tool) = p.tool {
        if !carried_ok {
            skip_rest(&mut out.steps, &wanted);
            return Ok(out);
        }
        let start = StartSession {
            workspace_id: ws.id,
            tool,
            title: None,
            initial_prompt: None,
            parent_id: None,
            role: None,
            cols: if p.cols == 0 { 120 } else { p.cols },
            rows: if p.rows == 0 { 32 } else { p.rows },
        };
        match crate::sessions::start_session(host.clone(), start).await {
            Ok(info) => {
                let failed = info.live.as_ref().and_then(|l| l.error.clone());
                out.steps.push(match &failed {
                    Some(e) => step(StepKind::StartCli, StepState::Failed, Some(e.clone())),
                    None => step(StepKind::StartCli, StepState::Done, Some(tool.label().into())),
                });
                out.session = Some(info);
            }
            Err(e) => out.steps.push(step(StepKind::StartCli, StepState::Failed, Some(e.message))),
        }
    }
    Ok(out)
}

/// git's own message without its "fatal:" prefix.
fn git_message(e: &str) -> String {
    let line = e.lines().find(|l| !l.trim().is_empty()).unwrap_or(e).trim();
    line.trim_start_matches("fatal: ").trim_start_matches("error: ").to_string()
}

/// Apply the source's uncommitted tracked changes in the new folder.
async fn carry(src: &Path, target: &Path) -> Result<String, String> {
    let patch = git(src, &["diff", "HEAD", "--binary"]).await.map_err(|e| git_message(&e))?;
    if patch.trim().is_empty() {
        return Ok("적용할 변경이 없었어요".into());
    }
    let file = std::env::temp_dir().join(format!("runwick-carry-{}.patch", Uuid::now_v7().simple()));
    std::fs::write(&file, format!("{patch}\n")).map_err(|e| format!("변경을 임시 파일에 쓰지 못했어요: {e}"))?;
    let file_s = file.to_string_lossy().into_owned();
    let applied = git(target, &["apply", "--whitespace=nowarn", &file_s]).await;
    let _ = std::fs::remove_file(&file);
    let files = patch.lines().filter(|l| l.starts_with("diff --git")).count();
    applied
        .map(|_| format!("{files}개 파일의 변경을 적용했어요 · 추적하지 않는 파일은 옮기지 않았어요"))
        .map_err(|e| format!("변경을 적용하지 못했어요: {} · 새 폴더에는 출발 기준 상태만 있어요", git_message(&e)))
}

/// A worktree row of the source's project. Written like `projects` writes its own rows
/// (`kind = 'worktree'`, with the start point as `base_ref`).
fn link(host: &Host, src: &Workspace, path: &str, name: &str, base: &str) -> Result<Workspace, RpcError> {
    let canonical = std::fs::canonicalize(path).map(|p| p.to_string_lossy().into_owned()).unwrap_or(path.to_string());
    let id = Uuid::now_v7();
    let now = ts_to_sql(&runwick_core::now());
    let base_short: String = base.chars().take(12).collect();
    host.store
        .with(|c| {
            c.execute(
                "INSERT INTO workspaces (id, project_id, name, path, kind, base_ref, created_at, last_used_at)
                 VALUES (?1, ?2, ?3, ?4, 'worktree', ?5, ?6, ?6)",
                rusqlite::params![id, src.project_id, name, canonical, base_short, now],
            )
        })
        .map_err(db_err)?;
    crate::projects::get_workspace(&host.store, id)
}

/// Make a worktree for `runwick delegate --split <branch>`: planned folder, current HEAD, no
/// carried changes, no CLI (the delegation starts its own).
pub async fn split_for(host: &Arc<Host>, from: WorkspaceId, branch: &str) -> Result<Workspace, RpcError> {
    let name: String = branch.rsplit('/').next().unwrap_or(branch).to_string();
    let p = Create {
        workspace_id: from,
        name,
        branch: branch.to_string(),
        base: None,
        use_existing_branch: false,
        path: None,
        carry_changes: false,
        tool: None,
        cols: 0,
        rows: 0,
    };
    let out = create(host, p).await?;
    out.workspace.ok_or_else(|| {
        let why = out.steps.iter().find(|s| s.state == StepState::Failed).and_then(|s| s.message.clone());
        RpcError::invalid(format!("분리한 공간을 만들지 못했어요: {}", why.unwrap_or_else(|| "알 수 없는 이유".into())))
    })
}

fn running_in(host: &Arc<Host>, ws: WorkspaceId) -> Vec<RunningSession> {
    host.sessions
        .running_ids()
        .into_iter()
        .filter_map(|id| crate::sessions::session_record(&host.store, id).ok())
        .filter(|s| s.workspace_id == ws)
        .map(|s| RunningSession { id: s.id, title: s.title, tool: s.tool })
        .collect()
}

pub async fn remove_check(host: &Arc<Host>, id: WorkspaceId) -> Result<RemoveCheck, RpcError> {
    let ws = local_workspace(host, id)?;
    let dir = PathBuf::from(&ws.path);
    let path_exists = dir.is_dir();
    let removable = ws.kind == WorkspaceKind::Worktree;
    let mut c = RemoveCheck {
        workspace_id: id,
        path: ws.path.clone(),
        path_exists,
        branch: ws.branch.clone(),
        removable,
        reason: (!removable).then(|| "기본 공간의 폴더는 여기서 제거하지 않아요".into()),
        running: running_in(host, id),
        dirty: Vec::new(),
        untracked: 0,
        unpushed: None,
        upstream: None,
    };
    if !path_exists {
        return Ok(c);
    }
    (c.dirty, c.untracked) = dirty(&dir).await;
    c.upstream = git(&dir, &["rev-parse", "--abbrev-ref", "@{u}"]).await.ok();
    let range = match (&c.upstream, &ws.base_ref) {
        (Some(u), _) => Some(format!("{u}..HEAD")),
        (None, Some(b)) => Some(format!("{b}..HEAD")),
        _ => None,
    };
    if let Some(r) = range {
        c.unpushed = git(&dir, &["rev-list", "--count", &r]).await.ok().and_then(|n| n.trim().parse().ok());
    }
    Ok(c)
}

pub async fn remove(host: &Arc<Host>, p: Remove) -> Result<(), RpcError> {
    let check = remove_check(host, p.workspace_id).await?;
    if !check.removable {
        return Err(RpcError::invalid(check.reason.unwrap_or_default()));
    }
    if !check.running.is_empty() {
        return Err(RpcError::new(
            codes::CONFLICT,
            format!("이 공간에서 실행 중인 세션 {}개를 먼저 종료해 주세요", check.running.len()),
        ));
    }
    if check.path_exists {
        let main = main_root(Path::new(&check.path));
        let mut args = vec!["worktree", "remove"];
        if p.force {
            args.push("--force");
        }
        args.push(&check.path);
        git(&main, &args)
            .await
            .map_err(|e| RpcError::new(codes::CONFLICT, format!("폴더를 제거하지 않았어요: {}", git_message(&e))))?;
    }
    // Records and results stay; the workspace leaves the list.
    crate::projects::update_workspace(
        &host.store,
        &WorkspaceUpdate { id: p.workspace_id, archived: Some(true), ..Default::default() },
    )?;
    host.bus.changed(&[topic::WORKSPACES]);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_paths() {
        assert!(valid_name("auth-tests") && valid_name("로그인_수정") && valid_name("v1.2"));
        assert!(!valid_name("") && !valid_name("../x") && !valid_name("a b") && !valid_name(".hidden"));
        assert_eq!(planned_path(Path::new("/w/app"), "x"), PathBuf::from("/w/app.worktrees/x"));
        assert_eq!(git_message("fatal: 'x' already exists\n"), "'x' already exists");
    }
}
