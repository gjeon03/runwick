//! Conversation records (no AI): collection from the CLIs' transcripts, keyword search, pins
//! and context reading.
//!
//! - `parse`: transcript lines → messages; `locate`: where transcripts live
//! - `capture`: cursor per session, background following of live runs
//! - `db`: message/cursor tables and reads; `search`: the index; `pins`: pins
//!
//! For other features (call from a blocking context or keep it short — the store is shared):
//! - [`messages_since`]: a project's messages written after a time (wiki, summaries)
//! - [`turn_info`]: newest answer and turn end of a session (collaboration)
//! - [`session_messages`]: the newest messages of one session (wiki drafts)

mod capture;
mod db;
mod locate;
mod parse;
mod pins;
mod search;

use crate::host::{Ctx, Host};
use crate::router::Router;
use crate::store::{Migration, Store, db_err};
use capture::Collector;
use runwick_core::Ts;
use runwick_core::api::projects::{ById, ProjectId};
use runwick_core::api::records::*;
use runwick_core::api::sessions::SessionId;
use runwick_core::rpc::{RpcError, topic};
use std::sync::{Arc, OnceLock};

/// Index triggers (also recreated after a rebuild, see `search::reindex`).
macro_rules! fts_triggers {
    () => {
        "CREATE TRIGGER messages_ai AFTER INSERT ON messages BEGIN
             INSERT INTO messages_fts (rowid, text) VALUES (new.rid, new.text);
         END;
         CREATE TRIGGER messages_ad AFTER DELETE ON messages BEGIN
             INSERT INTO messages_fts (messages_fts, rowid, text) VALUES ('delete', old.rid, old.text);
         END;
         CREATE TRIGGER messages_au AFTER UPDATE OF text ON messages BEGIN
             INSERT INTO messages_fts (messages_fts, rowid, text) VALUES ('delete', old.rid, old.text);
             INSERT INTO messages_fts (rowid, text) VALUES (new.rid, new.text);
         END;"
    };
}

pub(crate) const FTS_TRIGGERS: &str = fts_triggers!();

pub const MIGRATIONS: &[Migration] = &[(
    "records_1",
    concat!(
        "CREATE TABLE messages (
            rid INTEGER PRIMARY KEY,
            id BLOB NOT NULL UNIQUE,
            session_id BLOB NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
            project_id BLOB NOT NULL,
            workspace_id BLOB NOT NULL,
            -- Identity inside the transcript; the same line read twice is stored once.
            provider_msg_id TEXT NOT NULL,
            seq INTEGER NOT NULL,
            role TEXT NOT NULL,
            text TEXT NOT NULL,
            at TEXT NOT NULL,
            collected_at TEXT NOT NULL,
            sidechain INTEGER NOT NULL DEFAULT 0,
            tool_name TEXT,
            UNIQUE (session_id, provider_msg_id)
        );
        CREATE INDEX messages_session_seq ON messages(session_id, seq);
        CREATE INDEX messages_project_at ON messages(project_id, at);
        CREATE INDEX messages_at ON messages(at);
        CREATE VIRTUAL TABLE messages_fts USING fts5(text, content='messages', content_rowid='rid',
            tokenize='trigram case_sensitive 0');
        ",
        fts_triggers!(),
        "
        CREATE TABLE record_sources (
            session_id BLOB PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
            conv_id TEXT,
            path TEXT,
            offset INTEGER NOT NULL DEFAULT 0,
            head_len INTEGER NOT NULL DEFAULT 0,
            head_hash TEXT,
            next_seq INTEGER NOT NULL DEFAULT 0,
            state TEXT NOT NULL,
            last_error TEXT,
            last_collected_at TEXT,
            last_checked_at TEXT,
            turn_ended_at TEXT
        );
        -- No foreign keys: a pin keeps its snapshot when the message or session is gone.
        CREATE TABLE pins (
            id BLOB PRIMARY KEY,
            kind TEXT NOT NULL,
            title TEXT NOT NULL,
            note TEXT NOT NULL DEFAULT '',
            text TEXT NOT NULL,
            message_id BLOB,
            session_id BLOB,
            project_id BLOB,
            session_title TEXT NOT NULL DEFAULT '',
            tool TEXT,
            source_at TEXT NOT NULL,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );
        CREATE INDEX pins_message ON pins(message_id);
        CREATE INDEX pins_project ON pins(project_id, created_at);
        CREATE INDEX pins_created ON pins(created_at);"
    ),
)];

static COLLECTOR: OnceLock<Arc<Collector>> = OnceLock::new();

async fn collector() -> Arc<Collector> {
    if let Some(c) = COLLECTOR.get() {
        return c.clone();
    }
    // Reading the login environment may wait for the login shell once.
    let roots = tokio::task::spawn_blocking(locate::Roots::from_login_env).await.unwrap_or_else(|_| locate::Roots {
        claude: runwick_core::paths::user_home().join(".claude").join("projects"),
        codex: runwick_core::paths::user_home().join(".codex").join("sessions"),
    });
    COLLECTOR.get_or_init(|| Arc::new(Collector::new(roots))).clone()
}

/// Start background collection (called once after the host starts).
pub fn boot(host: &Arc<Host>) {
    let host = host.clone();
    tokio::spawn(async move {
        let c = collector().await;
        capture::run(host, c).await;
    });
}

async fn blocking<T: Send + 'static>(
    host: &Arc<Host>,
    f: impl FnOnce(&Store) -> Result<T, RpcError> + Send + 'static,
) -> Result<T, RpcError> {
    let h = host.clone();
    tokio::task::spawn_blocking(move || f(&h.store)).await.map_err(RpcError::internal)?
}

