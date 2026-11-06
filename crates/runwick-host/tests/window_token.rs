//! Only Runwick windows act on the host: a raw client on the host socket that claims to be a
//! window without this launch's window token can call the agent CLI's methods only. Global
//! variables, the external editor, sync, backup restore, sessions, files and stored settings
//! are refused; with the token from `<RUNWICK_HOME>/gui.token` the same requests work.

use runwick_core::api::backup::{ActivateRestore, BackupRestoreActivate};
use runwick_core::api::collab::{CollabList, RequestQuery};
use runwick_core::api::env::{EnvList, EnvRemove, EnvSet, SetVar, VarKey};
use runwick_core::api::files::{FilesWrite, WriteFile};
use runwick_core::api::host::{Hello, HostHello, KvGet, KvKey, KvPut, KvSet};
use runwick_core::api::prefs::{PrefsSetEditor, SetEditor};
use runwick_core::api::sessions::{SessionQuery, SessionsList, SessionsStart, StartSession, Tool};
use runwick_core::api::sync::{PreviewId, SyncConnect, SyncDisconnect};
use runwick_core::rpc::{Message, Method, ResBody, RpcError, codes};
use runwick_core::wire::{Decoder, Frame};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

struct Client {
    s: UnixStream,
    dec: Decoder,
    next: u64,
}

impl Client {
    fn connect(socket: &Path) -> Client {
        Client { s: UnixStream::connect(socket).unwrap(), dec: Decoder::new(), next: 0 }
    }

    fn call<M: Method>(&mut self, params: M::Params) -> Result<M::Output, RpcError> {
        self.next += 1;
        let id = self.next;
        let req = Message::Req { id, method: M::NAME.into(), params: serde_json::to_value(params).unwrap() };
        self.s.write_all(&Frame::Message(req).encode().unwrap()).unwrap();
        let mut buf = [0u8; 65536];
        loop {
            while let Some(f) = self.dec.next_frame().unwrap() {
                if let Frame::Message(Message::Res { id: rid, body }) = f
                    && rid == id
                {
                    return match body {
                        ResBody::Ok(v) => Ok(serde_json::from_value(v).unwrap()),
                        ResBody::Err(e) => Err(e),
                    };
                }
            }
            let n = self.s.read(&mut buf).unwrap();
            assert!(n > 0, "host closed the connection");
            self.dec.push(&buf[..n]);
        }
    }

    fn type_into(&mut self, session: uuid::Uuid, text: &str) {
        let f = Frame::Input { session, data: text.as_bytes().to_vec() };
        self.s.write_all(&f.encode().unwrap()).unwrap();
    }

    fn hello(&mut self, interactive: bool, token: Option<String>) {
        let h = Hello { client: "raw client".into(), version: "0".into(), interactive, token };
        self.call::<HostHello>(h).unwrap();
    }
}

/// One host for this test binary (the window token and `RUNWICK_HOME` are process-wide), on
/// a temporary data folder: (folder, data folder, socket).
fn start_host() -> (PathBuf, PathBuf, PathBuf) {
    static HOST: std::sync::OnceLock<(PathBuf, PathBuf, PathBuf)> = std::sync::OnceLock::new();
    HOST.get_or_init(|| {
        let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("window_token");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let dir = tempfile::Builder::new().prefix("rw").tempdir_in(&root).unwrap().keep();
        let home = dir.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let socket = dir.join("h.sock");
        unsafe {
            std::env::set_var("RUNWICK_HOME", &home);
            std::env::set_var("RUNWICK_SOCKET", &socket);
            std::env::set_var("SHELL", "/bin/sh");
        }
        let (h, s) = (home.clone(), socket.clone());
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
            rt.block_on(runwick_host::serve_at(&h, &s)).unwrap();
        });
        let deadline = Instant::now() + Duration::from_secs(15);
        while UnixStream::connect(&socket).is_err() {
            assert!(Instant::now() < deadline, "host did not start");
            std::thread::sleep(Duration::from_millis(20));
        }
        (dir, home, socket)
    })
    .clone()
}

fn denied<T: std::fmt::Debug>(r: Result<T, RpcError>) -> bool {
    r.is_err_and(|e| e.code == codes::DENIED)
}

fn var(key: &str) -> SetVar {
    SetVar { key: key.into(), value: Some("v".into()), description: String::new(), secret: false }
}

fn start_params() -> StartSession {
    StartSession {
        workspace_id: uuid::Uuid::nil(),
        tool: Tool::Shell,
        title: None,
        initial_prompt: None,
        parent_id: None,
        role: None,
        cols: 80,
        rows: 24,
    }
}

