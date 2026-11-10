//! "전체 활동" (S06): one grouping of everything that runs or waits, shared by the app's
//! activity list, the collaboration panel and `runwick status`, so the same run is never
//! counted differently in two places. Pure functions over observed state; nothing is guessed.

use crate::api::collab::{Overview, ReqStatus, Request};
use crate::api::sessions::{AttentionKind, Phase, Session, SessionId, SessionLive};
use std::collections::HashMap;

/// Activity groups in the order they are shown (what needs the person first).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Group {
    /// A prompt only the person can answer.
    Attention,
    /// Failed starts, uncertain delivery, or interrupted by a restart.
    Recover,
    /// Answered or new output not looked at yet.
    Fresh,
    /// Observed responding.
    Running,
    /// Queued or held, with a reason.
    Waiting,
    /// A result already read, or worker results rolled up to their parent.
    Recent,
}

impl Group {
    pub const ORDER: [Group; 4] = [Group::Attention, Group::Fresh, Group::Running, Group::Recent];

    /// Recovery and queued work share the four visible sections.
    pub fn section(self) -> Self {
        match self {
            Self::Recover => Self::Attention,
            Self::Waiting => Self::Running,
            g => g,
        }
    }

    pub fn needs_person(self) -> bool {
        matches!(self, Self::Attention | Self::Recover)
    }

    pub fn label(self) -> &'static str {
        match self {
            Group::Attention => "지금 확인",
            Group::Recover => "복구 필요",
            Group::Fresh => "새 응답",
            Group::Running => "실행 중",
            Group::Waiting => "대기 중",
            Group::Recent => "최근 결과",
        }
    }

    /// The group that needs the person more.
    pub fn most_urgent(a: Option<Group>, b: Option<Group>) -> Option<Group> {
        match (a, b) {
            (Some(x), Some(y)) => Some(x.min(y)),
            (x, None) => x,
            (None, y) => y,
        }
    }
}

/// A CLI waits on a numbered menu ([`AttentionKind::Choice`]): the same words in the sidebar,
/// tabs, session bar, collaboration panel and Activity.
pub const CHOICE_NEEDED: &str = "선택 필요 · CLI 화면에서 골라 주세요";

/// A session's group and the state shown for it. `None`: nothing to show (idle or ended).
pub fn session_group(s: &Session, live: Option<&SessionLive>) -> Option<(Group, &'static str)> {
    let Some(l) = live else {
        return s.interrupted.then_some((Group::Recover, "재시작 전에 실행 중이었음"));
    };
    if let Some(a) = &l.attention
        && matches!(l.phase, Phase::Running | Phase::Starting)
        && a.kind != AttentionKind::Notify
    {
        let text = match a.kind {
            AttentionKind::Approval => "승인 필요",
            AttentionKind::Auth => "인증·신뢰 확인",
            AttentionKind::Notify => "새 응답",
            AttentionKind::Choice => CHOICE_NEEDED,
        };
        return Some((Group::Attention, text));
    }
    match l.phase {
        Phase::Exited if l.error.is_some() => Some((Group::Recover, "시작 실패")),
        Phase::NotRunning if s.interrupted => Some((Group::Recover, "재시작 전에 실행 중이었음")),
        _ if has_new_response(l) => Some((Group::Fresh, "새 응답")),
        Phase::Starting => Some((Group::Running, "시작 중")),
        Phase::Running if l.busy => Some((Group::Running, "응답 생성 중")),
        _ => None,
    }
}

/// Quiet output (including a terminal bell) never means a person owes an answer.
pub fn has_new_response(live: &SessionLive) -> bool {
    live.unread || live.attention.as_ref().is_some_and(|a| a.kind == AttentionKind::Notify)
}

/// Worker results belong to the parent, independently of manual acknowledgment.
pub fn request_group(r: &Request) -> Option<Group> {
    match r.status {
        ReqStatus::NeedsInput => Some(Group::Attention),
        ReqStatus::Uncertain => Some(Group::Recover),
        ReqStatus::Done | ReqStatus::Failed if r.parent_id.is_none() => {
            Some(if r.acknowledged { Group::Recent } else { Group::Fresh })
        }
        ReqStatus::Delivered | ReqStatus::Running => Some(Group::Running),
        ReqStatus::Queued => Some(Group::Waiting),
        ReqStatus::Failed | ReqStatus::Done | ReqStatus::Cancelled => None,
    }
}

