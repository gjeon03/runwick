//! End-to-end session runtime against an in-process host on a temporary `RUNWICK_HOME`,
//! with `/bin/sh` as the user's shell so runs are fast and do not depend on personal rc files.

use alacritty_terminal::event::VoidListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::vte::ansi::Processor;
use runwick_core::api::host::{Hello, HostHello, HostShutdown, ShutdownParams};
use runwick_core::api::projects::{ById, OpenFolder, ProjectsOpen};
use runwick_core::api::sessions::*;
use runwick_core::rpc::{Message, Method, ResBody, RpcError, codes};
use runwick_core::wire::{Decoder, Frame};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::{OnceLock, mpsc};
use std::time::{Duration, Instant};
use uuid::Uuid;

const WAIT: Duration = Duration::from_secs(15);

struct Shared {
    home: PathBuf,
    socket: PathBuf,
}

static HOST: OnceLock<Shared> = OnceLock::new();

/// One host for the whole test binary (paths derive from `RUNWICK_HOME`, a process variable).
fn host() -> &'static Shared {
    HOST.get_or_init(|| {
        // Under the build's temp folder (the host outlives every test, so nothing removes it at
        // exit); runs left by earlier test processes are cleared first.
        let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("session_runtime");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let dir = tempfile::Builder::new().prefix("rw").tempdir_in(&root).unwrap().keep();
        let home = dir.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let socket = dir.join("h.sock");
        // Set before the host starts any thread; every test goes through this function first.
        unsafe {
            std::env::set_var("RUNWICK_HOME", &home);
            std::env::set_var("RUNWICK_SOCKET", &socket);
            std::env::set_var("SHELL", "/bin/sh");
        }
        serve(&home, &socket);
        Shared { home, socket }
    })
}

/// The current window token. Hosts started in this test process share it (a second host on
/// the same home renews it for both), so it is read at each connection.
fn window_token() -> String {
    std::fs::read_to_string(runwick_core::paths::gui_token_path()).unwrap().trim().to_string()
}

fn serve(home: &Path, socket: &Path) {
    let (h, s) = (home.to_path_buf(), socket.to_path_buf());
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        rt.block_on(runwick_host::serve_at(&h, &s)).unwrap();
    });
    let deadline = Instant::now() + WAIT;
    while UnixStream::connect(socket).is_err() {
        assert!(Instant::now() < deadline, "host did not start");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A protocol client that keeps every frame it sees, in arrival order.
struct Conn {
    w: UnixStream,
    rx: mpsc::Receiver<Frame>,
    next: u64,
    client_id: u64,
    output: HashMap<Uuid, Vec<u8>>,
    lives: Vec<SessionLive>,
}

impl Conn {
    fn open(socket: &Path, label: &str) -> Conn {
        let w = UnixStream::connect(socket).unwrap();
        let mut r = w.try_clone().unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut d = Decoder::new();
            let mut buf = [0u8; 65536];
            while let Ok(n) = r.read(&mut buf) {
                if n == 0 {
                    break;
                }
                d.push(&buf[..n]);
                while let Ok(Some(f)) = d.next_frame() {
                    if tx.send(f).is_err() {
                        return;
                    }
                }
            }
        });
        let mut c = Conn { w, rx, next: 1, client_id: 0, output: HashMap::new(), lives: Vec::new() };
        let info = c
            .call::<HostHello>(Hello {
                client: label.into(),
                version: "test".into(),
                interactive: true,
                // A window of this computer: the host's per-launch window token.
                token: Some(window_token()),
            })
            .unwrap();
        c.client_id = info.client_id;
        c
    }

    fn call<M: Method>(&mut self, params: M::Params) -> Result<M::Output, RpcError> {
        let id = self.next;
        self.next += 1;
        let req = Message::Req { id, method: M::NAME.into(), params: serde_json::to_value(params).unwrap() };
        self.w.write_all(&Frame::Message(req).encode().unwrap()).unwrap();
        let deadline = Instant::now() + WAIT;
        loop {
            let f = self.rx.recv_timeout(deadline.saturating_duration_since(Instant::now())).expect("response");
            match f {
                Frame::Message(Message::Res { id: rid, body }) if rid == id => {
                    return match body {
                        ResBody::Ok(v) => Ok(serde_json::from_value(v).unwrap()),
                        ResBody::Err(e) => Err(e),
                    };
                }
                other => self.keep(other),
            }
        }
    }

    fn keep(&mut self, f: Frame) {
        match f {
            Frame::Output { session, data } => self.output.entry(session).or_default().extend(data),
            Frame::Message(Message::Event { topic, data }) if topic == EVENT_LIVE => {
                if let Ok(l) = serde_json::from_value::<SessionLive>(data) {
                    self.lives.push(l);
                }
            }
            _ => {}
        }
    }

    fn input(&mut self, session: Uuid, text: &str) {
        for f in Frame::input_chunks(session, text.as_bytes()) {
            self.w.write_all(&f.encode().unwrap()).unwrap();
        }
    }

    fn text(&self, session: Uuid) -> String {
        String::from_utf8_lossy(self.output.get(&session).map(|v| v.as_slice()).unwrap_or(&[])).into_owned()
    }

    /// Receive until the session's output contains `needle` `times` times.
    fn wait_output(&mut self, session: Uuid, needle: &str, times: usize) -> String {
        let deadline = Instant::now() + WAIT;
        loop {
            let text = self.text(session);
            if text.matches(needle).count() >= times {
                return text;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            match self.rx.recv_timeout(left) {
                Ok(f) => self.keep(f),
                Err(_) => panic!("timed out waiting for {needle:?} x{times}; got {text:?}"),
            }
        }
    }

    fn wait_live(&mut self, session: Uuid, pred: impl Fn(&SessionLive) -> bool) -> SessionLive {
        let deadline = Instant::now() + WAIT;
        loop {
            if let Some(l) = self.lives.iter().rev().find(|l| l.id == session && pred(l)) {
                return l.clone();
            }
            let left = deadline.saturating_duration_since(Instant::now());
            match self.rx.recv_timeout(left) {
                Ok(f) => self.keep(f),
                Err(_) => panic!("timed out waiting for live state; last {:?}", self.lives.last()),
            }
        }
    }

    /// Receive whatever arrives within `d`.
    fn settle(&mut self, d: Duration) {
        let deadline = Instant::now() + d;
        while let Ok(f) = self.rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            self.keep(f);
        }
    }
}

