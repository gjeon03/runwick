//! A day without an AI summary: say so, offer "이 날 요약 만들기" (the person's request, also on
//! battery), and show how that request is doing. The summary itself arrives through the
//! timeline once the job stores it.

use super::fmt;
use crate::App;
use crate::client::HostKey;
use crate::theme::{self, Tone, c, font};
use chrono::NaiveDate;
use egui::RichText;
use runwick_core::api::jobs::{
    Job, JobKind, JobQuery, JobRef, JobStatus, JobTarget, JobsList, JobsRequest, JobsRetry, RequestJob,
};
use runwick_core::api::projects::ProjectId;

/// The newest summary job for this project day, if any.
fn latest_job(app: &mut App, project: ProjectId, day: NaiveDate) -> Option<Job> {
    let q = JobQuery { project_id: Some(project), active_only: false, limit: 30 };
    let list = app.hub.query::<JobsList>(HostKey::Local, &q).data?;
    let day = day.to_string();
    list.iter().find(|j| j.kind == JobKind::TimelineSummary && j.target.day.as_deref() == Some(day.as_str())).cloned()
}

/// What a job's state means here, with the tone it is shown in.
pub fn describe(j: &Job) -> (Tone, String) {
    match j.status {
        JobStatus::Queued => (Tone::Waiting, "요약 대기 중".into()),
        JobStatus::Running => {
            let by = j.provider.as_deref().map(|p| format!(" · {p}")).unwrap_or_default();
            (Tone::Running, format!("요약 만드는 중{by}"))
        }
        JobStatus::Held => {
            let why = j.held_reason.as_deref().unwrap_or("조건을 기다리는 중");
            (Tone::Waiting, format!("요약 보류 · {why}"))
        }
        JobStatus::Failed => {
            let why = j.error.as_deref().unwrap_or("이유를 받지 못했어요");
            (Tone::Recover, format!("요약 실패 · {why}"))
        }
        JobStatus::Stopped => (Tone::Waiting, "요약 중지됨".into()),
        JobStatus::Done => {
            let result = j.result.as_deref().unwrap_or("완료");
            (Tone::Neutral, format!("요약 작업 끝남 · {result}"))
        }
    }
}

fn request(app: &mut App, project: ProjectId, day: NaiveDate) {
    let req = RequestJob {
        kind: JobKind::TimelineSummary,
        project_id: project,
        target: JobTarget { day: Some(day.to_string()), ..Default::default() },
    };
    app.hub.call::<JobsRequest>(HostKey::Local, req, |app, res| match res {
        Ok(_) => app.toast_info("요약을 요청했어요. 진행 상황은 여기와 자동 작업에서 볼 수 있어요"),
        Err(e) => app.toast_error(&format!("요약을 요청하지 못했어요: {}", e.message)),
    });
}

pub(super) fn no_summary(app: &mut App, ui: &mut egui::Ui, project: ProjectId, day: NaiveDate, today: NaiveDate) {
    let job = latest_job(app, project, day);
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new("요약 없음 · 실제 기록만 보여요").font(font(12.0)).color(c::TEXT_3));
        let busy = job.as_ref().is_some_and(|j| j.status.is_active());
        if day <= today && !busy {
            let label = match job.as_ref().map(|j| j.status) {
                Some(JobStatus::Failed) => None, // offered as 다시 시도 below
                Some(JobStatus::Done) => Some("다시 요약 만들기"),
                _ => Some("이 날 요약 만들기"),
            };
            if let Some(label) = label
                && fmt::small_button(ui, label).clicked()
            {
                request(app, project, day);
            }
        }
    });
    let Some(j) = job else { return };
    let (tone, text) = describe(&j);
    ui.horizontal_wrapped(|ui| {
        theme::chip(ui, tone, &text);
        if j.status == JobStatus::Failed && fmt::small_button(ui, "다시 시도").clicked() {
            app.hub.call::<JobsRetry>(HostKey::Local, JobRef { id: j.id }, |app, res| {
                if let Err(e) = res {
                    app.toast_error(&format!("다시 시도하지 못했어요: {}", e.message));
                }
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(status: JobStatus) -> Job {
        Job {
            id: uuid::Uuid::now_v7(),
            kind: JobKind::TimelineSummary,
            project_id: uuid::Uuid::now_v7(),
            project_name: None,
            trigger: runwick_core::api::jobs::Trigger::Requested,
            reason: "요청".into(),
            target: JobTarget { day: Some("2026-11-05".into()), ..Default::default() },
            status,
            held_reason: Some("배터리 사용 중".into()),
            attempts: 1,
            retry_at: None,
            created_at: runwick_core::now(),
            started_at: None,
            finished_at: None,
            result: None,
            error: Some("CLI가 응답하지 않았어요".into()),
            provider: Some("Claude Code".into()),
            model: None,
            duration_ms: None,
        }
    }

    #[test]
    fn job_states_are_described_with_reasons() {
        assert_eq!(describe(&job(JobStatus::Running)).1, "요약 만드는 중 · Claude Code");
        assert_eq!(describe(&job(JobStatus::Held)).1, "요약 보류 · 배터리 사용 중");
        let (tone, text) = describe(&job(JobStatus::Failed));
        assert_eq!((tone, text.as_str()), (Tone::Recover, "요약 실패 · CLI가 응답하지 않았어요"));
    }
}
