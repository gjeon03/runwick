//! Method registry. Each feature module exposes `register(&mut Router)`; [`build`] wires them.

use crate::host::Ctx;
use runwick_core::rpc::{Method, RpcError, codes};
use serde_json::Value;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

pub type HandlerFuture = Pin<Box<dyn Future<Output = Result<Value, RpcError>> + Send>>;
type Handler = Arc<dyn Fn(Ctx, Value) -> HandlerFuture + Send + Sync>;

#[derive(Default, Clone)]
pub struct Router {
    map: HashMap<&'static str, Handler>,
}

impl Router {
    pub fn add<M, F, Fut>(&mut self, f: F)
    where
        M: Method,
        F: Fn(Ctx, M::Params) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<M::Output, RpcError>> + Send + 'static,
    {
        let f = Arc::new(f);
        let handler: Handler = Arc::new(move |ctx: Ctx, params: Value| {
            let f = f.clone();
            Box::pin(async move {
                // `()` params arrive as null; accept a missing params object for them too.
                let params: M::Params = serde_json::from_value(params.clone())
                    .or_else(|e| {
                        if params.is_object() && params.as_object().is_some_and(|o| o.is_empty()) {
                            serde_json::from_value(Value::Null)
                        } else {
                            Err(e)
                        }
                    })
                    .map_err(|e| RpcError::invalid(format!("{}: 잘못된 요청 값 ({e})", M::NAME)))?;
                let out = f(ctx, params).await?;
                serde_json::to_value(out).map_err(RpcError::internal)
            }) as HandlerFuture
        });
        if self.map.insert(M::NAME, handler).is_some() {
            panic!("method registered twice: {}", M::NAME);
        }
    }

    /// Run a request. A client that is not a Runwick window (no valid window token, see
    /// `gui_token`) may call only [`allowed_without_window`] methods; everything else is refused.
    pub async fn dispatch(&self, ctx: Ctx, method: &str, params: Value) -> Result<Value, RpcError> {
        if !ctx.client.is_interactive() && !allowed_without_window(method) {
            return Err(RpcError::new(codes::DENIED, "Runwick 창에서만 할 수 있는 요청이에요"));
        }
        match self.map.get(method) {
            Some(h) => h(ctx, params).await,
            None => Err(RpcError::new(codes::UNKNOWN_METHOD, format!("알 수 없는 요청: {method}"))),
        }
    }

    pub fn has(&self, method: &str) -> bool {
        self.map.contains_key(method)
    }
}

/// What a client without the window token may call: the handshake, reading a session, and the
/// collaboration methods, whose handlers decide themselves who the caller is (a session proven by
/// its run's token, or nobody) and what it may do (`collab::identity`). The menu bar is started
/// by the window and presents the token.
const WITHOUT_WINDOW: &[&str] = &[
    "host.hello",
    "host.ping",
    // What the connection is; a refused window shows why (`gui_token`).
    "host.window_status",
    // Only a window of another Runwick build may use it (checked in `gui_token`).
    "host.restart_for_update",
    "sessions.get",
];

pub fn allowed_without_window(method: &str) -> bool {
    WITHOUT_WINDOW.contains(&method) || method.starts_with("collab.")
}

/// All features. Add one line per feature module.
pub fn build() -> Router {
    let mut r = Router::default();
    crate::meta::register(&mut r);
    crate::gui_token::register(&mut r);
    crate::projects::register(&mut r);
    crate::sessions::register(&mut r);
    crate::tools::register(&mut r);
    crate::files::register(&mut r);
    crate::records::register(&mut r);
    crate::timeline::register(&mut r);
    crate::wiki::register(&mut r);
    crate::jobs::register(&mut r);
    crate::collab::register(&mut r);
    crate::worktree::register(&mut r);
    crate::env::register(&mut r);
    crate::mcp::register(&mut r);
    crate::prefs::register(&mut r);
    crate::refresh::register(&mut r);
    crate::roles::register(&mut r);
    crate::sync::register(&mut r);
    crate::backup::register(&mut r);
    crate::devices::register(&mut r);
    r
}

#[cfg(test)]
mod window_tests {
    use super::*;
    use runwick_core::api::{
        collab::*, env::*, files::*, host::*, jobs::*, prefs::*, projects::*, sessions::*, sync::*, wiki::*,
    };

    #[test]
    fn agents_get_only_the_handshake_and_collaboration() {
        let r = build();
        for m in WITHOUT_WINDOW {
            assert!(r.has(m), "allow-listed method does not exist: {m}");
        }
        // Collaboration methods go to their own caller checks (owner-only ones included).
        for m in [CollabHold::NAME, CollabCancel::NAME, CollabAck::NAME, CollabSettingsSet::NAME] {
            assert!(allowed_without_window(m), "{m}");
        }
        // What the `runwick` CLI calls (crates/runwick/src/agent.rs).
        for m in [
            HostHello::NAME,
            SessionsGet::NAME,
            CollabIdentify::NAME,
            CollabDelegate::NAME,
            CollabGet::NAME,
            CollabList::NAME,
            CollabOverview::NAME,
            CollabReport::NAME,
            CollabSend::NAME,
            CollabWait::NAME,
        ] {
            assert!(allowed_without_window(m), "{m}");
        }
        for m in [
            EnvSet::NAME,
            EnvList::NAME,
            PrefsSetEditor::NAME,
            SyncConnect::NAME,
            SessionsStart::NAME,
            SessionsStop::NAME,
            SessionsRestart::NAME,
            SessionsUpdate::NAME,
            SessionsAttach::NAME,
            SessionsList::NAME,
            FilesWrite::NAME,
            ProjectsUpdate::NAME,
            WikiSave::NAME,
            JobsList::NAME,
            KvGet::NAME,
            KvPut::NAME,
            "devices.list",
            "backup.restore_activate",
        ] {
            assert!(!allowed_without_window(m), "{m}");
        }
    }
}
