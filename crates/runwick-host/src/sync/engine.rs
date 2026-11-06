//! One sync: fetch → import what other computers changed → write this computer's records →
//! commit → merge (keeping our tree, which already contains theirs) → check → push, retrying
//! when another computer pushed at the same moment.
//!
//! Change detection is three-way per file: `base` is the origin commit whose changes are in
//! this computer's records *and committed in the working copy* (it moves only after that commit),
//! `remote` is the fetched origin, `local` is what this computer's records would write now. Only
//! files that changed on the remote since `base` (plus files that could not be imported before)
//! are imported, so git never has to merge text.
//!
//! Before pushing, the tree is compared with the remote: a file another computer wrote may only
//! disappear with a tombstone saying so. Anything else is put back from the remote first.

use super::export::Pending;
use super::format::{self, Manifest, Prefer, content_of};
use super::git::{Git, GitError, HOST_TRAILER, LOCAL, NETWORK};
use super::import::Input;
use super::kinds::{self, Cx, Tally};
use super::{state, tombstones};
use runwick_core::api::sync::{FailKind, RepoContent, SyncConflict, SyncKind, SyncReport, SyncStep};
use runwick_core::rpc::topic;
use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

pub use super::wc::WorkingCopy;

const ATTEMPTS: u32 = 3;

pub type Blobs = BTreeMap<String, Option<Vec<u8>>>;

/// Why a sync stopped, and at which step.
#[derive(Debug, Clone)]
pub struct Failure {
    pub step: SyncStep,
    pub error: GitError,
}

pub struct Outcome {
    pub report: SyncReport,
    pub topics: BTreeSet<&'static str>,
    /// Records fingerprint as exported (changes after it are still pending).
    pub fingerprint: Option<String>,
}

pub struct Engine<'a> {
    pub cx: &'a Cx<'a>,
    pub git: &'a Git,
    pub wc: &'a WorkingCopy,
    /// Told when each step starts.
    pub step: &'a (dyn Fn(SyncStep) + Sync),
    /// Stop (before the next step, and network commands are cut) when this passes. The
    /// records stay consistent and the unfinished part is picked up by the next sync.
    pub deadline: Option<Instant>,
}

pub fn at(step: SyncStep) -> impl Fn(GitError) -> Failure {
    move |error| Failure { step, error }
}

pub fn db(step: SyncStep) -> impl Fn(rusqlite::Error) -> Failure {
    move |e| Failure { step, error: GitError::other(format!("기록 저장소 오류: {e}")) }
}

pub fn rpc(step: SyncStep) -> impl Fn(runwick_core::RpcError) -> Failure {
    move |e| Failure { step, error: GitError::other(e.message) }
}

pub fn io_err(step: SyncStep) -> impl Fn(std::io::Error) -> Failure {
    move |e| Failure { step, error: GitError::other(format!("작업 사본에 쓰지 못했어요: {e}")) }
}

