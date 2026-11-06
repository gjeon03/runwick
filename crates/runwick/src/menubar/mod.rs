//! `runwick menubar`: Runwick in the macOS menu bar while the window is closed
//! (docs/design/screens/MenuBar.dc.html, RW-04). Closing the window ends the GUI process and the
//! host keeps the runs; this small process shows how many runs need the person (확인할 일) or are
//! running, and opens the window again, on 전체 활동, or on the 작업까지 종료 dialog.
//!
//! Rules
//! - Never starts a host. It looks at this computer's socket every few seconds and is idle
//!   otherwise.
//! - One per `RUNWICK_HOME` (`menubar.lock`). It steps aside when a window is open (the window
//!   is then the presence), when the host is gone, when no run is left, or when the
//!   `menubar.enabled` preference is off (see `runwick_app::presence`).
//! - The window starts it with `--after-window` when it closes while runs continue.

#[cfg(target_os = "macos")]
mod launch;
#[cfg(target_os = "macos")]
mod model;
#[cfg(target_os = "macos")]
mod poll;
#[cfg(target_os = "macos")]
mod tray;

use std::path::Path;

#[derive(clap::Args)]
pub struct MenubarArgs {
    /// Started by a closing window: wait until it has closed.
    #[arg(long, hide = true)]
    after_window: bool,
}

#[cfg(not(target_os = "macos"))]
pub fn run(_exe: &Path, _args: MenubarArgs) -> anyhow::Result<()> {
    log("메뉴 막대 아이콘은 macOS에서만 지원해요");
    Ok(())
}

#[cfg(target_os = "macos")]
pub fn run(exe: &Path, args: MenubarArgs) -> anyhow::Result<()> {
    use std::time::Duration;
    let Some(_instance) = launch::single_instance()? else {
        log("이미 메뉴 막대에 있어요");
        return Ok(());
    };
    if args.after_window && !launch::wait_window_closed(Duration::from_secs(10)) {
        log(model::Exit::WindowOpen.reason());
        return Ok(());
    }
    let first = match model::Watch::first(poll::look(), runwick_app::presence::window_open()) {
        Ok(view) => view,
        Err(exit) => {
            log(exit.reason());
            return Ok(());
        }
    };
    log(&format!("메뉴 막대에 표시해요 ({})", model::status_line(&first)));
    tray::run(exe.to_path_buf(), first)
}

/// One line on stderr (the app sends it to `logs/menubar.log`).
fn log(msg: &str) {
    eprintln!("{} runwick menubar: {msg}", runwick_core::now().format("%Y-%m-%dT%H:%M:%SZ"));
}