pub fn register(r: &mut Router) {
    r.add::<RecordsStatus, _, _>(|ctx: Ctx, q: StatusQuery| async move {
        let running = ctx.host.sessions.running_ids();
        let f = db::StatusFilter { project_id: q.project_id, session_id: q.session_id };
        blocking(&ctx.host, move |s| s.with(|c| db::statuses(c, &f, &running)).map_err(db_err)).await
    });
    r.add::<RecordsSearch, _, _>(|ctx: Ctx, q: SearchQuery| async move {
        let host = ctx.host.clone();
        let out = blocking(&host, move |s| search::search(s, &q).map_err(db_err)).await?;
        if out.index_error.is_some() && search::begin_reindex() {
            let h = host.clone();
            tokio::task::spawn_blocking(move || match search::reindex(&h.store) {
                Ok(()) => tracing::info!("search index rebuilt"),
                Err(e) => tracing::error!("search index rebuild failed: {e}"),
            });
        }
        Ok(out)
    });
    r.add::<RecordsContext, _, _>(|ctx: Ctx, q: ContextQuery| async move {
        blocking(&ctx.host, move |s| {
            s.with(|c| db::context(c, &q))
                .map_err(db_err)?
                .ok_or_else(|| RpcError::not_found("기록에서 찾을 수 없어요"))
        })
        .await
    });
    r.add::<RecordsTurn, _, _>(
        |ctx: Ctx, p: ById| async move { blocking(&ctx.host, move |s| turn_info(s, p.id)).await },
    );
    r.add::<RecordsCollect, _, _>(|ctx: Ctx, p: ById| async move {
        let host = ctx.host.clone();
        let running = host.sessions.running_ids().contains(&p.id);
        let c = collector().await;
        let out = capture::pass(&host, &c, vec![(p.id, running)], true).await;
        tracing::debug!(session = %p.id, added = out.added, "collected on request");
        let f = db::StatusFilter { project_id: None, session_id: Some(p.id) };
        blocking(&host, move |s| s.with(|c| db::statuses(c, &f, &[])).map_err(db_err))
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| RpcError::not_found("세션을 찾을 수 없어요"))
    });
    r.add::<PinsList, _, _>(|ctx: Ctx, q: PinQuery| async move {
        blocking(&ctx.host, move |s| s.with(|c| pins::list(c, &q, None)).map_err(db_err)).await
    });
    r.add::<PinsCreate, _, _>(|ctx: Ctx, p: NewPin| async move {
        let (pin, created) = blocking(&ctx.host, move |s| pins::create(s, &p)).await?;
        if created {
            ctx.host.bus.changed(&[topic::PINS, topic::TIMELINE]);
        }
        Ok(pin)
    });
    r.add::<PinsUpdate, _, _>(|ctx: Ctx, u: PinUpdate| async move {
        let pin = blocking(&ctx.host, move |s| pins::update(s, &u)).await?;
        ctx.host.bus.changed(&[topic::PINS, topic::TIMELINE]);
        Ok(pin)
    });
    r.add::<PinsRemove, _, _>(|ctx: Ctx, p: ById| async move {
        if blocking(&ctx.host, move |s| pins::remove(s, p.id)).await? {
            ctx.host.bus.changed(&[topic::PINS, topic::TIMELINE]);
        }
        Ok(())
    });
}

/// Messages of a project written after `since` (by the CLI's own timestamps), oldest first,
/// at most `limit`. To continue, pass the `at` of the last message returned. Blocking.
pub fn messages_since(store: &Store, project_id: ProjectId, since: Ts, limit: usize) -> Result<Vec<Message>, RpcError> {
    store.with(|c| db::since(c, project_id, &since, limit)).map_err(db_err)
}

/// The newest `limit` messages of a session (all roles; tool rows are "도구 사용: …"), oldest
/// first. Blocking.
pub fn session_messages(store: &Store, session_id: SessionId, limit: usize) -> Result<Vec<Message>, RpcError> {
    store.with(|c| db::session_tail(c, session_id, limit)).map_err(db_err)
}

/// The newest assistant answer of a session and when its newest turn ended. Blocking.
pub fn turn_info(store: &Store, session_id: SessionId) -> Result<TurnInfo, RpcError> {
    store.with(|c| db::turn_info(c, session_id)).map_err(db_err)
}

/// Whether the CLI saved the session's conversation, so `--resume` can continue it: its
/// messages are in the records, or (before they are collected) Claude's transcript exists.
/// Every Claude session gets its id at launch, so an id alone does not mean anything was
/// saved. A Codex thread id is only known once its file exists. Blocking.
pub fn conversation_saved(store: &Store, s: &runwick_core::api::sessions::Session) -> bool {
    use runwick_core::api::sessions::Tool;
    let Some(id) = s.provider_conv_id.as_deref() else { return false };
    if session_messages(store, s.id, 1).is_ok_and(|m| !m.is_empty()) {
        return true;
    }
    match s.tool {
        Tool::Claude => locate::claude_transcript(&locate::Roots::from_login_env().claude, &s.cwd, id, true).is_some(),
        Tool::Codex => true,
        Tool::Shell => false,
    }
}

/// Pins of a project, newest first (for timeline and handover).
pub fn project_pins(store: &Store, project_id: ProjectId, limit: u32) -> Result<Vec<Pin>, RpcError> {
    let q = PinQuery { project_id: Some(project_id), session_id: None };
    store.with(|c| pins::list(c, &q, Some(limit))).map_err(db_err)
}

#[cfg(test)]
mod tests;
