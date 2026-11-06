//! Where Runwick keeps its data on a host.
//!
//! Everything lives under `RUNWICK_HOME` (default `~/.runwick`) so tests and a second instance
//! can run in isolation by pointing the variable at another folder.

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

/// Unix socket of the local host. macOS limits socket paths to 104 bytes, so long homes
/// (for example test folders) fall back to a short hashed name in the temp folder.
pub fn socket_path() -> PathBuf {
    if let Some(p) = std::env::var_os(ENV_SOCKET).filter(|v| !v.is_empty()) {
        return PathBuf::from(p);
    }
    let preferred = home().join("host.sock");
    if preferred.as_os_str().len() < 100 {
        return preferred;
    }
    // Not /tmp: another local user could pre-create a predictable name there. The default data
    // folder is private (0700) and short enough for the 104-byte limit.
    user_home().join(".runwick").join("run").join(format!("{:016x}.sock", fnv1a(home().as_os_str().as_encoded_bytes())))
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
