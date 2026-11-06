//! Job rows and planning against a real (in-memory) store, and turning AI answers into
//! proposals and drafts with a canned answer (no CLI, no tokens).

use super::conditions::{Decision, HELD_BATTERY, HELD_BUSY};
use super::{runner, store, work};
use crate::host::Host;
use crate::store::Store;
use runwick_core::api::host::HostInfo;
use runwick_core::api::jobs::*;
use runwick_core::api::records::{Message, Role};
use runwick_core::api::wiki::{PageStatus, ProposalQuery};
use std::sync::Arc;
use uuid::Uuid;

fn host() -> (Arc<Host>, Uuid, tempfile::TempDir) {
    let store = Store::open_in_memory().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let project = crate::projects::open_folder(&store, dir.path().to_str().unwrap()).unwrap().project.id;
    let info = HostInfo {
        host_id: Uuid::new_v4(),
        device_name: "test".into(),
        version: "0".into(),
        os: "test".into(),
        arch: "test".into(),
        home: "/tmp".into(),
        started_at: runwick_core::now(),
        pid: 0,
        client_id: 0,
    };
    (Host::new(info, store), project, dir)
}

fn jobs(h: &Host, project: Uuid) -> Vec<Job> {
    h.store.with(|c| store::list(c, &JobQuery { project_id: Some(project), active_only: false, limit: 0 })).unwrap()
}

#[test]
fn automatic_jobs_are_held_queued_and_released_with_power() {
    let (h, p, _d) = host();
    let hold =
        Decision::Hold { reason: "자동 · 대화 기록 24건이 새로 쌓였어요".into(), held: HELD_BATTERY };
    assert!(runner::settle(&h, p, JobKind::WikiProposals, None, hold.clone()));
    assert!(!runner::settle(&h, p, JobKind::WikiProposals, None, hold), "same decision changes nothing");
    let j = &jobs(&h, p)[0];
    assert_eq!((j.status, j.trigger, j.held_reason.as_deref()), (JobStatus::Held, Trigger::Auto, Some(HELD_BATTERY)));
    // Power returns: the battery hold is released.
    assert!(runner::release_held(&h, PowerSource::Ac));
    assert_eq!(jobs(&h, p)[0].status, JobStatus::Queued);
    // Back on battery: queued automatic work waits again.
    assert!(runner::release_held(&h, PowerSource::Battery));
    assert_eq!(jobs(&h, p)[0].held_reason.as_deref(), Some(HELD_BATTERY));
    // Busy project: hold reason changes in place, still one job.
    runner::settle(&h, p, JobKind::WikiProposals, None, Decision::Hold { reason: "자동".into(), held: HELD_BUSY });
    assert_eq!(jobs(&h, p).len(), 1);
    assert_eq!(jobs(&h, p)[0].held_reason.as_deref(), Some(HELD_BUSY));
    // Conditions gone: the waiting automatic job is closed, not left hanging.
    runner::settle(&h, p, JobKind::WikiProposals, None, Decision::Skip);
    assert_eq!(jobs(&h, p)[0].status, JobStatus::Stopped);
}

#[test]
fn requested_jobs_are_never_changed_by_automation() {
    let (h, p, _d) = host();
    let target = JobTarget::default();
    let id = h
        .store
        .with(|c| {
            store::insert(
                c,
                &store::NewJob {
                    kind: JobKind::WikiProposals,
                    project_id: p,
                    trigger: Trigger::Requested,
                    reason: "직접 요청",
                    target: &target,
                    status: JobStatus::Queued,
                    held_reason: None,
                },
            )
        })
        .unwrap();
    assert!(!runner::settle(&h, p, JobKind::WikiProposals, None, Decision::Skip));
    assert!(!runner::release_held(&h, PowerSource::Battery));
    let j = h.store.with(|c| store::get(c, id)).unwrap().unwrap();
    assert_eq!(j.status, JobStatus::Queued);
}

#[test]
fn restart_stops_unfinished_jobs_and_failures_retry_with_backoff() {
    let (h, p, _d) = host();
    runner::settle(&h, p, JobKind::WikiProposals, None, Decision::Run { reason: "자동".into() });
    let id = jobs(&h, p)[0].id;
    let now = runwick_core::now();
    h.store.with(|c| store::mark_started(c, id, &now)).unwrap();
    let retry_at = now + chrono::Duration::minutes(2);
    let f = store::Finish {
        status: JobStatus::Failed,
        result: None,
        error: Some("오류"),
        provider: Some("Claude Code"),
        model: None,
        duration_ms: Some(5),
        retry_at: Some(retry_at),
    };
    h.store.with(|c| store::finish(c, id, &f, &now)).unwrap();
    let j = &jobs(&h, p)[0];
    assert_eq!((j.status, j.attempts, j.error.as_deref()), (JobStatus::Queued, 1, Some("오류")));
    assert!(j.retry_at.is_some());
    assert_eq!(h.store.with(|c| store::stop_unfinished(c, &now)).unwrap(), 1);
    let j = &jobs(&h, p)[0];
    assert_eq!(j.status, JobStatus::Stopped);
    assert!(j.retry_at.is_none());
}

