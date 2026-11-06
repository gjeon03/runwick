//! Wiki storage, conflicts, external edits, proposals and the graph, on a temporary folder.

use super::{files, graph, ops};
use crate::store::Store;
use runwick_core::api::wiki::*;
use std::path::PathBuf;
use uuid::Uuid;

struct Fixture {
    store: Store,
    root: PathBuf,
    project: Uuid,
    _dirs: (tempfile::TempDir, tempfile::TempDir),
}

fn fixture() -> Fixture {
    let store = Store::open_in_memory().unwrap();
    let folder = tempfile::tempdir().unwrap();
    let wiki = tempfile::tempdir().unwrap();
    let project = crate::projects::open_folder(&store, folder.path().to_str().unwrap()).unwrap().project.id;
    Fixture { store, root: wiki.path().to_path_buf(), project, _dirs: (folder, wiki) }
}

fn page(f: &Fixture, title: &str, body: &str) -> Page {
    ops::create(
        &f.store,
        &f.root,
        ops::NewPage {
            project_id: f.project,
            title: title.into(),
            body: body.into(),
            tags: vec!["#인증".into(), "인증".into(), " ".into()],
            sources: Vec::new(),
            created_by: Author::User,
            status: PageStatus::Reviewed,
            saved_by: SavedBy::User,
            note: None,
            slug: None,
        },
    )
    .unwrap()
}

fn save(f: &Fixture, slug: &str, base: u32, body: &str) -> SaveOutcome {
    ops::save(
        &f.store,
        &f.root,
        ops::Save {
            project_id: f.project,
            slug,
            base_version: base,
            title: "로그인과 세션",
            body,
            tags: None,
            mark_reviewed: false,
            add_sources: &[],
            saved_by: SavedBy::User,
            note: None,
        },
    )
    .unwrap()
}

fn saved(o: SaveOutcome) -> Page {
    match o {
        SaveOutcome::Saved { page } => page,
        c => panic!("expected a save, got {c:?}"),
    }
}

#[test]
fn korean_pages_roundtrip_through_files_and_versions() {
    let f = fixture();
    let p = page(&f, "로그인과 세션", "# 로그인과 세션\n\n세션은 서버 만료 시각을 기준으로 한다.\n");
    assert_eq!(p.summary.slug, "로그인과-세션");
    assert_eq!(p.summary.tags, vec!["인증"]);
    assert_eq!(p.summary.version, 1);
    let file = files::page_path(&f.root, f.project, "로그인과-세션");
    let doc = files::parse(&std::fs::read_to_string(&file).unwrap());
    assert_eq!(doc.meta.title.as_deref(), Some("로그인과 세션"));
    assert_eq!(doc.body, p.body);

    let p2 = saved(save(&f, &p.summary.slug, 1, "# 로그인과 세션\n\n만료는 초 단위로 저장한다.\n"));
    assert_eq!(p2.summary.version, 2);
    // Saving the same text again does not add a version.
    assert_eq!(saved(save(&f, &p.summary.slug, 2, &p2.body)).summary.version, 2);
    let hist = ops::history(&f.store, &f.root, f.project, &p.summary.slug).unwrap();
    assert_eq!(hist.iter().map(|v| v.version).collect::<Vec<_>>(), vec![2, 1]);
    assert_eq!((hist[0].added, hist[0].removed), (1, 1));
    let v1 = ops::version(&f.store, &VersionRef { project_id: f.project, slug: p.summary.slug.clone(), version: 1 })
        .unwrap();
    assert!(v1.body.contains("서버 만료 시각"));
    // A second page with the same title gets its own file.
    assert_eq!(page(&f, "로그인과 세션", "다른 글").summary.slug, "로그인과-세션-2");
}

