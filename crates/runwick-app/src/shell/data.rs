//! Host data the shell reads (cached queries) and how projects, workspaces and sessions are
//! described to the person. Only observed state is described; unknown stays unknown.

use crate::App;
use crate::client::{HostKey, Query};
use crate::state::WsKey;
use crate::theme::Tone;
use runwick_core::Ts;
use runwick_core::api::projects::{Project, ProjectId, ProjectsList, Workspace, WorkspaceQuery, WorkspacesList};
use runwick_core::api::sessions::{
    AttentionKind, Phase, Session, SessionInfo, SessionLive, SessionQuery, SessionsList, Tool,
};
use runwick_core::api::tools::{ToolInfo, ToolsList, ToolsQuery};

pub fn projects(app: &mut App) -> Query<Vec<Project>> {
    app.hub.query::<ProjectsList>(HostKey::Local, &())
}

/// All non-archived workspaces of this computer's host.
pub fn workspaces(app: &mut App) -> Query<Vec<Workspace>> {
    app.hub.query::<WorkspacesList>(HostKey::Local, &WorkspaceQuery::default())
}

/// Every session a host knows (tabs closed included, archived excluded).
pub fn all_sessions(app: &mut App, host: HostKey) -> Query<Vec<SessionInfo>> {
    let q = SessionQuery { include_hidden: true, ..Default::default() };
    app.hub.query::<SessionsList>(host, &q)
}

pub fn sessions_in(app: &mut App, ws: WsKey) -> Query<Vec<SessionInfo>> {
    let q = SessionQuery { workspace_id: Some(ws.id), include_hidden: true, ..Default::default() };
    app.hub.query::<SessionsList>(ws.host, &q)
}

pub fn tools(app: &mut App, host: HostKey) -> Query<Vec<ToolInfo>> {
    app.hub.query::<ToolsList>(host, &ToolsQuery::default())
}

pub fn visible_projects(list: &[Project]) -> impl Iterator<Item = &Project> {
    list.iter().filter(|p| !p.archived)
}

/// The workspace key the GUI uses: remote workspaces are addressed on their own device.
pub fn ws_key(ws: &Workspace) -> WsKey {
    match &ws.remote {
        Some(r) => WsKey { host: HostKey::Device(r.device_id), id: r.remote_workspace_id },
        None => WsKey { host: HostKey::Local, id: ws.id },
    }
}

/// Find the local record of a workspace key (also for remote links).
pub fn find_ws<'a>(list: &'a [Workspace], key: &WsKey) -> Option<&'a Workspace> {
    list.iter().find(|w| ws_key(w) == *key)
}

pub fn project_of(list: &[Project], id: ProjectId) -> Option<&Project> {
    list.iter().find(|p| p.id == id)
}

/// Runtime state with the newest `session.live` the terminal views have seen applied.
pub fn live_of(app: &App, host: HostKey, info: &SessionInfo) -> Option<SessionLive> {
    crate::terminal::live(app, host, info.session.id).or_else(|| info.live.clone())
}

pub fn tool_mark(tool: Tool) -> &'static str {
    match tool {
        Tool::Claude => "Cl",
        Tool::Codex => "Cx",
        Tool::Shell => "$",
    }
}

/// How a session is doing, in words and tone.
#[derive(Debug, Clone, PartialEq)]
pub struct Status {
    pub tone: Tone,
    pub label: String,
    /// Observed activity: the dot breathes.
    pub pulse: bool,
    /// The process is alive.
    pub alive: bool,
}

impl Status {
    fn new(tone: Tone, label: impl Into<String>, pulse: bool, alive: bool) -> Self {
        Self { tone, label: label.into(), pulse, alive }
    }
}

pub fn attention_label(kind: AttentionKind) -> &'static str {
    match kind {
        AttentionKind::Approval => "승인 필요",
        AttentionKind::Auth => "인증·신뢰 확인",
        AttentionKind::Notify => "새 응답",
        AttentionKind::Choice => runwick_core::api::activity::CHOICE_NEEDED,
    }
}