fn write_params() -> WriteFile {
    WriteFile { workspace_id: uuid::Uuid::nil(), path: "x.txt".into(), text: "x".into(), base_hash: None }
}

/// Everything a non-window client must not do (each would fail differently if it ran).
fn assert_refused(c: &mut Client) {
    assert!(denied(c.call::<EnvSet>(var("RW_RAW"))));
    assert!(denied(c.call::<EnvList>(())));
    assert!(denied(c.call::<PrefsSetEditor>(SetEditor { command: "/bin/sh -c id".into() })));
    assert!(denied(c.call::<SyncConnect>(PreviewId { id: uuid::Uuid::nil() })));
    assert!(denied(c.call::<SyncDisconnect>(())));
    assert!(denied(c.call::<BackupRestoreActivate>(ActivateRestore::default())));
    assert!(denied(c.call::<KvGet>(KvKey { key: "files.editor".into() })));
    assert!(denied(c.call::<KvPut>(KvSet { key: "files.editor".into(), value: "x".into() })));
    assert!(denied(c.call::<SessionsList>(SessionQuery::default())));
    assert!(denied(c.call::<SessionsStart>(start_params())));
    assert!(denied(c.call::<FilesWrite>(write_params())));
    // The agent CLI's collaboration methods still answer.
    c.call::<CollabList>(RequestQuery::default()).unwrap();
}

#[test]
fn a_window_claim_without_the_token_is_limited_to_agent_methods() {
    let (_dir, home, socket) = start_host();
    let token_path = home.join("gui.token");
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(std::fs::metadata(&token_path).unwrap().permissions().mode() & 0o777, 0o600);
    let token = std::fs::read_to_string(&token_path).unwrap();

    // No token, a wrong one, or the right one with a byte added.
    for claim in [None, Some("f".repeat(64)), Some(format!("{token}0"))] {
        let mut raw = Client::connect(&socket);
        raw.hello(true, claim);
        assert_refused(&mut raw);
    }
    // A client that sends no hello at all, and an agent's honest hello, are the same.
    assert_refused(&mut Client::connect(&socket));
    let mut agent = Client::connect(&socket);
    agent.hello(false, Some(token.trim().to_string()));
    assert_refused(&mut agent);

    // The window token from the data folder makes the connection a window.
    let mut window = Client::connect(&socket);
    window.hello(true, Some(token.trim().to_string()));
    window.call::<EnvSet>(var("RW_WINDOW")).unwrap();
    assert!(window.call::<EnvList>(()).unwrap().vars.iter().any(|v| v.key == "RW_WINDOW"));
    window.call::<EnvRemove>(VarKey { key: "RW_WINDOW".into() }).unwrap();
    window.call::<SessionsList>(SessionQuery::default()).unwrap();
    window.call::<KvGet>(KvKey { key: "menubar.enabled".into() }).unwrap();
    // Asking to be a window again without the token takes it away.
    window.hello(true, None);
    assert!(denied(window.call::<EnvSet>(var("RW_AGAIN"))));
}

/// Not a test of its own: the tests below start this binary again as a separate client process
/// (`RW_HELPER_*` set), which claims to be a window with the token and tries to change a
/// variable, writing the outcome ("ok" or the error code) to `RW_HELPER_OUT`.
#[test]
#[ignore]
fn helper_window_client() {
    let (Ok(sock), Ok(out)) = (std::env::var("RW_HELPER_SOCKET"), std::env::var("RW_HELPER_OUT")) else {
        return;
    };
    let mut c = Client::connect(Path::new(&sock));
    c.hello(true, std::env::var("RW_HELPER_TOKEN").ok());
    let outcome = match c.call::<EnvSet>(var("RW_HELPER")) {
        Ok(_) => "ok".to_string(),
        Err(e) => e.code,
    };
    std::fs::write(out, outcome).unwrap();
}

/// A client that is not Runwick: hello as a window with the token, then `env.set` and
/// `host.window_status`. Prints "<env.set result> <refusal> <may_restart?>".
const OTHER_PROGRAM: &str = r#"
import json, socket, struct, sys
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.connect(sys.argv[1])
def req(i, method, params):
    b = json.dumps({"t": "req", "id": i, "method": method, "params": params}).encode()
    s.sendall(bytes([1]) + struct.pack(">I", len(b)) + b)
