//! MCP (S13): what each provider really uses, read from its own files, and connection status
//! from its own CLI on demand. Runwick never edits provider configuration.
//!
//! - `config`: definitions and fingerprints from `~/.claude.json`, `.mcp.json`, Codex TOML
//! - `check`: `claude mcp list` / `codex mcp list --json`
//! - `toml`: the small TOML reader used for Codex

pub mod check;
pub mod config;
mod toml;

use crate::host::{Ctx, Host};
use crate::router::Router;
use runwick_core::api::mcp::*;
use runwick_core::api::sessions::Tool;
use runwick_core::rpc::RpcError;
use std::collections::HashMap;
use std::sync::Mutex;

/// Status checks by provider and folder.
type Checks = HashMap<(Tool, Option<String>), McpCheckResult>;

/// Last status check per provider and folder.
static LAST: Mutex<Option<Checks>> = Mutex::new(None);

/// The latest status check for a provider and folder, if one ran since the host started.
pub fn last_result(tool: Tool, cwd: &Option<String>) -> Option<McpCheckResult> {
    LAST.lock().ok()?.as_ref()?.get(&(tool, cwd.clone())).cloned()
}

fn remember(r: &McpCheckResult) {
    if let Ok(mut m) = LAST.lock() {
        m.get_or_insert_with(HashMap::new).insert((r.provider, r.cwd.clone()), r.clone());
    }
}

/// The environment a new run would get: login environment without other terminals' markers,
/// plus the global variables. Used to run provider CLIs for checks.
pub fn run_env(host: &Host) -> Vec<(String, String)> {
    let login = crate::sessions::env::login_env();
    let mut vars: std::collections::BTreeMap<String, String> = login
        .vars
        .iter()
        .filter(|(k, _)| !crate::sessions::env::is_stripped(k))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    if let Ok(o) = crate::env::overlay(&host.store, crate::env::service()) {
        vars.extend(o.set);
    }
    vars.into_iter().collect()
}

/// Run a status check and keep it as the latest result. Blocking.
pub fn check_now(host: &Host, tool: Tool, cwd: Option<String>) -> McpCheckResult {
    let r = check::run(tool, cwd.as_deref(), &run_env(host));
    remember(&r);
    r
}

pub fn register(r: &mut Router) {
    r.add::<McpList, _, _>(|_ctx: Ctx, q: McpQuery| async move {
        tokio::task::spawn_blocking(move || {
            let files = config::Files::from_login_env();
            let cwd = q.cwd.filter(|c| !c.trim().is_empty());
            let (defs, sources) = config::read(&files, cwd.as_deref(), &[Tool::Claude, Tool::Codex]);
            McpConfig { servers: defs.into_iter().map(|d| d.server).collect(), sources, read_at: runwick_core::now() }
        })
        .await
        .map_err(RpcError::internal)
    });
    r.add::<McpCheck, _, _>(|ctx: Ctx, q: CheckQuery| async move {
        let host = ctx.host.clone();
        let cwd = q.cwd.filter(|c| !c.trim().is_empty());
        let r =
            tokio::task::spawn_blocking(move || check_now(&host, q.provider, cwd)).await.map_err(RpcError::internal)?;
        ctx.host.bus.changed(&[TOPIC]);
        Ok(r)
    });
    r.add::<McpLastCheck, _, _>(|_ctx: Ctx, q: CheckQuery| async move {
        let cwd = q.cwd.filter(|c| !c.trim().is_empty());
        Ok(last_result(q.provider, &cwd))
    });
}
