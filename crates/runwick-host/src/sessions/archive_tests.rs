//! Archive contracts exercised through the router without a socket or personal data.

use super::*;
use crate::store::Store;
use runwick_core::api::host::HostInfo;
use runwick_core::api::records::{Anchor, ContextQuery, RecordsContext, RecordsSearch, SearchQuery};
use runwick_core::rpc::Method;
use std::sync::atomic::Ordering;

fn fixture() -> (Ctx, Router, SessionId, Arc<Rt>) {
    let store = Store::open_in_memory().unwrap();
    let (project, workspace, session, message) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    store.with(|c| {
        c.execute("INSERT INTO projects (id, name, root, created_at) VALUES (?1, 'archive test', '/test', '2026-01-01T00:00:00Z')", [project])?;
        c.execute("INSERT INTO workspaces (id, project_id, name, path, kind, created_at) VALUES (?1, ?2, 'default', '/test', 'default', '2026-01-01T00:00:00Z')", rusqlite::params![workspace, project])?;
        c.execute("INSERT INTO sessions (id, project_id, workspace_id, title, tool, cwd, created_at, ended_at, hidden) VALUES (?1, ?2, ?3, 'saved conversation', 'claude', '/test', '2026-01-01T00:00:00Z', '2026-01-02T00:00:00Z', 1)", rusqlite::params![session, project, workspace])?;
        c.execute("INSERT INTO messages (id, session_id, project_id, workspace_id, provider_msg_id, seq, role, text, at, collected_at) VALUES (?1, ?2, ?3, ?4, 'message', 1, 'assistant', 'Archived records stay searchable', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')", rusqlite::params![message, session, project, workspace])?;
        Ok(())
    }).unwrap();
    let host = Host::new(
        HostInfo {
            host_id: Uuid::new_v4(),
            device_name: "test".into(),
            version: runwick_core::VERSION.into(),
            os: "test".into(),
            arch: "test".into(),
            home: "/test".into(),
            started_at: runwick_core::now(),
            pid: std::process::id(),
            client_id: 1,
        },
        store,
    );
    let runtime = Arc::new(Rt::new(session, Tool::Claude, Size::clamped(80, 24)));
    runtime.lock().phase = Phase::Exited;
    host.sessions.rts.lock().unwrap().insert(session, runtime.clone());
    let (tx, _) = tokio::sync::mpsc::unbounded_channel();
    let client = Arc::new(Client::new(1, tx));
    client.interactive.store(true, Ordering::Relaxed);
    let mut router = Router::default();
    register(&mut router);
    crate::records::register(&mut router);
    (Ctx { host, client }, router, session, runtime)
}

async fn call<M: Method>(ctx: &Ctx, router: &Router, params: M::Params) -> Result<M::Output, RpcError> {
    let out = router.dispatch(ctx.clone(), M::NAME, serde_json::to_value(params).unwrap()).await?;
    Ok(serde_json::from_value(out).unwrap())
}

#[tokio::test]
async fn archive_keeps_records_searchable_and_restore_preserves_ended_hidden_session() {
    let (ctx, router, id, _) = fixture();
    let original = record::get(&ctx.host.store, id).unwrap().session;
    let query = SessionQuery { workspace_id: Some(original.workspace_id), include_hidden: true, ..Default::default() };
    call::<SessionsUpdate>(&ctx, &router, SessionUpdate { id, archived: Some(true), ..Default::default() })
        .await
        .unwrap();
    assert!(call::<SessionsList>(&ctx, &router, query.clone()).await.unwrap().is_empty());
    let archived =
        call::<SessionsList>(&ctx, &router, SessionQuery { include_archived: true, ..query.clone() }).await.unwrap();
    assert_eq!(archived.len(), 1);
    assert!(archived[0].session.archived);
    let found = call::<RecordsSearch>(&ctx, &router, SearchQuery { text: "searchable".into(), ..Default::default() })
        .await
        .unwrap();
    assert_eq!(found.total, 1);
    let context =
        call::<RecordsContext>(&ctx, &router, ContextQuery { anchor: Anchor::SessionEnd { id }, before: 30, after: 0 })
            .await
            .unwrap();
    assert_eq!(context.session_id, id);
    let restored =
        call::<SessionsUpdate>(&ctx, &router, SessionUpdate { id, archived: Some(false), ..Default::default() })
            .await
            .unwrap();
    assert_eq!(restored, original);
    assert_eq!(call::<SessionsList>(&ctx, &router, query).await.unwrap().len(), 1);
}

#[tokio::test]
async fn archive_rechecks_runtime_and_rejects_agent_clients() {
    let (ctx, router, id, runtime) = fixture();
    let update = || SessionUpdate { id, archived: Some(true), ..Default::default() };
    for phase in [Phase::Starting, Phase::Running] {
        runtime.lock().phase = phase;
        assert_eq!(call::<SessionsUpdate>(&ctx, &router, update()).await.unwrap_err().code, codes::BUSY);
        assert!(!record::get(&ctx.host.store, id).unwrap().session.archived);
    }
    runtime.lock().phase = Phase::Exited;
    runtime.lock().leftovers.push("sleep (pid 7)".into());
    assert_eq!(call::<SessionsUpdate>(&ctx, &router, update()).await.unwrap_err().code, codes::BUSY);
    runtime.lock().leftovers.clear();
    ctx.client.interactive.store(false, Ordering::Relaxed);
    assert_eq!(call::<SessionsUpdate>(&ctx, &router, update()).await.unwrap_err().code, codes::DENIED);
    assert!(!record::get(&ctx.host.store, id).unwrap().session.archived);
}
