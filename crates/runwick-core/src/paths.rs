//! Where Runwick keeps its data on a host.
//!
//! Data lives under `RUNWICK_HOME` (default `~/.runwick`) so tests and a second instance
//! can run in isolation. Long explicit homes use a short socket in the user's private macOS
//! temp directory, with the default data folder's `run` directory as a last resort.

use std::path::{Path, PathBuf};

pub const ENV_HOME: &str = "RUNWICK_HOME";
pub const ENV_SOCKET: &str = "RUNWICK_SOCKET";
/// Set inside every managed terminal so agent CLIs can call `runwick` for their own session.
pub const ENV_SESSION_ID: &str = "RUNWICK_SESSION_ID";
/// A secret for each run of a managed session: the `runwick` command shows it to the host,
/// which then knows the session for sure (the id above is only a claim). Never the window token.
pub const ENV_SESSION_TOKEN: &str = "RUNWICK_SESSION_TOKEN";

pub fn home() -> PathBuf {
    if let Some(p) = std::env::var_os(ENV_HOME).filter(|v| !v.is_empty()) {
        return PathBuf::from(p);
    }
    user_home().join(".runwick")
}

pub fn user_home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

pub fn db_path() -> PathBuf {
    home().join("runwick.db")
}

pub fn logs_dir() -> PathBuf {
    home().join("logs")
}

/// Folder whose `runwick` entry is prepended to PATH inside managed terminals.
pub fn bin_dir() -> PathBuf {
    home().join("bin")
}

pub fn wiki_dir() -> PathBuf {
    home().join("wiki")
}

pub fn sync_dir() -> PathBuf {
    home().join("sync")
}

pub fn backups_dir() -> PathBuf {
    home().join("backups")
}

pub fn pid_path() -> PathBuf {
    home().join("host.pid")
}

/// Per-launch token the host writes (mode 0600) for Runwick windows of this computer; a client
/// that presents it in `host.hello` may change settings only a person should change.
pub fn gui_token_path() -> PathBuf {
    home().join("gui.token")
}

/// Unix socket of the local host. Long explicit homes first try the OS-provided private
/// temp directory; if unavailable, use `~/.runwick/run/<hash>.sock` as for a long default home.
/// Never use a predictable name in a shared temp directory. Paths must leave room for the
/// terminating NUL in macOS's 104-byte socket address.
pub fn socket_path() -> PathBuf {
    if let Some(p) = std::env::var_os(ENV_SOCKET).filter(|v| !v.is_empty()) {
        return PathBuf::from(p);
    }
    let explicit = std::env::var_os(ENV_HOME).is_some_and(|v| !v.is_empty());
    socket_for_home(&home(), &user_home(), explicit, private_temp_dir)
}

fn socket_for_home(
    home: &Path,
    user_home: &Path,
    explicit: bool,
    private_temp: impl FnOnce() -> Option<PathBuf>,
) -> PathBuf {
    let preferred = home.join("host.sock");
    if preferred.as_os_str().len() < 100 {
        return preferred;
    }
    let hash = fnv1a(home.as_os_str().as_encoded_bytes());
    if explicit
        && let Some(dir) = private_temp()
        // SAFETY: geteuid has no preconditions and does not mutate process state.
        && let Some(socket) = private_temp_socket(&dir, hash, unsafe { libc::geteuid() })
    {
        return socket;
    }
    // Not /tmp: another local user could pre-create a predictable name there. The host makes
    // this legacy fallback directory private (0700) before binding.
    user_home.join(".runwick").join("run").join(format!("{hash:016x}.sock"))
}

fn private_temp_socket(dir: &Path, hash: u64, uid: u32) -> Option<PathBuf> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::symlink_metadata(dir).ok()?;
    if !dir.is_absolute() || !meta.is_dir() || meta.uid() != uid || meta.mode() & 0o022 != 0 {
        return None;
    }
    let socket = dir.join(format!("runwick-{hash:016x}.sock"));
    (socket.as_os_str().len() < 104).then_some(socket)
}

#[cfg(target_os = "macos")]
fn private_temp_dir() -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    let mut buf = [0u8; libc::PATH_MAX as usize];
    // SAFETY: buf is writable for its full length; confstr returns the required size including NUL.
    let len = unsafe { libc::confstr(libc::_CS_DARWIN_USER_TEMP_DIR, buf.as_mut_ptr().cast(), buf.len()) };
    if len <= 1 || len > buf.len() {
        return None;
    }
    let path = std::ffi::CStr::from_bytes_with_nul(&buf[..len]).ok()?;
    Some(PathBuf::from(std::ffi::OsStr::from_bytes(path.to_bytes())))
}

