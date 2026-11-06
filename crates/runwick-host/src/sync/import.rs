//! Bringing what other computers changed into this computer's records.
//!
//! Records are applied in short transactions (a few hundred files, or a few thousand messages,
//! each), so a large import never holds the store for long. Each transaction reads this
//! computer's records inside it, so no edit can slip in between reading and writing. Nothing
//! moves the merge base forward here; the engine does that only after the result is committed.

use super::engine::{Blobs, Engine, Failure, at, db, rpc};
use super::format::{self, Prefer, canonical, lost_sides, merge3};
use super::kinds::conflicts::{self, Found, Side};
use super::kinds::{self, Cx, Tally, collab, env, messages, pins, roles, sessions, wiki};
use super::tombstones;
use runwick_core::api::sync::{SyncKind, SyncStep};
use runwick_core::rpc::topic;
use serde_json::Value;

/// Files per transaction.
const BATCH: usize = 200;

pub struct Input<'a> {
    pub delta: &'a [String],
    pub based: &'a Blobs,
    pub remote: &'a Blobs,
    pub origin: Option<&'a str>,
    pub prefer: Prefer,
}

fn blob(m: &Blobs, p: &str) -> Option<Vec<u8>> {
    m.get(p).cloned().flatten()
}

impl Engine<'_> {
    pub(super) fn import(&self, inp: &Input, tally: &mut Tally) -> Result<(), Failure> {
        let cx = self.cx;
        let mut order: Vec<&String> = inp.delta.iter().collect();
        order.sort_by_key(|p| (rank(p), p.as_str()));
        // Records a tombstone points at, as they are at origin (a record re-added there wins).
        let tombs: Vec<String> = order
            .iter()
            .filter(|p| p.starts_with(tombstones::DIR) && blob(inp.remote, p).is_some())
            .filter_map(|p| tombstones::entity_of(p))
            .collect();
        let tomb_targets = match inp.origin {
            Some(o) if !tombs.is_empty() => self.git.read_at(&self.wc.dir, o, &tombs).map_err(at(SyncStep::Import))?,
            _ => Blobs::new(),
        };
        let mut wiki_paths = Vec::new();
        let mut wiki_tombs = Vec::new();
        let mut env_changed = false;
        let mut batch: Vec<&str> = Vec::new();
        for p in &order {
            let p = p.as_str();
            match kinds::kind_of(p) {
                Some(SyncKind::Messages) => {
                    self.import_batch(&mut batch, inp, &tomb_targets, &mut wiki_tombs, tally)?;
                    let Some(bytes) = blob(inp.remote, p) else {
                        continue;
                    };
                    match messages::import(cx.store, p, &bytes) {
                        Ok(n) if n > 0 => {
                            *tally.imported.entry(SyncKind::Messages).or_default() += n;
                            tally.topics.insert(topic::MESSAGES);
                        }
                        Ok(_) => {}
                        Err(e) => tally.skip(p, e),
                    }
                }
                Some(SyncKind::Wiki) => wiki_paths.push(p.to_string()),
                Some(SyncKind::Env) => env_changed = true,
                _ => {
                    batch.push(p);
                    if batch.len() >= BATCH {
                        self.import_batch(&mut batch, inp, &tomb_targets, &mut wiki_tombs, tally)?;
                    }
                }
            }
        }
        self.import_batch(&mut batch, inp, &tomb_targets, &mut wiki_tombs, tally)?;

        // Kinds with their own write paths (files, secret store) run after; each record is
        // applied atomically and a rerun finds finished ones unchanged.
        if env_changed {
            self.import_env(inp, tally)?;
        }
        if !wiki_paths.is_empty() || !wiki_tombs.is_empty() {
            let local = wiki::render(cx).map_err(rpc(SyncStep::Import))?;
            let text = |m: &Blobs, p: &str| blob(m, p).map(|b| String::from_utf8(b).map_err(|_| ()));
            for p in &wiki_paths {
                let Ok(r) = text(inp.remote, p).transpose() else {
                    tally.skip(p, "UTF-8 텍스트가 아니에요");
                    continue;
                };
                let b = text(inp.based, p).and_then(Result::ok);
                wiki::apply(cx, p, b.as_deref(), local.get(p).map(String::as_str), r.as_deref(), tally);
            }
            for (entity, when) in wiki_tombs {
                let Some((project, slug)) = wiki::parse_path(&entity) else {
                    continue;
                };
                if wiki::updated_at(cx, project, &slug).is_some_and(|t| t <= when) {
                    let l = local.get(&entity).map(String::as_str);
                    wiki::apply(cx, &entity, l, l, None, tally);
                }
            }
        }
        Ok(())
    }

    /// One transaction over a batch of small record files.
    fn import_batch(
        &self,
        batch: &mut Vec<&str>,
        inp: &Input,
        tomb_targets: &Blobs,
        wiki_tombs: &mut Vec<(String, runwick_core::Ts)>,
        tally: &mut Tally,
    ) -> Result<(), Failure> {
        if batch.is_empty() {
            return Ok(());
        }
        let cx = self.cx;
        let paths = std::mem::take(batch);
        cx.store
            .tx(|c| {
                // This computer's records, read inside the transaction.
                let shared = kinds::render_shared(cx, c)?;
                for p in &paths {
                    let p = *p;
                    if let Some(kind) = kinds::kind_of(p) {
                        if kinds::is_shared_json(kind) {
                            import_shared(cx, c, kind, p, shared.get(p), blob(inp.based, p), blob(inp.remote, p), inp.prefer, tally);
                            continue;
                        }
                        let Some(bytes) = blob(inp.remote, p) else { continue };
                        let parsed = format::parse(&bytes).ok_or("JSON을 읽지 못했어요".to_string());
                        match kind {
                            SyncKind::Sessions => match parsed.and_then(|v| sessions::apply(cx, c, p, &v)) {
                                Ok(true) => tally.imported(SyncKind::Sessions, &[topic::SESSIONS]),
                                Ok(false) => {}
                                Err(e) => tally.skip(p, e),
                            },
                            SyncKind::Collab => match parsed.and_then(|v| collab::apply(cx, c, &v)) {
                                Ok(true) => tally.imported(SyncKind::Collab, &[topic::COLLAB]),
                                Ok(false) => {}
                                Err(e) => tally.skip(p, e),
                            },
                            _ => {}
                        }
                    } else if p.starts_with(conflicts::RESOLVED) {
                        let Some(bytes) = blob(inp.remote, p) else { continue };
                        match format::parse(&bytes).ok_or("JSON을 읽지 못했어요".to_string()).and_then(|v| conflicts::apply_resolution(c, &v)) {
                            Ok(new) => {
                                if new {
                                    tally.topics.insert(topic::SYNC);
                                }
                            }
                            Err(e) => tally.skip(p, e),
                        }
                    } else if p.starts_with(conflicts::DIR) {
                        // Another computer's conflict: listed under '겹친 변경' (it changes nothing here).
                        let Some(bytes) = blob(inp.remote, p) else { continue };
                        match format::parse(&bytes).ok_or("JSON을 읽지 못했어요".to_string()).and_then(|v| conflicts::apply(cx, c, &v, |path| blob(inp.based, path))) {
                            Ok(Some((kind, title, true))) => {
                                tally.topics.insert(topic::SYNC);
                                let more = if kind == SyncKind::Wiki { " · 이전 버전은 문서 기록에도 있어요" } else { "" };
                                tally.conflict(kind, title, format!("다른 컴퓨터의 변경으로 바뀌었어요 · 이 컴퓨터 내용은 '겹친 변경'에 있어요{more}"));
                            }
                            Ok(new) => {
                                if new.is_some() {
                                    tally.topics.insert(topic::SYNC);
                                }
                            }
                            Err(e) => tally.skip(p, e),
                        }
                    } else if let Some(entity) = tombstones::entity_of(p) {
                        let Some(when) = blob(inp.remote, p).and_then(|b| tombstones::deleted_at(&b)) else { continue };
                        if blob(tomb_targets, &entity).is_some() {
                            continue;
                        }
                        let kind = kinds::kind_of(&entity);
                        let local_at = match kind {
                            Some(SyncKind::Pins) => kinds::uuid_at(&entity, 1).and_then(|id| pins::updated_at(c, id).ok().flatten()),
                            Some(SyncKind::Roles) => kinds::uuid_at(&entity, 1).and_then(|id| roles::updated_at(c, id).ok().flatten()),
                            Some(SyncKind::Wiki) => {
                                wiki_tombs.push((entity, when));
                                continue;
                            }
                            _ => None,
                        };
                        let Some(local_at) = local_at.as_deref().and_then(format::parse_ts) else { continue };
                        if let Some(kind) = kind
                            && local_at <= when
                        {
                            match kinds::apply_shared(cx, c, kind, &entity, None) {
                                Ok(()) => tally.imported(kind, topics_of(kind)),
                                Err(e) => tally.skip(&entity, e),
                            }
                        }
                    }
                }
                Ok(())
            })
            .map_err(db(SyncStep::Import))?;
        kinds::breathe();
        Ok(())
    }

    fn import_env(&self, inp: &Input, tally: &mut Tally) -> Result<(), Failure> {
        let cx = self.cx;
        let local = env::render(cx).map_err(rpc(SyncStep::Import))?;
        let raw = blob(inp.remote, env::PATH);
        let r = raw.as_deref().and_then(format::parse).map(|mut v| {
            format::clamp_future(&mut v);
            v
        });
        if raw.is_some() && r.is_none() {
            tally.skip(env::PATH, "JSON을 읽지 못했어요");
            return Ok(());
        }
        let b = blob(inp.based, env::PATH).and_then(|b| format::parse(&b));
        let Some(merged) = merge3(b.as_ref(), Some(&local), r.as_ref(), inp.prefer) else {
            return Ok(());
        };
        // No '겹친 변경' for variables: nothing from another computer changes them here without
        // review, and a value waiting here is written back unchanged, so no value is lost.
        if merged != local {
            let device = inp
                .origin
                .map(|o| self.git.author_of(&self.wc.dir, o, env::PATH))
                .unwrap_or_else(|| (None, "다른 컴퓨터".into()));
            env::apply(cx, &local, &merged, &device, tally);
        }
        Ok(())
    }

    /// Record the conflicts found by the import, with who wrote the other side.
    pub(super) fn record_conflicts(&self, origin: Option<&str>, tally: &mut Tally) -> Result<(), Failure> {
        let found = std::mem::take(&mut tally.found);
        for f in &found {
            let file = f.path.split('#').next().unwrap_or(&f.path);
            let author = match origin {
                Some(o) => self.git.author_of(&self.wc.dir, o, file),
                None => (None, "다른 컴퓨터".into()),
            };
            self.cx.store.tx(|c| conflicts::record(self.cx, c, f, &author)).map_err(db(SyncStep::Import))?;
        }
        if !found.is_empty() {
            tally.topics.insert(topic::SYNC);
        }
        Ok(())
    }
}