pub fn status(s: &Session, live: Option<&SessionLive>) -> Status {
    status_at(s, live, runwick_core::now())
}

/// A run that ended because the host stopped (crash or restart), not by itself.
const INTERRUPTED: &str = "중단됨 · 실행 관리자 재시작";

fn status_at(s: &Session, live: Option<&SessionLive>, now: Ts) -> Status {
    let Some(l) = live else {
        return if s.interrupted {
            Status::new(Tone::Recover, INTERRUPTED, false, false)
        } else if s.ended_at.is_some() {
            Status::new(Tone::Waiting, "실행 종료", false, false)
        } else {
            Status::new(Tone::Waiting, "실행 안 함", false, false)
        };
    };
    match l.phase {
        Phase::Starting => Status::new(Tone::Running, "시작 중", true, true),
        Phase::Running => {
            // Waiting for input only when that is observed: the shell has the terminal back,
            // or the agent's input prompt is on screen. A silent command is still running.
            let waiting = if s.tool.is_agent() { l.input_prompt } else { !l.foreground_job };
            if let Some(a) = &l.attention
                && a.kind != AttentionKind::Notify
            {
                Status::new(Tone::Attention, attention_label(a.kind), false, true)
            } else if runwick_core::api::activity::has_new_response(l) {
                Status::new(Tone::Info, runwick_core::api::activity::fresh_label(s.tool), false, true)
            } else if l.busy {
                Status::new(Tone::Running, "실행 중", true, true)
            } else if waiting {
                Status::new(Tone::Neutral, "대기 중", false, true)
            } else {
                let quiet = l.last_output_at.map_or(0, |t| (now - t).num_minutes());
                let label = if quiet >= 1 {
                    format!("실행 중 · 출력 없음 {quiet}분")
                } else {
                    "실행 중 · 출력 없음".to_string()
                };
                Status::new(Tone::Running, label, false, true)
            }
        }
        Phase::Exited => match (&l.error, s.exit_code) {
            // Stopping did not end every process of the run: never shown as a clean end.
            _ if !l.leftovers.is_empty() => {
                Status::new(Tone::Recover, format!("남은 프로세스 {}개", l.leftovers.len()), false, false)
            }
            (Some(_), _) => Status::new(Tone::Recover, "시작 실패", false, false),
            (None, Some(code)) if code != 0 => {
                Status::new(Tone::Waiting, format!("실행 종료 · 코드 {code}"), false, false)
            }
            _ => Status::new(Tone::Waiting, "실행 종료", false, false),
        },
        Phase::NotRunning if s.interrupted => Status::new(Tone::Recover, INTERRUPTED, false, false),
        // A run that ended earlier reads the same whether or not its tab is open.
        Phase::NotRunning if s.ended_at.is_some() => match s.exit_code {
            Some(code) if code != 0 => Status::new(Tone::Waiting, format!("실행 종료 · 코드 {code}"), false, false),
            _ => Status::new(Tone::Waiting, "실행 종료", false, false),
        },
        Phase::NotRunning => Status::new(Tone::Waiting, "실행 안 함", false, false),
    }
}

/// Last time something happened in the session, for "N분 전".
pub fn last_activity(s: &Session, live: Option<&SessionLive>) -> Ts {
    live.and_then(|l| l.last_output_at).or(s.last_active_at).unwrap_or(s.created_at)
}

/// Short Korean relative time.
pub fn ago(ts: Ts) -> String {
    ago_from(ts, runwick_core::now())
}

