//! What the menu bar shows and when it steps aside. Pure functions over observed host state.
//!
//! Action-needed counts share the window's classification. Quiet output and worker results
//! remain visible without increasing that count; idle live sessions still keep runs alive.

use runwick_core::Ts;
use runwick_core::api::activity::{Group, RequestIndex, headline_counts, request_group, session_group};
use runwick_core::api::collab::{ReqStatus, Request};
use runwick_core::api::jobs::{Job, JobStatus};
use runwick_core::api::projects::{Project, ProjectId, Workspace, WorkspaceId};
use runwick_core::api::sessions::{Phase, SessionId, SessionInfo};
use std::collections::HashMap;

/// Rows listed in the menu; the rest are reachable through 전체 활동.
pub const MAX_ROWS: usize = 6;
const MAX_TITLE_CHARS: usize = 32;
const MAX_META_CHARS: usize = 44;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Counts {
    /// 확인할 일.
    pub needs: usize,
    /// 실행: live runs (alive sessions and running automatic jobs), the title bar's count.
    pub running: usize,
    /// Of those, alive sessions waiting for input with nothing else to show.
    pub idle: usize,
}

/// Where a row leads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Session(SessionId),
    /// Automatic jobs have no tab of their own; 전체 활동 lists them.
    Activity,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub target: Target,
    pub title: String,
    /// The same words the window uses for this item ("승인 필요", "응답 생성 중", "대기 중", …).
    pub state: String,
    /// Where it runs, as the window writes it: "payments-api / 기본 공간".
    pub place: String,
    /// With what: the CLI ("Claude Code", "Codex", "셸") or "자동 작업".
    pub tool: &'static str,
    /// `None`: alive but quiet (대기 중).
    pub group: Option<Group>,
    pub at: Option<Ts>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Snapshot {
    pub counts: Counts,
    /// Live processes the host keeps: alive sessions and running automatic jobs.
    pub alive: usize,
    /// Action needed, new responses, running work, then recent results; stable timestamps within.
    pub rows: Vec<Row>,
}

impl Snapshot {
    /// Nothing is left that continues without the window.
    pub fn nothing_runs(&self) -> bool {
        self.alive == 0 && self.counts.running == 0
    }
}

/// One look at the host.
#[derive(Debug)]
pub enum Poll {
    /// Nothing answers on the socket: the host is not running.
    Gone,
    /// The host answered but the state could not be read (shown as unknown, never as zero).
    Failed(String),
    /// The person turned the menu bar icon off (`menubar.enabled`).
    Disabled,
    Ok(Snapshot),
}

/// What the menu shows: `None` when the last look failed.
pub type View = Option<Snapshot>;

/// Project and workspace names by id, for the second line of a row.
#[derive(Debug, Default)]
pub struct Names {
    projects: HashMap<ProjectId, String>,
    workspaces: HashMap<WorkspaceId, String>,
}

impl Names {
    pub fn new(projects: &[Project], workspaces: &[Workspace]) -> Self {
        Self {
            projects: projects.iter().map(|p| (p.id, p.name.clone())).collect(),
            workspaces: workspaces.iter().map(|w| (w.id, w.name.clone())).collect(),
        }
    }

    /// "payments-api / 기본 공간"; a project that is not known is said so, never guessed.
    fn place(&self, project: ProjectId, workspace: WorkspaceId) -> String {
        let Some(p) = self.projects.get(&project) else {
            return "프로젝트 확인 불가".into();
        };
        match self.workspaces.get(&workspace) {
            Some(w) => format!("{} / {}", one_line(p), one_line(w)),
            None => one_line(p),
        }
    }
}

