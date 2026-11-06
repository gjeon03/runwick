//! The last screen of each session, kept on disk so it can be shown again after the host or the
//! computer restarts ("화면과 기록은 복원했고"). Saved at most every few seconds while output
//! changes, and when the run ends. Files are private to the user (0600) and hold a bounded
//! amount of scrollback.

use super::term::Size;
use runwick_core::api::sessions::SessionId;
use std::collections::HashMap;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

/// Scrollback lines kept in a saved screen.
pub const HISTORY: usize = 2000;
/// Minimum time between saves of a session that keeps printing.
pub const SAVE_GAP: Duration = Duration::from_secs(15);

fn dir() -> PathBuf {
    runwick_core::paths::home().join("screens")
}

fn path(id: SessionId) -> PathBuf {
    dir().join(format!("{id}.term"))
}

/// The newest content version of one session on disk (`None`: not saved by this process).
type Slot = Arc<Mutex<Option<u64>>>;

/// Per session: the newest content version on disk. The live loop and the end of a run may
/// save at the same time; saves of one session take turns, and an older snapshot that comes
/// second never replaces a newer one.
fn slot(id: SessionId) -> Slot {
    static SLOTS: OnceLock<Mutex<HashMap<SessionId, Slot>>> = OnceLock::new();
    let mut slots = SLOTS.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
    slots.entry(id).or_default().clone()
}

/// Write `<cols> <rows>\n` followed by snapshot bytes, replacing the previous file atomically.
/// `version` orders snapshots of one session (its content counter when taken).
pub fn save(id: SessionId, version: u64, size: Size, snapshot: &[u8]) -> std::io::Result<()> {
    save_in(&dir(), id, version, size, snapshot)
}

fn save_in(dir: &Path, id: SessionId, version: u64, size: Size, snapshot: &[u8]) -> std::io::Result<()> {
    let slot = slot(id);
    let mut saved = slot.lock().unwrap_or_else(|e| e.into_inner());
    if saved.is_some_and(|v| v > version) {
        return Ok(()); // a newer screen is already on disk
    }
    std::fs::create_dir_all(dir)?;
    // A name of its own: another host process on the same folder may be saving too.
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let tmp = dir.join(format!("{id}.{}.{}.tmp", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    let written = (|| {
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp)?;
        writeln!(f, "{} {}", size.cols, size.rows)?;
        f.write_all(snapshot)?;
        f.sync_data()?;
        std::fs::rename(&tmp, dir.join(format!("{id}.term")))
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written?;
    *saved = Some(version);
    Ok(())
}

pub fn load(id: SessionId) -> Option<(Size, Vec<u8>)> {
    parse(&std::fs::read(path(id)).ok()?)
}

fn parse(data: &[u8]) -> Option<(Size, Vec<u8>)> {
    let nl = data.iter().position(|b| *b == b'\n')?;
    let header = std::str::from_utf8(&data[..nl]).ok()?;
    let (c, r) = header.split_once(' ')?;
    let size = Size::clamped(c.parse().ok()?, r.parse().ok()?);
    Some((size, data[nl + 1..].to_vec()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sessions::term::HostTerm;

    #[test]
    fn saved_screen_rebuilds_the_terminal() {
        let mut t = HostTerm::new(Size { cols: 20, rows: 4 });
        t.advance("저장된 화면\r\nline 2".as_bytes());
        let mut file = b"20 4\n".to_vec();
        file.extend(t.snapshot_with_history(HISTORY));
        let (size, bytes) = parse(&file).unwrap();
        assert_eq!(size, Size { cols: 20, rows: 4 });
        let mut restored = HostTerm::new(size);
        restored.advance(&bytes);
        assert_eq!(restored.screen_lines(), t.screen_lines());
        assert!(parse(b"garbage").is_none());
    }

    #[test]
    fn concurrent_saves_never_fail_and_the_newest_screen_wins() {
        let dir = tempfile::tempdir().unwrap();
        let id = uuid::Uuid::new_v4();
        let size = Size { cols: 10, rows: 2 };
        let handles: Vec<_> = (0..16u64)
            .map(|v| {
                let d = dir.path().to_path_buf();
                std::thread::spawn(move || save_in(&d, id, v, size, format!("screen {v}").as_bytes()))
            })
            .collect();
        for h in handles {
            h.join().unwrap().expect("every save succeeds");
        }
        let file = std::fs::read(dir.path().join(format!("{id}.term"))).unwrap();
        assert_eq!(parse(&file).unwrap().1, b"screen 15", "the newest content stays");
        // An older snapshot that finishes late does not replace it.
        save_in(dir.path(), id, 3, size, b"stale").unwrap();
        assert_eq!(parse(&std::fs::read(dir.path().join(format!("{id}.term"))).unwrap()).unwrap().1, b"screen 15");
        let leftovers: Vec<_> = std::fs::read_dir(dir.path()).unwrap().filter_map(|e| e.ok()).collect();
        assert_eq!(leftovers.len(), 1, "no temporary files left behind");
    }
}
