//! Process rules of the menu bar: one per `RUNWICK_HOME`, out of the way of an open window, and
//! opening the window again (the app bundle when it runs from one).

use runwick_app::presence::{OpenRequest, window_open};
use runwick_core::paths;
use std::ffi::OsString;
use std::fs::{File, OpenOptions, TryLockError};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Held for the life of the menu bar process. `None`: another one already shows this home.
pub fn single_instance() -> std::io::Result<Option<File>> {
    std::fs::create_dir_all(paths::home())?;
    let file = OpenOptions::new().create(true).truncate(false).write(true).open(paths::home().join("menubar.lock"))?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(e)) => Err(e),
    }
}

/// A closing window started us: give it time to finish closing (it holds its lock until the
/// process ends). `false` when a window is still open after `limit` (another window, or the
/// person opened it again), which then is the presence.
pub fn wait_window_closed(limit: Duration) -> bool {
    let deadline = Instant::now() + limit;
    while window_open() {
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    true
}

/// Open the window, optionally on a given view. Returns once it was asked to start.
pub fn open_window(exe: &Path, req: Option<OpenRequest>) -> std::io::Result<()> {
    let mut cmd = match app_bundle(exe) {
        Some(bundle) => {
            let mut c = Command::new("/usr/bin/open");
            c.args(open_args(&bundle, req, &forwarded_env()));
            c
        }
        None => {
            let mut c = Command::new(exe);
            if let Some(r) = req {
                c.args(["--open", &r.to_arg()]);
            }
            c
        }
    };
    let mut child = cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).process_group(0).spawn()?;
    // Reap it without blocking the menu (`open` returns at once; a direct window runs on).
    std::thread::spawn(move || child.wait());
    Ok(())
}

/// `…/Runwick.app` when `exe` is the app bundle's executable.
pub fn app_bundle(exe: &Path) -> Option<PathBuf> {
    let macos = exe.parent()?;
    let contents = macos.parent()?;
    let bundle = contents.parent()?;
    let is_bundle = macos.file_name()? == "MacOS"
        && contents.file_name()? == "Contents"
        && bundle.extension().is_some_and(|e| e == "app");
    is_bundle.then(|| bundle.to_path_buf())
}

/// Apps started by Launch Services do not inherit our environment: pass the data folder on.
fn forwarded_env() -> Vec<(String, String)> {
    [paths::ENV_HOME, paths::ENV_SOCKET]
        .into_iter()
        .filter_map(|k| std::env::var(k).ok().filter(|v| !v.is_empty()).map(|v| (k.to_string(), v)))
        .collect()
}

/// `open -n` starts a new instance: this menu bar process is itself the bundle's executable, so a
/// plain `open` would only activate it instead of opening the window.
fn open_args(bundle: &Path, req: Option<OpenRequest>, env: &[(String, String)]) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec!["-n".into()];
    for (k, v) in env {
        args.push("--env".into());
        args.push(format!("{k}={v}").into());
    }
    args.push(bundle.into());
    if let Some(r) = req {
        args.extend(["--args".into(), "--open".into(), r.to_arg().into()]);
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_bundle_only_for_its_executable() {
        let exe = Path::new("/Applications/Runwick.app/Contents/MacOS/runwick");
        assert_eq!(app_bundle(exe), Some(PathBuf::from("/Applications/Runwick.app")));
        assert_eq!(app_bundle(Path::new("/repo/target/debug/runwick")), None);
        assert_eq!(app_bundle(Path::new("/x/Runwick/Contents/MacOS/runwick")), None);
    }

    #[test]
    fn open_passes_home_and_request_to_a_new_instance() {
        let env = vec![("RUNWICK_HOME".to_string(), "/tmp/rw home".to_string())];
        let args = open_args(Path::new("/A/Runwick.app"), Some(OpenRequest::Activity), &env);
        let args: Vec<String> = args.iter().map(|a| a.to_string_lossy().into_owned()).collect();
        assert_eq!(
            args,
            ["-n", "--env", "RUNWICK_HOME=/tmp/rw home", "/A/Runwick.app", "--args", "--open", "activity"]
        );
        let plain = open_args(Path::new("/A/Runwick.app"), None, &[]);
        assert_eq!(plain, vec![OsString::from("-n"), OsString::from("/A/Runwick.app")]);
    }
}