#[test]
fn stale_saves_conflict_and_never_overwrite() {
    let f = fixture();
    let p = page(&f, "로그인과 세션", "처음\n");
    saved(save(&f, &p.summary.slug, 1, "다른 창에서 고침\n"));
    match save(&f, &p.summary.slug, 1, "내 편집\n") {
        SaveOutcome::Conflict { current, yours_body, .. } => {
            assert_eq!(current.body, "다른 창에서 고침\n");
            assert_eq!(current.summary.version, 2);
            assert_eq!(yours_body, "내 편집\n");
        }
        o => panic!("{o:?}"),
    }
    let now = ops::get(&f.store, &f.root, f.project, &p.summary.slug).unwrap();
    assert_eq!(now.body, "다른 창에서 고침\n");
}

#[test]
fn files_changed_outside_are_the_persons_edits() {
    let f = fixture();
    let p = page(&f, "배포 절차", "스테이징 확인\n");
    let file = files::page_path(&f.root, f.project, &p.summary.slug);
    // Edit the file by hand (keep the frontmatter, change the body and title).
    let text = std::fs::read_to_string(&file)
        .unwrap()
        .replace("스테이징 확인", "스테이징 확인 후 태그")
        .replace("배포 절차", "배포 순서");
    std::fs::write(&file, text).unwrap();
    let now = ops::get(&f.store, &f.root, f.project, &p.summary.slug).unwrap();
    assert_eq!(now.summary.version, 2);
    assert_eq!(now.summary.title, "배포 순서");
    assert!(now.body.contains("태그"));
    let hist = ops::history(&f.store, &f.root, f.project, &p.summary.slug).unwrap();
    assert_eq!(hist[0].saved_by, SavedBy::External);
    // A save based on the version before the hand edit conflicts.
    assert!(matches!(save(&f, &p.summary.slug, 1, "x\n"), SaveOutcome::Conflict { .. }));
    // A new file dropped into the folder becomes a page; a removed file leaves the list.
    std::fs::write(files::project_dir(&f.root, f.project).join("메모.md"), "# 회의 메모\n내용\n").unwrap();
    std::fs::remove_file(&file).unwrap();
    let list = ops::list(&f.store, &f.root, Some(f.project)).unwrap();
    assert_eq!(list.iter().map(|p| p.title.as_str()).collect::<Vec<_>>(), vec!["회의 메모"]);
}

#[test]
fn import_reads_frontmatter_titles_and_skips_empty_files() {
    let f = fixture();
    let report = ops::import(
        &f.store,
        &f.root,
        &ImportPages {
            project_id: f.project,
            files: vec![
                ImportFile {
                    name: "git-rules.md".into(),
                    text: "---\ntitle: Git 커밋 규칙\ntags: [규칙]\n---\n본문\n".into(),
                },
                ImportFile { name: "디버깅.md".into(), text: "# 디버깅 순서\n먼저 로그\n".into() },
                ImportFile { name: "빈.md".into(), text: "  \n".into() },
            ],
        },
    )
    .unwrap();
    assert_eq!(report.created, vec!["git-rules", "디버깅"]);
    assert_eq!(report.skipped.len(), 1);
    let list = ops::list(&f.store, &f.root, Some(f.project)).unwrap();
    let git = list.iter().find(|p| p.slug == "git-rules").unwrap();
    assert_eq!(
        (git.title.as_str(), git.created_by, git.tags.clone()),
        ("Git 커밋 규칙", Author::Import, vec!["규칙".to_string()])
    );
    assert_eq!(list.iter().find(|p| p.slug == "디버깅").unwrap().title, "디버깅 순서");
}

fn propose(f: &Fixture, slug: Option<&str>, title: &str, body: &str) -> Uuid {
    ops::add_proposal(
        &f.store,
        &f.root,
        ops::AiProposal {
            project_id: f.project,
            slug: slug.map(str::to_string),
            title: title.into(),
            body: body.into(),
            rationale: "대화에서 확인".into(),
            sources: vec![SourceRef {
                session_id: Uuid::new_v4(),
                message_id: Some("m1".into()),
                session_title: None,
                at: None,
            }],
            ai_flag: None,
            job_id: None,
        },
    )
    .unwrap()
    .expect("a proposal")
}