pub fn ago_from(ts: Ts, now: Ts) -> String {
    let secs = (now - ts).num_seconds();
    match secs {
        s if s < 10 => "방금".to_string(),
        s if s < 60 => format!("{s}초 전"),
        s if s < 3600 => format!("{}분 전", s / 60),
        s if s < 86_400 => format!("{}시간 전", s / 3600),
        s if s < 2 * 86_400 => "어제".to_string(),
        s if s < 7 * 86_400 => format!("{}일 전", s / 86_400),
        _ => {
            let local = ts.with_timezone(&chrono::Local);
            local.format("%-m월 %-d일").to_string()
        }
    }
}

/// `~/work/x` instead of `/Users/me/work/x` (display only).
pub fn tilde(path: &str) -> String {
    if let Some(home) = std::env::var_os("HOME") {
        let home = home.to_string_lossy();
        if !home.is_empty() {
            if path == home {
                return "~".to_string();
            }
            if let Some(rest) = path.strip_prefix(&format!("{home}/")) {
                return format!("~/{rest}");
            }
        }
    }
    path.to_string()
}

/// Aggregate state of the sessions in one workspace for its sidebar dot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WsState {
    Attention,
    Recover,
    Running,
    Fresh,
    Idle,
    /// No live run.
    None,
    /// We cannot see this workspace's host.
    Unknown,
}

impl WsState {
    pub fn describe(self) -> (Tone, &'static str) {
        match self {
            WsState::Attention => (Tone::Attention, "확인 필요"),
            WsState::Recover => (Tone::Recover, "복구 필요"),
            WsState::Running => (Tone::Running, "실행 중"),
            WsState::Fresh => (Tone::Info, "새 응답"),
            WsState::Idle => (Tone::Neutral, "대기 중"),
            WsState::None => (Tone::Waiting, "실행 없음"),
            WsState::Unknown => (Tone::Waiting, "상태 확인 불가"),
        }
    }
}

pub fn ws_state<'a>(lives: impl Iterator<Item = Option<&'a SessionLive>>) -> WsState {
    let mut best = WsState::None;
    let rank = |s: WsState| match s {
        WsState::Attention => 6,
        WsState::Recover => 5,
        WsState::Running => 4,
        WsState::Fresh => 3,
        WsState::Idle => 2,
        WsState::None | WsState::Unknown => 0,
    };
    for l in lives.flatten() {
        let s = match l.phase {
            Phase::Running | Phase::Starting
                if l.attention.as_ref().is_some_and(|a| a.kind != AttentionKind::Notify) =>
            {
                WsState::Attention
            }
            _ if runwick_core::api::activity::has_new_response(l) => WsState::Fresh,
            Phase::Starting => WsState::Running,
            Phase::Running if l.busy => WsState::Running,
            Phase::Running => WsState::Idle,
            Phase::Exited if l.error.is_some() => WsState::Recover,
            _ => WsState::None,
        };
        if rank(s) > rank(best) {
            best = s;
        }
    }
    best
}

/// Counts for the title bar: (needs an answer, running). `None` when not observed.
pub fn activity_counts(app: &mut App) -> Option<(usize, usize)> {
    // One grouping for the title bar and the activity list (views::activity).
    crate::views::activity::counts(app)
}

/// A CLI that is not on this computer: a known state with an install hint, shown the same way
/// everywhere (welcome, start, settings, devices) — these words, [`NOT_INSTALLED_ICON`] and
/// `Tone::Check`, never the "unknown" look.
pub const NOT_INSTALLED: &str = "설치 안 됨";
pub const NOT_INSTALLED_ICON: &str = crate::icons::DOWNLOAD;

/// Text for a tool on the start screens, from what `tools.list` observed.
pub struct ToolLine {
    pub text: String,
    pub tone: Tone,
    /// The tool can be started right now (or we could not tell, so let the host decide).
    pub startable: bool,
    /// Suggested help action ("로그인 방법", "설치 방법").
    pub help: Option<&'static str>,
}

