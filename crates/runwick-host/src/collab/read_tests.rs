//! Read endpoints through the router, without a socket or a running CLI.

use super::*;
use crate::host::Client;
use crate::store::Store;
use runwick_core::api::host::HostInfo;
use runwick_core::api::records::*;
use runwick_core::rpc::Method;

struct Reads {
    ctx: Ctx,
    router: Router,
    session: Uuid,
    _dir: tempfile::TempDir,
}

impl Reads {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in_memory().unwrap();
        let project = Uuid::new_v4();
        let workspace = Uuid::new_v4();
        let session = Uuid::new_v4();
        let path = dir.path().to_str().unwrap();
        store.with(|c| {
            c.execute("INSERT INTO projects (id, name, root, created_at) VALUES (?1, 'test', ?2, '2026-11-01T00:00:00Z')", rusqlite::params![project, path])?;
            c.execute("INSERT INTO workspaces (id, project_id, name, path, kind, created_at) VALUES (?1, ?2, 'test', ?3, 'default', '2026-11-01T00:00:00Z')", rusqlite::params![workspace, project, path])?;
            c.execute("INSERT INTO sessions (id, project_id, workspace_id, title, tool, cwd, created_at) VALUES (?1, ?2, ?3, 'test', 'claude', ?4, '2026-11-01T00:00:00Z')", rusqlite::params![session, project, workspace, path])?;
            Ok(())
        }).unwrap();
        let host = Host::new(
            HostInfo {
                host_id: Uuid::new_v4(),
                device_name: "test".into(),
                version: "test".into(),
                os: "test".into(),
                arch: "test".into(),
                home: path.into(),
                started_at: runwick_core::now(),
                pid: std::process::id(),
                client_id: 0,
            },
            store,
        );
        let (tx, _) = tokio::sync::mpsc::unbounded_channel();
        let client = Arc::new(Client::new(1, tx));
        client.interactive.store(true, std::sync::atomic::Ordering::Relaxed);
        Self { ctx: Ctx { host, client }, router: crate::router::build(), session, _dir: dir }
    }

    async fn call<M: Method>(&self, params: M::Params) -> Result<M::Output, RpcError> {
        let value = self.router.dispatch(self.ctx.clone(), M::NAME, serde_json::to_value(params).unwrap()).await?;
        Ok(serde_json::from_value(value).unwrap())
    }

    fn request(&self, summary: Option<&str>) -> Request {
        let now = runwick_core::now();
        let request = Request {
            id: Uuid::new_v4(),
            parent_id: None,
            worker_id: self.session,
            previous_id: None,
            attempt: 1,
            title: "test".into(),
            role: None,
            tool: Tool::Claude,
            prompt: "test".into(),
            source: Source::Person,
            via: Via::Typed,
            interrupt: false,
            status: ReqStatus::Done,
            reason: None,
            start_rev: None,
            created_at: now,
            delivered_at: Some(now),
            finished_at: Some(now),
            updated_at: now,
            result: Some(WorkResult {
                summary: summary.map(str::to_owned),
                summary_from: summary.map(|_| SummaryFrom::Report),
                checks: vec![Check {
                    command: "cargo test".into(),
                    passed: true,
                    revision: None,
                    fingerprint: Some("old fingerprint".into()),
                    log: None,
                    at: now,
                }],
                ..Default::default()
            }),
            acknowledged: false,
            notes: vec![],
        };
        db::insert(&self.ctx.host.store, &request, None).unwrap();
        request
    }
}

#[tokio::test]
async fn pin_by_id_stays_small_and_window_only_when_the_list_exceeds_a_frame() {
    let t = Reads::new();
    let small = t
        .call::<PinsCreate>(NewPin {
            source: PinSource::Terminal { session_id: t.session, text: "Small saved pin".into() },
            title: None,
            note: None,
        })
        .await
        .unwrap();
    for _ in 0..300 {
        t.call::<PinsCreate>(NewPin {
            source: PinSource::Terminal { session_id: t.session, text: "가".repeat(20_000) },
            title: None,
            note: None,
        })
        .await
        .unwrap();
    }
    let list = t.call::<PinsList>(PinQuery::default()).await.unwrap();
    assert!(serde_json::to_vec(&list).unwrap().len() > runwick_core::wire::MAX_FRAME);
    let fetched = t.call::<PinsGet>(ById { id: small.id }).await.unwrap().unwrap();
    assert_eq!(fetched, small);
    assert!(serde_json::to_vec(&fetched).unwrap().len() < 2048);
    assert!(t.call::<PinsGet>(ById { id: Uuid::new_v4() }).await.unwrap().is_none());
    t.call::<PinsRemove>(ById { id: small.id }).await.unwrap();
    assert!(t.call::<PinsGet>(ById { id: small.id }).await.unwrap().is_none());

    t.ctx.client.interactive.store(false, std::sync::atomic::Ordering::Relaxed);
    assert_eq!(t.call::<PinsGet>(ById { id: small.id }).await.unwrap_err().code, codes::DENIED);
    assert!(!crate::router::allowed_without_window(PinsGet::NAME));
}

#[tokio::test]
async fn previews_do_not_check_freshness_but_result_details_do() {
    let t = Reads::new();
    let report = t.request(Some("Reported summary\nMore detail"));
    let transcript = t.request(None);
    t.ctx.host.store.with(|c| {
        c.execute(
            "INSERT INTO messages (id, session_id, project_id, workspace_id, provider_msg_id, seq, role, text, at, collected_at)
             SELECT ?1, id, project_id, workspace_id, 'answer', 1, 'assistant', ?2, ?3, ?3 FROM sessions WHERE id = ?4",
            rusqlite::params![Uuid::new_v4(), "가".repeat(300), crate::store::ts_to_sql(&(runwick_core::now() + chrono::Duration::seconds(1))), t.session],
        )?;
        Ok(())
    }).unwrap();
    changes::FINGERPRINT_CALLS
        .scope(std::cell::Cell::new(0), async {
            for _ in 0..3 {
                assert_eq!(
                    t.call::<CollabPreview>(ById { id: report.id }).await.unwrap().as_deref(),
                    Some("Reported summary")
                );
                let preview = t.call::<CollabPreview>(ById { id: transcript.id }).await.unwrap().unwrap();
                assert_eq!(preview, format!("{}…", "가".repeat(120)));
                assert_eq!(changes::FINGERPRINT_CALLS.with(std::cell::Cell::get), 0);
                t.ctx.host.bus.changed(&[topic::MESSAGES]);
            }
            let detail = t.call::<CollabGet>(ById { id: report.id }).await.unwrap();
            assert_eq!(changes::FINGERPRINT_CALLS.with(std::cell::Cell::get), 1);
            assert!(detail.checks_stale);
            assert_eq!(detail.answer.as_deref(), Some("Reported summary\nMore detail"));
        })
        .await;
    assert_eq!(t.call::<CollabPreview>(ById { id: Uuid::new_v4() }).await.unwrap_err().code, codes::NOT_FOUND);
    assert!(t.router.has(CollabPreview::NAME));
    t.ctx.client.interactive.store(false, std::sync::atomic::Ordering::Relaxed);
    assert_eq!(t.call::<CollabPreview>(ById { id: report.id }).await.unwrap_err().code, codes::DENIED);
}