/// Wait for the shell prompt: input typed before the shell sets up its terminal may be dropped.
/// "$" alone: when the attach snapshot comes after the prompt, its trailing space is not part
/// of it (a snapshot reproduces cells, and a blank at the end of a line is none).
fn wait_prompt(c: &mut Conn, id: Uuid) {
    c.wait_output(id, "$", 1);
}

fn new_folder() -> tempfile::TempDir {
    tempfile::Builder::new().prefix("rwws").tempdir_in("/tmp").unwrap()
}

fn start_shell(c: &mut Conn, dir: &Path, cols: u16, rows: u16) -> SessionInfo {
    let opened = c.call::<ProjectsOpen>(OpenFolder { path: dir.to_string_lossy().into() }).unwrap();
    c.call::<SessionsStart>(StartSession {
        workspace_id: opened.workspace.id,
        tool: Tool::Shell,
        title: None,
        initial_prompt: None,
        parent_id: None,
        role: None,
        cols,
        rows,
    })
    .unwrap()
}

/// Visible rows after feeding `bytes` into a fresh terminal, trailing blanks trimmed.
fn render(bytes: &[u8], cols: usize, rows: usize) -> Vec<String> {
    let mut term = Term::new(Config::default(), &TermSize::new(cols, rows), VoidListener);
    let mut p: Processor = Processor::new();
    p.advance(&mut term, bytes);
    screen_rows(&term)
}

fn screen_rows<T>(term: &Term<T>) -> Vec<String> {
    let grid = term.grid();
    let mut rows: Vec<String> = (0..grid.screen_lines() as i32)
        .map(|l| {
            let row = &grid[Line(l)];
            let s: String = (0..grid.columns())
                .map(|c| &row[Column(c)])
                .filter(|cell| !cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER))
                .map(|cell| cell.c)
                .collect();
            s.trim_end().to_string()
        })
        .collect();
    while rows.last().is_some_and(|r| r.is_empty()) {
        rows.pop();
    }
    rows
}

