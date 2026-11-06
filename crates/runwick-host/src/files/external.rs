//! Showing a file in the system file manager and opening it in another app on the computer
//! that owns the workspace. Executables are never launched from here.

use runwick_core::api::files::OpenWith;
use runwick_core::josa;
use runwick_core::rpc::{RpcError, codes};
use std::path::Path;
use std::time::Duration;

/// Extensions that run something when "opened" by the system.
const LAUNCHES: &[&str] = &[
    "app",
    "command",
    "tool",
    "terminal",
    "workflow",
    "pkg",
    "mpkg",
    "scpt",
    "applescript",
    "jar",
    "action",
    "fileloc",
    "inetloc",
    "webloc",
    "url",
    "shortcut",
    "prefpane",
    "saver",
];

fn launches_code(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let ext = path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase());
    if ext.as_deref().is_some_and(|e| LAUNCHES.contains(&e)) {
        return true;
    }
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

async fn run(program: &str, args: &[&std::ffi::OsStr], what: &str) -> Result<(), RpcError> {
    let child = tokio::process::Command::new(program).args(args).kill_on_drop(false).output();
    match tokio::time::timeout(Duration::from_secs(10), child).await {
        Err(_) => Ok(()), // Still opening; the app took over.
        Ok(Err(e)) => Err(RpcError::internal(format!("{what} 못했어요: {e}"))),
        Ok(Ok(out)) if out.status.success() => Ok(()),
        Ok(Ok(out)) => {
            let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
            Err(RpcError::internal(format!("{what} 못했어요: {err}")))
        }
    }
}

/// Select the file (or folder) in Finder.
pub async fn reveal(path: &Path) -> Result<(), RpcError> {
    if cfg!(target_os = "macos") {
        run("open", &["-R".as_ref(), path.as_os_str()], "Finder에서 보여주지").await
    } else {
        let dir = if path.is_dir() { path } else { path.parent().unwrap_or(path) };
        run("xdg-open", &[dir.as_os_str()], "파일 관리자를 열지").await
    }
}

/// Open in the configured editor command, or the system's text editor / default app.
pub async fn open(path: &Path, with: OpenWith, editor: Option<String>) -> Result<(), RpcError> {
    match with {
        OpenWith::Editor => {
            if let Some(cmd) = editor.filter(|c| !c.trim().is_empty()) {
                let mut parts = cmd.split_whitespace();
                let program = parts.next().unwrap_or_default().to_string();
                let mut args: Vec<std::ffi::OsString> = parts.map(Into::into).collect();
                args.push(path.as_os_str().to_owned());
                // Editors may keep running (`code --wait`); do not wait for them.
                return tokio::process::Command::new(&program)
                    .args(&args)
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn()
                    .map(|_| ())
                    .map_err(|e| {
                        RpcError::internal(format!(
                            "에디터 {} 열지 못했어요: {e}",
                            josa::eul_reul(&format!("‘{program}’"))
                        ))
                    });
            }
            if cfg!(target_os = "macos") {
                run("open", &["-t".as_ref(), path.as_os_str()], "텍스트 에디터로 열지").await
            } else {
                run("xdg-open", &[path.as_os_str()], "외부 앱으로 열지").await
            }
        }
        OpenWith::Default => {
            if launches_code(path) {
                return Err(RpcError::new(
                    codes::DENIED,
                    "실행할 수 있는 파일은 여기서 바로 열지 않아요. Finder에서 보기를 써 주세요",
                ));
            }
            let program = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
            run(program, &[path.as_os_str()], "외부 앱으로 열지").await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn executables_are_not_launched() {
        let d = tempfile::tempdir().unwrap();
        let script = d.path().join("run.sh");
        std::fs::write(&script, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(launches_code(&script));
        assert!(launches_code(&d.path().join("x.command")));
        let doc = d.path().join("a.md");
        std::fs::write(&doc, "x").unwrap();
        assert!(!launches_code(&doc));
    }
}
