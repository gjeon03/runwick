//! Connection handshake, key/value storage and shutdown.

use crate::host::Ctx;
use crate::router::Router;
use crate::store::{Migration, Store, db_err};
use runwick_core::api::host::*;
use runwick_core::rpc::{RpcError, codes, topic};
use rusqlite::OptionalExtension;

pub const MIGRATIONS: &[Migration] =
    &[("0001_kv", "CREATE TABLE kv (key TEXT PRIMARY KEY, value TEXT NOT NULL, updated_at TEXT NOT NULL);")];

pub fn register(r: &mut Router) {
    r.add::<HostHello, _, _>(|ctx: Ctx, hello: Hello| async move {
        *ctx.client.label.lock().unwrap() = hello.client;
        // A window claim counts only with this launch's token from this Runwick build outside
        // every managed run (see `gui_token`); the socket path already logged a refusal.
        crate::gui_token::apply_claim(&ctx.host, &ctx.client, hello.interactive, hello.token.as_deref(), false);
        let mut info = ctx.host.info.clone();
        info.client_id = ctx.client.id;
        Ok(info)
    });
    r.add::<HostPing, _, _>(|_ctx: Ctx, _: ()| async move { Ok(runwick_core::now()) });
    r.add::<KvGet, _, _>(|ctx: Ctx, k: KvKey| async move { kv_get(&ctx.host.store, &k.key).map_err(db_err) });
    r.add::<KvPut, _, _>(|ctx: Ctx, s: KvSet| async move {
        if !client_writable(&s.key) {
            return Err(RpcError::new(codes::DENIED, "이 설정은 해당 설정 화면에서만 바꿀 수 있어요"));
        }
        kv_put(&ctx.host.store, &s.key, &s.value).map_err(db_err)?;
        ctx.host.bus.changed(&[topic::SETTINGS]);
        Ok(())
    });
    r.add::<HostShutdown, _, _>(|ctx: Ctx, p: ShutdownParams| async move { shutdown(&ctx.host, p.stop_runs).await });
}

/// Stop the runs (when asked; refused while runs are alive otherwise) and exit the host.
pub async fn shutdown(host: &std::sync::Arc<crate::host::Host>, stop_runs: bool) -> Result<ShutdownReport, RpcError> {
    let running = host.sessions.running_ids();
    if !running.is_empty() && !stop_runs {
        return Err(RpcError::new(codes::BUSY, format!("실행 중인 작업 {}개가 있어 종료하지 않았어요", running.len())));
    }
    // Stop runs first: "작업까지 종료" must not wait on the network.
    let report = crate::sessions::stop_all(host).await;
    let host = host.clone();
    tokio::spawn(async move {
        // Let the response reach the client before the listener closes.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        // Pending automatic sync, bounded; an unfinished one resumes at the next start.
        crate::sync::on_shutdown(&host).await;
        host.shutdown.notify_waiters();
    });
    Ok(report)
}

/// Keys any client may write through the generic `kv.put`. Everything else (the external editor
/// command, sync repository, collaboration and preference records, host identity) is changed only
/// through its own validated method, so a client cannot plant a command or a push target.
const CLIENT_KEYS: &[&str] = &["ui.layout", "docs.drafts", "wiki.drafts", "menubar.enabled"];

pub fn client_writable(key: &str) -> bool {
    CLIENT_KEYS.contains(&key)
}

pub fn kv_get(store: &Store, key: &str) -> rusqlite::Result<Option<serde_json::Value>> {
    store.with(|c| {
        let s: Option<String> = c.query_row("SELECT value FROM kv WHERE key = ?1", [key], |r| r.get(0)).optional()?;
        Ok(s.and_then(|s| serde_json::from_str(&s).ok()))
    })
}

pub fn kv_put(store: &Store, key: &str, value: &serde_json::Value) -> rusqlite::Result<()> {
    store.with(|c| {
        c.execute(
            "INSERT INTO kv (key, value, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
            rusqlite::params![key, value.to_string(), crate::store::ts_to_sql(&runwick_core::now())],
        )?;
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generic_kv_put_cannot_touch_protected_keys() {
        for k in ["ui.layout", "docs.drafts", "wiki.drafts", "menubar.enabled"] {
            assert!(client_writable(k), "{k}");
        }
        for k in ["files.editor", "sync.repo", "sync.auto", "host.id", "prefs", "collab.settings", ""] {
            assert!(!client_writable(k), "{k}");
        }
    }
}
