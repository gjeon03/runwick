//! Build the workspace forest once; rendering never scans the session list for each parent.

use runwick_core::api::sessions::{Phase, SessionId, SessionInfo, SessionLive};
use std::collections::{HashMap, HashSet};

pub(super) struct SessionGroups<'a> {
    pub live: Vec<&'a SessionInfo>,
    pub ended: Vec<&'a SessionInfo>,
    pub ended_ids: Vec<SessionId>,
    pub children: HashMap<SessionId, Vec<&'a SessionInfo>>,
    pub lives: HashMap<SessionId, Option<SessionLive>>,
}

pub(super) fn still_running(live: Option<&SessionLive>) -> bool {
    live.is_some_and(|l| matches!(l.phase, Phase::Starting | Phase::Running) || !l.leftovers.is_empty())
}

impl<'a> SessionGroups<'a> {
    pub fn new(list: &'a [SessionInfo], live_of: impl Fn(&SessionInfo) -> Option<SessionLive>) -> Self {
        let mut visible: Vec<_> = list.iter().filter(|s| !s.session.archived).collect();
        // Activity timestamps never take part in sidebar ordering.
        visible.sort_by_key(|s| (s.session.created_at, s.session.id));
        let ids: HashSet<_> = visible.iter().map(|s| s.session.id).collect();
        let lives: HashMap<_, _> = visible.iter().map(|s| (s.session.id, live_of(s))).collect();
        let mut children: HashMap<_, Vec<_>> = HashMap::new();
        let mut roots = Vec::new();
        for s in visible {
            if let Some(parent) = s.session.parent_id.filter(|p| *p != s.session.id && ids.contains(p)) {
                children.entry(parent).or_default().push(s);
            } else {
                roots.push(s);
            }
        }
        let mut out = Self { live: Vec::new(), ended: Vec::new(), ended_ids: Vec::new(), children, lives };
        for root in roots {
            let mut members = Vec::new();
            let mut pending = vec![root];
            let mut running = false;
            while let Some(s) = pending.pop() {
                members.push(s.session.id);
                running |= still_running(out.lives[&s.session.id].as_ref());
                if let Some(kids) = out.children.get(&s.session.id) {
                    pending.extend(kids.iter().rev().copied());
                }
            }
            if running {
                // A stopped parent must not hide workers that are still running.
                out.live.push(root);
            } else {
                out.ended.push(root);
                out.ended_ids.extend(members);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn session(id: u128, parent: Option<u128>, phase: Option<Phase>) -> SessionInfo {
        serde_json::from_value(serde_json::json!({
            "session": {
                "id": Uuid::from_u128(id), "project_id": Uuid::nil(), "workspace_id": Uuid::nil(),
                "title": format!("session {id}"), "title_user_set": false, "tool": "shell", "provider_conv_id": null,
                "parent_id": parent.map(Uuid::from_u128), "role": null, "cwd": "/",
                "created_at": "2026-01-01T00:00:00Z", "last_active_at": null,
                "ended_at": null, "exit_code": null, "hidden": false, "archived": false, "env_version": 0
            },
            "live": phase.map(|phase| serde_json::json!({
                "id": Uuid::from_u128(id), "phase": phase, "busy": false, "attention": null,
                "last_output_at": null, "cols": 80, "rows": 24, "pid": null,
                "input_owner": null, "unread": false
            }))
        }))
        .unwrap()
    }

    fn ids(sessions: &[&SessionInfo]) -> Vec<u128> {
        sessions.iter().map(|s| s.session.id.as_u128()).collect()
    }

    #[test]
    fn live_first_and_ended_keep_creation_order_not_activity_order() {
        let mut list = vec![
            session(5, None, Some(Phase::Running)),
            session(1, None, Some(Phase::Exited)),
            session(3, None, None),
            session(2, None, Some(Phase::Starting)),
        ];
        list[0].session.created_at -= chrono::Duration::days(1);
        list[1].session.hidden = true;
        list[3].session.last_active_at = Some(runwick_core::now());
        let groups = SessionGroups::new(&list, |s| s.live.clone());
        assert_eq!(ids(&groups.live), [5, 2]);
        assert_eq!(ids(&groups.ended), [1, 3]);
        assert_eq!(groups.ended_ids, [Uuid::from_u128(1), Uuid::from_u128(3)]);
        list[0].live.as_mut().unwrap().last_output_at = Some(runwick_core::now());
        let groups = SessionGroups::new(&list, |s| s.live.clone());
        assert_eq!(ids(&groups.live), [5, 2]);
    }

    #[test]
    fn children_stay_with_parents_and_live_descendants_keep_the_family_above_the_fold() {
        let list = vec![
            session(1, None, Some(Phase::Exited)),
            session(2, Some(1), None),
            session(3, Some(2), Some(Phase::Running)),
            session(4, None, None),
            session(5, Some(4), Some(Phase::Exited)),
            session(6, None, Some(Phase::Running)),
            session(7, Some(6), None),
        ];
        let groups = SessionGroups::new(&list, |s| s.live.clone());
        assert_eq!(ids(&groups.live), [1, 6]);
        assert_eq!(ids(&groups.ended), [4]);
        assert_eq!(ids(&groups.children[&Uuid::from_u128(1)]), [2]);
        assert_eq!(ids(&groups.children[&Uuid::from_u128(2)]), [3]);
        assert_eq!(ids(&groups.children[&Uuid::from_u128(6)]), [7]);
        assert_eq!(groups.ended_ids, [Uuid::from_u128(4), Uuid::from_u128(5)]);
    }

    #[test]
    fn archive_and_restore_preserve_children_and_the_original_position() {
        let mut list = vec![session(1, None, None), session(2, Some(1), None), session(3, Some(99), None)];
        list[0].session.archived = true;
        let groups = SessionGroups::new(&list, |s| s.live.clone());
        assert_eq!(ids(&groups.ended), [2, 3]);
        list[0].session.archived = false;
        let groups = SessionGroups::new(&list, |s| s.live.clone());
        assert_eq!(ids(&groups.ended), [1, 3]);
        assert_eq!(ids(&groups.children[&Uuid::from_u128(1)]), [2]);
    }

    #[test]
    fn latest_runtime_state_and_surviving_processes_prevent_bulk_archive() {
        let mut list = vec![session(1, None, Some(Phase::Exited)), session(2, None, Some(Phase::Exited))];
        list[1].live.as_mut().unwrap().leftovers.push("sleep (pid 7)".into());
        let groups = SessionGroups::new(&list, |s| {
            let mut live = s.live.clone().unwrap();
            if s.session.id == Uuid::from_u128(1) {
                live.phase = Phase::Running;
            }
            Some(live)
        });
        assert_eq!(ids(&groups.live), [1, 2]);
        assert!(groups.ended_ids.is_empty());
    }

    #[test]
    fn hundreds_of_sessions_resolve_runtime_state_once_per_visible_session() {
        let mut list = Vec::new();
        for parent in 1..=300 {
            list.push(session(parent, None, Some(Phase::Running)));
            list.push(session(parent + 300, Some(parent), None));
        }
        let visits = std::cell::Cell::new(0);
        let groups = SessionGroups::new(&list, |s| {
            visits.set(visits.get() + 1);
            s.live.clone()
        });
        assert_eq!(visits.get(), 600);
        assert_eq!(groups.live.len(), 300);
        assert_eq!(groups.children.len(), 300);
        assert!(groups.ended.is_empty());
    }
}