/// One row per worker. An unresolved older request wins over newer queued or finished work.
/// Build once per host, rather than scanning all requests for every session.
#[derive(Default)]
pub struct RequestIndex<'a> {
    workers: HashMap<SessionId, &'a Request>,
    unread_results: HashMap<SessionId, Vec<uuid::Uuid>>,
    results: HashMap<SessionId, usize>,
}

impl<'a> RequestIndex<'a> {
    pub fn new(requests: &'a [Request]) -> Self {
        let mut index = Self::default();
        for r in requests {
            if request_group(r) == Some(Group::Fresh) {
                index.unread_results.entry(r.worker_id).or_default().push(r.id);
            }
            if let Some(parent) = r.parent_id
                && matches!(r.status, ReqStatus::Done | ReqStatus::Failed)
            {
                *index.results.entry(parent).or_default() += 1;
            }
            let rank = |r: &Request| request_group(r).unwrap_or(Group::Recent);
            index
                .workers
                .entry(r.worker_id)
                .and_modify(|old| {
                    if rank(r) < rank(old) || (rank(r) == rank(old) && (r.created_at, r.id) > (old.created_at, old.id))
                    {
                        *old = r;
                    }
                })
                .or_insert(r);
        }
        index
    }

    pub fn request(&self, worker: SessionId) -> Option<&'a Request> {
        self.workers.get(&worker).copied()
    }

    pub fn unread_results(&self, worker: SessionId) -> &[uuid::Uuid] {
        self.unread_results.get(&worker).map(Vec::as_slice).unwrap_or_default()
    }

    pub fn worker_results(&self, parent: SessionId) -> usize {
        self.results.get(&parent).copied().unwrap_or_default()
    }

    pub fn group(&self, s: &Session, live: Option<&SessionLive>) -> Option<Group> {
        let group = row_group(s, live, self.request(s.id));
        group.or_else(|| (self.worker_results(s.id) > 0).then_some(Group::Recent))
    }
}

/// The request and session share one row; an actual prompt always remains visible.
pub fn row_group(s: &Session, live: Option<&SessionLive>, request: Option<&Request>) -> Option<Group> {
    let mut own = session_group(s, live).map(|g| g.0);
    let delegated = s.parent_id.is_some() || request.is_some_and(|r| r.parent_id.is_some());
    let finished = request.is_none_or(|r| matches!(r.status, ReqStatus::Done | ReqStatus::Failed));
    if delegated && (own == Some(Group::Fresh) || (finished && own == Some(Group::Recover) && !s.interrupted)) {
        own = None;
    }
    Group::most_urgent(own, request.and_then(request_group))
}

/// (Action needed, live runs). Quiet responses and results never increase the first number.
pub fn headline_counts(items: impl IntoIterator<Item = (Option<Group>, bool)>) -> (usize, usize) {
    items.into_iter().fold((0, 0), |(needs, running), (g, alive)| {
        (needs + usize::from(g.is_some_and(Group::needs_person)), running + usize::from(alive))
    })
}

