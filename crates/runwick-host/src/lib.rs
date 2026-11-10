//! The Runwick host keeps CLI runs alive independently of any window and owns this computer's
//! records. The GUI (and remote GUIs over SSH) connect to it through a Unix socket.

pub mod backup;
pub mod bridge;
pub mod collab;
pub mod devices;
pub mod env;
pub mod files;
pub mod git;
pub mod gui_token;
pub mod host;
pub mod jobs;
pub mod mcp;
pub mod meta;
pub mod prefs;
pub mod projects;
pub mod records;
pub mod refresh;
pub mod roles;
pub mod router;
pub mod server;
pub mod sessions;
pub mod store;
pub mod sync;
pub mod timeline;
pub mod tools;
pub mod wiki;
pub mod worktree;

use anyhow::Context;
use runwick_core::api::host::HostInfo;
use runwick_core::paths;
use std::sync::Arc;
use uuid::Uuid;

pub use host::{Ctx, Host};

/// Run the host in the foreground until `host.shutdown` is requested.
pub fn run() -> anyhow::Result<()> {
    paths::ensure_dirs().context("create data folders")?;
    init_logging();
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    rt.block_on(run_async())
}

async fn run_async() -> anyhow::Result<()> {
    let sock = paths::socket_path();
    if std::os::unix::net::UnixStream::connect(&sock).is_ok() {
        anyhow::bail!("another Runwick host is already running ({})", sock.display());
    }
    serve_at(&paths::home(), &sock).await
}

/// Serve the host with the data in `home` on the Unix socket `socket` until `host.shutdown`.
/// `run` uses it with the configured paths; in-process integration tests call it directly
/// (with `RUNWICK_HOME` pointing at the same `home`, which other paths derive from).
pub async fn serve_at(home: &std::path::Path, sock: &std::path::Path) -> anyhow::Result<()> {
    if let Some(dir) = sock.parent() {
        // The legacy fallback (~/.runwick/run) needs private permissions. The macOS private
        // temp fallback is already checked by paths::socket_path; do not change its permissions.
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(dir).ok();
        if dir.ends_with("run") {
            let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
        }
    }
    let _ = std::fs::remove_file(sock);
    // Before any client can connect: windows read it to prove they are windows.
    gui_token::issue(&home.join("gui.token")).context("write the window token")?;
    let listener = tokio::net::UnixListener::bind(sock).with_context(|| format!("bind {}", sock.display()))?;
    restrict_permissions(sock);

    let store = store::Store::open(&home.join("runwick.db"))?;
    let host_id = match meta::kv_get(&store, "host.id")?.and_then(|v| v.as_str().and_then(|s| s.parse::<Uuid>().ok())) {
        Some(id) => id,
        None => {
            let id = Uuid::now_v7();
            meta::kv_put(&store, "host.id", &serde_json::json!(id.to_string()))?;
            id
        }
    };
    let info = HostInfo {
        host_id,
        device_name: device_name(),
        version: runwick_core::VERSION.to_string(),
        os: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        home: paths::display_path(home),
        started_at: runwick_core::now(),
        pid: std::process::id(),
        client_id: 0,
    };
    std::fs::write(paths::pid_path(), std::process::id().to_string()).ok();
    tracing::info!(version = %info.version, socket = %sock.display(), "host started");

    let host = Host::new(info, store);
    let router = Arc::new(router::build());
    env::boot(&host);
    refresh::boot(&host);
    sessions::boot(&host);
    records::boot(&host);
    jobs::boot(&host);
    collab::boot(&host);
    backup::boot();
    sync::boot(&host);
    server::serve(host.clone(), router, listener).await;

    let _ = std::fs::remove_file(sock);
    let _ = std::fs::remove_file(paths::pid_path());
    tracing::info!("host stopped");
    Ok(())
}

fn restrict_permissions(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

/// Friendly computer name ("MacBook Pro"), falling back to the host name.
pub fn device_name() -> String {
    #[cfg(target_os = "macos")]
    if let Ok(out) = std::process::Command::new("scutil").args(["--get", "ComputerName"]).output() {
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if out.status.success() && !s.is_empty() {
            return s;
        }
    }
    let mut buf = [0u8; 256];
    let rc = unsafe { libc::gethostname(buf.as_mut_ptr() as *mut libc::c_char, buf.len()) };
    if rc == 0 {
        let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
        let s = String::from_utf8_lossy(&buf[..end]).to_string();
        if !s.is_empty() {
            return s.trim_end_matches(".local").to_string();
        }
    }
    "이 컴퓨터".to_string()
}

fn init_logging() {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_env("RUNWICK_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    let file = std::fs::OpenOptions::new().create(true).append(true).open(paths::logs_dir().join("host.log"));
    match file {
        Ok(f) => {
            let _ =
                tracing_subscriber::fmt().with_env_filter(filter).with_ansi(false).with_writer(Arc::new(f)).try_init();
        }
        Err(_) => {
            let _ = tracing_subscriber::fmt().with_env_filter(filter).try_init();
        }
    }
}