#[test]
fn shell_startup_stays_read_until_input_and_resets_on_restart() {
    let h = host();
    let mut c = Conn::open(&h.socket, "unread-startup");
    let dir = new_folder();
    let id = start_shell(&mut c, dir.path(), 80, 24).session.id;
    c.call::<SessionsAttach>(Attach { id, cols: 80, rows: 24, interactive: true }).unwrap();
    wait_prompt(&mut c, id);
    let live = c.call::<SessionsGet>(ById { id }).unwrap().live.unwrap();
    assert!(!live.unread, "an unseen first prompt stays read");
    assert!(!runwick_core::api::activity::has_new_response(&live));

    let prompts = c.text(id).matches("$ ").count();
    c.input(id, "echo UNREAD_$((40+2))\r");
    c.wait_output(id, "UNREAD_42", 1);
    c.wait_output(id, "$ ", prompts + 1);
    let info = c.call::<SessionsGet>(ById { id }).unwrap();
    assert!(info.live.as_ref().unwrap().unread);
    assert_eq!(
        runwick_core::api::activity::session_group(&info.session, info.live.as_ref()),
        Some((runwick_core::api::activity::Group::Fresh, "새 출력"))
    );
    c.call::<SessionsView>(View { id, viewing: true }).unwrap();
    assert!(!c.call::<SessionsGet>(ById { id }).unwrap().live.unwrap().unread);
    c.call::<SessionsView>(View { id, viewing: false }).unwrap();

    c.output.remove(&id);
    c.call::<SessionsRestart>(Restart { id, resume: false, cols: 80, rows: 24 }).unwrap();
    wait_prompt(&mut c, id);
    assert!(!c.call::<SessionsGet>(ById { id }).unwrap().live.unwrap().unread);
    c.call::<SessionsStop>(Stop { id, force: true }).unwrap();
}

#[test]
fn shell_session_end_to_end() {
    let h = host();
    let mut c = Conn::open(&h.socket, "first");
    let dir = new_folder();
    let info = start_shell(&mut c, dir.path(), 80, 24);
    let id = info.session.id;
    assert_eq!(info.session.title, "셸");
    let live = info.live.expect("runtime state");
    assert!(matches!(live.phase, Phase::Starting | Phase::Running), "{live:?}");
    assert_eq!(live.error, None);

    let live = c.call::<SessionsAttach>(Attach { id, cols: 80, rows: 24, interactive: true }).unwrap();
    assert_eq!(live.input_owner_client, Some(c.client_id), "first interactive attach owns input");
    assert!(c.output[&id].starts_with(b"\x1bc"), "snapshot starts with a full reset");
    wait_prompt(&mut c, id);
    // A short prompt keeps readline's resize redraw from overwriting earlier rows
    // when the default prompt (host name, folder) is wider than the terminal.
    c.input(id, "PS1='rw$ '\r");
    c.wait_output(id, "rw$ ", 2); // the echoed command, then the new prompt

    c.input(id, "echo RW_$((40+2))\r");
    c.wait_output(id, "RW_42", 1);
    // Korean text through input and output, split across UTF-8 boundaries by the PTY.
    c.input(id, "echo 안녕$((1+1)) 한글\r");
    c.wait_output(id, "안녕2 한글", 1);
    c.wait_live(id, |l| l.phase == Phase::Running);

    // Resize, then let the program report its size.
    let resize_at = c.output[&id].len();
    c.call::<SessionsResize>(Resize { id, cols: 100, rows: 30 }).unwrap();
    c.input(id, "stty size\r");
    c.wait_output(id, "30 100", 1);
    c.settle(Duration::from_millis(300));

    // The first client's stream renders to exactly the host's screen.
    let bytes = c.output[&id].clone();
    let mut term = Term::new(Config::default(), &TermSize::new(80, 24), VoidListener);
    let mut p: Processor = Processor::new();
    p.advance(&mut term, &bytes[..resize_at]);
    term.resize(TermSize::new(100, 30));
    p.advance(&mut term, &bytes[resize_at..]);
    let host_screen = c.call::<SessionsScreenText>(ScreenQuery { id, scrollback: 0 }).unwrap();
    assert_eq!(screen_rows(&term).join("\n"), host_screen);
    assert!(host_screen.contains("안녕2 한글"), "{host_screen}");

    // A second client attaching now gets a snapshot that renders the same screen.
    let mut c2 = Conn::open(&h.socket, "second");
    let live2 = c2.call::<SessionsAttach>(Attach { id, cols: 60, rows: 10, interactive: false }).unwrap();
    assert_eq!((live2.cols, live2.rows), (100, 30), "a viewer does not resize the run");
    assert_eq!(live2.input_owner_client, Some(c.client_id));
    c2.settle(Duration::from_millis(200));
    assert_eq!(render(&c2.output[&id], 100, 30).join("\n"), host_screen);

    // Input from a client that does not hold input is dropped.
    c2.input(id, "echo FROM_$((1))_SECOND\r");
    c.input(id, "echo FROM_$((2))_FIRST\r");
    c.wait_output(id, "FROM_2_FIRST", 1);
    c.settle(Duration::from_millis(300));
    assert!(!c.text(id).contains("FROM_1_SECOND"), "non-owner input reached the program");

    // Taking input moves ownership; the previous owner can no longer resize.
    c2.call::<SessionsTakeInput>(ById { id }).unwrap();
    let l = c.wait_live(id, |l| l.input_owner_client == Some(c2.client_id));
    assert_eq!(l.input_owner.as_deref(), Some("second"));
    let err = c.call::<SessionsResize>(Resize { id, cols: 90, rows: 20 }).unwrap_err();
    assert_eq!(err.code, codes::DENIED);
    c2.input(id, "echo OWNER_$((3))\r");
    c.wait_output(id, "OWNER_3", 1);

    // Stopping hangs up the process group; the record keeps the end.
    c.call::<SessionsStop>(Stop { id, force: false }).unwrap();
    let l = c.wait_live(id, |l| l.phase == Phase::Exited);
    assert_eq!(l.pid, None);
    let got = c.call::<SessionsGet>(ById { id }).unwrap();
    assert!(got.session.ended_at.is_some());
    assert!(got.session.exit_code.is_some());
    assert!(!got.session.interrupted);
    assert_eq!(got.live.map(|l| l.phase), Some(Phase::Exited));
}

