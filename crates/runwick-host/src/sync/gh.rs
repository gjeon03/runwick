//! GitHub CLI (`gh`) for listing and creating the person's repositories. Git itself never
//! needs it: sync uses whatever git credentials the person has.

use super::git::{GitEnv, GitError, classify, run};
use runwick_core::api::sync::{GhRepo, GhStatus};
use serde::Deserialize;
use std::time::Duration;

const LIMIT: Duration = Duration::from_secs(30);

pub fn status(env: &GitEnv) -> GhStatus {
    let Some(gh) = env.find("gh") else {
        return GhStatus {
            installed: false,
            logged_in: false,
            account: None,
            problem: Some("GitHub CLI(gh)가 설치되어 있지 않아요. 저장소 주소를 직접 붙여 넣을 수 있어요".into()),
        };
    };
    match run(env, &gh, None, &["auth", "status", "--hostname", "github.com"], None, LIMIT) {
        Ok(out) if out.ok => {
            let text = format!("{}\n{}", String::from_utf8_lossy(&out.stdout), out.stderr);
            let account = text
                .lines()
                .find_map(|l| {
                    l.split_once(" account ").map(|(_, rest)| rest.split_whitespace().next().unwrap_or("").to_string())
                })
                .filter(|a| !a.is_empty());
            GhStatus { installed: true, logged_in: true, account, problem: None }
        }
        Ok(_) => GhStatus {
            installed: true,
            logged_in: false,
            account: None,
            problem: Some(
                "GitHub CLI에 로그인되어 있지 않아요. 터미널에서 `gh auth login`을 실행하거나 주소를 붙여 넣어 주세요"
                    .into(),
            ),
        },
        Err(e) => GhStatus { installed: true, logged_in: false, account: None, problem: Some(e.message) },
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Listed {
    name_with_owner: String,
    url: String,
    ssh_url: String,
    is_private: bool,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    updated_at: Option<String>,
}

fn prefers_ssh(env: &GitEnv, gh: &std::path::Path) -> bool {
    run(env, gh, None, &["config", "get", "git_protocol", "--host", "github.com"], None, LIMIT)
        .map(|o| o.ok && o.text().trim() == "ssh")
        .unwrap_or(false)
}

fn to_repo(l: Listed, ssh: bool) -> GhRepo {
    GhRepo {
        url: if ssh { l.ssh_url } else { format!("{}.git", l.url.trim_end_matches('/')) },
        name: l.name_with_owner,
        private: l.is_private,
        description: l.description.unwrap_or_default(),
        updated_at: l.updated_at.and_then(|s| super::format::parse_ts(&s)),
    }
}

const FIELDS: &str = "nameWithOwner,url,sshUrl,isPrivate,description,updatedAt";

fn gh_path(env: &GitEnv) -> Result<std::path::PathBuf, GitError> {
    env.find("gh").ok_or_else(|| GitError::other("GitHub CLI(gh)가 설치되어 있지 않아요"))
}

/// The person's repositories, most recently updated first.
pub fn repos(env: &GitEnv) -> Result<Vec<GhRepo>, GitError> {
    let gh = gh_path(env)?;
    let out = run(env, &gh, None, &["repo", "list", "--limit", "200", "--json", FIELDS], None, LIMIT)?;
    if !out.ok {
        return Err(classify(&out.stderr));
    }
    let list: Vec<Listed> =
        serde_json::from_slice(&out.stdout).map_err(|_| GitError::other("GitHub CLI 응답을 읽지 못했어요"))?;
    let ssh = prefers_ssh(env, &gh);
    let mut repos: Vec<GhRepo> = list.into_iter().map(|l| to_repo(l, ssh)).collect();
    repos.sort_by_key(|r| std::cmp::Reverse(r.updated_at));
    Ok(repos)
}

/// A repository name GitHub accepts.
pub fn check_name(name: &str) -> Result<String, GitError> {
    let n = name.trim();
    let ok = !n.is_empty()
        && n.len() <= 100
        && !n.starts_with(['-', '.'])
        && n.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if ok {
        Ok(n.to_string())
    } else {
        Err(GitError::other("저장소 이름은 영문, 숫자, -, _, . 로만 지어 주세요"))
    }
}

/// Create a private repository on the person's account.
pub fn create(env: &GitEnv, name: &str) -> Result<GhRepo, GitError> {
    let name = check_name(name)?;
    let gh = gh_path(env)?;
    let out = run(
        env,
        &gh,
        None,
        &["repo", "create", &name, "--private", "--description", "Runwick 기록 동기화 (자동 관리)"],
        None,
        LIMIT,
    )?;
    if !out.ok {
        let lower = out.stderr.to_lowercase();
        if lower.contains("already exists") {
            return Err(GitError::other(format!("같은 이름의 저장소가 이미 있어요: {name}. 목록에서 골라 주세요")));
        }
        return Err(classify(&out.stderr));
    }
    let view = run(env, &gh, None, &["repo", "view", &name, "--json", FIELDS], None, LIMIT)?;
    if !view.ok {
        return Err(classify(&view.stderr));
    }
    let l: Listed =
        serde_json::from_slice(&view.stdout).map_err(|_| GitError::other("만든 저장소 정보를 읽지 못했어요"))?;
    if !l.is_private {
        return Err(GitError::other("저장소가 비공개로 만들어지지 않았어요. GitHub에서 공개 범위를 확인해 주세요"));
    }
    Ok(to_repo(l, prefers_ssh(env, &gh)))
}
