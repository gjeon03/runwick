//! Collaboration settings (Settings › 협업) and the short guidance agent CLIs get at launch so
//! they know the `runwick` command exists.

use crate::host::Host;
use crate::store::{Store, db_err};
use runwick_core::api::collab::CollabSettings;
use runwick_core::api::sessions::{Session, Tool};
use runwick_core::rpc::RpcError;
use std::sync::Arc;

const KEY: &str = "collab.settings";
pub const MAX_LIMIT: u32 = 16;

/// Given to Claude Code at launch (`--append-system-prompt`) and to a delegated Codex worker in
/// front of its first instruction (a Codex `-c` flag would turn off its shared background server).
pub const GUIDANCE: &str = "You are running inside Runwick, a workroom that can run other AI coding CLIs next to you. \
Delegate only when it clearly helps (independent work, a second opinion, a review); otherwise do the work yourself.
- runwick delegate --tool codex|claude [--role <role>] [--title <title>] [--split <new-branch>] \"<task>\"  starts a worker and prints its id (--split works in a separate git worktree)
- runwick wait <id> [--timeout <seconds>]  waits until the worker finished its turn and prints its answer; a timeout is not a failure
- runwick send <id> \"<message>\" [--now]  gives a worker a follow-up (queued while it is busy)
- runwick workers | runwick result <id> | runwick status
If another session gave you this work, end it with: runwick report --summary \"<what you did>\" [--file <path>]... [--check \"<command>=pass|fail\"]... [--not-done \"<item>\"]...
A worker's answer is not verified work: check its result before relying on it.";

pub fn load(store: &Store) -> CollabSettings {
    crate::meta::kv_get(store, KEY)
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_value::<CollabSettings>(v).ok())
        .map(normalize)
        .unwrap_or_default()
}

pub fn save(store: &Store, s: CollabSettings) -> Result<CollabSettings, RpcError> {
    if s.auto_limit == 0 || s.auto_limit > MAX_LIMIT {
        return Err(RpcError::invalid(format!("자동 배정 한도는 1에서 {MAX_LIMIT} 사이로 정해 주세요")));
    }
    let s = normalize(s);
    let v = serde_json::to_value(&s).map_err(RpcError::internal)?;
    crate::meta::kv_put(store, KEY, &v).map_err(db_err)?;
    Ok(s)
}

fn normalize(mut s: CollabSettings) -> CollabSettings {
    s.auto_limit = s.auto_limit.clamp(1, MAX_LIMIT);
    s.allowed_tools.retain(|t| t.is_agent());
    s.allowed_tools.dedup();
    s
}

/// Install the launch hook: Claude Code gets [`GUIDANCE`] while the setting is on. Codex gets it
/// with its first delegated instruction instead (`engine::delivery_text`).
pub fn install(host: &Arc<Host>) {
    host.sessions.set_guidance_hook(Arc::new(|host: &Host, s: &Session| {
        (s.tool == Tool::Claude && load(&host.store).guidance).then(|| GUIDANCE.to_string())
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_default_validate_and_persist() {
        let store = Store::open_in_memory().unwrap();
        assert_eq!(load(&store), CollabSettings::default());
        let bad = CollabSettings { auto_limit: 0, ..Default::default() };
        assert!(save(&store, bad).is_err());
        let s = CollabSettings { guidance: false, auto_limit: 5, allowed_tools: vec![Tool::Codex, Tool::Shell] };
        let saved = save(&store, s).unwrap();
        assert_eq!(saved.allowed_tools, vec![Tool::Codex]);
        assert_eq!(load(&store), saved);
        assert!(GUIDANCE.contains("runwick report") && GUIDANCE.len() < 1600);
    }
}