#[test]
fn restart_starts_a_new_run_below_the_previous_output() {
    let h = host();
    let mut c = Conn::open(&h.socket, "restart");
    let dir = new_folder();
    let id = start_shell(&mut c, dir.path(), 70, 20).session.id;
    c.call::<SessionsAttach>(Attach { id, cols: 70, rows: 20, interactive: true }).unwrap();
    wait_prompt(&mut c, id);
    c.input(id, "echo BEFORE_$((5))\r");
    c.wait_output(id, "BEFORE_5", 1);
    let first_pid = c.wait_live(id, |l| l.pid.is_some()).pid;

    let before = c.output[&id].len();
    let prompts = c.text(id).matches("$ ").count();
    let info = c.call::<SessionsRestart>(Restart { id, resume: false, cols: 70, rows: 20 }).unwrap();
    assert_eq!(info.session.id, id);
    let live = info.live.unwrap();
    assert!(matches!(live.phase, Phase::Starting | Phase::Running));
    assert_ne!(live.pid, first_pid);
    assert!(!c.output[&id][before..].windows(2).any(|w| w == b"\x1bc"), "the previous output is not wiped");
    c.wait_output(id, "$ ", prompts + 1);
    c.input(id, "echo AFTER_$((6))\r");
    c.wait_output(id, "AFTER_6", 1);
    let screen = c.call::<SessionsScreenText>(ScreenQuery { id, scrollback: 1000 }).unwrap();
    // Earlier output stays, above a dim separator line.
    let (old, new) = screen.split_once("── 새로 실행 · ").unwrap_or_else(|| panic!("no separator: {screen}"));
    assert!(old.contains("BEFORE_5") && new.contains("AFTER_6"), "{screen}");
    c.call::<SessionsStop>(Stop { id, force: true }).unwrap();
    c.wait_live(id, |l| l.phase == Phase::Exited);
}

#[test]
fn missing_workspace_folder_is_a_visible_start_error() {
    let h = host();
    let mut c = Conn::open(&h.socket, "missing");
    let dir = new_folder();
    let opened = c.call::<ProjectsOpen>(OpenFolder { path: dir.path().to_string_lossy().into() }).unwrap();
    drop(dir);
    let info = c
        .call::<SessionsStart>(StartSession {
            workspace_id: opened.workspace.id,
            tool: Tool::Shell,
            title: Some("  빌드 ".into()),
            initial_prompt: None,
            parent_id: None,
            role: None,
            cols: 80,
            rows: 24,
        })
        .unwrap();
    assert_eq!(info.session.title, "빌드");
    assert!(info.session.title_user_set);
    let live = info.live.unwrap();
    assert_eq!(live.phase, Phase::Exited);
    assert_eq!(live.error_kind, Some(StartErrorKind::FolderMissing));
    assert!(live.error.unwrap().contains("작업 폴더를 찾을 수 없어요"));
}