req(1, "host.hello", {"client": "py", "version": "0", "interactive": True, "token": sys.argv[2]})
req(2, "env.set", {"key": "RW_PY", "value": "1"})
req(3, "host.window_status", None)
got, buf = {}, b""
while len(got) < 3:
    buf += s.recv(65536)
    while len(buf) >= 5 and len(buf) >= 5 + struct.unpack(">I", buf[1:5])[0]:
        n = struct.unpack(">I", buf[1:5])[0]
        m = json.loads(buf[5:5 + n]); buf = buf[5 + n:]
        if m.get("t") == "res":
            got[m["id"]] = m
env = "ok" if "ok" in got[2] else got[2]["err"]["code"]
st = got[3]["ok"]
print(env, st.get("refusal"), "may_restart" if st.get("may_restart") else "no_restart")
"#;

fn helper_args() -> [&'static str; 4] {
    ["helper_window_client", "--exact", "--ignored", "--nocapture"]
}

fn wait_for(path: &Path) -> String {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Ok(s) = std::fs::read_to_string(path)
            && !s.is_empty()
        {
            return s;
        }
        assert!(Instant::now() < deadline, "the helper did not answer: {}", path.display());
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The token alone is not enough: the connecting process must be this Runwick executable and
/// must not run inside a managed session's terminal.
#[test]
fn the_token_works_only_for_this_binary_outside_managed_runs() {
    use runwick_core::api::projects::{OpenFolder, ProjectsOpen};
    use runwick_core::api::sessions::{Attach, SessionsAttach};
    use std::process::Command;
    let (dir, home, socket) = start_host();
    let token = std::fs::read_to_string(home.join("gui.token")).unwrap().trim().to_string();
    let exe = std::env::current_exe().unwrap();
    let run_with = |program: &Path, out: &Path, extra: &[(&str, &str)]| {
        let status = Command::new(program)
            .args(helper_args())
            .envs(extra.iter().copied())
            .env("RW_HELPER_SOCKET", &socket)
            .env("RW_HELPER_TOKEN", &token)
            .env("RW_HELPER_OUT", out)
            .stdout(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success());
        wait_for(out)
    };
    let run = |program: &Path, out: &Path| run_with(program, out, &[]);

    // (d) This executable, started directly (as the app starts itself): a window.
    assert_eq!(run(&exe, &dir.join("direct.out")), "ok");

    // This executable started with verification hooks (scripted input, a prepared UI state):
    // not a window on a host without the test flag.
    for hook in [("RUNWICK_VERIFY_INPUT", "key:enter"), ("RUNWICK_UI_STATE", "{}"), ("RUNWICK_SNAPSHOT", "/x.png")] {
        let out = dir.join(format!("hook-{}.out", hook.0));
        assert_eq!(run_with(&exe, &out, &[hook]), codes::DENIED, "{hook:?}");
    }

    // The same program at another path (the app moved, or copied): still a window.
    let moved = dir.join("moved-runwick");
    std::fs::copy(&exe, &moved).unwrap();
    assert_eq!(run(&moved, &dir.join("moved.out")), "ok");

    // (a) Another program holding the same token: not a window, told why, and — like any
    // program of this user outside a run, which could end the host anyway — offered the restart.
    let script = dir.join("other_program.py");
    std::fs::write(&script, OTHER_PROGRAM).unwrap();
    let out = Command::new("python3").arg("-I").arg(&script).arg(&socket).arg(&token).output().expect("python3 runs");
    let answer = String::from_utf8_lossy(&out.stdout).trim().to_string();
    assert_eq!(answer, "denied different_binary may_restart", "{}", String::from_utf8_lossy(&out.stderr));

    // (b) This executable started from inside a managed terminal (as an agent could): refused.
    let mut window = Client::connect(&socket);
    window.hello(true, Some(token.clone()));
    let folder = dir.join("work");
    std::fs::create_dir_all(&folder).unwrap();
    let ws =
        window.call::<ProjectsOpen>(OpenFolder { path: folder.to_string_lossy().into_owned() }).unwrap().workspace.id;
    let mut p = start_params();
    p.workspace_id = ws;
    let session = window.call::<SessionsStart>(p).unwrap().session.id;
    window.call::<SessionsAttach>(Attach { id: session, cols: 120, rows: 30, interactive: true }).unwrap();
    let out = dir.join("inside.out");
    let line = format!(
        "RW_HELPER_SOCKET='{}' RW_HELPER_TOKEN='{token}' RW_HELPER_OUT='{}' '{}' {} >/dev/null 2>&1\r",
        socket.display(),
        out.display(),
        exe.display(),
        helper_args().join(" ")
    );
    window.type_into(session, &line);
    assert_eq!(wait_for(&out), codes::DENIED);
}