#[test]
fn switches_remember_when_timeline_was_turned_on() {
    let (h, p, _d) = host();
    let now = runwick_core::now();
    h.store.with(|c| store::set_switches(c, p, Some(true), None, &now)).unwrap();
    let sw = h.store.with(|c| store::switches(c, p)).unwrap();
    assert!(sw.wiki && !sw.timeline && sw.timeline_since.is_none());
    h.store.with(|c| store::set_switches(c, p, None, Some(true), &now)).unwrap();
    let later = now + chrono::Duration::days(3);
    h.store.with(|c| store::set_switches(c, p, Some(false), None, &later)).unwrap();
    let sw = h.store.with(|c| store::switches(c, p)).unwrap();
    assert!(!sw.wiki && sw.timeline);
    assert_eq!(sw.timeline_since.map(|t| t.timestamp()), Some(now.timestamp()));
    assert_eq!(h.store.with(store::opted_in).unwrap(), vec![p]);
}

fn message(project: Uuid, session: Uuid, text: &str, min: i64) -> Message {
    let at = runwick_core::now() - chrono::Duration::minutes(min);
    Message {
        id: Uuid::now_v7(),
        session_id: session,
        project_id: project,
        workspace_id: Uuid::nil(),
        role: Role::Assistant,
        text: text.into(),
        at,
        collected_at: at,
        seq: 0,
        sidechain: false,
        tool_name: None,
    }
}

fn job(project: Uuid, kind: JobKind, sessions: Vec<Uuid>) -> Job {
    Job {
        id: Uuid::now_v7(),
        kind,
        project_id: project,
        project_name: None,
        trigger: Trigger::Requested,
        reason: "직접 요청".into(),
        target: JobTarget { day: None, sessions },
        status: JobStatus::Running,
        held_reason: None,
        attempts: 1,
        retry_at: None,
        created_at: runwick_core::now(),
        started_at: None,
        finished_at: None,
        result: None,
        error: None,
        provider: None,
        model: None,
        duration_ms: None,
    }
}

#[test]
fn answers_become_proposals_and_bad_answers_change_nothing() {
    let (h, p, _d) = host();
    let wiki = tempfile::tempdir().unwrap();
    let root = wiki.path();
    let session = Uuid::new_v4();
    let m1 = message(p, session, "만료는 서버 시각 기준으로 하기로 했어요", 30);
    let m2 = message(p, session, "재시도는 세 번까지", 10);
    let basis = work::Basis::Wiki { messages: vec![m1.clone(), m2.clone()], read_to: m2.at };
    let j = job(p, JobKind::WikiProposals, vec![]);

    let bad = work::complete(&h, root, &j, &basis, "죄송해요, JSON을 만들지 못했어요", "Claude Code");
    assert!(bad.is_err());
    let none = h
        .store
        .with(|c| crate::wiki::proposals::list(c, &ProposalQuery { project_id: Some(p), ..Default::default() }))
        .unwrap();
    assert!(none.is_empty());

    let answer = format!(
        "```json\n{{\"proposals\":[{{\"page\":null,\"title\":\"로그인과 세션\",\"content\":\"# 로그인과 세션\\n만료는 서버 시각 기준\\n\",\
         \"rationale\":\"대화에서 결정\",\"sources\":[\"{}\",\"없는-id\"]}}]}}\n```",
        m1.id
    );
    assert_eq!(work::complete(&h, root, &j, &basis, &answer, "Claude Code").unwrap(), "제안 1건");
    let list = h
        .store
        .with(|c| crate::wiki::proposals::list(c, &ProposalQuery { project_id: Some(p), ..Default::default() }))
        .unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].sources.len(), 1, "unknown ids are dropped");
    assert_eq!(list[0].sources[0].session_id, session);
    // The cursor moved: the next run starts after these messages.
    assert_eq!(work::wiki_cursor(&h, p).map(|t| t.timestamp_millis()), Some(m2.at.timestamp_millis()));

    let draft = job(p, JobKind::WikiDraft, vec![session]);
    let dbasis = work::Basis::Draft { messages: vec![m1.clone()], sessions: vec![session] };
    let out = work::complete(
        &h,
        root,
        &draft,
        &dbasis,
        "{\"title\":\"세션 정리\",\"content\":\"# 세션 정리\\n내용\"}",
        "Codex",
    )
    .unwrap();
    assert_eq!(out, "초안 “세션 정리”");
    let pages = crate::wiki::ops::list(&h.store, root, Some(p)).unwrap();
    let page = pages.iter().find(|x| x.title == "세션 정리").unwrap();
    assert_eq!((page.status, page.sources.clone()), (PageStatus::Draft, vec![session]));
}

