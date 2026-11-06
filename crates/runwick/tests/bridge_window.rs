//! A `runwick host --stdio` bridge is a window only inside a real SSH login. Here the real host
//! and the real bridge run from this build; the bridge has `SSH_CONNECTION` set (so it adds the
//! device's window token to the hello it relays) but no `sshd` above it, as when a program in a
//! terminal starts one: the host must refuse it.

use runwick_core::api::env::{EnvSet, SetVar};
use runwick_core::api::host::{Hello, HostHello};
use runwick_core::rpc::{Message, Method, ResBody, RpcError, codes};
use runwick_core::wire::{Decoder, Frame};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const EXE: &str = env!("CARGO_BIN_EXE_runwick");

struct Host {
    child: Child,
    dir: PathBuf,
}

impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A real host on its own short folder (the socket stays there, never in ~/.runwick), with
/// `extra` environment variables.
fn start_host(name: &str, extra: &[(&str, &str)]) -> (Host, PathBuf) {
    let dir = PathBuf::from(format!("/tmp/rwb-{}-{name}", std::process::id()));
    let home = dir.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let child = Command::new(EXE)
        .arg("host")
        .envs(extra.iter().copied())
        .env("RUNWICK_HOME", &home)
        .env("RUNWICK_SOCKET", dir.join("h.sock"))
        .env("SHELL", "/bin/sh")
        .env("HISTFILE", "/dev/null")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let socket = dir.join("h.sock");
    let deadline = Instant::now() + Duration::from_secs(20);
    while UnixStream::connect(&socket).is_err() || !home.join("gui.token").is_file() {
        assert!(Instant::now() < deadline, "host did not start");
        std::thread::sleep(Duration::from_millis(50));
    }
    (Host { child, dir }, home)
}

/// Requests through a bridge process; returns each response by id.
fn through_bridge(
    home: &PathBuf,
    ssh: bool,
    requests: Vec<(u64, &str, serde_json::Value)>,
) -> Vec<Result<(), RpcError>> {
    let mut cmd = Command::new(EXE);
    let socket = home.parent().unwrap().join("h.sock");
    cmd.args(["host", "--stdio"]).env("RUNWICK_HOME", home).env("RUNWICK_SOCKET", socket);
    if ssh {
        cmd.env("SSH_CONNECTION", "192.0.2.1 50000 192.0.2.2 22");
    } else {
        cmd.env_remove("SSH_CONNECTION");
    }
    let mut bridge = cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
    let mut stdin = bridge.stdin.take().unwrap();
    for (id, method, params) in &requests {
        let f = Frame::Message(Message::Req { id: *id, method: (*method).into(), params: params.clone() });
        stdin.write_all(&f.encode().unwrap()).unwrap();
        stdin.flush().unwrap();
        // One at a time, so the hello is settled before the next request is read.
        std::thread::sleep(Duration::from_millis(200));
    }
    let mut stdout = bridge.stdout.take().unwrap();
    let mut dec = Decoder::new();
    let mut got: Vec<Option<Result<(), RpcError>>> = vec![None; requests.len()];
    let mut buf = [0u8; 65536];
    while got.iter().any(Option::is_none) {
        let n = stdout.read(&mut buf).unwrap();
        assert!(n > 0, "bridge closed");
        dec.push(&buf[..n]);
        while let Some(f) = dec.next_frame().unwrap() {
            if let Frame::Message(Message::Res { id, body }) = f
                && let Some(i) = requests.iter().position(|r| r.0 == id)
            {
                got[i] = Some(match body {
                    ResBody::Ok(_) => Ok(()),
                    ResBody::Err(e) => Err(e),
                });
            }
        }
    }
    drop(stdin);
    let _ = bridge.kill();
    let _ = bridge.wait();
    got.into_iter().map(Option::unwrap).collect()
}

fn hello() -> serde_json::Value {
    let h = Hello { client: "remote window".into(), version: "0".into(), interactive: true, token: None };
    serde_json::to_value(h).unwrap()
}

fn set_var() -> serde_json::Value {
    serde_json::to_value(SetVar { key: "RW_BRIDGE".into(), value: Some("1".into()), ..Default::default() }).unwrap()
}

#[test]
fn a_bridge_outside_an_ssh_login_is_not_a_window() {
    let (_host, home) = start_host("bridge", &[]);
    for ssh in [true, false] {
        let r = through_bridge(&home, ssh, vec![(1, HostHello::NAME, hello()), (2, EnvSet::NAME, set_var())]);
        assert!(r[0].is_ok(), "the hello itself answers");
        let e = r[1].clone().unwrap_err();
        assert_eq!(e.code, codes::DENIED, "SSH_CONNECTION={ssh}: {e:?}");
    }
}

