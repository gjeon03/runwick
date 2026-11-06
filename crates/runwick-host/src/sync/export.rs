//! Writing this computer's records into the working copy (only files that changed are
//! rewritten; another computer's files are never written here).

use super::engine::{Engine, Failure, db, io_err, rpc};
use super::format::{self, canonical};
use super::kinds::conflicts;
use super::kinds::{self, Tally, collab, env, messages, sessions, wiki};
use super::{state, tombstones};
use runwick_core::api::sync::{SyncKind, SyncStep};
use serde_json::Value;
use std::collections::BTreeSet;

/// What export decided, saved once it is committed.
#[derive(Default)]
pub struct Pending {
    pub marks: Vec<(String, String)>,
    pub deleted: Vec<(String, String, String)>,
}

impl Engine<'_> {
    fn put(&self, path: &str, text: &str) -> Result<bool, Failure> {
        self.wc.write(path, text.as_bytes()).map_err(io_err(SyncStep::Export))
    }

    fn remove(&self, path: &str) -> Result<bool, Failure> {
        self.wc.remove(path).map_err(io_err(SyncStep::Export))
    }

    pub(super) fn export(&self, tally: &mut Tally) -> Result<Pending, Failure> {
        let cx = self.cx;
        let step = SyncStep::Export;
        let mut pending = Pending::default();
        let mut live: BTreeSet<String> = BTreeSet::new();

        let shared = cx.store.with(|c| kinds::render_shared(cx, c)).map_err(db(step))?;
        for (p, v) in &shared {
            if self.put(p, &canonical(v))? {
                tally.exported(kinds::kind_of(p).unwrap_or(SyncKind::Projects));
            }
            live.insert(p.clone());
        }
        let env_value = env::render(cx).map_err(rpc(step))?;
        if self.put(env::PATH, &canonical(&env_value))? {
            tally.exported(SyncKind::Env);
        }
        // Plain values now in the repository stay in its history; remember which.
        let plain: Vec<String> = env_value
            .get("vars")
            .and_then(Value::as_object)
            .map(|m| {
                m.iter()
                    .filter(|(_, v)| v.get("value").and_then(Value::as_str).is_some_and(|s| !s.is_empty()))
                    .map(|(k, _)| k.clone())
                    .collect()
            })
            .unwrap_or_default();
        state::expose(cx.store, &plain).map_err(db(step))?;
        for (p, text) in wiki::render(cx).map_err(rpc(step))? {
            if self.put(&p, &text)? {
                tally.exported(SyncKind::Wiki);
            }
            live.insert(p);
        }
        for (p, v) in cx.store.with(|c| sessions::render_own(cx, c)).map_err(db(step))? {
            if self.put(&p, &canonical(&v))? {
                tally.exported(SyncKind::Sessions);
            }
        }
        let marks = state::marks(cx.store).map_err(db(step))?;
        for (p, sig, session) in cx.store.with(messages::own).map_err(db(step))? {
            if marks.get(&p) == Some(&sig) && self.wc.exists(&p) {
                continue;
            }
            let text = cx.store.with(|c| messages::render(c, session)).map_err(db(step))?;
            if self.put(&p, &text)? {
                tally.exported(SyncKind::Messages);
            }
            pending.marks.push((p, sig));
        }
        for (p, v) in cx.store.with(collab::render_own).map_err(db(step))? {
            if self.put(&p, &canonical(&v))? {
                tally.exported(SyncKind::Collab);
            }
        }
        for (p, v) in cx.store.with(|c| conflicts::render_own(cx, c)).map_err(db(step))? {
            self.put(&p, &canonical(&v))?;
        }

        // Records removed here: drop their files and leave a tombstone.
        for entry in state::deleted(cx.store).map_err(db(step))? {
            let (kind, key, deleted_at) = &entry;
            if let Some(entity) = tombstones::entity_of_deleted(kind, key)
                && !live.contains(&entity)
                && self.remove(&entity)?
            {
                tally.exported(kinds::kind_of(&entity).unwrap_or(SyncKind::Pins));
                if let Some(t) = tombstones::for_entity(&entity)
                    && !self.wc.exists(&t)
                {
                    self.put(&t, &tombstones::render(deleted_at, &cx.device.name))?;
                }
            }
            pending.deleted.push(entry);
        }
        // A record that exists again needs no tombstone.
        for p in &live {
            if let Some(t) = tombstones::for_entity(p) {
                self.remove(&t)?;
            }
        }
        if !self.wc.exists(format::README) {
            self.put(format::README, format::README_TEXT)?;
        }
        Ok(pending)
    }
}