impl Engine<'_> {
    /// Start `step`, unless the deadline has passed.
    fn begin(&self, step: SyncStep) -> Result<(), Failure> {
        if self.deadline.is_some_and(|d| Instant::now() >= d) {
            return Err(Failure {
                step,
                error: GitError {
                    kind: FailKind::Network,
                    message: "정해진 시간 안에 동기화를 마치지 못해 멈췄어요. 남은 변경은 다음 동기화 때 올려요".into(),
                },
            });
        }
        (self.step)(step);
        Ok(())
    }

    /// Time limit for a network command: never past the deadline.
    fn limit(&self) -> Duration {
        match self.deadline {
            Some(d) => NETWORK.min(d.saturating_duration_since(Instant::now())).max(Duration::from_millis(500)),
            None => NETWORK,
        }
    }

    fn remote_ref(&self) -> String {
        format!("refs/remotes/origin/{}", self.wc.branch)
    }

    fn store(&self) -> &crate::store::Store {
        self.cx.store
    }

    pub fn run(&self) -> Result<Outcome, Failure> {
        let mut tally = Tally::default();
        self.clean_leftovers().map_err(at(SyncStep::Fetch))?;
        // After a backup restore the restored records win once.
        let prefer = if state::restored(self.store()) { Prefer::Local } else { Prefer::Newer };
        let mut commit = None;
        let mut attempts = 0;
        let mut fingerprint = None;
        for attempt in 1..=ATTEMPTS {
            attempts = attempt;
            self.begin(SyncStep::Fetch)?;
            self.git
                .ok(Some(&self.wc.dir), &["fetch", "-q", "--prune", "origin"], self.limit())
                .map_err(at(SyncStep::Fetch))?;
            let origin = self.git.rev(&self.wc.dir, &self.remote_ref()).map_err(at(SyncStep::Fetch))?;

            self.begin(SyncStep::Import)?;
            self.check_format(origin.as_deref())?;
            let base = match state::base(self.store()) {
                Some(b) if self.git.rev(&self.wc.dir, &b).map_err(at(SyncStep::Import))?.is_some() => Some(b),
                _ => None,
            };
            let mut delta = self.delta(base.as_deref(), origin.as_deref())?;
            // Files that could not be imported before are read again (with no base: their
            // record may be importable now).
            let retried: Vec<String> = match &origin {
                Some(_) => state::skips(self.store())
                    .map_err(db(SyncStep::Import))?
                    .into_iter()
                    .filter(|p| super::wc::safe_path(p) && !delta.contains(p))
                    .collect(),
                None => Vec::new(),
            };
            delta.extend(retried.iter().cloned());
            // Entries Runwick never reads (links, submodules, names that collide on macOS) and
            // files too large to read are left alone and reported; they never stop the sync.
            let odd: Vec<String> = match &origin {
                Some(o) => {
                    let odd = self.git.tree_problems(&self.wc.dir, o).map_err(at(SyncStep::Import))?;
                    for (p, why) in &odd {
                        if delta.contains(p) {
                            tally
                                .skipped
                                .push(format!("{p}: 읽지 않았어요 ({why}) · 이 컴퓨터가 올리는 저장소에서는 빠져요"));
                        }
                    }
                    odd.into_iter().map(|(p, _)| p).collect()
                }
                None => Vec::new(),
            };
            delta.retain(|p| !odd.contains(p));
            if let Some(o) = &origin {
                for p in self.git.too_big(&self.wc.dir, o, &delta).map_err(at(SyncStep::Import))? {
                    tally.skip(&p, format!("{}MB보다 커서 읽지 않았어요", super::git::MAX_BLOB >> 20));
                    delta.retain(|q| q != &p);
                }
            }
            let remote = match &origin {
                Some(o) => self.git.read_at(&self.wc.dir, o, &delta).map_err(at(SyncStep::Import))?,
                None => Blobs::new(),
            };
            let mut based = match &base {
                Some(b) => self.git.read_at(&self.wc.dir, b, &delta).map_err(at(SyncStep::Import))?,
                None => Blobs::new(),
            };
            for p in &retried {
                based.remove(p);
            }
            let head_manifest =
                self.wc.read(format::MANIFEST).and_then(|b| serde_json::from_slice::<Manifest>(&b).ok());
            let inp = Input { delta: &delta, based: &based, remote: &remote, origin: origin.as_deref(), prefer };
            self.import(&inp, &mut tally)?;
            self.record_conflicts(origin.as_deref(), &mut tally)?;
            self.apply_delta(&delta, &remote)?;

            self.begin(SyncStep::Export)?;
            fingerprint = state::fingerprint(self.store()).ok();
            let before: u64 = tally.exported.values().sum();
            let pending = self.export(&mut tally)?;
            let wrote = tally.exported.values().sum::<u64>() > before;
            self.write_manifest(head_manifest, wrote)?;

            self.begin(SyncStep::Commit)?;
            self.commit(&tally, origin.as_deref())?;
            self.saved(&pending, origin.as_deref(), &delta, &tally)?;
            let head = self.git.rev(&self.wc.dir, "HEAD").map_err(at(SyncStep::Commit))?;
            if let (Some(o), Some(h)) = (&origin, &head)
                && !self.git.is_ancestor(&self.wc.dir, o, h).map_err(at(SyncStep::Commit))?
            {
                self.merge_ours(o)?;
            }

            self.begin(SyncStep::Push)?;
            let Some(mut head) = self.git.rev(&self.wc.dir, "HEAD").map_err(at(SyncStep::Push))? else {
                break;
            };
            if origin.as_deref() == Some(head.as_str()) {
                commit = Some(head);
                break;
            }
            if let Some(o) = &origin {
                let kept = self.keep_others(o)?;
                if kept > 0 {
                    tally.conflicts.push(SyncConflict {
                        kind: SyncKind::Sessions,
                        title: format!("다른 컴퓨터의 기록 {kept}개"),
                        resolution: "이 컴퓨터가 지우지 않도록 저장소의 것을 그대로 두었어요".into(),
                    });
                    head = self.git.rev(&self.wc.dir, "HEAD").map_err(at(SyncStep::Push))?.unwrap_or(head);
                }
            }
            let spec = format!("HEAD:refs/heads/{}", self.wc.branch);
            let out = self
                .git
                .raw(Some(&self.wc.dir), &["push", "-q", "origin", &spec], None, self.limit())
                .map_err(at(SyncStep::Push))?;
            if out.ok {
                state::set_base(self.store(), Some(&head)).map_err(db(SyncStep::Push))?;
                // Keep the remote-tracking ref in step without another fetch.
                let _ = self.git.raw(Some(&self.wc.dir), &["update-ref", &self.remote_ref(), &head], None, LOCAL);
                // Pack objects now and then (never in the middle of a sync, never on a deadline).
                if self.deadline.is_none() {
                    let _ =
                        self.git.raw(Some(&self.wc.dir), &["-c", "gc.auto=256", "gc", "--auto", "-q"], None, NETWORK);
                }
                commit = Some(head);
                break;
            }
            let rejected = ["rejected", "non-fast-forward", "fetch first"].iter().any(|n| out.stderr.contains(n));
            if !rejected {
                return Err(Failure { step: SyncStep::Push, error: super::git::classify(&out.stderr) });
            }
            if attempt == ATTEMPTS {
                return Err(Failure {
                    step: SyncStep::Push,
                    error: GitError {
                        kind: FailKind::Other,
                        message: "다른 컴퓨터가 계속 같은 저장소에 올리고 있어 멈췄어요. 잠시 뒤 다시 동기화해 주세요"
                            .into(),
                    },
                });
            }
            tracing::info!(attempt, "sync push raced with another computer; retrying");
        }
        let report = SyncReport {
            finished_at: runwick_core::now(),
            imported: Tally::counts(&tally.imported),
            exported: Tally::counts(&tally.exported),
            conflicts: tally.conflicts.clone(),
            skipped: tally.skipped.clone(),
            commit: commit.map(|c| c.chars().take(8).collect()),
            attempts,
        };
        let mut topics = tally.topics.clone();
        topics.insert(topic::SYNC);
        Ok(Outcome { report, topics, fingerprint })
    }

    /// After the commit: everything the remote had is in HEAD, so the next sync starts from
    /// it; files that could not be imported are kept for the next try; the restore preference
    /// has been used.
    fn saved(&self, pending: &Pending, origin: Option<&str>, delta: &[String], tally: &Tally) -> Result<(), Failure> {
        let store = self.store();
        let step = SyncStep::Commit;
        state::set_marks(store, &pending.marks).map_err(db(step))?;
        state::clear_deleted(store, &pending.deleted).map_err(db(step))?;
        if let Some(o) = origin {
            state::set_base(store, Some(o)).map_err(db(step))?;
            state::update_skips(store, delta, &tally.skip_paths).map_err(db(step))?;
        }
        state::clear_restored(store).map_err(db(step))?;
        Ok(())
    }

    /// Undo whatever an interrupted sync left in the working copy. Nothing there is lost: the
    /// merge base only moves after a commit, so received files are fetched and applied again.
    fn clean_leftovers(&self) -> Result<(), GitError> {
        // Only one sync runs at a time, so a lock file here is left from a stopped process.
        let _ = std::fs::remove_file(self.wc.dir.join(".git").join("index.lock"));
        if self.git.rev(&self.wc.dir, "HEAD")?.is_some() {
            self.git.ok(Some(&self.wc.dir), &["reset", "-q", "--hard", "HEAD"], LOCAL)?;
        }
        self.git.ok(Some(&self.wc.dir), &["clean", "-q", "-fd"], LOCAL)?;
        Ok(())
    }

    fn check_format(&self, origin: Option<&str>) -> Result<(), Failure> {
        let Some(o) = origin else { return Ok(()) };
        let fail = |message: &str| Failure {
            step: SyncStep::Import,
            error: GitError { kind: FailKind::Format, message: message.into() },
        };
        let files = self.git.files(&self.wc.dir, o).map_err(at(SyncStep::Import))?;
        let manifest =
            self.git.read_at(&self.wc.dir, o, &[format::MANIFEST.to_string()]).map_err(at(SyncStep::Import))?;
        let bytes = manifest.get(format::MANIFEST).cloned().flatten();
        match content_of(&files, bytes.as_deref()).0 {
            RepoContent::Empty | RepoContent::Runwick => Ok(()),
            RepoContent::Newer => {
                Err(fail("저장소가 더 새로운 Runwick 형식으로 바뀌었어요. Runwick을 업데이트한 뒤 동기화해 주세요"))
            }
            RepoContent::Other => {
                Err(fail("Runwick 기록 저장소가 아닌 파일이 들어 있어요. 기록 전용 저장소를 연결해 주세요"))
            }
        }
    }

    /// Files the remote changed since `base` (every file when there is no base).
    fn delta(&self, base: Option<&str>, origin: Option<&str>) -> Result<Vec<String>, Failure> {
        let list = match (base, origin) {
            (_, None) => Vec::new(),
            (Some(b), Some(o)) if b == o => Vec::new(),
            (Some(b), Some(o)) => self.git.changed(&self.wc.dir, b, o).map_err(at(SyncStep::Import))?,
            (None, Some(o)) => self.git.files(&self.wc.dir, o).map_err(at(SyncStep::Import))?,
        };
        Ok(list.into_iter().filter(|p| super::wc::safe_path(p)).collect())
    }

    /// Bring the working copy to the remote's version of every file it changed (our own
    /// records are written over it by export).
    fn apply_delta(&self, delta: &[String], remote: &Blobs) -> Result<(), Failure> {
        let step = SyncStep::Import;
        for p in delta {
            match remote.get(p).cloned().flatten() {
                Some(bytes) => {
                    self.wc.write(p, &bytes).map_err(io_err(step))?;
                }
                None => {
                    self.wc.remove(p).map_err(io_err(step))?;
                }
            }
            state::drop_mark(self.store(), p).map_err(db(step))?;
        }
        Ok(())
    }

    /// Keep every computer's entry (ours and the remote's). Our entry is stamped, and the
    /// app version updated, only when this computer wrote records; otherwise the manifest
    /// stays as the remote has it, so computers never trade commits over it.
    fn write_manifest(&self, head: Option<Manifest>, wrote: bool) -> Result<(), Failure> {
        let theirs = self.wc.read(format::MANIFEST).and_then(|b| serde_json::from_slice::<Manifest>(&b).ok());
        let mut m = Manifest::new();
        if let Some(v) = theirs.as_ref().or(head.as_ref()).map(|x| x.app_version.clone()).filter(|v| !v.is_empty()) {
            m.app_version = v;
        }
        for src in [head, theirs].into_iter().flatten() {
            for (host, e) in src.devices {
                let keep = m.devices.get(&host).is_some_and(|cur| cur.last_sync >= e.last_sync);
                if !keep {
                    m.devices.insert(host, e);
                }
            }
        }
        let me = self.cx.device.host_id;
        let renamed = m.devices.get(&me).is_none_or(|e| e.name != self.cx.device.name);
        let entry =
            m.devices.entry(me).or_insert(format::DeviceEntry { name: self.cx.device.name.clone(), last_sync: None });
        entry.name = self.cx.device.name.clone();
        if wrote || renamed {
            entry.last_sync = Some(crate::store::ts_to_sql(&runwick_core::now()));
            m.app_version = runwick_core::VERSION.into();
        }
        self.wc.write(format::MANIFEST, m.render().as_bytes()).map_err(io_err(SyncStep::Export))?;
        Ok(())
    }

    fn identity(&self) -> (String, String) {
        let email = self
            .git
            .raw(Some(&self.wc.dir), &["config", "--get", "user.email"], None, LOCAL)
            .ok()
            .filter(|o| o.ok)
            .map(|o| o.text())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "runwick@localhost".into());
        (format!("user.name={}", self.cx.device.name), format!("user.email={email}"))
    }

    /// A commit message with the trailer naming this computer.
    fn message(&self, subject: &str) -> String {
        format!("{subject}\n\n{HOST_TRAILER}: {}", self.cx.device.host_id)
    }

    fn commit_now(&self, subject: &str, step: SyncStep) -> Result<(), Failure> {
        let (name, email) = self.identity();
        let msg = self.message(subject);
        self.git
            .ok(Some(&self.wc.dir), &["-c", &name, "-c", &email, "commit", "-q", "--no-verify", "-m", &msg], LOCAL)
            .map_err(at(step))?;
        Ok(())
    }

    fn commit(&self, tally: &Tally, origin: Option<&str>) -> Result<(), Failure> {
        let step = SyncStep::Commit;
        self.git.ok(Some(&self.wc.dir), &["add", "-A"], LOCAL).map_err(at(step))?;
        // Nothing of ours to add and the tree is exactly the remote's: move to it instead of
        // making a commit (receiving changes never creates commits).
        if let Some(o) = origin {
            let head = self.git.rev(&self.wc.dir, "HEAD").map_err(at(step))?;
            let behind = match &head {
                None => true,
                Some(h) => self.git.is_ancestor(&self.wc.dir, h, o).map_err(at(step))?,
            };
            if behind
                && self
                    .git
                    .raw(Some(&self.wc.dir), &["diff", "--cached", "--quiet", o, "--"], None, LOCAL)
                    .map_err(at(step))?
                    .ok
            {
                self.git.ok(Some(&self.wc.dir), &["reset", "-q", "--hard", o], LOCAL).map_err(at(step))?;
                return Ok(());
            }
        }
        if self.git.raw(Some(&self.wc.dir), &["diff", "--cached", "--quiet"], None, LOCAL).map_err(at(step))?.ok {
            return Ok(());
        }
        let parts: Vec<String> = tally.exported.iter().map(|(k, n)| format!("{} {n}", k.label())).collect();
        let subject = if parts.is_empty() {
            format!("Runwick 동기화 · {}", self.cx.device.name)
        } else {
            format!("Runwick 동기화 · {} · {}", self.cx.device.name, parts.join(", "))
        };
        self.commit_now(&subject, step)
    }

    /// Record the remote history as merged while keeping our tree (which already holds the
    /// remote's changes): no textual merge ever happens.
    fn merge_ours(&self, origin: &str) -> Result<(), Failure> {
        let (name, email) = self.identity();
        let msg = self.message(&format!("Runwick 동기화 · {} · 합침", self.cx.device.name));
        self.git
            .ok(
                Some(&self.wc.dir),
                &[
                    "-c",
                    &name,
                    "-c",
                    &email,
                    "merge",
                    "-q",
                    "-s",
                    "ours",
                    "--no-edit",
                    "--no-verify",
                    "--allow-unrelated-histories",
                    "-m",
                    &msg,
                    origin,
                ],
                LOCAL,
            )
            .map_err(at(SyncStep::Commit))?;
        Ok(())
    }

    /// Before pushing: every file the remote has must still be in our tree, unless a tombstone
    /// in our tree removes it (or it is a tombstone of a record that exists again). Anything
    /// else is taken back from the remote and committed. Returns how many files were kept.
    fn keep_others(&self, origin: &str) -> Result<usize, Failure> {
        let step = SyncStep::Push;
        let out = self
            .git
            .ok(
                Some(&self.wc.dir),
                &["diff", "--name-only", "--no-renames", "--diff-filter=D", "-z", origin, "HEAD", "--"],
                LOCAL,
            )
            .map_err(at(step))?;
        let removed: Vec<String> = out
            .stdout
            .split(|b| *b == 0)
            .filter(|s| !s.is_empty())
            .map(|s| String::from_utf8_lossy(s).to_string())
            .collect();
        if removed.is_empty() {
            return Ok(0);
        }
        let head: BTreeSet<String> = self.git.files(&self.wc.dir, "HEAD").map_err(at(step))?.into_iter().collect();
        let allowed = |p: &String| {
            let entity_kind = kinds::kind_of(p).filter(|k| kinds::deletable(*k));
            match (entity_kind, tombstones::entity_of(p)) {
                (Some(_), _) => tombstones::for_entity(p).is_some_and(|t| head.contains(&t)),
                (None, Some(entity)) => head.contains(&entity),
                _ => false,
            }
        };
        // Entries Runwick never reads (links, submodules, colliding names) are not carried over.
        let odd: BTreeSet<String> =
            self.git.tree_problems(&self.wc.dir, origin).map_err(at(step))?.into_iter().map(|(p, _)| p).collect();
        let back: Vec<String> = removed.into_iter().filter(|p| !allowed(p) && !odd.contains(p)).collect();
        if back.is_empty() {
            return Ok(0);
        }
        tracing::warn!(files = back.len(), "sync would have removed records it does not own; keeping them");
        let mut list = Vec::new();
        for p in &back {
            list.extend_from_slice(p.as_bytes());
            list.push(0);
        }
        let restored = self
            .git
            .raw(
                Some(&self.wc.dir),
                &["checkout", origin, "--pathspec-from-file=-", "--pathspec-file-nul"],
                Some(list),
                LOCAL,
            )
            .map_err(at(step))?;
        if !restored.ok {
            return Err(Failure { step, error: super::git::classify(&restored.stderr) });
        }
        self.commit_now(&format!("Runwick 동기화 · {} · 다른 컴퓨터 기록 보존", self.cx.device.name), step)?;
        Ok(back.len())
    }

    /// Commits only here / only on the remote.
    pub fn ahead_behind(&self) -> Option<(u32, u32)> {
        let range = format!("HEAD...{}", self.remote_ref());
        let out =
            self.git.raw(Some(&self.wc.dir), &["rev-list", "--left-right", "--count", &range], None, LOCAL).ok()?;
        if !out.ok {
            return None;
        }
        let text = out.text();
        let mut it = text.split_whitespace().map(|n| n.parse::<u32>().ok());
        Some((it.next()??, it.next()??))
    }
}