#[test]
fn proposals_rebase_on_later_edits_and_apply_only_clean_parts() {
    let f = fixture();
    let p = page(
        &f,
        "로그인과 세션",
        "# 로그인과 세션\n만료는 Date.now() 기준\n갱신은 만료 전에만\n실패하면 로그인 화면\n",
    );
    let slug = p.summary.slug.clone();
    let id = propose(
        &f,
        Some(&slug),
        "로그인과 세션",
        "# 로그인과 세션\n만료는 서버 시각 기준\n갱신은 만료 전에만\n실패하면 원래 경로를 기억하고 로그인 화면\n",
    );
    // An unchanged proposal against the same text is not stored.
    assert!(
        ops::add_proposal(
            &f.store,
            &f.root,
            ops::AiProposal {
                project_id: f.project,
                slug: Some(slug.clone()),
                title: String::new(),
                body: p.body.clone(),
                rationale: String::new(),
                sources: vec![],
                ai_flag: None,
                job_id: None,
            }
        )
        .unwrap()
        .is_none()
    );
    let listed = ops::list(&f.store, &f.root, Some(f.project)).unwrap();
    assert_eq!(listed[0].open_proposals, 1);

    // The person edits line 2 after the proposal was made.
    let p2 = saved(save(
        &f,
        &slug,
        1,
        "# 로그인과 세션\n만료는 초 단위, 서버 시각 기준 (직접 고침)\n갱신은 만료 전에만\n실패하면 로그인 화면\n",
    ));
    let d = ops::proposal_detail(&f.store, &f.root, id).unwrap();
    assert!(d.proposal.needs_rebase);
    assert_eq!(d.against_version, Some(2));
    let conflict: Vec<&Hunk> = d.hunks.iter().filter(|h| h.kind == HunkKind::Conflict).collect();
    let clean: Vec<&Hunk> = d.hunks.iter().filter(|h| h.kind == HunkKind::Clean).collect();
    assert_eq!(conflict.len(), 1);
    assert_eq!(conflict[0].old, vec!["만료는 초 단위, 서버 시각 기준 (직접 고침)"]);
    assert_eq!(clean.len(), 1);

    // Applying against the version the proposal was made on is refused.
    let stale = ops::apply(&f.store, &f.root, &ApplyProposal { id, against_version: Some(1), hunks: None });
    assert_eq!(stale.unwrap_err().code, runwick_core::rpc::codes::CONFLICT);
    let (page, prop) =
        ops::apply(&f.store, &f.root, &ApplyProposal { id, against_version: Some(2), hunks: None }).unwrap();
    assert_eq!(prop.status, ProposalStatus::Partial);
    assert_eq!(page.summary.version, p2.summary.version + 1);
    assert!(page.body.contains("(직접 고침)"), "the person's line stays: {}", page.body);
    assert!(page.body.contains("원래 경로를 기억하고"));
    assert_eq!(page.summary.sources.len(), 1);
    let hist = ops::history(&f.store, &f.root, f.project, &slug).unwrap();
    assert_eq!(hist[0].saved_by, SavedBy::Proposal);
    // A closed proposal cannot be applied or held again.
    assert!(ops::apply(&f.store, &f.root, &ApplyProposal { id, against_version: Some(3), hunks: None }).is_err());
    assert!(ops::set_proposal_status(&f.store, id, ProposalStatus::Held).is_err());
}