/// Verification data for GUI snapshots: `RUNWICK_SEED_HOME=<dir> cargo test -p runwick-host
/// seed_demo_home -- --ignored --nocapture` fills `<dir>` with a project, wiki pages, proposals
/// and jobs, and prints the ids to put into `RUNWICK_UI_STATE`.
#[test]
#[ignore]
fn seed_demo_home() {
    let Some(home) = std::env::var_os("RUNWICK_SEED_HOME").map(std::path::PathBuf::from) else {
        return;
    };
    let folder = home.parent().unwrap().join("payments-api");
    std::fs::create_dir_all(&folder).unwrap();
    let store = Store::open(&home.join("runwick.db")).unwrap();
    let opened = crate::projects::open_folder(&store, folder.to_str().unwrap()).unwrap();
    let (p, ws) = (opened.project.id, opened.workspace.id);
    let root = home.join("wiki");
    let mk = |title: &str, body: &str, tags: &[&str], by: Author, status: PageStatus, sources: Vec<Uuid>| {
        crate::wiki::ops::create(
            &store,
            &root,
            crate::wiki::ops::NewPage {
                project_id: p,
                title: title.into(),
                body: body.into(),
                tags: tags.iter().map(|t| t.to_string()).collect(),
                sources,
                created_by: by,
                status,
                saved_by: SavedBy::User,
                note: None,
                slug: None,
            },
        )
        .unwrap()
    };
    use runwick_core::api::wiki::{Author, SavedBy, SourceRef};
    let s1 = Uuid::new_v4();
    let login = mk(
        "로그인과 세션",
        "# 로그인과 세션\n\n세션은 서버에서 발급한 만료 시각을 기준으로 관리한다. 클라이언트 시계는 믿지 않는다.\n\n## 토큰 만료 처리\n\n만료 비교는 Date.now() 기준으로 한다.\n\n## 갱신 경로\n\nrefresh 토큰은 만료 전에만 재발급한다. 실패하면 로그인 화면으로 보내고 원래 경로를 기억한다.\n\n관련 문서: [[주요 결정]]\n",
        &["인증", "세션"],
        Author::User,
        PageStatus::Reviewed,
        vec![s1],
    );
    mk(
        "주요 결정",
        "# 주요 결정\n\n- 쿠키 만료는 서버 시각 기준으로 판단\n- 재시도는 세 번까지\n",
        &["결정"],
        Author::User,
        PageStatus::Reviewed,
        vec![],
    );
    mk(
        "결제 웹훅 재시도",
        "# 결제 웹훅 재시도\n\n실패한 웹훅은 1분, 5분, 30분 간격으로 세 번까지 다시 보낸다. 멱등 키로 중복 처리를 막는다.\n",
        &["결제"],
        Author::Ai,
        PageStatus::Draft,
        vec![Uuid::new_v4()],
    );
    let deploy = mk(
        "배포 절차",
        "# 배포 절차\n\n1. 스테이징 확인\n2. 태그\n3. 롤백 준비\n",
        &["배포"],
        Author::User,
        PageStatus::Reviewed,
        vec![],
    );
    mk(
        "토큰 갱신 실패 대응",
        "# 토큰 갱신 실패 대응\n\n토큰 만료 후 갱신이 실패하면 서버 시각 기준으로 다시 로그인하게 한다.\n",
        &["인증"],
        Author::User,
        PageStatus::Reviewed,
        vec![],
    );
    mk(
        "Git 커밋 규칙",
        "# Git 커밋 규칙\n\n`type: 설명` 형식으로 쓴다.\n",
        &["규칙"],
        Author::Import,
        PageStatus::Reviewed,
        vec![],
    );
    let src = vec![SourceRef {
        session_id: s1,
        message_id: Some(Uuid::new_v4().to_string()),
        session_title: Some("로그인 오류 수정".into()),
        at: Some(runwick_core::now()),
    }];
    let prop = |slug: Option<&str>, title: &str, body: &str, why: &str| {
        crate::wiki::ops::add_proposal(
            &store,
            &root,
            crate::wiki::ops::AiProposal {
                project_id: p,
                slug: slug.map(str::to_string),
                title: title.into(),
                body: body.into(),
                rationale: why.into(),
                sources: src.clone(),
                ai_flag: None,
                job_id: None,
            },
        )
        .unwrap()
    };
    prop(
        Some(&login.summary.slug),
        "로그인과 세션",
        &login.body.replace(
            "만료 비교는 Date.now() 기준으로 한다.",
            "만료 시각은 초 단위로 저장한다. Date.now()와 비교할 때는 1000을 곱해 단위를 맞춘다.\n\n쿠키 만료는 클라이언트 시각 기준으로 판단한다.",
        ),
        "오늘 대화에서 만료 단위가 달라 생긴 오류를 확인했어요.",
    );
    prop(
        Some(&deploy.summary.slug),
        "배포 절차",
        "# 배포 절차\n\n1. 스테이징 확인\n2. 마이그레이션 점검\n3. 태그\n4. 롤백 준비\n",
        "배포 점검 대화에서 마이그레이션 단계를 추가하기로 했어요.",
    );
    prop(
        None,
        "웹훅 서명 검증",
        "# 웹훅 서명 검증\n\n결제사가 보낸 웹훅은 서명 헤더를 HMAC으로 검증한 뒤에만 처리한다.\n",
        "웹훅 조사 대화에서 서명 검증 순서를 정했어요.",
    );
    // The person edits the deploy page after its proposal: the proposal needs re-comparison.
    crate::wiki::ops::save(
        &store,
        &root,
        crate::wiki::ops::Save {
            project_id: p,
            slug: &deploy.summary.slug,
            base_version: deploy.summary.version,
            title: "배포 절차",
            body: "# 배포 절차\n\n1. 스테이징 확인 (QA 체크리스트 포함)\n2. 태그\n3. 롤백 준비\n",
            tags: None,
            mark_reviewed: false,
            add_sources: &[],
            saved_by: SavedBy::User,
            note: None,
        },
    )
    .unwrap();
    let now = runwick_core::now();
    store.with(|c| store::set_switches(c, p, Some(true), Some(true), &now)).unwrap();
    let target = JobTarget::default();
    let day = JobTarget {
        day: Some((chrono::Local::now().date_naive() - chrono::Duration::days(1)).to_string()),
        sessions: vec![],
    };
    store
        .with(|c| {
            let held = store::insert(
                c,
                &store::NewJob {
                    kind: JobKind::WikiProposals,
                    project_id: p,
                    trigger: Trigger::Auto,
                    reason: "자동 · 대화 기록 24건이 새로 쌓였어요",
                    target: &target,
                    status: JobStatus::Held,
                    held_reason: Some(HELD_BATTERY),
                },
            )?;
            let _ = held;
            let done = store::insert(
                c,
                &store::NewJob {
                    kind: JobKind::TimelineSummary,
                    project_id: p,
                    trigger: Trigger::Auto,
                    reason: "자동 · 어제 기록 42건",
                    target: &day,
                    status: JobStatus::Queued,
                    held_reason: None,
                },
            )?;
            store::mark_started(c, done, &now)?;
            store::finish(
                c,
                done,
                &store::Finish {
                    status: JobStatus::Done,
                    result: Some("요약 저장 · 기록 42건 기준"),
                    error: None,
                    provider: Some("Claude Code"),
                    model: Some("claude-sonnet-4-5"),
                    duration_ms: Some(18_400),
                    retry_at: None,
                },
                &now,
            )?;
            let failed = store::insert(
                c,
                &store::NewJob {
                    kind: JobKind::WikiDraft,
                    project_id: p,
                    trigger: Trigger::Requested,
                    reason: "직접 요청",
                    target: &target,
                    status: JobStatus::Queued,
                    held_reason: None,
                },
            )?;
            store::mark_started(c, failed, &now)?;
            store::finish(
                c,
                failed,
                &store::Finish {
                    status: JobStatus::Failed,
                    result: None,
                    error: Some("AI 응답에서 JSON을 찾지 못했어요"),
                    provider: Some("Claude Code"),
                    model: None,
                    duration_ms: Some(9_100),
                    retry_at: None,
                },
                &now,
            )
        })
        .unwrap();
    println!("SEED project={p} ws={ws}");
}

/// One real call through the installed CLI with a tiny prompt (spends a little of the
/// subscription): `cargo test -p runwick-host real_cli_call -- --ignored --nocapture`.
#[test]
#[ignore]
fn real_cli_call() {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let (h, _, _d) = host();
    let setup = super::ai::setup(&h);
    let provider = setup.provider.expect("claude or codex on the job PATH");
    let dir = tempfile::tempdir().unwrap();
    let prompt = super::prompts::summary_prompt("demo", "2026-11-05", "10:00 · 대화 · 로그인 오류 수정 (3건)");
    let out = rt.block_on(super::ai::run(&provider, &setup.env, &prompt, &dir.path().join("w"), |_| {})).unwrap();
    println!("provider={} model={:?} ms={} text={}", provider.label(), out.model, out.duration_ms, out.text);
    let summary = super::prompts::parse_summary(&out.text).unwrap();
    assert!(!summary.is_empty());
}