/// `requests` newest first (as `collab.list` returns them); `jobs` the active ones.
pub fn snapshot(sessions: &[SessionInfo], requests: &[Request], jobs: &[Job], names: &Names) -> Snapshot {
    let mut snap = Snapshot::default();
    // (group, alive) of everything counted, for the shared headline numbers.
    let mut heads: Vec<(Option<Group>, bool)> = Vec::new();
    let index = RequestIndex::new(requests);
    for s in sessions.iter().filter(|s| !s.session.archived) {
        let live = s.live.as_ref();
        let alive = live.is_some_and(|l| matches!(l.phase, Phase::Running | Phase::Starting));
        snap.alive += usize::from(alive);
        let latest = index.request(s.session.id);
        let group = index.group(&s.session, live);
        let latest = latest.filter(|r| request_group(r).is_some());
        if group.is_none() && !alive {
            continue;
        }
        heads.push((group, alive));
        let own = session_group(&s.session, live);
        let (title, mut state) = match (group, latest) {
            (None, _) => {
                snap.counts.idle += 1;
                let state = runwick_app::shell::data::status(&s.session, live).label;
                (s.session.title.clone(), state)
            }
            // A delegated run reads as its request, unless the run itself asks the person.
            (Some(_), Some(r)) => {
                let state = if own.is_some_and(|(g, _)| Some(g) == group && g.needs_person()) {
                    own.unwrap().1.to_string()
                } else if matches!(r.status, ReqStatus::Done | ReqStatus::Failed) && !r.acknowledged {
                    format!("새 결과 · {}", r.status.label())
                } else if own.is_some_and(|(g, _)| Some(g) == group) {
                    own.unwrap().1.to_string()
                } else {
                    r.status.label().to_string()
                };
                (r.title.clone(), state)
            }
            (Some(_), None) => (s.session.title.clone(), own.map_or("", |g| g.1).to_string()),
        };
        let results = index.worker_results(s.session.id);
        if results > 0 {
            state = if state.is_empty() {
                format!("작업자 결과 {results}")
            } else {
                format!("작업자 결과 {results} · {state}")
            };
        }
        let place = names.place(s.session.project_id, s.session.workspace_id);
        snap.rows.push(Row {
            target: Target::Session(s.session.id),
            title: one_line(&title),
            state,
            place,
            tool: s.session.tool.label(),
            group,
            at: live
                .and_then(|l| l.attention.as_ref().map(|a| a.since))
                .or_else(|| latest.map(|r| r.finished_at.unwrap_or(r.created_at)))
                .or(Some(s.session.created_at)),
        });
    }
    for j in jobs {
        let (group, state) = match j.status {
            JobStatus::Running => (Group::Running, "자동 · 실행 중".to_string()),
            JobStatus::Queued => (Group::Waiting, "자동 · 대기".to_string()),
            JobStatus::Held => {
                let why = j.held_reason.as_deref().unwrap_or("이유 확인 불가");
                (Group::Waiting, format!("보류 · {why}"))
            }
            _ => continue,
        };
        snap.alive += usize::from(j.status == JobStatus::Running);
        heads.push((Some(group), j.status == JobStatus::Running));
        let project = match &j.project_name {
            Some(name) => one_line(name),
            None => names.place(j.project_id, WorkspaceId::nil()),
        };
        snap.rows.push(Row {
            target: Target::Activity,
            title: j.kind.label().to_string(),
            state,
            place: project,
            tool: "자동 작업",
            group: Some(group),
            at: j.started_at.or(Some(j.created_at)),
        });
    }
    (snap.counts.needs, snap.counts.running) = headline_counts(heads);
    // `None` (quiet) sorts after every group.
    snap.rows.sort_by(|a, b| {
        let rank = |r: &Row| r.group.map_or(Group::Running, Group::section);
        rank(a).cmp(&rank(b)).then(a.at.cmp(&b.at)).then(a.title.cmp(&b.title))
    });
    snap
}

fn one_line(s: &str) -> String {
    let s: String = s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    let s = s.trim();
    if s.is_empty() { "이름 없는 세션".into() } else { s.into() }
}

/// Whether two views draw the same menu (times only order the rows), so a busy run does not
/// wake the menu bar on every look.
pub fn looks_same(a: &View, b: &View) -> bool {
    let shown = |v: &View| {
        v.as_ref().map(|s| {
            (
                s.counts,
                s.rows.len(),
                s.rows
                    .iter()
                    .take(MAX_ROWS)
                    .map(|r| (r.target, row_text(r), needs_person(r.group)))
                    .collect::<Vec<_>>(),
            )
        })
    };
    shown(a) == shown(b)
}

/// Whether a row's group needs the person (shown in the ember-equivalent color).
pub fn needs_person(group: Option<Group>) -> bool {
    group.is_some_and(Group::needs_person)
}

