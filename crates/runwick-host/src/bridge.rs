//! `runwick host --stdio`: relay frames between stdin/stdout and the local host socket.
//! A GUI on another computer runs this over SSH to reach this computer's host.
//!
//! That GUI never sends its own computer's window token here, so inside an SSH login
//! (`SSH_CONNECTION`) the bridge adds this device's token to the GUI's `host.hello`. The bridge
//! is not what decides: the host makes the connection a window only when this bridge process
//! has an `sshd` ancestor and is outside every managed run (`gui_token::window_claim`), so a
//! bridge started by a program in a Runwick terminal, or detached from one, gets nothing.

use runwick_core::wire::Decoder;
use runwick_core::{Frame, Message};
use serde_json::Value;
use std::io::{Read, Write};
use std::path::Path;

pub fn run_stdio(exe: &Path) -> anyhow::Result<()> {
    let sock = runwick_core::launch::connect_or_spawn(exe)?;
    let mut to_host = sock.try_clone()?;
    let mut from_host = sock;

    let upstream = std::thread::spawn(move || {
        let mut stdin = std::io::stdin().lock();
        let mut buf = vec![0u8; 64 * 1024];
        let mut dec = Decoder::new();
        'relay: loop {
            match stdin.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    dec.push(&buf[..n]);
                    loop {
                        let frame = match dec.next_frame() {
                            Ok(Some(f)) => f,
                            Ok(None) => break,
                            Err(_) => break 'relay,
                        };
                        let token = || {
                            std::env::var_os("SSH_CONNECTION")?;
                            std::fs::read_to_string(runwick_core::paths::gui_token_path()).ok()
                        };
                        let Ok(bytes) = with_device_token(frame, token).encode() else {
                            break 'relay;
                        };
                        if to_host.write_all(&bytes).is_err() {
                            break 'relay;
                        }
                    }
                }
            }
        }
        let _ = to_host.shutdown(std::net::Shutdown::Write);
    });

    let mut stdout = std::io::stdout().lock();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        match from_host.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if stdout.write_all(&buf[..n]).is_err() || stdout.flush().is_err() {
                    break;
                }
            }
        }
    }
    drop(upstream);
    Ok(())
}

/// A window's `host.hello` gets this device's window token (`token` reads it when needed).
fn with_device_token(frame: Frame, token: impl FnOnce() -> Option<String>) -> Frame {
    match frame {
        Frame::Message(Message::Req { id, method, mut params }) if method == "host.hello" => {
            let window = params.get("interactive").and_then(Value::as_bool) == Some(true);
            if window
                && let Some(t) = token()
                && let Some(obj) = params.as_object_mut()
            {
                obj.insert("token".into(), Value::String(t.trim().to_string()));
            }
            Frame::Message(Message::Req { id, method, params })
        }
        other => other,
    }
}

/// Detach from the launching terminal so closing it does not stop runs.
pub fn daemonize() {
    unsafe {
        libc::setsid();
        libc::signal(libc::SIGHUP, libc::SIG_IGN);
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn req(method: &str, params: Value) -> Frame {
        Frame::Message(Message::Req { id: 1, method: method.into(), params })
    }

    #[test]
    fn only_a_window_hello_gets_the_device_token() {
        let t = || Some("tok\n".to_string());
        let hello = with_device_token(req("host.hello", json!({"client": "gui", "interactive": true})), t);
        let Frame::Message(Message::Req { params, .. }) = hello else { panic!() };
        assert_eq!(params["token"], "tok");
        // An agent's hello and every other request pass through unchanged.
        let agent = req("host.hello", json!({"client": "agent", "interactive": false}));
        assert_eq!(with_device_token(agent.clone(), t), agent);
        let other = req("env.set", json!({"key": "A", "value": "1"}));
        assert_eq!(with_device_token(other.clone(), t), other);
        let input = Frame::Input { session: uuid::Uuid::nil(), data: b"ls\r".to_vec() };
        assert_eq!(with_device_token(input.clone(), t), input);
        // No token file on this device: the hello stays as it was.
        let gui = req("host.hello", json!({"interactive": true}));
        assert_eq!(with_device_token(gui.clone(), || None), gui);
    }
}
