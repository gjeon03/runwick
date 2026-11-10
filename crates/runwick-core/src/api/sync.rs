//! GitHub backup and sync between computers (S14).
//!
//! One private git repository holds a Runwick data folder (format [`FORMAT`] v[`FORMAT_VERSION`]):
//! one file per record, written deterministically, so syncing never produces textual git
//! conflicts. The host keeps a working copy under `RUNWICK_HOME/sync` and uses the person's own
//! git credentials; Runwick stores no tokens.
//!
//! A sync fetches, imports what other computers changed since the last sync into this host's
//! store, writes the merged state back, commits and pushes. Records keep their ids, so
//! importing the same data again never duplicates anything.

use crate::Ts;
use crate::api::projects::ProjectId;
use crate::method;
use crate::rpc::topic;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// `manifest.json` `format` value of a Runwick data repository.
pub const FORMAT: &str = "runwick-sync";
pub const FORMAT_VERSION: u32 = 1;
/// Suggested name for a new repository.
pub const DEFAULT_REPO_NAME: &str = "runwick-data";
pub const DEFAULT_BRANCH: &str = "main";

/// Kinds of records that travel between computers.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum SyncKind {
    Projects,
    Sessions,
    Messages,
    Pins,
    Wiki,
    Timeline,
    Roles,
    Env,
    Prefs,
    Collab,
}

impl SyncKind {
    pub const ALL: [SyncKind; 10] = [
        SyncKind::Projects,
        SyncKind::Sessions,
        SyncKind::Messages,
        SyncKind::Pins,
        SyncKind::Wiki,
        SyncKind::Timeline,
        SyncKind::Roles,
        SyncKind::Env,
        SyncKind::Prefs,
        SyncKind::Collab,
    ];

    pub fn label(self) -> &'static str {
        match self {
            SyncKind::Projects => "프로젝트",
            SyncKind::Sessions => "세션",
            SyncKind::Messages => "대화",
            SyncKind::Pins => "핀",
            SyncKind::Wiki => "위키 문서",
            SyncKind::Timeline => "타임라인 메모·요약",
            SyncKind::Roles => "역할",
            SyncKind::Env => "공통 환경변수",
            SyncKind::Prefs => "일반 설정",
            SyncKind::Collab => "협업 기록",
        }
    }

    /// What exactly travels, for the include list.
    pub fn detail(self) -> &'static str {
        match self {
            SyncKind::Projects => "이름 · 순서 · 그룹 · 즐겨찾기 · 보관 · 저장소 주소 · 기기별 폴더 위치",
            SyncKind::Sessions => "제목 · 도구 · 시각 · 만든 기기 (다른 기기에서는 읽기 전용)",
            SyncKind::Messages => "수집한 대화 원문 (만든 기기만 씀)",
            SyncKind::Pins => "고정한 내용과 메모",
            SyncKind::Wiki => "현재 문서 (두 기기에서 고치면 다른 쪽 변경은 제안으로)",
            SyncKind::Timeline => "프로젝트 메모 · 남은 일 · 날짜별 요약",
            SyncKind::Roles => "지시문 · 선호 도구 · 절차",
            SyncKind::Env => "이름 · 설명 · 비밀 여부 · 비밀이 아닌 값",
            SyncKind::Prefs => "터미널 글자 크기 · 움직임 줄이기 · Option 키 · 중지 확인",
            SyncKind::Collab => "끝난 맡긴 일과 결과",
        }
    }
}

/// What never leaves this computer through sync, for the exclude list.
pub const NOT_SYNCED: &[&str] = &[
    "비밀값 (보안 저장소에 그대로)",
    "CLI 원본 대화 파일",
    "터미널 화면 기록",
    "검색 색인 · 캐시",
    "자동 작업 실행 상태",
    "실행 상태 파일",
    "이 컴퓨터에서 분리한 작업 폴더",
    "창 배치",
];

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct KindCount {
    pub kind: SyncKind,
    pub count: u64,
}

/// One step of a sync, in order.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SyncStep {
    /// Read the repository (fetch).
    Fetch,
    /// Bring other computers' changes into this computer's records.
    Import,
    /// Write this computer's records into the working copy.
    Export,
    Commit,
    Push,
}