#[allow(clippy::too_many_arguments)]
fn import_shared(
    cx: &Cx,
    c: &rusqlite::Connection,
    kind: SyncKind,
    path: &str,
    local: Option<&Value>,
    base: Option<Vec<u8>>,
    remote: Option<Vec<u8>>,
    prefer: Prefer,
    tally: &mut Tally,
) {
    let r = match remote {
        Some(bytes) => match format::parse(&bytes) {
            Some(mut v) => {
                format::clamp_future(&mut v);
                Some(v)
            }
            None => return tally.skip(path, "JSON을 읽지 못했어요"),
        },
        None => None,
    };
    let b = base.and_then(|b| format::parse(&b));
    let merged = merge3(b.as_ref(), local, r.as_ref(), prefer);
    // Content either side changed that the merge did not keep is recorded, never dropped.
    if let (Some(l), Some(rv), Some(m)) = (local, r.as_ref(), merged.as_ref())
        && l != rv
        && kind != SyncKind::Prefs
    {
        let (lost_l, lost_r) = lost_sides(b.as_ref(), l, rv, m);
        let title = kinds::title_of(kind, l);
        for (lost, side, v) in [(lost_l, Side::Local, l), (lost_r, Side::Remote, rv)] {
            if lost {
                tally.found.push(Found {
                    kind,
                    path: path.to_string(),
                    title: title.clone(),
                    lost: side,
                    content: canonical(v),
                });
            }
        }
        if lost_l || lost_r {
            let kept = if lost_r { "이 컴퓨터 쪽을 뒀어요" } else { "다른 컴퓨터 쪽을 뒀어요" };
            tally.conflict(
                kind,
                title,
                format!("두 컴퓨터에서 같은 항목을 바꿔서 {kept} · 나머지 내용은 '겹친 변경'에 있어요"),
            );
        }
    }
    if merged.as_ref() == local {
        return;
    }
    match kinds::apply_shared(cx, c, kind, path, merged.as_ref()) {
        Ok(()) => tally.imported(kind, topics_of(kind)),
        Err(e) => tally.skip(path, e),
    }
}

pub fn topics_of(kind: SyncKind) -> &'static [&'static str] {
    match kind {
        SyncKind::Projects => &[topic::PROJECTS, topic::WORKSPACES],
        SyncKind::Sessions => &[topic::SESSIONS],
        SyncKind::Messages => &[topic::MESSAGES],
        SyncKind::Pins => &[topic::PINS],
        SyncKind::Wiki => &[topic::WIKI],
        SyncKind::Timeline => &[topic::TIMELINE],
        SyncKind::Roles => &[runwick_core::api::roles::TOPIC],
        SyncKind::Env => &[topic::ENV],
        SyncKind::Prefs => &[topic::SETTINGS],
        SyncKind::Collab => &[topic::COLLAB],
    }
}

/// Import order: projects before what belongs to them, tombstones last.
fn rank(path: &str) -> u8 {
    match kinds::kind_of(path) {
        Some(SyncKind::Projects) => 0,
        Some(SyncKind::Sessions) => 1,
        Some(SyncKind::Messages) => 2,
        Some(SyncKind::Collab) => 3,
        Some(_) => 4,
        None => 5,
    }
}