/// Text next to the menu bar icon. The leading count (확인할 일) is tinted by the tray.
pub fn title(view: &View) -> Option<String> {
    let c = view.as_ref()?.counts;
    match (c.needs, c.running) {
        (0, 0) => None,
        (0, r) => Some(r.to_string()),
        (n, r) => Some(format!("{n} · {r}")),
    }
}

/// Characters of [`title`] to tint: the 확인할 일 count, in UTF-16 units (AppKit ranges).
pub fn needs_span(view: &View) -> usize {
    match view {
        Some(s) if s.counts.needs > 0 => s.counts.needs.to_string().len(),
        _ => 0,
    }
}

pub fn tooltip(view: &View) -> String {
    match view {
        Some(s) => {
            let c = s.counts;
            let idle = if c.idle > 0 { format!(" (대기 중 {}개)", c.idle) } else { String::new() };
            format!("Runwick: 확인할 일 {}개, 실행 중 {}개{idle}", c.needs, c.running)
        }
        None => "Runwick: 실행 상태를 확인하지 못했어요".into(),
    }
}

pub fn status_line(view: &View) -> String {
    let Some(s) = view else {
        return "실행 상태를 확인하지 못했어요".into();
    };
    let c = s.counts;
    if c == Counts::default() {
        return "실행 중인 작업 없음".into();
    }
    let idle = if c.idle > 0 { format!(" (대기 중 {})", c.idle) } else { String::new() };
    format!("확인할 일 {} · 실행 중 {}{idle}", c.needs, c.running)
}

/// Separates a row's title from its state in the menu.
pub const ROW_SEPARATOR: &str = " — ";

/// A row as shown: "<title> — <state>", the title shortened to fit the menu.
pub fn row_label(row: &Row) -> String {
    format!("{}{ROW_SEPARATOR}{}", short_title(&row.title), row.state)
}

/// The row's second line: place and tool. A long place is shortened; the tool always shows.
pub fn row_meta(row: &Row) -> String {
    let room = MAX_META_CHARS.saturating_sub(row.tool.chars().count() + 3).max(8);
    format!("{} · {}", shorten(&row.place, room), row.tool)
}

/// Both lines of a row as shown in the menu.
pub fn row_text(row: &Row) -> String {
    format!("{}\n{}", row_label(row), row_meta(row))
}

pub fn short_title(title: &str) -> String {
    shorten(title, MAX_TITLE_CHARS)
}

fn shorten(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut: String = s.chars().take(max - 1).collect();
    format!("{}…", cut.trim_end())
}

pub fn more_label(hidden: usize) -> String {
    format!("외 {hidden}개 — 전체 활동에서 보기")
}

/// Why the menu bar process leaves. Written to its log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    WindowOpen,
    HostGone,
    NoRuns,
    Disabled,
}

impl Exit {
    pub fn reason(self) -> &'static str {
        match self {
            Exit::WindowOpen => "창이 열려 있어 메뉴 막대 아이콘을 닫아요",
            Exit::HostGone => "호스트가 실행 중이 아니라서 메뉴 막대 아이콘을 닫아요",
            Exit::NoRuns => "남은 실행이 없어 메뉴 막대 아이콘을 닫아요",
            Exit::Disabled => "설정에서 메뉴 막대 아이콘을 껐어요",
        }
    }
}

/// Consecutive looks that must agree before leaving, so one missed answer or the moment
/// between two runs does not close the icon.
const CONFIRM: u32 = 2;

/// Decides, look by look, what to show or whether to leave.
#[derive(Debug, Default)]
pub struct Watch {
    gone: u32,
    idle: u32,
}

impl Watch {
    /// The first look decides alone: never show an icon for nothing.
    pub fn first(poll: Poll, window_open: bool) -> Result<View, Exit> {
        let gone = matches!(poll, Poll::Gone);
        match Self::default().observe(poll, window_open)? {
            None if gone => Err(Exit::HostGone),
            Some(s) if s.nothing_runs() => Err(Exit::NoRuns),
            view => Ok(view),
        }
    }