/// Footer line of the activity list and the collaboration panel: observed runs and the
/// automatic assignment limit are separate numbers.
pub fn slots_line(ov: &Overview) -> String {
    let limit = ov.settings.auto_limit;
    let over = if ov.in_use >= limit { " · 새 자동 배정 보류" } else { "" };
    format!("응답 중 {}개(직접 입력 포함) · 자동 배정 {}/{limit}{over}", ov.observed_busy, ov.assigned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::sessions::{Attention, Tool};

    fn session() -> Session {
        Session {
            id: uuid::Uuid::nil(),
            project_id: uuid::Uuid::nil(),
            workspace_id: uuid::Uuid::nil(),
            title: "t".into(),
            title_user_set: false,
            tool: Tool::Claude,
            provider_conv_id: None,
            parent_id: None,
            role: None,
            cwd: "/".into(),
            created_at: crate::now(),
            last_active_at: None,
            ended_at: None,
            exit_code: None,
            hidden: false,
            archived: false,
            env_version: 0,
            interrupted: false,
            origin_device: None,
        }
    }

    fn live(phase: Phase, busy: bool) -> SessionLive {
        SessionLive {
            id: uuid::Uuid::nil(),
            phase,
            busy,
            attention: None,
            last_output_at: None,
            cols: 80,
            rows: 24,
            pid: None,
            input_owner: None,
            unread: false,
            error: None,
            error_kind: None,
            input_owner_client: None,
            foreground_job: false,
            input_prompt: false,
            leftovers: Vec::new(),
        }
    }

    fn request(status: ReqStatus, parent_id: Option<SessionId>) -> Request {
        Request {
            id: uuid::Uuid::new_v4(),
            parent_id,
            worker_id: uuid::Uuid::nil(),
            previous_id: None,
            attempt: 1,
            title: "work".into(),
            role: None,
            tool: Tool::Claude,
            prompt: "task".into(),
            source: crate::api::collab::Source::Person,
            via: crate::api::collab::Via::Typed,
            interrupt: false,
            status,
            reason: None,
            start_rev: None,
            created_at: crate::now(),
            delivered_at: None,
            finished_at: None,
            updated_at: crate::now(),
            result: None,
            acknowledged: false,
            notes: vec![],
        }
    }

    #[test]
    fn only_live_prompts_and_recovery_need_action() {
        let mut s = session();
        let mut l = live(Phase::Running, true);
        for kind in [AttentionKind::Approval, AttentionKind::Auth, AttentionKind::Choice, AttentionKind::Notify] {
            l.attention = Some(Attention { kind, text: "prompt".into(), since: crate::now() });
            assert_eq!(
                headline_counts([(session_group(&s, Some(&l)).map(|g| g.0), true)]).0,
                usize::from(kind != AttentionKind::Notify)
            );
        }
        assert_eq!(session_group(&s, Some(&l)), Some((Group::Fresh, "새 응답")));
        l.attention = None;
        l.unread = true;
        assert_eq!(session_group(&s, Some(&l)), Some((Group::Fresh, "새 응답")));
        l.phase = Phase::Exited;
        l.attention = Some(Attention { kind: AttentionKind::Approval, text: "old".into(), since: crate::now() });
        assert!(!session_group(&s, Some(&l)).unwrap().0.needs_person());
        l.error = Some("launch failed".into());
        assert_eq!(session_group(&s, Some(&l)).unwrap().0, Group::Recover);
        s.interrupted = true;
        assert_eq!(session_group(&s, None).unwrap().0, Group::Recover);
        assert_eq!(session_group(&s, Some(&live(Phase::Running, false))), None);
        s.interrupted = false;
        assert_eq!(session_group(&s, None), None);
    }

    #[test]
    fn results_are_quiet_and_workers_roll_up_without_acknowledgment() {
        let parent = uuid::Uuid::new_v4();
        let s = session();
        let mut l = live(Phase::Running, false);
        l.unread = true;
        for status in [ReqStatus::Done, ReqStatus::Failed] {
            let mut r = request(status, Some(parent));
            assert_eq!(request_group(&r), None);
            assert_eq!(row_group(&s, Some(&l), Some(&r)), None);
            for acknowledged in [false, true] {
                r.acknowledged = acknowledged;
                let rows = [r.clone()];
                let index = RequestIndex::new(&rows);
                assert_eq!(index.worker_results(parent), 1);
                let mut p = session();
                p.id = parent;
                assert_eq!(index.group(&p, None), Some(Group::Recent));
                assert_eq!(index.group(&p, Some(&live(Phase::Running, true))), Some(Group::Running));
            }
            r.parent_id = None;
            r.acknowledged = false;
            assert_eq!(request_group(&r), Some(Group::Fresh));
            assert_eq!(headline_counts([(request_group(&r), false)]).0, 0);
            r.acknowledged = true;
            assert_eq!(request_group(&r), Some(Group::Recent));
        }
    }

    #[test]
    fn reading_all_includes_every_unread_person_result_for_the_row() {
        let mut done = request(ReqStatus::Done, None);
        let failed = request(ReqStatus::Failed, None);
        let child = request(ReqStatus::Done, Some(uuid::Uuid::new_v4()));
        let rows = [done.clone(), failed.clone(), child];
        let index = RequestIndex::new(&rows);
        assert_eq!(index.unread_results(done.worker_id), &[done.id, failed.id]);
        done.acknowledged = true;
        let rows = [done, failed.clone()];
        assert_eq!(RequestIndex::new(&rows).unread_results(failed.worker_id), &[failed.id]);
    }

    #[test]
    fn old_worker_output_does_not_reappear_when_history_leaves_the_recent_window() {
        let mut child = session();
        child.parent_id = Some(uuid::Uuid::new_v4());
        let mut l = live(Phase::Running, false);
        l.unread = true;
        assert_eq!(RequestIndex::new(&[]).group(&child, Some(&l)), None);
        l.phase = Phase::Exited;
        l.error = Some("failed".into());
        assert_eq!(RequestIndex::new(&[]).group(&child, Some(&l)), None);
        child.interrupted = true;
        l.phase = Phase::NotRunning;
        assert_eq!(RequestIndex::new(&[]).group(&child, Some(&l)), Some(Group::Recover));
    }

    #[test]
    fn old_child_prompts_survive_newer_work_and_busy_parents() {
        let parent = uuid::Uuid::new_v4();
        let mut ask = request(ReqStatus::NeedsInput, Some(parent));
        ask.created_at -= chrono::Duration::days(90);
        let recent = request(ReqStatus::Queued, Some(parent));
        let rows = [recent, ask.clone()];
        let index = RequestIndex::new(&rows);
        assert_eq!(index.request(ask.worker_id).unwrap().id, ask.id);
        assert_eq!(index.group(&session(), Some(&live(Phase::Running, true))), Some(Group::Attention));
        let mut done = request(ReqStatus::Done, Some(parent));
        let mut l = live(Phase::Running, true);
        for kind in [AttentionKind::Approval, AttentionKind::Auth, AttentionKind::Choice] {
            l.attention = Some(Attention { kind, text: "prompt".into(), since: crate::now() });
            assert_eq!(row_group(&session(), Some(&l), Some(&done)), Some(Group::Attention));
        }
        done.status = ReqStatus::Uncertain;
        assert_eq!(request_group(&done), Some(Group::Recover));
        done.acknowledged = true;
        assert_eq!(request_group(&done), Some(Group::Recover));
    }

    #[test]
    fn attention_wins_and_idle_is_not_listed() {
        let s = session();
        let mut l = live(Phase::Running, true);
        assert_eq!(session_group(&s, Some(&l)).map(|g| g.0), Some(Group::Running));
        l.attention = Some(Attention { kind: AttentionKind::Approval, text: "x".into(), since: crate::now() });
        assert_eq!(session_group(&s, Some(&l)), Some((Group::Attention, "승인 필요")));
        l.attention.as_mut().unwrap().kind = AttentionKind::Choice;
        assert_eq!(session_group(&s, Some(&l)), Some((Group::Attention, "선택 필요 · CLI 화면에서 골라 주세요")));
        assert_eq!(session_group(&s, Some(&live(Phase::Running, false))), None);
        let mut interrupted = session();
        interrupted.interrupted = true;
        assert_eq!(session_group(&interrupted, None).map(|g| g.0), Some(Group::Recover));
        assert_eq!(Group::most_urgent(Some(Group::Running), Some(Group::Recover)), Some(Group::Recover));
        let ov = |observed_busy, assigned, in_use| Overview {
            settings: crate::api::collab::CollabSettings::default(),
            assigned,
            observed_busy,
            in_use,
            held_parents: vec![],
            direct_workers: vec![],
        };
        assert_eq!(slots_line(&ov(4, 1, 4)), "응답 중 4개(직접 입력 포함) · 자동 배정 1/3 · 새 자동 배정 보류");
        assert_eq!(slots_line(&ov(1, 1, 1)), "응답 중 1개(직접 입력 포함) · 자동 배정 1/3");
    }

    #[test]
    fn headline_counts_needs_and_live_runs() {
        let items = [
            // A live run waiting at a trust prompt: needs the person and is running.
            (Some(Group::Attention), true),
            (Some(Group::Recover), false),
            (Some(Group::Fresh), true),
            (Some(Group::Running), true),
            // A live CLI waiting for input is a run too.
            (None, true),
            (Some(Group::Waiting), false),
        ];
        assert_eq!(headline_counts(items), (2, 4));
        assert_eq!(headline_counts([]), (0, 0));
        let s = session();
        assert_eq!(row_group(&s, Some(&live(Phase::Running, false)), None), None);
        assert_eq!(row_group(&s, Some(&live(Phase::Running, true)), None), Some(Group::Running));
    }
}