#[test]
fn a_host_restart_leaves_runs_as_records_offered_for_resume() {
    let h = host();
    let mut c = Conn::open(&h.socket, "before-restart");
    let dir = new_folder();
    let id = start_shell(&mut c, dir.path(), 80, 24).session.id;
    c.call::<SessionsAttach>(Attach { id, cols: 80, rows: 24, interactive: true }).unwrap();
    wait_prompt(&mut c, id);
    c.input(id, "echo ALIVE\r");
    c.wait_output(id, "ALIVE", 2);
    // The screen is saved once output pauses.
    std::thread::sleep(Duration::from_millis(1600));

    // A second host process on the same records (as after a reboot): nothing is started.
    let socket2 = h.socket.with_file_name("h2.sock");
    serve(&h.home, &socket2);
    let mut c2 = Conn::open(&socket2, "after-restart");
    let got = c2.call::<SessionsGet>(ById { id }).unwrap();
    assert!(got.session.interrupted, "offered for resume");
    assert_eq!(got.live, None, "record only");
    let live = c2.call::<SessionsAttach>(Attach { id, cols: 80, rows: 24, interactive: true }).unwrap();
    assert_eq!(live.phase, Phase::NotRunning);
    assert_eq!(live.pid, None);
    // The last screen is shown again (not re-run).
    c2.settle(Duration::from_millis(200));
    let restored = render(&c2.output[&id], 80, 24).join("\n");
    assert!(restored.contains("ALIVE"), "{restored}");

    // Ending the run on the first host clears the offer.
    c.call::<SessionsStop>(Stop { id, force: false }).unwrap();
    c.wait_live(id, |l| l.phase == Phase::Exited);
    assert!(!c2.call::<SessionsGet>(ById { id }).unwrap().session.interrupted);
    c2.call::<HostShutdown>(ShutdownParams { stop_runs: false }).unwrap();
}