#[test]
fn new_page_proposals_create_pages_and_decisions_are_flagged() {
    let f = fixture();
    page(&f, "주요 결정", "- 쿠키 만료는 서버 시각 기준으로 판단\n");
    let id = propose(&f, None, "토큰 갱신", "쿠키 만료는 클라이언트 시각 기준으로 판단한다\n");
    let d = ops::proposal_detail(&f.store, &f.root, id).unwrap();
    assert!(d.proposal.slug.is_none());
    assert!(d.proposal.decision_flag.as_deref().is_some_and(|f| f.contains("범위가 겹쳐요")));
    let (page, prop) =
        ops::apply(&f.store, &f.root, &ApplyProposal { id, against_version: None, hunks: None }).unwrap();
    assert_eq!(prop.status, ProposalStatus::Applied);
    assert_eq!((page.summary.title.as_str(), page.summary.created_by), ("토큰 갱신", Author::Ai));

    let rejected = propose(&f, Some(&page.summary.slug), "", "전혀 다른 내용\n");
    assert_eq!(
        ops::set_proposal_status(&f.store, rejected, ProposalStatus::Rejected).unwrap().status,
        ProposalStatus::Rejected
    );
    assert_eq!(ops::open_proposals(&f.store, f.project).unwrap(), 0);
}

#[test]
fn graph_links_sources_and_similarity_suggestions() {
    let f = fixture();
    let a =
        page(&f, "로그인과 세션", "토큰 만료는 서버 시각 기준. 토큰 갱신은 만료 전에만. [[주요 결정]] [[없는 문서]]\n");
    let b = page(&f, "주요 결정", "- 쿠키 만료는 서버 기준\n");
    let c = page(&f, "토큰 갱신 실패 대응", "토큰이 만료되어 갱신이 실패하면 서버 시각 기준으로 다시 로그인\n");
    page(&f, "배포 절차", "스테이징 확인 후 태그와 롤백\n");
    let session = Uuid::new_v4();
    saved(
        ops::save(
            &f.store,
            &f.root,
            ops::Save {
                project_id: f.project,
                slug: &b.summary.slug,
                base_version: 1,
                title: "주요 결정",
                body: &b.body,
                tags: None,
                mark_reviewed: false,
                add_sources: &[session],
                saved_by: SavedBy::User,
                note: None,
            },
        )
        .unwrap(),
    );
    let g = graph::build(&f.store, &f.root, Some(f.project)).unwrap();
    let has = |k: EdgeKind, from: &str, to: &str| {
        g.edges.iter().any(|e| e.kind == k && ((e.from == from && e.to == to) || (e.from == to && e.to == from)))
    };
    assert!(has(EdgeKind::Link, &a.summary.slug, &b.summary.slug));
    assert!(has(EdgeKind::Source, &b.summary.slug, &session.to_string()));
    assert!(has(EdgeKind::Similar, &a.summary.slug, &c.summary.slug));
    assert_eq!(g.broken, vec![(a.summary.slug.clone(), "없는 문서".to_string())]);
    assert_eq!(g.sessions.len(), 1);
    assert!(!g.sessions[0].exists);

    // "관련 없음" is remembered; "링크로 연결" writes a real link through a save.
    let pair = PagePair { project_id: f.project, from: a.summary.slug.clone(), to: c.summary.slug.clone() };
    ops::dismiss(&f.store, &pair).unwrap();
    let g = graph::build(&f.store, &f.root, Some(f.project)).unwrap();
    assert!(
        !g.edges.iter().any(|e| e.kind == EdgeKind::Similar && (e.from == c.summary.slug || e.to == c.summary.slug))
    );
    let linked = ops::link(&f.store, &f.root, &pair).unwrap();
    assert!(linked.body.ends_with("관련 문서: [[토큰 갱신 실패 대응]]\n"), "{}", linked.body);
    assert_eq!(linked.summary.version, a.summary.version + 1);
    let g = graph::build(&f.store, &f.root, Some(f.project)).unwrap();
    assert!(g.edges.iter().any(|e| e.kind == EdgeKind::Link && e.from == a.summary.slug && e.to == c.summary.slug));
}
