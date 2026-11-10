//! One look at this computer's host: a short connection with pipelined requests (the
//! `menubar.enabled` preference, then the sessions, delegated requests and active automatic jobs
//! the window's 전체 활동 is built from, and the project and workspace names), then closed. A connection kept open would queue every
//! event the host broadcasts; a fresh one every few seconds costs nothing.
//! Never starts a host: when nothing answers, the host is gone.

use super::model::{self, Poll};
use runwick_app::presence::{MENUBAR_ENABLED_KEY, menubar_enabled};
use runwick_core::api::collab::{CollabList, RequestQuery};
use runwick_core::api::host::{Hello, HostHello, KvGet, KvKey};
use runwick_core::api::jobs::{JobQuery, JobsList};
use runwick_core::api::projects::{ProjectsList, WorkspaceQuery, WorkspacesList};
use runwick_core::api::sessions::{SessionQuery, SessionsList};
use runwick_core::rpc::ResBody;
use runwick_core::wire::Decoder;
use runwick_core::{Frame, Message, Method, paths};
use serde_json::Value;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(5);
/// Request id of the handshake (the looks use 0..6).
const HELLO_ID: u64 = u64::MAX;

pub fn look() -> Poll {
    let Ok(stream) = UnixStream::connect(paths::socket_path()) else {
        return Poll::Gone;
    };
    match exchange(stream) {
        Ok(Some(snapshot)) => Poll::Ok(snapshot),
        Ok(None) => Poll::Disabled,
        Err(e) => Poll::Failed(e),
    }
}

/// `None` when the menu bar icon is turned off.
fn exchange(mut s: UnixStream) -> Result<Option<model::Snapshot>, String> {
    s.set_read_timeout(Some(TIMEOUT)).map_err(|e| e.to_string())?;
    s.set_write_timeout(Some(TIMEOUT)).map_err(|e| e.to_string())?;
    // The same queries as the window (shell::data::all_sessions, views::activity).
    let requests = [
        request::<KvGet>(&KvKey { key: MENUBAR_ENABLED_KEY.into() }),
        request::<SessionsList>(&SessionQuery { include_hidden: true, ..Default::default() }),
        request::<CollabList>(&RequestQuery { activity: true, limit: 100, ..Default::default() }),
        request::<JobsList>(&JobQuery { active_only: true, ..Default::default() }),
        request::<ProjectsList>(&()),
        request::<WorkspacesList>(&WorkspaceQuery { include_archived: true, ..Default::default() }),
    ];
    // The menu bar is started by the window and speaks for it: the window token first (read on
    // every look, so a restarted host's new token is used). Its answer is not needed.
    let token = std::fs::read_to_string(paths::gui_token_path()).ok().map(|t| t.trim().to_string());
    let hello =
        Hello {
            client: "Runwick 메뉴 막대".into(), version: runwick_core::VERSION.into(), interactive: true, token
        };
    let mut out = Frame::Message(Message::Req {
        id: HELLO_ID,
        method: HostHello::NAME.into(),
        params: serde_json::to_value(hello).map_err(|e| e.to_string())?,
    })
    .encode()
    .map_err(|e| e.to_string())?;
    for (id, (method, params)) in (0u64..).zip(requests) {
        let frame = Frame::Message(Message::Req { id, method: method.into(), params: params? });
        out.extend(frame.encode().map_err(|e| e.to_string())?);
    }
    s.write_all(&out).map_err(|e| format!("보내기 실패: {e}"))?;

    let mut answers: [Option<Value>; 6] = Default::default();
    let mut dec = Decoder::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        while let Some(frame) = dec.next_frame().map_err(|e| e.to_string())? {
            let Frame::Message(Message::Res { id, body }) = frame else {
                continue;
            };
            if id == HELLO_ID {
                continue;
            }
            let Some(slot) = usize::try_from(id).ok().and_then(|i| answers.get_mut(i)) else {
                continue;
            };
            *slot = Some(match body {
                ResBody::Ok(v) => v,
                ResBody::Err(e) => return Err(e.message),
            });
        }
        if answers.iter().all(Option::is_some) {
            break;
        }
        let n = s.read(&mut buf).map_err(|e| format!("응답 없음: {e}"))?;
        if n == 0 {
            return Err("호스트가 연결을 닫았어요".into());
        }
        dec.push(&buf[..n]);
    }
    let [enabled, sessions, reqs, jobs, projects, workspaces] = answers.map(Option::unwrap_or_default);
    if !menubar_enabled(decode::<KvGet>(enabled)?.as_ref()) {
        return Ok(None);
    }
    let sessions = decode::<SessionsList>(sessions)?;
    let reqs = decode::<CollabList>(reqs)?;
    let jobs = decode::<JobsList>(jobs)?;
    let names = model::Names::new(&decode::<ProjectsList>(projects)?, &decode::<WorkspacesList>(workspaces)?);
    Ok(Some(model::snapshot(&sessions, &reqs, &jobs, &names)))
}

fn request<M: Method>(params: &M::Params) -> (&'static str, Result<Value, String>) {
    (M::NAME, serde_json::to_value(params).map_err(|e| e.to_string()))
}

fn decode<M: Method>(v: Value) -> Result<M::Output, String> {
    serde_json::from_value(v).map_err(|e| format!("{}: 응답 형식 오류 ({e})", M::NAME))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    /// The look works against a host that only answers windows: the menu bar presents the
    /// window token it finds in the data folder.
    #[test]
    fn a_look_presents_the_window_token_and_reads_the_state() {
        let dir = std::env::temp_dir().join(format!("rw-menubar-{}", std::process::id()));
        let home = dir.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let socket = dir.join("h.sock");
        unsafe {
            std::env::set_var(paths::ENV_HOME, &home);
            std::env::set_var(paths::ENV_SOCKET, &socket);
            std::env::set_var("SHELL", "/bin/sh");
        }
        std::thread::spawn(runwick_host::run);
        let deadline = Instant::now() + Duration::from_secs(15);
        while UnixStream::connect(&socket).is_err() || !paths::gui_token_path().is_file() {
            assert!(Instant::now() < deadline, "host did not start");
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(matches!(look(), Poll::Ok(_)), "the menu bar could not read the host");
        // Without the token the same look is refused (shown as unknown, never as zero).
        std::fs::remove_file(paths::gui_token_path()).unwrap();
        assert!(matches!(look(), Poll::Failed(_)));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