#[test]
fn listing_filters_by_workspace_and_hides_closed_tabs() {
    let h = host();
    let mut c = Conn::open(&h.socket, "list");
    let dir = new_folder();
    let info = start_shell(&mut c, dir.path(), 80, 24);
    let ws = info.session.workspace_id;
    let q = SessionQuery { workspace_id: Some(ws), ..Default::default() };
    assert_eq!(c.call::<SessionsList>(q.clone()).unwrap().len(), 1);
    let upd = SessionUpdate { id: info.session.id, hidden: Some(true), ..Default::default() };
    c.call::<SessionsUpdate>(upd).unwrap();
    assert!(c.call::<SessionsList>(q.clone()).unwrap().is_empty());
    let all = c.call::<SessionsList>(SessionQuery { include_hidden: true, ..q }).unwrap();
    assert_eq!(all.len(), 1);
    let renamed = c
        .call::<SessionsUpdate>(SessionUpdate {
            id: info.session.id,
            title: Some("배포 셸".into()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(renamed.title, "배포 셸");
    assert!(
        c.call::<SessionsUpdate>(SessionUpdate { id: info.session.id, title: Some(" ".into()), ..Default::default() })
            .is_err()
    );
    c.call::<SessionsStop>(Stop { id: info.session.id, force: true }).unwrap();
}

/// Poll the record until its title is `want` (titles apply once the program's title settles).
fn wait_title(c: &mut Conn, id: Uuid, want: &str) -> String {
    let deadline = Instant::now() + WAIT;
    loop {
        let title = c.call::<SessionsGet>(ById { id }).unwrap().session.title;
        if title == want || Instant::now() > deadline {
            return title;
        }
        c.settle(Duration::from_millis(100));
    }
}

#[test]
fn shell_titles_follow_commands_but_not_the_prompt_or_a_given_name() {
    let h = host();
    let mut c = Conn::open(&h.socket, "titles");
    let dir = new_folder();
    let id = start_shell(&mut c, dir.path(), 80, 24).session.id;
    c.call::<SessionsAttach>(Attach { id, cols: 80, rows: 24, interactive: true }).unwrap();
    wait_prompt(&mut c, id);
    let title = |c: &mut Conn, t: &str, mark: &str| {
        c.input(id, &format!("printf '\\033]2;{t}\\007'; echo {mark}_$((1))\r"));
        c.wait_output(id, &format!("{mark}_1"), 1);
    };

    title(&mut c, "npm run dev", "A");
    assert_eq!(wait_title(&mut c, id, "npm run dev"), "npm run dev");
    // Back at the prompt: `user@host:path` is not a name for the session.
    title(&mut c, "jy@Jeongs-MacBook-Pro:~/work", "B");
    assert_eq!(wait_title(&mut c, id, "셸"), "셸");

    // A name the person gave wins over the program.
    c.call::<SessionsUpdate>(SessionUpdate { id, title: Some("배포".into()), ..Default::default() }).unwrap();
    title(&mut c, "vim notes.md", "C");
    c.settle(Duration::from_millis(1500));
    assert_eq!(c.call::<SessionsGet>(ById { id }).unwrap().session.title, "배포");
    c.call::<SessionsStop>(Stop { id, force: true }).unwrap();
}

#[test]
fn a_client_that_falls_behind_is_resynced_instead_of_queued_without_limit() {
    let h = host();
    let mut c = Conn::open(&h.socket, "fast");
    let dir = new_folder();
    let id = start_shell(&mut c, dir.path(), 80, 24).session.id;
    c.call::<SessionsAttach>(Attach { id, cols: 80, rows: 24, interactive: true }).unwrap();
    wait_prompt(&mut c, id);

    // A viewer that stops reading its socket.
    let mut slow = UnixStream::connect(&h.socket).unwrap();
    let req = |i: u64, method: &str, params: serde_json::Value| {
        Frame::Message(Message::Req { id: i, method: method.into(), params }).encode().unwrap()
    };
    // A window of this computer (only windows may attach), like `Conn::open`.
    let hello = serde_json::json!({"client": "slow", "version": "test", "interactive": true, "token": window_token()});
    slow.write_all(&req(1, "host.hello", hello)).unwrap();
    let attach = serde_json::json!({"id": id, "cols": 80, "rows": 24, "interactive": false});
    slow.write_all(&req(2, "sessions.attach", attach)).unwrap();
    std::thread::sleep(Duration::from_millis(300));

    // Far more output than a client may have queued.
    let total = runwick_host::host::MAX_QUEUED_OUTPUT + 16 * 1024 * 1024;
    c.input(id, &format!("yes 0123456789abcdefghijklmnopqrstuvwxyz | head -c {total}; echo END_$((6*7))\r"));
    let deadline = Instant::now() + Duration::from_secs(120);
    while !c.text(id).contains("END_42") {
        assert!(Instant::now() < deadline, "output did not finish");
        c.settle(Duration::from_millis(200));
    }
    let screen = c.call::<SessionsScreenText>(ScreenQuery { id, scrollback: 0 }).unwrap();

    // Reading again: the dropped output is replaced by a fresh snapshot of the current screen.
    slow.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
    let mut d = Decoder::new();
    let mut got = Vec::new();
    let mut buf = vec![0u8; 1 << 16];
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        match slow.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => d.push(&buf[..n]),
            Err(_) => {
                let resynced = got.windows(2).filter(|w| w == b"\x1bc").count() >= 2;
                if resynced && String::from_utf8_lossy(&got).contains("END_42") || Instant::now() > deadline {
                    break;
                }
            }
        }
        while let Ok(Some(f)) = d.next_frame() {
            match f {
                Frame::Output { session, data } if session == id => got.extend(data),
                Frame::Message(Message::Res { body: ResBody::Err(e), .. }) => {
                    panic!("slow client request failed: {e:?}")
                }
                _ => {}
            }
        }
    }
    assert!(got.len() < total, "the host dropped output for the slow client ({} bytes)", got.len());
    let last_reset = got.windows(2).rposition(|w| w == b"\x1bc").unwrap_or_else(|| {
        panic!("no snapshot in {} bytes: {:?}", got.len(), String::from_utf8_lossy(&got[..got.len().min(300)]))
    });
    assert!(last_reset > 0, "a resync snapshot followed the first one");
    assert_eq!(render(&got[last_reset..], 80, 24).join("\n"), screen);
    c.call::<SessionsStop>(Stop { id, force: true }).unwrap();
}

/// Processes whose command line matches `pattern` (`pgrep -f`).
fn processes(pattern: &str) -> Vec<String> {
    let out = std::process::Command::new("pgrep").args(["-fl", pattern]).output().unwrap();
    String::from_utf8_lossy(&out.stdout).lines().map(str::to_string).collect()
}

#[test]
fn stopping_ends_every_job_of_the_run() {
    let h = host();
    let mut c = Conn::open(&h.socket, "stop");
    let dir = new_folder();
    // Jobs in their own process groups, some ignoring hang-up, and a pipeline.
    let cmds = [
        "sleep 61.7341",
        "sleep 61.7342 &",
        "sh -c \"trap '' HUP; exec sleep 61.7343\"",
        "(trap '' HUP; exec sleep 61.7344) &",
        "sleep 61.7345 | cat",
    ];
    let mut ids = Vec::new();
    for cmd in cmds {
        let id = start_shell(&mut c, dir.path(), 80, 24).session.id;
        c.call::<SessionsAttach>(Attach { id, cols: 80, rows: 24, interactive: true }).unwrap();
        wait_prompt(&mut c, id);
        c.input(id, &format!("{cmd}\r"));
        ids.push(id);
    }
    // Anchored: only the jobs themselves, not a command line that mentions them.
    // Short-lived and unique: a test that is cut off leaves nothing behind for long.
    let pattern = "^sleep 61\\.734[1-5]$";
    let deadline = Instant::now() + WAIT;
    while processes(pattern).len() < 5 {
        assert!(Instant::now() < deadline, "jobs did not start: {:?}", processes(pattern));
        c.settle(Duration::from_millis(100));
    }

    // A silent foreground job keeps the shell "running", not waiting for input.
    let fg = c.call::<SessionsGet>(ById { id: ids[0] }).unwrap().live.unwrap();
    assert!(fg.foreground_job, "{fg:?}");
    let bg = c.call::<SessionsGet>(ById { id: ids[1] }).unwrap().live.unwrap();
    assert!(!bg.foreground_job, "a background job leaves the prompt to the shell");

    for id in &ids {
        c.call::<SessionsStop>(Stop { id: *id, force: false }).unwrap();
    }
    let deadline = Instant::now() + Duration::from_secs(15);
    while !processes(pattern).is_empty() {
        assert!(Instant::now() < deadline, "left behind: {:?}", processes(pattern));
        c.settle(Duration::from_millis(100));
    }
    for id in &ids {
        let l = c.wait_live(*id, |l| l.phase == Phase::Exited);
        assert!(l.leftovers.is_empty(), "{:?}", l.leftovers);
    }
}

#[test]
fn stopping_ends_children_that_left_the_session_but_nothing_else() {
    let h = host();
    let mut c = Conn::open(&h.socket, "stop-family");
    let dir = new_folder();
    // Unrelated to any run, same user: must survive.
    let mut bystander = std::process::Command::new("sleep")
        .arg("61")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let id = start_shell(&mut c, dir.path(), 80, 24).session.id;
    c.call::<SessionsAttach>(Attach { id, cols: 80, rows: 24, interactive: true }).unwrap();
    wait_prompt(&mut c, id);
    // A tool command in a session of its own (like Node's `detached: true`), its parent alive.
    c.input(id, "perl -e 'use POSIX; if (fork() == 0) { POSIX::setsid(); exec(\"sleep\", \"61.7351\") } exec(\"sleep\", \"61.7352\")'\r");
    let pattern = "^sleep 61\\.735[12]$";
    let deadline = Instant::now() + WAIT;
    while processes(pattern).len() < 2 {
        assert!(Instant::now() < deadline, "not started: {:?}", processes(pattern));
        c.settle(Duration::from_millis(100));
    }
    c.call::<SessionsStop>(Stop { id, force: false }).unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    while !processes(pattern).is_empty() {
        assert!(Instant::now() < deadline, "left behind: {:?}", processes(pattern));
        c.settle(Duration::from_millis(100));
    }
    let l = c.wait_live(id, |l| l.phase == Phase::Exited);
    assert!(l.leftovers.is_empty(), "{:?}", l.leftovers);
    assert!(bystander.try_wait().unwrap().is_none(), "a process outside the run was touched");
    bystander.kill().unwrap();
    let _ = bystander.wait();
}

/// Linux only: macOS does not let the host read another process's environment.
#[cfg(target_os = "linux")]
#[test]
fn a_daemon_that_left_the_run_is_reported_not_killed() {
    let h = host();
    let mut c = Conn::open(&h.socket, "stop-detached");
    let dir = new_folder();
    let id = start_shell(&mut c, dir.path(), 80, 24).session.id;
    c.call::<SessionsAttach>(Attach { id, cols: 80, rows: 24, interactive: true }).unwrap();
    wait_prompt(&mut c, id);
    // Forks twice: handed to launchd before the stop, but it keeps the run's environment.
    c.input(id, "perl -e 'use POSIX; fork and exit; POSIX::setsid(); exec(\"sleep\", \"61.7361\")' </dev/null >/dev/null 2>&1\r");
    let pattern = "^sleep 61\\.7361$";
    let deadline = Instant::now() + WAIT;
    while processes(pattern).is_empty() {
        assert!(Instant::now() < deadline, "daemon did not start");
        c.settle(Duration::from_millis(100));
    }
    c.call::<SessionsStop>(Stop { id, force: false }).unwrap();
    let l = c.wait_live(id, |l| l.phase == Phase::Exited && !l.leftovers.is_empty());
    assert_eq!(l.leftovers.len(), 1, "{:?}", l.leftovers);
    assert!(l.leftovers[0].starts_with("sleep (pid ") && l.leftovers[0].ends_with("· 세션에서 떨어져 계속 실행 중"));
    let error = c.call::<SessionsUpdate>(SessionUpdate { id, archived: Some(true), ..Default::default() }).unwrap_err();
    assert_eq!(error.code, codes::BUSY, "surviving processes must stay visible");
    let daemon = processes(pattern);
    assert_eq!(daemon.len(), 1, "never killed");
    let pid: i32 = daemon[0].split_whitespace().next().unwrap().parse().unwrap();
    unsafe { libc_kill(pid, 15) };
}

#[cfg(target_os = "linux")]
unsafe extern "C" {
    #[link_name = "kill"]
    fn libc_kill(pid: i32, sig: i32) -> i32;
}

#[test]
fn archive_requires_a_completed_stop_and_restore_preserves_the_session() {
    let h = host();
    let mut c = Conn::open(&h.socket, "archive-restore");
    let dir = new_folder();
    let info = start_shell(&mut c, dir.path(), 80, 24);
    let id = info.session.id;
    let archive = || SessionUpdate { id, archived: Some(true), ..Default::default() };
    assert_eq!(c.call::<SessionsUpdate>(archive()).unwrap_err().code, codes::BUSY);
    c.call::<SessionsStop>(Stop { id, force: false }).unwrap();
    // No polling: the stop response is the handoff for the window's archive action.
    let stopped = c.call::<SessionsGet>(ById { id }).unwrap();
    assert_eq!(stopped.live.as_ref().unwrap().phase, Phase::Exited);
    assert!(stopped.live.as_ref().unwrap().leftovers.is_empty());
    assert!(stopped.session.ended_at.is_some());
    let archived = c.call::<SessionsUpdate>(archive()).unwrap();
    assert!(archived.archived);
    let query =
        SessionQuery { workspace_id: Some(info.session.workspace_id), include_hidden: true, ..Default::default() };
    assert!(c.call::<SessionsList>(query.clone()).unwrap().is_empty());
    let all = c.call::<SessionsList>(SessionQuery { include_archived: true, ..query.clone() }).unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].session.id, id);
    let restored = c.call::<SessionsUpdate>(SessionUpdate { id, archived: Some(false), ..Default::default() }).unwrap();
    assert!(!restored.archived);
    assert_eq!(restored.ended_at, stopped.session.ended_at);
    assert_eq!(restored.title, stopped.session.title);
    assert_eq!(restored.cwd, stopped.session.cwd);
    assert_eq!(c.call::<SessionsList>(query).unwrap().len(), 1);
    assert_eq!(c.call::<SessionsGet>(ById { id }).unwrap().live.unwrap().phase, Phase::Exited);
}