impl SyncStep {
    pub const ALL: [SyncStep; 5] =
        [SyncStep::Fetch, SyncStep::Import, SyncStep::Export, SyncStep::Commit, SyncStep::Push];

    pub fn label(self) -> &'static str {
        match self {
            SyncStep::Fetch => "저장소 읽기",
            SyncStep::Import => "다른 컴퓨터 변경 반영",
            SyncStep::Export => "이 컴퓨터 기록 쓰기",
            SyncStep::Commit => "커밋",
            SyncStep::Push => "올리기",
        }
    }
}

/// Why a repository operation failed, so the screen can say what to do.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FailKind {
    /// Git could not authenticate (SSH key, credential helper, `gh auth`).
    Auth,
    /// The repository does not exist or this account cannot see it.
    NotFound,
    /// The network or the server could not be reached.
    Network,
    /// The repository holds something other than Runwick data, or a newer format.
    Format,
    /// Another sync is running.
    Busy,
    /// Anything else (git error, local disk, records).
    Other,
}

impl FailKind {
    pub fn label(self) -> &'static str {
        match self {
            FailKind::Auth => "인증 필요",
            FailKind::NotFound => "저장소 없음",
            FailKind::Network => "네트워크",
            FailKind::Format => "형식 다름",
            FailKind::Busy => "이미 동기화 중",
            FailKind::Other => "오류",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SyncFailure {
    pub kind: FailKind,
    #[serde(default)]
    pub step: Option<SyncStep>,
    /// Korean explanation; git's own message is appended when it helps.
    pub message: String,
    pub at: Ts,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RepoRef {
    /// URL as given (SSH or HTTPS); used with the person's git credentials.
    pub url: String,
    pub branch: String,
    /// Short name for display, e.g. `gjeon03/runwick-data`.
    pub display: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct AutoSync {
    pub enabled: bool,
    /// Sync after changes at most this often.
    pub interval_min: u32,
}

impl Default for AutoSync {
    fn default() -> Self {
        Self { enabled: false, interval_min: 10 }
    }
}

pub const AUTO_INTERVALS: [u32; 4] = [5, 10, 30, 60];

/// A record kept on both sides that changed on both since the last sync, and what was kept.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SyncConflict {
    pub kind: SyncKind,
    pub title: String,
    pub resolution: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SyncReport {
    pub finished_at: Ts,
    /// Records brought in from other computers.
    pub imported: Vec<KindCount>,
    /// Records this computer wrote into the repository.
    pub exported: Vec<KindCount>,
    pub conflicts: Vec<SyncConflict>,
    /// Repository files that could not be read; they are left untouched.
    #[serde(default)]
    pub skipped: Vec<String>,
    /// Short id of the commit that is now on the remote.
    #[serde(default)]
    pub commit: Option<String>,
    /// Push attempts (more than one after another computer pushed at the same time).
    pub attempts: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SyncStatus {
    pub host_id: Uuid,
    pub device_name: String,
    /// None until a repository is connected.
    pub repo: Option<RepoRef>,
    #[serde(default)]
    pub connected_at: Option<Ts>,
    /// The step running now.
    #[serde(default)]
    pub running: Option<SyncStep>,
    #[serde(default)]
    pub last_success: Option<Ts>,
    #[serde(default)]
    pub last_attempt: Option<Ts>,
    /// Failure of the latest attempt (cleared by a successful sync).
    #[serde(default)]
    pub last_error: Option<SyncFailure>,
    /// Local records changed since the last successful sync. None when unknown.
    #[serde(default)]
    pub pending: Option<bool>,
    /// Commits only here / only on the remote, as of the last fetch. None when unknown.
    #[serde(default)]
    pub ahead: Option<u32>,
    #[serde(default)]
    pub behind: Option<u32>,
    pub auto: AutoSync,
    #[serde(default)]
    pub last_report: Option<SyncReport>,
}

/// Whether `gh` (GitHub CLI) can list and create repositories for the person.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GhStatus {
    pub installed: bool,
    pub logged_in: bool,
    #[serde(default)]
    pub account: Option<String>,
    /// Why it is not usable, in Korean.
    #[serde(default)]
    pub problem: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GhRepo {
    /// `owner/name`
    pub name: String,
    /// Clone URL matching the person's git protocol setting (SSH or HTTPS).
    pub url: String,
    pub private: bool,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub updated_at: Option<Ts>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RepoUrl {
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct NewRepo {
    pub name: String,
}

/// What a repository holds.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RepoContent {
    /// No commits yet: this computer writes the first data.
    Empty,
    /// A Runwick data folder this version can read.
    Runwick,
    /// Files that are not Runwick data (Runwick will not write there).
    Other,
    /// Runwick data in a newer format (update Runwick first).
    Newer,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RepoDevice {
    pub host_id: Uuid,
    pub name: String,
    #[serde(default)]
    pub last_sync: Option<Ts>,
    /// This computer.
    pub this: bool,
}

/// What connecting to (or restoring from) a repository would do. Nothing is imported yet;
/// the repository was cloned into a separate folder and read.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ConnectPreview {
    /// Token for [`SyncConnect`].
    pub id: Uuid,
    pub repo: RepoRef,
    pub content: RepoContent,
    #[serde(default)]
    pub devices: Vec<RepoDevice>,
    /// Records in the repository per kind.
    pub in_repo: Vec<KindCount>,
    /// Of those, records this computer does not have yet.
    pub new_here: Vec<KindCount>,
    /// Projects in the repository that have no folder on this computer yet.
    #[serde(default)]
    pub unlinked_projects: Vec<String>,
    /// Problems found while reading (unreadable files are listed and left alone).
    #[serde(default)]
    pub problems: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PreviewId {
    pub id: Uuid,
}

/// One sync commit in the data repository.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SyncCommit {
    pub id: String,
    pub at: Ts,
    /// Computer that made it (commit author name).
    pub device: String,
    pub summary: String,
}

/// A project whose records came from another computer and has no folder here yet.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UnlinkedProject {
    pub id: ProjectId,
    pub name: String,
    #[serde(default)]
    pub remote_url: Option<String>,
    /// Where it lives on other computers: "MacBook Air · ~/code/app".
    #[serde(default)]
    pub elsewhere: Vec<String>,
}

// Also refetched when synced records change (the "올리지 않은 변경" state).
method!(SyncStatusGet, "sync.status", () => SyncStatus, [
    topic::SYNC,
    topic::PROJECTS,
    topic::SESSIONS,
    topic::MESSAGES,
    topic::PINS,
    topic::WIKI,
    topic::TIMELINE,
    topic::ENV,
    topic::SETTINGS,
    topic::COLLAB,
]);
method!(SyncGhStatus, "sync.gh_status", () => GhStatus);
method!(SyncGhRepos, "sync.gh_repos", () => Vec<GhRepo>);
// Creates a private repository on the person's GitHub account with `gh repo create --private`.
method!(SyncCreateRepo, "sync.create_repo", NewRepo => GhRepo);
// Clone into a separate folder, check access and format, count what would come.
method!(SyncPreview, "sync.preview", RepoUrl => ConnectPreview);
method!(SyncDiscardPreview, "sync.discard_preview", PreviewId => ());
// Use the previewed repository from now on and run the first sync (a union: nothing is deleted).
method!(SyncConnect, "sync.connect", PreviewId => SyncReport);
method!(SyncRun, "sync.run", () => SyncReport);
method!(SyncSetAuto, "sync.set_auto", AutoSync => SyncStatus);
method!(SyncHistory, "sync.history", () => Vec<SyncCommit>, [topic::SYNC]);
// Stop syncing. Local records and the remote repository stay as they are.
method!(SyncDisconnect, "sync.disconnect", () => ());
method!(SyncUnlinked, "sync.unlinked", () => Vec<UnlinkedProject>, [topic::PROJECTS, topic::SYNC]);

/// A record changed on two computers where one side's content was not kept. Shown under
/// "겹친 변경" on every computer, with that content and a way to put it back.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SyncConflictItem {
    pub id: Uuid,
    pub kind: SyncKind,
    pub title: String,
    pub at: Ts,
    /// Computer that found and resolved it.
    pub found_on: String,
    pub kept_device: String,
    pub lost_device: String,
    /// The content that was not kept was this computer's (only for conflicts this computer
    /// found itself; a record from the repository cannot claim it).
    pub lost_here: bool,
    /// This computer found it (otherwise another computer's record from the repository).
    #[serde(default)]
    pub found_here: bool,
    /// The content that was not kept, readable.
    pub lost_text: String,
    pub can_restore: bool,
    /// For wiki pages (open the page and its history) and project notes.
    #[serde(default)]
    pub project_id: Option<ProjectId>,
    #[serde(default)]
    pub slug: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ConflictRef {
    pub id: Uuid,
}

/// A change to a global variable that came from another computer: a new or different value, or
/// a removal. It is not used on this computer until the person accepts it (a key=value accepted
/// once is applied directly the next time).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StagedVar {
    pub key: String,
    /// The value the other computer has (empty for a removal).
    pub value: String,
    #[serde(default)]
    pub description: String,
    /// Computer it came from.
    pub device: String,
    pub at: Ts,
    /// The person chose not to use it on this computer.
    pub rejected: bool,
    /// The other computer removed the variable.
    #[serde(default)]
    pub remove: bool,
    /// The variable changes how programs start or where they connect ([`runs_programs`]).
    #[serde(default)]
    pub dangerous: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StagedDecision {
    pub key: String,
    pub accept: bool,
}

/// Variables that change which programs run, how they start or where they connect. Every value
/// from another computer waits for review; these get a stronger warning there.
pub fn runs_programs(key: &str) -> bool {
    const EXACT: &[&str] = &[
        "NODE_OPTIONS",
        "NODE_PATH",
        "BASH_ENV",
        "ENV",
        "ZDOTDIR",
        "PROMPT_COMMAND",
        "PERL5OPT",
        "PERL5LIB",
        "PERLLIB",
        "RUBYOPT",
        "RUBYLIB",
        "EDITOR",
        "VISUAL",
        "PAGER",
        "BROWSER",
        "LESSOPEN",
        "LESSCLOSE",
        "JAVA_TOOL_OPTIONS",
        "_JAVA_OPTIONS",
        "SHELLOPTS",
        "BASHOPTS",
        "PS4",
        "IFS",
        "GCONV_PATH",
        "DOTNET_STARTUP_HOOKS",
        "SSL_CERT_FILE",
        "SSL_CERT_DIR",
        "REQUESTS_CA_BUNDLE",
        "CURL_CA_BUNDLE",
        "NODE_EXTRA_CA_CERTS",
        "NO_PROXY",
    ];
    const PREFIXES: &[&str] = &[
        "LD_",
        "DYLD_",
        "GIT_",
        "NPM_CONFIG_",
        "PYTHON",
        "CARGO_",
        "RUSTC_",
        "RUSTUP_",
        "PIP_",
        "XDG_",
        "JAVA_",
        "DOTNET_",
    ];
    const SUFFIXES: &[&str] =
        &["_ASKPASS", "_COMMAND", "_PROXY", "_BASE_URL", "_WRAPPER", "_CA_CERTS", "_CA_BUNDLE", "_ENDPOINT"];
    let k = key.to_ascii_uppercase();
    EXACT.contains(&k.as_str()) || PREFIXES.iter().any(|p| k.starts_with(p)) || SUFFIXES.iter().any(|x| k.ends_with(x))
}

method!(SyncConflicts, "sync.conflicts", () => Vec<SyncConflictItem>, [topic::SYNC]);
method!(SyncConflictDismiss, "sync.conflict_dismiss", ConflictRef => ());
// Put the content that was not kept back, as a new edit on this computer.
method!(SyncConflictRestore, "sync.conflict_restore", ConflictRef => ());
// Variables whose plain value is in the repository history (changing the value does not remove it).
method!(SyncExposedKeys, "sync.exposed_keys", () => Vec<String>, [topic::SYNC, topic::ENV]);
method!(SyncStagedVars, "sync.staged_vars", () => Vec<StagedVar>, [topic::SYNC, topic::ENV]);
method!(SyncDecideStaged, "sync.decide_staged", StagedDecision => ());

/// Short display name of a repository URL: `owner/name` for GitHub-like URLs.
pub fn display_name(url: &str) -> String {
    let u = url.trim().trim_end_matches('/');
    let u = u.strip_suffix(".git").unwrap_or(u);
    let tail = match u.rsplit_once(':') {
        // scp-like `git@host:owner/name`
        Some((head, rest)) if !head.contains("//") => rest,
        _ => u.split_once("://").map(|(_, r)| r.split_once('/').map(|(_, p)| p).unwrap_or(r)).unwrap_or(u),
    };
    let parts: Vec<&str> = tail.split('/').filter(|s| !s.is_empty()).collect();
    match parts.as_slice() {
        [.., owner, name] => format!("{owner}/{name}"),
        [name] => name.to_string(),
        [] => url.to_string(),
    }
}

/// A remote URL safe to save or show: for http(s) the user part (which can hold a token) is
/// removed; for other schemes only a password is.
pub fn without_credentials(url: &str) -> String {
    let u = url.trim();
    let Some((scheme, rest)) = u.split_once("://") else {
        return u.to_string();
    };
    let (authority, path) = rest.split_once('/').map(|(a, p)| (a, format!("/{p}"))).unwrap_or((rest, String::new()));
    let Some((userinfo, host)) = authority.rsplit_once('@') else {
        return u.to_string();
    };
    let http = scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https");
    match (http, userinfo.split_once(':')) {
        (true, _) => format!("{scheme}://{host}{path}"),
        (false, Some((user, _))) => format!("{scheme}://{user}@{host}{path}"),
        (false, None) => u.to_string(),
    }
}

/// Normalized form of a git remote URL for matching projects across computers
/// (`git@github.com:me/app.git` and `https://github.com/me/app` are the same).
pub fn normalize_remote(url: &str) -> String {
    let u = url.trim().trim_end_matches('/');
    let u = u.strip_suffix(".git").unwrap_or(u);
    let (host, path) = if let Some((_, rest)) = u.split_once("://") {
        let rest = rest.rsplit_once('@').map(|(_, r)| r).unwrap_or(rest);
        rest.split_once('/').unwrap_or((rest, ""))
    } else if let Some((head, path)) = u.split_once(':') {
        (head.rsplit_once('@').map(|(_, h)| h).unwrap_or(head), path)
    } else {
        ("", u)
    };
    let host = host.split(':').next().unwrap_or(host);
    format!("{}/{}", host.to_lowercase(), path.trim_matches('/').to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_names_and_remote_matching() {
        assert_eq!(display_name("git@github.com:gjeon03/runwick-data.git"), "gjeon03/runwick-data");
        assert_eq!(display_name("https://github.com/gjeon03/runwick-data"), "gjeon03/runwick-data");
        assert_eq!(display_name("/tmp/x/remote.git"), "x/remote");
        let a = normalize_remote("git@github.com:Me/App.git");
        assert_eq!(a, normalize_remote("https://github.com/me/app/"));
        assert_eq!(a, normalize_remote("ssh://git@github.com/me/app.git"));
        assert_ne!(a, normalize_remote("git@github.com:me/other.git"));
        assert_eq!(without_credentials("https://me:ghp_x@github.com/me/app.git"), "https://github.com/me/app.git");
        assert_eq!(without_credentials("https://ghp_x@github.com/me/app"), "https://github.com/me/app");
        assert_eq!(without_credentials("ssh://git:pw@host/x.git"), "ssh://git@host/x.git");
        assert_eq!(without_credentials("git@github.com:me/app.git"), "git@github.com:me/app.git");
    }

    #[test]
    fn variables_that_run_programs_are_recognized() {
        for k in [
            "NODE_OPTIONS",
            "GIT_SSH_COMMAND",
            "SSH_ASKPASS",
            "DYLD_INSERT_LIBRARIES",
            "LD_PRELOAD",
            "GIT_CONFIG_COUNT",
            "BASH_ENV",
            "prompt_command",
        ] {
            assert!(runs_programs(k), "{k}");
        }
        for k in ["ANTHROPIC_BASE_URL", "HTTPS_PROXY", "RUSTC_WRAPPER", "GIT_AUTHOR_NAME"] {
            assert!(runs_programs(k), "{k}");
        }
        for k in ["API_BASE", "GITHUB_TOKEN", "LANG", "NODE_ENV"] {
            assert!(!runs_programs(k), "{k}");
        }
    }
}