pub fn tool_line(tool: Tool, info: Option<&ToolInfo>, for_start: bool) -> ToolLine {
    use runwick_core::api::tools::AuthState;
    let Some(t) = info else {
        return ToolLine { text: "상태 확인 불가".into(), tone: Tone::Waiting, startable: true, help: None };
    };
    if !t.installed {
        return ToolLine {
            text: NOT_INSTALLED.into(), tone: Tone::Check, startable: false, help: Some("설치 방법")
        };
    }
    if tool == Tool::Shell {
        let name = shell_name(t);
        return if for_start {
            ToolLine {
                text: format!("{name} · 대화 기록 없음"), tone: Tone::Neutral, startable: true, help: None
            }
        } else {
            ToolLine { text: "사용 가능".into(), tone: Tone::Running, startable: true, help: None }
        };
    }
    match (t.auth, for_start) {
        (AuthState::LoggedIn, true) => {
            ToolLine { text: "바로 시작할 수 있어요".into(), tone: Tone::Running, startable: true, help: None }
        }
        (AuthState::LoggedIn, false) => {
            ToolLine { text: "설치됨 · 인증 완료".into(), tone: Tone::Running, startable: true, help: None }
        }
        (AuthState::NeedsLogin, true) => {
            ToolLine {
                text: "CLI의 로그인 화면이 열려요".into(), tone: Tone::Attention, startable: true, help: None
            }
        }
        (AuthState::NeedsLogin, false) => ToolLine {
            text: "설치됨 · 로그인 필요".into(),
            tone: Tone::Attention,
            startable: true,
            help: Some("로그인 방법"),
        },
        (AuthState::Unknown, _) => {
            ToolLine {
                text: "설치됨 · 로그인 상태 확인 불가".into(), tone: Tone::Neutral, startable: true, help: None
            }
        }
    }
}

impl ToolLine {
    /// Icon next to the words (tool lines use `Tone::Check` only for "not installed").
    pub fn glyph(&self) -> &'static str {
        match self.tone {
            Tone::Running => crate::icons::CHECK,
            Tone::Attention => crate::icons::CIRCLE_ALERT,
            Tone::Check => NOT_INSTALLED_ICON,
            _ => crate::icons::CIRCLE_HELP,
        }
    }
}

/// Each tool's line for a host, covering loading, failure and an unreachable device.
pub fn tool_lines(app: &mut App, host: HostKey, for_start: bool) -> Vec<(Tool, ToolLine, Option<ToolInfo>)> {
    let reachable = app.hub.state(host).is_connected();
    let q = tools(app, host);
    let waiting =
        |text: &str, startable: bool| ToolLine { text: text.into(), tone: Tone::Waiting, startable, help: None };
    Tool::ALL
        .into_iter()
        .map(|tool| {
            let info = q.data.as_ref().and_then(|l| l.iter().find(|t| t.tool == tool)).cloned();
            let line = match (&q.data, &q.error) {
                (Some(_), _) => tool_line(tool, info.as_ref(), for_start),
                (None, _) if !reachable && host != HostKey::Local => waiting("기기에 연결되어 있지 않음", false),
                (None, Some(_)) => waiting("상태 확인 불가", true),
                (None, None) => waiting("상태 확인 중…", true),
            };
            (tool, line, info)
        })
        .collect()
}

