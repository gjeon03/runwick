//! Following transcripts with a byte cursor per session.
//!
//! The cursor sits at a line boundary and survives host restarts. A shorter file, or a file whose
//! first bytes changed, is read again from the start; stored messages are deduplicated by their
//! transcript id, so a re-read never doubles them. Sessions are read every 2 s only while their
//! run is alive, once more when it ends, and all of them at host start.

use super::db::{self, Source};
use super::locate::{self, Roots};
use super::parse::{self, Item};
use crate::host::Host;
use crate::store::Store;
use runwick_core::api::records::CaptureState;
use runwick_core::api::sessions::{SessionId, Tool};
use runwick_core::rpc::topic;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const POLL: Duration = Duration::from_secs(2);
/// Bytes of the file start remembered to notice a replaced file.
const HEAD: u64 = 1024;
/// Stop a pass after this much; the next pass continues (keeps the store lock short).
const PASS_BYTES: u64 = 32 * 1024 * 1024;
/// The slow search for a missing transcript runs at most this often per session.
const SCAN_GAP: Duration = Duration::from_secs(60);

/// What one pass changed, for change events.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct Outcome {
    pub added: usize,
    pub title_changed: bool,
    pub status_changed: bool,
}

impl Outcome {
    fn merge(&mut self, o: Outcome) {
        self.added += o.added;
        self.title_changed |= o.title_changed;
        self.status_changed |= o.status_changed;
    }
}

/// Transcript roots plus when each session last had a slow search.
pub struct Collector {
    pub roots: Roots,
    scans: Mutex<HashMap<SessionId, Instant>>,
}

impl Collector {
    pub fn new(roots: Roots) -> Self {
        Self { roots, scans: Mutex::new(HashMap::new()) }
    }

    fn may_scan(&self, id: SessionId, force: bool) -> bool {
        let Ok(mut m) = self.scans.lock() else {
            return false;
        };
        let due = force || m.get(&id).is_none_or(|t| t.elapsed() >= SCAN_GAP);
        if due {
            m.insert(id, Instant::now());
        }
        due
    }

    /// Read what is new in one session's transcript. Blocking.
    pub fn collect(&self, store: &Store, id: SessionId, running: bool, force_scan: bool) -> rusqlite::Result<Outcome> {
        let Some(s) = store.with(|c| db::session_row(c, id))? else {
            return Ok(Outcome::default());
        };
        let prev = store.with(|c| db::load_source(c, id))?;
        let mut src = prev.clone().unwrap_or_else(|| Source::new(CaptureState::Collecting));
        let now = runwick_core::now();
        let done = |src: &Source| Outcome {
            status_changed: prev.as_ref().is_none_or(|p| p.state != src.state || p.last_error != src.last_error),
            ..Default::default()
        };

        let conv = match (s.tool, s.conv.clone()) {
            (Tool::Shell, _) => {
                src.state = CaptureState::NotApplicable;
                store.with(|c| db::save_source(c, id, &src))?;
                return Ok(done(&src));
            }
            (_, None) => {
                src.state = CaptureState::WaitingForId;
                store.with(|c| db::save_source(c, id, &src))?;
                return Ok(done(&src));
            }
            (_, Some(conv)) => conv,
        };
        if src.conv.as_deref() != Some(conv.as_str()) {
            // A new conversation in the same session writes a new file.
            src.conv = Some(conv.clone());
            src.path = None;
            src.rewind();
        }
        let known = src.path.as_ref().map(PathBuf::from).filter(|p| p.is_file());
        let path = known.or_else(|| {
            // The slow search: for live runs, the first read of a session, or when asked. A session
            // already known to have no file is not searched again at every host start.
            let scan = (force_scan || running || prev.is_none()) && self.may_scan(id, force_scan);
            match s.tool {
                Tool::Claude => locate::claude_transcript(&self.roots.claude, &s.cwd, &conv, scan),
                _ => {
                    let near = s.created_at.with_timezone(&chrono::Local);
                    locate::codex_transcript(&self.roots.codex, &conv, near, scan)
                }
            }
        });
        src.last_checked_at = Some(now);
        let Some(path) = path else {
            src.path = None;
            src.last_error = None;
            // Claude writes the file with the first message; while the run is alive, keep looking.
            src.state = if running { CaptureState::Collecting } else { CaptureState::NoFile };
            store.with(|c| db::save_source(c, id, &src))?;
            return Ok(done(&src));
        };
        if src.path.as_deref() != Some(&*path.to_string_lossy()) {
            src.path = Some(path.to_string_lossy().into_owned());
            src.rewind();
        }
        let items = match read_new(&path, &mut src, s.tool) {
            Ok(items) => {
                src.state = if running { CaptureState::Collecting } else { CaptureState::UpToDate };
                src.last_error = None;
                items
            }
            Err(e) => {
                src.state = CaptureState::Error;
                src.last_error = Some(format!("기록 파일을 읽지 못했어요: {e}"));
                store.with(|c| db::save_source(c, id, &src))?;
                return Ok(done(&src));
            }
        };
        let mut out = store.tx(|tx| {
            let added = db::store_items(tx, &s, &mut src, &items, &now)?;
            if added > 0 {
                src.last_collected_at = Some(now);
            }
            db::save_source(tx, id, &src)?;
            let mut title_changed = false;
            if added > 0
                && !s.title_user_set
                && s.title == s.tool.label()
                && let Some(title) = db::first_user_text(tx, id)?.as_deref().and_then(db::title_from)
            {
                title_changed = db::set_default_title(tx, &s, &title)?;
            }
            Ok(Outcome { added, title_changed, status_changed: false })
        })?;
        out.status_changed = done(&src).status_changed;
        Ok(out)
    }
}

fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// Lines after the cursor. Moves the cursor past complete lines only; rewinds when the file
/// was truncated or replaced.
fn read_new(path: &Path, src: &mut Source, tool: Tool) -> std::io::Result<Vec<Item>> {
    let mut f = std::fs::File::open(path)?;
    let len = f.metadata()?.len();
    if src.offset > len || src.head_len > len {
        src.rewind();
    }
    if src.head_len > 0 {
        let mut head = vec![0u8; src.head_len as usize];
        f.read_exact(&mut head)?;
        if src.head_hash.as_deref() != Some(digest(&head).as_str()) {
            src.rewind();
        }
    }
    if src.head_len == 0 && len > 0 {
        let n = len.min(HEAD);
        let mut head = vec![0u8; n as usize];
        f.seek(SeekFrom::Start(0))?;
        f.read_exact(&mut head)?;
        src.head_len = n;
        src.head_hash = Some(digest(&head));
    }
    f.seek(SeekFrom::Start(src.offset))?;
    let mut reader = BufReader::new(f);
    let mut items = Vec::new();
    let mut line = Vec::new();
    let mut read = 0u64;
    loop {
        line.clear();
        let n = reader.read_until(b'\n', &mut line)?;
        if n == 0 || line.last() != Some(&b'\n') {
            break; // end, or a line the CLI is still writing
        }
        src.offset += n as u64;
        read += n as u64;
        let text = String::from_utf8_lossy(&line);
        let text = text.trim_end();
        if !text.is_empty() {
            items.extend(match tool {
                Tool::Claude => parse::claude_line(text),
                _ => parse::codex_line(text),
            });
        }
        if read >= PASS_BYTES {
            break;
        }
    }
    Ok(items)
}

/// Tell clients what changed.
pub fn announce(host: &Host, o: Outcome) {
    let mut topics = Vec::new();
    if o.added > 0 || o.status_changed {
        topics.push(topic::MESSAGES);
    }
    if o.title_changed {
        topics.push(topic::SESSIONS);
    }
    if o.added > 0 {
        topics.push(topic::TIMELINE);
    }
    if !topics.is_empty() {
        host.bus.changed(&topics);
    }
}

