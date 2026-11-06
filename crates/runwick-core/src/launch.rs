//! Start the local host when it is not running and connect to it.

use crate::paths;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Connect to the local host, starting `exe host --daemon` first if nothing answers.
pub fn connect_or_spawn(exe: &Path) -> std::io::Result<UnixStream> {
    let sock = paths::socket_path();
    if let Ok(s) = UnixStream::connect(&sock) {
        return Ok(s);
    }
    paths::ensure_dirs()?;
    let log = std::fs::OpenOptions::new().create(true).append(true).open(paths::logs_dir().join("host-stderr.log"))?;
    Command::new(exe)
        .args(["host", "--daemon"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(log))
        .process_group(0)
        .spawn()?;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match UnixStream::connect(&sock) {
            Ok(s) => return Ok(s),
            Err(e) if Instant::now() > deadline => return Err(e),
            Err(_) => std::thread::sleep(Duration::from_millis(50)),
        }
    }
}
