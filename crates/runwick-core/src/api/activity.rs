//! "전체 활동" (S06): one grouping of everything that runs or waits, shared by the app's
//! activity list, the collaboration panel and `runwick status`, so the same run is never
//! counted differently in two places. Pure functions over observed state; nothing is guessed.

use crate::api::collab::{Overview, ReqStatus, Request};
use crate::api::sessions::{AttentionKind, Phase, Session, SessionLive};

/// Activity groups in the order they are shown (what needs the person first).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Group {
    /// A prompt only the person can answer.
    Attention,
    /// Failed, uncertain delivery, or interrupted by a restart.
    Recover,
    /// Answered or new output not looked at yet.
    Fresh,
    /// Observed responding.
    Running,
    /// Queued or held, with a reason.
    Waiting,
}

impl Group {
    pub const ORDER: [Group; 5] = [Group::Attention, Group::Recover, Group::Fresh, Group::Running, Group::Waiting];

    pub fn label(self) -> &'static str {
        match self {
            Group::Attention => "내 답변·승인 필요",
            Group::Recover => "복구 필요",
            Group::Fresh => "새 결과",
            Group::Running => "실행 중",
            Group::Waiting => "대기",
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
    {
        let text = match a.kind {
            AttentionKind::Approval => "승인 필요",
            AttentionKind::Auth => "인증·신뢰 확인",
            AttentionKind::Notify => "확인 필요",
            AttentionKind::Choice => CHOICE_NEEDED,
        };
        return Some((Group::Attention, text));
    }
    match l.phase {
        Phase::Exited if l.error.is_some() => Some((Group::Recover, "시작 실패")),
        Phase::Starting => Some((Group::Running, "시작 중")),
        Phase::Running if l.busy => Some((Group::Running, "응답 생성 중")),
        Phase::Running if l.unread => Some((Group::Fresh, "새 출력")),
        Phase::NotRunning if s.interrupted => Some((Group::Recover, "재시작 전에 실행 중이었음")),
        _ => None,
    }
}

/// A delegated request's group. Acknowledged results and cancelled requests are not listed.
pub fn request_group(r: &Request) -> Option<Group> {
    match r.status {
        ReqStatus::NeedsInput => Some(Group::Attention),
        ReqStatus::Uncertain => Some(Group::Recover),
        ReqStatus::Failed if !r.acknowledged => Some(Group::Recover),
        ReqStatus::Done if !r.acknowledged => Some(Group::Fresh),
        ReqStatus::Delivered | ReqStatus::Running => Some(Group::Running),
        ReqStatus::Queued => Some(Group::Waiting),
        ReqStatus::Failed | ReqStatus::Done | ReqStatus::Cancelled => None,
    }
}

/// Group of a session's row: the newest request it works on (`latest`, if any) and its own
/// state, whichever needs the person more.
pub fn row_group(s: &Session, live: Option<&SessionLive>, latest: Option<&Request>) -> Option<Group> {
    let own = session_group(s, live).map(|g| g.0);
    match latest {
        Some(r) => Group::most_urgent(request_group(r), own),
        None => own,
    }
}

/// The two numbers of the title bar and the menu bar, from one list of items (each with its
/// group, if listed, and whether its process is alive): (needs the person — 답변·승인, 복구,
/// 새 결과 —, 실행: live runs, whatever they are doing, a run waiting at a prompt included).
/// "Responding" is shown as a state of a run, never as this count.
pub fn headline_counts(items: impl IntoIterator<Item = (Option<Group>, bool)>) -> (usize, usize) {
    items.into_iter().fold((0, 0), |(needs, running), (g, alive)| {
        let need = matches!(g, Some(Group::Attention | Group::Recover | Group::Fresh));
        (needs + usize::from(need), running + usize::from(alive))
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
        assert_eq!(headline_counts(items), (3, 4));
        assert_eq!(headline_counts([]), (0, 0));
        let s = session();
        assert_eq!(row_group(&s, Some(&live(Phase::Running, false)), None), None);
        assert_eq!(row_group(&s, Some(&live(Phase::Running, true)), None), Some(Group::Running));
    }
}