    pub fn observe(&mut self, poll: Poll, window_open: bool) -> Result<View, Exit> {
        if window_open {
            return Err(Exit::WindowOpen);
        }
        match poll {
            Poll::Disabled => Err(Exit::Disabled),
            Poll::Gone => {
                self.gone += 1;
                if self.gone >= CONFIRM { Err(Exit::HostGone) } else { Ok(None) }
            }
            Poll::Failed(_) => {
                self.gone = 0;
                Ok(None)
            }
            Poll::Ok(s) => {
                self.gone = 0;
                if s.nothing_runs() {
                    self.idle += 1;
                    if self.idle >= CONFIRM {
                        return Err(Exit::NoRuns);
                    }
                } else {
                    self.idle = 0;
                }
                Ok(Some(s))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use runwick_core::api::sessions::{Attention, AttentionKind, Session, SessionLive, Tool};

    fn info(title: &str, phase: Phase, attention: bool, busy: bool) -> SessionInfo {
        let id = uuid::Uuid::new_v4();
        SessionInfo {
            session: Session {
                id,
                project_id: uuid::Uuid::nil(),
                workspace_id: uuid::Uuid::nil(),
                title: title.into(),
                title_user_set: false,
                tool: Tool::Claude,
                provider_conv_id: None,
                parent_id: None,
                role: None,
                cwd: "/".into(),
                created_at: runwick_core::now(),
                last_active_at: None,
                ended_at: None,
                exit_code: None,
                hidden: false,
                archived: false,
                env_version: 0,
                interrupted: false,
                origin_device: None,
            },
            live: Some(SessionLive {
                id,
                phase,
                busy,
                attention: attention.then(|| Attention {
                    kind: AttentionKind::Approval,
                    text: "Allow?".into(),
                    since: runwick_core::now(),
                }),
                last_output_at: None,
                cols: 80,
                rows: 24,
                pid: Some(1),
                input_owner: None,
                unread: false,
                error: None,
                error_kind: None,
                input_owner_client: None,
                foreground_job: false,
                input_prompt: false,
                leftovers: Vec::new(),
            }),
        }
    }

    fn snap(needs: usize, running: usize, idle: usize, alive: usize) -> Snapshot {
        Snapshot { counts: Counts { needs, running, idle }, alive, rows: Vec::new() }
    }

    #[test]
    fn counts_follow_the_title_bar_and_quiet_sessions_come_last() {
        let mut list = vec![
            info("로그인 오류 수정", Phase::Running, false, true),
            info("회귀 검토", Phase::Running, true, false),
            info("끝난 셸", Phase::Exited, false, false),
            info("대기 중인 셸", Phase::Running, false, false),
        ];
        // A shell back at its prompt (no foreground job) waits for input.
        list[3].session.tool = Tool::Shell;
        let s = snapshot(&list, &[], &[], &Names::default());
        assert_eq!(
            s.counts,
            Counts { needs: 1, running: 3, idle: 1 },
            "실행 counts every live run, the one waiting for approval and the idle one included"
        );
        assert_eq!(s.alive, 3);
        let labels: Vec<String> = s.rows.iter().map(row_label).collect();
        assert_eq!(labels, ["회귀 검토 — 승인 필요", "로그인 오류 수정 — 응답 생성 중", "대기 중인 셸 — 대기 중"]);
        assert!(needs_person(s.rows[0].group) && !needs_person(s.rows[1].group) && s.rows[2].group.is_none());
    }

    #[test]
    fn title_status_and_tooltip_text() {
        let v = Some(snap(1, 3, 0, 4));
        assert_eq!(title(&v).as_deref(), Some("1 · 3"));
        assert_eq!(needs_span(&v), 1);
        assert_eq!(status_line(&v), "확인할 일 1 · 실행 중 3");
        assert_eq!(tooltip(&v), "Runwick: 확인할 일 1개, 실행 중 3개");
        let v = Some(snap(0, 2, 1, 3));
        assert_eq!(title(&v).as_deref(), Some("2"));
        assert_eq!(needs_span(&v), 0);
        assert_eq!(status_line(&v), "확인할 일 0 · 실행 중 2 (대기 중 1)");
        assert_eq!(tooltip(&v), "Runwick: 확인할 일 0개, 실행 중 2개 (대기 중 1개)");
        let v = Some(snap(12, 30, 0, 30));
        assert_eq!((title(&v).as_deref(), needs_span(&v)), (Some("12 · 30"), 2));
        assert_eq!(title(&Some(snap(0, 2, 2, 2))).as_deref(), Some("2"), "idle live runs are runs");
        assert_eq!(title(&Some(snap(0, 0, 0, 0))), None);
        assert_eq!(status_line(&Some(snap(0, 0, 0, 0))), "실행 중인 작업 없음");
        assert_eq!(status_line(&None), "실행 상태를 확인하지 못했어요");
        assert_eq!(title(&None), None);
    }

    #[test]
    fn long_and_multiline_titles_stay_on_one_short_line() {
        let long = "사내 정산 리포트 자동화 및 문서 정리 프로젝트의 초안 작성\n둘째 줄";
        let s = snapshot(&[info(long, Phase::Running, false, true)], &[], &[], &Names::default());
        let label = row_label(&s.rows[0]);
        assert!(!label.contains('\n'));
        assert!(label.starts_with("사내 정산 리포트 자동화 및 문서 정리 프로젝트의 초안…"), "{label}");
        assert!(label.ends_with("— 응답 생성 중"));
        let unnamed = snapshot(&[info("  ", Phase::Starting, false, false)], &[], &[], &Names::default());
        assert_eq!(unnamed.rows[0].title, "이름 없는 세션");
    }

    #[test]
    fn rows_name_their_project_workspace_and_tool() {
        let (payments, billing) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let (main_ws, tests_ws) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let names = Names {
            projects: HashMap::from([
                (payments, "payments-api".into()),
                (billing, "사내 정산 리포트 자동화 및 문서 정리 프로젝트".into()),
            ]),
            workspaces: HashMap::from([(main_ws, "기본 공간".into()), (tests_ws, "auth-tests".into())]),
        };
        let mut a = info("로그인 오류 수정", Phase::Running, true, false);
        (a.session.project_id, a.session.workspace_id) = (payments, main_ws);
        let mut b = info("로그인 오류 수정", Phase::Running, false, true);
        (b.session.project_id, b.session.workspace_id, b.session.tool) = (billing, tests_ws, Tool::Codex);
        let mut c = info("이름 모를 곳", Phase::Running, false, true);
        c.session.project_id = uuid::Uuid::new_v4();
        let s = snapshot(&[a, b, c], &[], &[], &names);
        let texts: Vec<String> = s.rows.iter().map(row_text).collect();
        assert_eq!(texts[0], "로그인 오류 수정 — 승인 필요\npayments-api / 기본 공간 · Claude Code");
        assert!(
            texts.contains(
                &"로그인 오류 수정 — 응답 생성 중\n사내 정산 리포트 자동화 및 문서 정리 프로젝트 / auth-t… · Codex"
                    .to_string()
            ),
            "{texts:?}"
        );
        assert!(
            texts.contains(&"이름 모를 곳 — 응답 생성 중\n프로젝트 확인 불가 · Claude Code".to_string()),
            "{texts:?}"
        );
        // Same title, different places: the rows can be told apart.
        assert_ne!(texts[0].lines().nth(1), texts[1].lines().nth(1));
    }

    #[test]
    fn newer_output_alone_does_not_change_the_menu() {
        let list = vec![info("빌드", Phase::Running, false, true)];
        let a = Some(snapshot(&list, &[], &[], &Names::default()));
        let mut b = a.clone();
        b.as_mut().unwrap().rows[0].at = Some(runwick_core::now());
        assert!(looks_same(&a, &b));
        b.as_mut().unwrap().rows[0].state = "새 응답".into();
        assert!(!looks_same(&a, &b));
        assert!(!looks_same(&a, &None));
    }

    #[test]
    fn worker_results_roll_up_but_worker_prompts_remain_visible() {
        let parent = info("parent", Phase::Running, false, true);
        let mut child = info("worker", Phase::Running, false, false);
        child.session.parent_id = Some(parent.session.id);
        child.live.as_mut().unwrap().unread = true;
        let request: Request = serde_json::from_value(serde_json::json!({
            "id": uuid::Uuid::new_v4(), "parent_id": parent.session.id, "worker_id": child.session.id,
            "attempt": 1, "title": "result", "tool": "claude", "prompt": "work", "source": "agent",
            "via": "typed", "interrupt": false, "status": "done", "created_at": runwick_core::now(),
            "updated_at": runwick_core::now(), "acknowledged": false, "notes": []
        }))
        .unwrap();
        let requests = [request];
        let s = snapshot(&[parent.clone(), child.clone()], &requests, &[], &Names::default());
        assert_eq!((s.counts.needs, s.counts.running), (0, 2));
        assert!(s.rows.iter().any(|r| r.title == "parent" && r.state.contains("작업자 결과 1")));
        assert!(!s.rows.iter().any(|r| r.group == Some(Group::Fresh)));
        child.live.as_mut().unwrap().attention =
            Some(Attention { kind: AttentionKind::Approval, text: "allow".into(), since: runwick_core::now() });
        let s = snapshot(&[parent, child], &requests, &[], &Names::default());
        assert_eq!(s.counts.needs, 1);
        assert_eq!(s.rows[0].state, "승인 필요");
    }

    #[test]
    fn hidden_rows_do_not_rebuild_the_menu_but_overflow_and_counts_do() {
        let list: Vec<_> = (0..8).map(|n| info(&format!("session {n}"), Phase::Running, false, false)).collect();
        let a = Some(snapshot(&list, &[], &[], &Names::default()));
        let mut b = a.clone();
        b.as_mut().unwrap().rows[7].title = "changed offscreen".into();
        assert!(looks_same(&a, &b));
        b.as_mut().unwrap().rows.pop();
        assert!(!looks_same(&a, &b));
        let mut b = a.clone();
        b.as_mut().unwrap().counts.needs += 1;
        assert!(!looks_same(&a, &b));
    }

    #[test]
    fn bells_and_unread_output_are_quiet_and_precede_running_rows() {
        let mut fresh = info("new", Phase::Running, false, true);
        fresh.live.as_mut().unwrap().attention =
            Some(Attention { kind: AttentionKind::Notify, text: "bell".into(), since: runwick_core::now() });
        let list = [info("busy", Phase::Running, false, true), fresh, info("approval", Phase::Running, true, false)];
        let s = snapshot(&list, &[], &[], &Names::default());
        assert_eq!(s.counts.needs, 1);
        assert_eq!(s.rows.iter().map(|r| r.title.as_str()).collect::<Vec<_>>(), ["approval", "new", "busy"]);
        assert!(!needs_person(s.rows[1].group));
        assert_eq!(s.rows[1].state, "새 응답");
    }

    #[test]
    fn steps_aside_only_after_confirmed_looks() {
        assert_eq!(Watch::first(Poll::Gone, false), Err(Exit::HostGone));
        assert_eq!(Watch::first(Poll::Ok(snap(0, 0, 0, 0)), false), Err(Exit::NoRuns));
        assert_eq!(Watch::first(Poll::Ok(snap(1, 0, 0, 0)), false), Err(Exit::NoRuns));
        assert_eq!(Watch::first(Poll::Ok(snap(0, 0, 1, 1)), false), Ok(Some(snap(0, 0, 1, 1))));
        assert_eq!(Watch::first(Poll::Ok(snap(0, 1, 0, 1)), true), Err(Exit::WindowOpen));
        assert_eq!(Watch::first(Poll::Disabled, false), Err(Exit::Disabled));
        assert_eq!(Watch::first(Poll::Failed("timeout".into()), false), Ok(None));

        let mut w = Watch::default();
        assert_eq!(w.observe(Poll::Gone, false), Ok(None));
        assert_eq!(w.observe(Poll::Ok(snap(0, 1, 0, 1)), false), Ok(Some(snap(0, 1, 0, 1))));
        assert_eq!(w.observe(Poll::Gone, false), Ok(None));
        assert_eq!(w.observe(Poll::Gone, false), Err(Exit::HostGone));

        let mut w = Watch::default();
        assert_eq!(w.observe(Poll::Ok(snap(0, 0, 0, 0)), false), Ok(Some(snap(0, 0, 0, 0))));
        assert_eq!(w.observe(Poll::Failed("timeout".into()), false), Ok(None));
        assert_eq!(w.observe(Poll::Ok(snap(0, 0, 0, 0)), false), Err(Exit::NoRuns));
        assert_eq!(Watch::default().observe(Poll::Ok(snap(0, 1, 0, 1)), true), Err(Exit::WindowOpen));
    }
}