/// Collect several sessions on a blocking thread.
pub async fn pass(
    host: &Arc<Host>,
    collector: &Arc<Collector>,
    targets: Vec<(SessionId, bool)>,
    force_scan: bool,
) -> Outcome {
    let (h, c) = (host.clone(), collector.clone());
    let res = tokio::task::spawn_blocking(move || {
        let mut total = Outcome::default();
        for (id, running) in targets {
            match c.collect(&h.store, id, running, force_scan) {
                Ok(o) => total.merge(o),
                Err(e) => tracing::warn!(session = %id, "collecting records failed: {e}"),
            }
        }
        total
    })
    .await;
    let out = res.unwrap_or_default();
    announce(host, out);
    out
}

/// Background collection: catch up everything once, then follow live runs.
pub async fn run(host: Arc<Host>, collector: Arc<Collector>) {
    let ids = host.store.with(db::agent_session_ids).unwrap_or_else(|e| {
        tracing::warn!("cannot list sessions for records: {e}");
        Vec::new()
    });
    let running: HashSet<SessionId> = host.sessions.running_ids().into_iter().collect();
    let targets = ids.into_iter().map(|id| (id, running.contains(&id))).collect();
    let o = pass(&host, &collector, targets, false).await;
    tracing::info!(added = o.added, "records caught up");

    let mut prev: HashSet<SessionId> = HashSet::new();
    loop {
        tokio::time::sleep(POLL).await;
        let running: HashSet<SessionId> = host.sessions.running_ids().into_iter().collect();
        // Live runs, plus runs that ended since the last pass (read to the end once more).
        let mut targets: Vec<(SessionId, bool)> = running.iter().map(|id| (*id, true)).collect();
        targets.extend(prev.difference(&running).map(|id| (*id, false)));
        prev = running;
        if !targets.is_empty() {
            pass(&host, &collector, targets, false).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;
    use runwick_core::api::records::Role;
    use std::io::Write;
    use uuid::Uuid;

    struct Fixture {
        store: Store,
        _dirs: (tempfile::TempDir, tempfile::TempDir),
        collector: Collector,
        session: SessionId,
        file: PathBuf,
    }

    /// A project, a Claude session with conversation id "conv-1" in `/w/app`, and its (empty)
    /// transcript path.
    fn fixture(tool: Tool) -> Fixture {
        let store = Store::open_in_memory().unwrap();
        let (p, w, s) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
        let now = crate::store::ts_to_sql(&runwick_core::now());
        store
            .with(|c| {
                c.execute(
                    "INSERT INTO projects (id, name, root, is_git, favorite, archived, created_at) VALUES (?1, 'app', '/w/app', 0, 0, 0, ?2)",
                    rusqlite::params![p, now],
                )?;
                c.execute(
                    "INSERT INTO workspaces (id, project_id, name, path, kind, archived, created_at) VALUES (?1, ?2, '기본', '/w/app', 'default', 0, ?3)",
                    rusqlite::params![w, p, now],
                )?;
                c.execute(
                    "INSERT INTO sessions (id, project_id, workspace_id, title, tool, provider_conv_id, cwd, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, 'conv-1', '/w/app', ?6)",
                    rusqlite::params![s, p, w, tool.label(), db::tool_to_sql(tool), now],
                )
            })
            .unwrap();
        let claude = tempfile::tempdir().unwrap();
        let codex = tempfile::tempdir().unwrap();
        let dir = claude.path().join("-w-app");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("conv-1.jsonl");
        let collector = Collector::new(Roots { claude: claude.path().into(), codex: codex.path().into() });
        Fixture { store, _dirs: (claude, codex), collector, session: s, file }
    }

    fn user(uuid: &str, text: &str) -> String {
        serde_json::json!({"type":"user","uuid":uuid,"timestamp":"2026-11-05T02:00:00.000Z","message":{"content":text}})
            .to_string()
            + "\n"
    }

    fn answer(uuid: &str, text: &str) -> String {
        serde_json::json!({"type":"assistant","uuid":uuid,"timestamp":"2026-11-05T02:00:05.000Z",
            "message":{"stop_reason":"end_turn","content":[{"type":"text","text":text}]}})
        .to_string()
            + "\n"
    }

    fn append(path: &Path, s: &str) {
        std::fs::OpenOptions::new().create(true).append(true).open(path).unwrap().write_all(s.as_bytes()).unwrap();
    }

    fn texts(f: &Fixture) -> Vec<(i64, String)> {
        f.store
            .with(|c| {
                let mut st = c.prepare("SELECT seq, text FROM messages WHERE session_id = ?1 ORDER BY seq")?;
                let rows = st.query_map([f.session], |r| Ok((r.get(0)?, r.get(1)?)))?;
                rows.collect()
            })
            .unwrap()
    }

    #[test]
    fn missing_file_then_incremental_lines_and_partial_line() {
        let f = fixture(Tool::Claude);
        let o = f.collector.collect(&f.store, f.session, false, false).unwrap();
        assert_eq!(o.added, 0);
        let src = f.store.with(|c| db::load_source(c, f.session)).unwrap().unwrap();
        assert_eq!(src.state, CaptureState::NoFile);

        append(&f.file, &user("u1", "만료 시각 확인해줘"));
        // Half of the next line: not read until it is complete.
        let a = answer("a1", "원인을 찾았습니다");
        append(&f.file, &a[..10]);
        let o = f.collector.collect(&f.store, f.session, true, false).unwrap();
        assert_eq!(o.added, 1);
        assert!(o.title_changed, "default title replaced by the first message");
        append(&f.file, &a[10..]);
        let o = f.collector.collect(&f.store, f.session, true, false).unwrap();
        assert_eq!(o.added, 1);
        assert_eq!(texts(&f), vec![(0, "만료 시각 확인해줘".into()), (1, "원인을 찾았습니다".into())]);
        let title: String = f.store.with(|c| c.query_row("SELECT title FROM sessions", [], |r| r.get(0))).unwrap();
        assert_eq!(title, "만료 시각 확인해줘");
        let src = f.store.with(|c| db::load_source(c, f.session)).unwrap().unwrap();
        assert_eq!(src.state, CaptureState::Collecting);
        assert!(src.turn_ended_at.is_some());
    }

    #[test]
    fn cursor_survives_a_new_collector_and_rewrites_are_deduplicated() {
        let f = fixture(Tool::Claude);
        append(&f.file, &user("u1", "첫 질문"));
        append(&f.file, &answer("a1", "첫 답"));
        f.collector.collect(&f.store, f.session, false, false).unwrap();
        // Host restart: a fresh collector continues from the stored cursor.
        let again = Collector::new(f.collector.roots.clone());
        append(&f.file, &user("u2", "둘째 질문"));
        let o = again.collect(&f.store, f.session, false, false).unwrap();
        assert_eq!(o.added, 1);

        // The CLI rewrites the file (same messages, different first bytes, shorter): read again
        // from the start without duplicates; the new message is added once.
        std::fs::write(&f.file, user("u1", "첫 질문") + &user("u3", "셋째")).unwrap();
        let o = again.collect(&f.store, f.session, false, false).unwrap();
        assert_eq!(o.added, 1);
        assert_eq!(texts(&f).len(), 4);
        let o = again.collect(&f.store, f.session, false, false).unwrap();
        assert_eq!(o.added, 0);
        let src = f.store.with(|c| db::load_source(c, f.session)).unwrap().unwrap();
        assert_eq!(src.state, CaptureState::UpToDate);
    }

    #[test]
    fn codex_sessions_are_found_by_thread_id() {
        let f = fixture(Tool::Codex);
        let day = chrono::Local::now();
        let dir = f.collector.roots.codex.join(day.format("%Y/%m/%d").to_string());
        std::fs::create_dir_all(&dir).unwrap();
        let line = serde_json::json!({"timestamp":"2026-11-05T02:00:00.000Z","type":"response_item",
            "payload":{"type":"message","role":"assistant","id":"r1","content":[{"type":"output_text","text":"재시도 상한을 60초로"}]}});
        std::fs::write(dir.join("rollout-2026-11-05T11-00-00-conv-1.jsonl"), format!("{line}\n")).unwrap();
        let o = f.collector.collect(&f.store, f.session, false, false).unwrap();
        assert_eq!(o.added, 1);
        let role: String = f.store.with(|c| c.query_row("SELECT role FROM messages", [], |r| r.get(0))).unwrap();
        assert_eq!(role, db::role_to_sql(Role::Assistant));
    }
}