pub fn shell_name(t: &ToolInfo) -> String {
    t.path
        .as_deref()
        .and_then(|p| std::path::Path::new(p).file_name())
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "셸".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    #[test]
    fn relative_time_reads_naturally() {
        let now = runwick_core::now();
        assert_eq!(ago_from(now, now), "방금");
        assert_eq!(ago_from(now - Duration::seconds(12), now), "12초 전");
        assert_eq!(ago_from(now - Duration::minutes(5), now), "5분 전");
        assert_eq!(ago_from(now - Duration::hours(3), now), "3시간 전");
        assert_eq!(ago_from(now - Duration::hours(30), now), "어제");
        assert_eq!(ago_from(now - Duration::days(3), now), "3일 전");
    }

    fn live(phase: Phase, busy: bool) -> SessionLive {
        // Built from JSON so fields added to the contract later keep their defaults here.
        serde_json::from_value(serde_json::json!({
            "id": uuid::Uuid::new_v4(), "phase": phase, "busy": busy, "attention": null,
            "last_output_at": null, "cols": 80, "rows": 24, "pid": null, "input_owner": null, "unread": false,
        }))
        .expect("valid SessionLive")
    }

    fn session(tool: &str, interrupted: bool) -> Session {
        serde_json::from_value(serde_json::json!({
            "id": uuid::Uuid::nil(), "project_id": uuid::Uuid::nil(), "workspace_id": uuid::Uuid::nil(),
            "title": "t", "title_user_set": false, "tool": tool, "provider_conv_id": null,
            "parent_id": null, "role": null, "cwd": "/", "created_at": "2026-01-01T00:00:00Z",
            "last_active_at": null, "ended_at": null, "exit_code": null, "hidden": false, "archived": false,
            "env_version": 0, "interrupted": interrupted
        }))
        .expect("valid Session")
    }

    #[test]
    fn silent_commands_and_interruptions_are_named_for_what_they_are() {
        let now = runwick_core::now();
        let sh = session("shell", false);
        let mut quiet = live(Phase::Running, false);
        assert_eq!(status_at(&sh, Some(&quiet), now).label, "대기 중");
        // A job holds the terminal without printing: still a run, not a prompt.
        quiet.foreground_job = true;
        quiet.last_output_at = Some(now - Duration::minutes(3));
        let st = status_at(&sh, Some(&quiet), now);
        assert_eq!((st.label.as_str(), st.tone, st.alive), ("실행 중 · 출력 없음 3분", Tone::Running, true));
        // Agents wait for input only when their prompt is seen.
        let claude = session("claude", false);
        let mut agent = live(Phase::Running, false);
        assert_eq!(status_at(&claude, Some(&agent), now).label, "실행 중 · 출력 없음");
        agent.input_prompt = true;
        assert_eq!(status_at(&claude, Some(&agent), now).label, "대기 중");
        // Ended by a host restart or crash, and a stop that left processes behind.
        let st = status_at(&session("claude", true), None, now);
        assert_eq!((st.tone, st.label.as_str()), (Tone::Recover, INTERRUPTED));
        let mut ended = live(Phase::Exited, false);
        ended.leftovers = vec!["sleep (pid 7)".into()];
        let st = status_at(&sh, Some(&ended), now);
        assert_eq!((st.tone, st.label.as_str()), (Tone::Recover, "남은 프로세스 1개"));
    }

    #[test]
    fn unread_shell_state_is_output_and_agent_state_is_response() {
        let mut l = live(Phase::Running, false);
        l.unread = true;
        assert_eq!(status(&session("shell", false), Some(&l)).label, "새 출력");
        assert_eq!(status(&session("claude", false), Some(&l)).label, "새 응답");
        l.unread = false;
        l.attention = Some(runwick_core::api::sessions::Attention {
            kind: AttentionKind::Notify,
            text: "bell".into(),
            since: runwick_core::now(),
        });
        assert_eq!(status(&session("shell", false), Some(&l)).label, "새 출력");
    }

    #[test]
    fn workspace_state_prefers_what_needs_the_person() {
        let run = live(Phase::Running, true);
        let mut ask = live(Phase::Running, false);
        ask.attention = Some(runwick_core::api::sessions::Attention {
            kind: AttentionKind::Approval,
            text: "Allow?".into(),
            since: runwick_core::now(),
        });
        assert_eq!(ws_state([Some(&run), None].into_iter()), WsState::Running);
        assert_eq!(ws_state([Some(&run), Some(&ask)].into_iter()), WsState::Attention);
        assert_eq!(ws_state(std::iter::empty()), WsState::None);
    }
}