/// This test program (not the Runwick executable) asks to be a window with the token directly.
fn scripted_client(home: &std::path::Path) -> Result<(), RpcError> {
    let socket = home.parent().unwrap().join("h.sock");
    let token = std::fs::read_to_string(home.join("gui.token")).unwrap().trim().to_string();
    let mut s = UnixStream::connect(socket).unwrap();
    let hello = Hello { client: "script".into(), version: "0".into(), interactive: true, token: Some(token) };
    let reqs = [(1u64, HostHello::NAME, serde_json::to_value(hello).unwrap()), (2, EnvSet::NAME, set_var())];
    for (id, method, params) in reqs {
        let f = Frame::Message(Message::Req { id, method: method.into(), params });
        s.write_all(&f.encode().unwrap()).unwrap();
    }
    let mut dec = Decoder::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = s.read(&mut buf).unwrap();
        assert!(n > 0, "host closed");
        dec.push(&buf[..n]);
        while let Some(f) = dec.next_frame().unwrap() {
            if let Frame::Message(Message::Res { id: 2, body }) = f {
                return match body {
                    ResBody::Ok(_) => Ok(()),
                    ResBody::Err(e) => Err(e),
                };
            }
        }
    }
}

/// Not a test of its own: started by the test below as a separate client process with
/// verification hooks in its environment (`RW_HELPER_HOME` set).
#[test]
#[ignore]
fn helper_scripted_client() {
    let Ok(home) = std::env::var("RW_HELPER_HOME") else {
        return;
    };
    let outcome = match scripted_client(std::path::Path::new(&home)) {
        Ok(()) => "ok".to_string(),
        Err(e) => e.code,
    };
    std::fs::write(std::path::Path::new(&home).join("helper.out"), outcome).unwrap();
}

#[test]
fn scripted_clients_with_the_token_are_windows_only_on_a_verification_host() {
    let (_plain, home) = start_host("plain", &[]);
    assert_eq!(scripted_client(&home).unwrap_err().code, codes::DENIED);
    let (_verify, home) = start_host("verify", &[("RUNWICK_TEST_WINDOW_CLIENTS", "1")]);
    scripted_client(&home).unwrap();
    // On a verification host a client started with verification hooks is a window too.
    let status = Command::new(std::env::current_exe().unwrap())
        .args(["helper_scripted_client", "--exact", "--ignored", "--nocapture"])
        .env("RW_HELPER_HOME", &home)
        .env("RUNWICK_UI_STATE", "{}")
        .env("RUNWICK_VERIFY_INPUT", "key:enter")
        .stdout(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success());
    assert_eq!(std::fs::read_to_string(home.join("helper.out")).unwrap(), "ok");
}

/// One request after a window hello with the token, from this test program (another build than
/// the host's runwick), returning the raw result.
fn as_other_build(home: &std::path::Path, method: &str, token: bool) -> Result<serde_json::Value, RpcError> {
    let socket = home.parent().unwrap().join("h.sock");
    let token = token.then(|| std::fs::read_to_string(home.join("gui.token")).unwrap().trim().to_string());
    let mut s = UnixStream::connect(socket).unwrap();
    let hello = Hello { client: "other build".into(), version: "0".into(), interactive: true, token };
    let reqs = [(1u64, HostHello::NAME, serde_json::to_value(hello).unwrap()), (2, method, serde_json::Value::Null)];
    for (id, method, params) in reqs {
        let f = Frame::Message(Message::Req { id, method: method.into(), params });
        s.write_all(&f.encode().unwrap()).unwrap();
    }
    let mut dec = Decoder::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = s.read(&mut buf).unwrap();
        assert!(n > 0, "host closed");
        dec.push(&buf[..n]);
        while let Some(f) = dec.next_frame().unwrap() {
            if let Frame::Message(Message::Res { id: 2, body }) = f {
                return match body {
                    ResBody::Ok(v) => Ok(v),
                    ResBody::Err(e) => Err(e),
                };
            }
        }
    }
}

#[test]
fn a_window_of_another_build_is_told_why_and_may_restart_the_host() {
    use runwick_core::api::host::{HostRestartForUpdate, HostWindowStatus, WindowRefusal, WindowStatus};
    let (mut host, home) = start_host("update", &[]);
    let status: WindowStatus =
        serde_json::from_value(as_other_build(&home, HostWindowStatus::NAME, true).unwrap()).unwrap();
    assert_eq!(
        (status.window, status.refusal, status.may_restart),
        (false, Some(WindowRefusal::DifferentBinary), true)
    );
    assert!(status.host_exe.is_some_and(|p| p.ends_with("runwick")));
    // Without the token (an agent or another data folder) the restart is refused.
    assert_eq!(as_other_build(&home, HostRestartForUpdate::NAME, false).unwrap_err().code, codes::DENIED);
    assert!(host.child.try_wait().unwrap().is_none(), "still running");
    // The other build's window asks for it: the host stops its runs and exits.
    as_other_build(&home, HostRestartForUpdate::NAME, true).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while host.child.try_wait().unwrap().is_none() {
        assert!(Instant::now() < deadline, "the host did not exit");
        std::thread::sleep(Duration::from_millis(50));
    }
}