#[cfg(not(target_os = "macos"))]
fn private_temp_dir() -> Option<PathBuf> {
    None
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// Replace the user's home prefix with `~` for display.
pub fn display_path(p: &Path) -> String {
    let home = user_home();
    match p.strip_prefix(&home) {
        Ok(rest) if !rest.as_os_str().is_empty() => format!("~/{}", rest.display()),
        Ok(_) => "~".to_string(),
        Err(_) => p.display().to_string(),
    }
}

pub fn ensure_dirs() -> std::io::Result<()> {
    for d in [home(), logs_dir(), bin_dir(), wiki_dir(), backups_dir()] {
        std::fs::create_dir_all(&d)?;
    }
    // Records, screens and drafts are private: only this user may enter the data folder.
    use std::os::unix::fs::PermissionsExt;
    let home = home();
    if let Ok(meta) = std::fs::metadata(&home)
        && meta.permissions().mode() & 0o077 != 0
    {
        std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

    struct PrivateDir(PathBuf);

    impl PrivateDir {
        fn new() -> Self {
            // A random, atomically created private fixture, short enough to test sun_path limits.
            let path = PathBuf::from("/tmp").join(format!("rw-{}", uuid::Uuid::new_v4().simple()));
            std::fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for PrivateDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn uid() -> u32 {
        // SAFETY: geteuid has no preconditions.
        unsafe { libc::geteuid() }
    }

    #[test]
    fn short_homes_and_default_home_keep_their_socket_paths() {
        let user = Path::new("/Users/example");
        let short = user.join(".runwick");
        for explicit in [false, true] {
            assert_eq!(
                socket_for_home(&short, user, explicit, || panic!("no fallback needed")),
                short.join("host.sock")
            );
        }
        let long = PathBuf::from(format!("/{}", "x".repeat(100)));
        let hash = fnv1a(long.as_os_str().as_encoded_bytes());
        let fallback = user.join(format!(".runwick/run/{hash:016x}.sock"));
        assert_eq!(socket_for_home(&long, user, false, || panic!("default homes keep the old path")), fallback);
        assert_eq!(socket_for_home(&long, user, true, || None), fallback);
    }

    #[test]
    fn long_explicit_homes_use_distinct_stable_private_temp_sockets() {
        let dir = PrivateDir::new();
        let user = Path::new("/Users/example");
        let a = PathBuf::from(format!("/{}", "a".repeat(110)));
        let b = PathBuf::from(format!("/{}", "b".repeat(110)));
        let resolve = |p: &Path| socket_for_home(p, user, true, || Some(dir.0.clone()));
        let socket = resolve(&a);
        assert_eq!(socket.parent(), Some(dir.0.as_path()));
        assert!(socket.file_name().unwrap().to_str().unwrap().starts_with("runwick-"));
        assert!(socket.as_os_str().len() < 104);
        assert_eq!(socket, resolve(&a));
        assert_ne!(socket, resolve(&b));
        assert!(!socket.starts_with(user.join(".runwick")));
    }

    #[test]
    fn private_temp_rejects_wrong_owner_writable_missing_and_symlink_directories() {
        let dir = PrivateDir::new();
        assert!(private_temp_socket(&dir.0, 1, uid()).is_some());
        assert!(private_temp_socket(&dir.0, 1, uid().wrapping_add(1)).is_none());
        for mode in [0o720, 0o702, 0o777] {
            std::fs::set_permissions(&dir.0, std::fs::Permissions::from_mode(mode)).unwrap();
            assert!(private_temp_socket(&dir.0, 1, uid()).is_none());
            let long = PathBuf::from(format!("/{}", "x".repeat(100)));
            let fallback = socket_for_home(&long, Path::new("/Users/example"), true, || Some(dir.0.clone()));
            assert!(fallback.starts_with("/Users/example/.runwick/run"));
        }
        std::fs::set_permissions(&dir.0, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(private_temp_socket(&dir.0.join("missing"), 1, uid()).is_none());
        let link = dir.0.join("link");
        std::os::unix::fs::symlink(&dir.0, &link).unwrap();
        assert!(private_temp_socket(&link, 1, uid()).is_none());
        let file = dir.0.join("file");
        std::fs::write(&file, b"").unwrap();
        assert!(private_temp_socket(&file, 1, uid()).is_none());
    }

    #[test]
    fn private_temp_paths_leave_room_for_the_terminating_nul() {
        let dir = PrivateDir::new();
        let suffix_len = "/runwick-0000000000000001.sock".len();
        for length in [103, 104] {
            let name_len = length - suffix_len - dir.0.as_os_str().len() - 1;
            let path = dir.0.join("x".repeat(name_len));
            std::fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
            let socket = private_temp_socket(&path, 1, uid());
            assert_eq!(socket.is_some(), length == 103);
            if let Some(socket) = socket {
                assert_eq!(socket.as_os_str().len(), length);
            }
        }
        let unicode = dir.0.join("한".repeat(20));
        std::fs::DirBuilder::new().mode(0o700).create(&unicode).unwrap();
        assert!(private_temp_socket(&unicode, 1, uid()).is_none(), "limit is bytes, not characters");
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn darwin_temp_dir_is_private_and_short_enough() {
        let dir = private_temp_dir().expect("macOS supplies a user temp directory");
        assert!(private_temp_socket(&dir, 1, uid()).is_some());
    }
}
