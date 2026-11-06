//! Files API end to end: an in-process host on a temporary `RUNWICK_HOME`, a workspace on a
//! temporary folder, and typed requests over the socket protocol.

use runwick_core::api::files::*;
use runwick_core::api::host::{Hello, HostHello};
use runwick_core::api::projects::{OpenFolder, ProjectsOpen};
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
}

fn start_host() -> (tempfile::TempDir, Client) {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("files_wire");
    std::fs::create_dir_all(&root).unwrap();
    let dir = tempfile::Builder::new().prefix("rw").tempdir_in(&root).unwrap();
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let socket = dir.path().join("h.sock");
    unsafe {
        std::env::set_var("RUNWICK_HOME", &home);
        std::env::set_var("RUNWICK_SOCKET", &socket);
    }
    let (h, s) = (home.clone(), socket.clone());
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        rt.block_on(runwick_host::serve_at(&h, &s)).unwrap();
    });
    let deadline = Instant::now() + Duration::from_secs(15);
    let stream = loop {
        if let Ok(s) = UnixStream::connect(&socket) {
            break s;
        }
        assert!(Instant::now() < deadline, "host did not start");
        std::thread::sleep(Duration::from_millis(20));
    };
    let mut c = Client { s: stream, dec: Decoder::new(), next: 0 };
    c.call::<HostHello>(Hello {
        client: "files test".into(),
        version: "0".into(),
        // A window of this computer: the host's per-launch window token.
        interactive: true,
        token: std::fs::read_to_string(runwick_core::paths::gui_token_path()).ok().map(|t| t.trim().to_string()),
    })
    .unwrap();
    (dir, c)
}

fn write(root: &Path, rel: &str, bytes: &[u8]) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, bytes).unwrap();
}

#[test]
fn files_api_over_the_wire() {
    let (_home, mut c) = start_host();
    let proj = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(proj.path()).unwrap();
    write(&root, "docs/auth-notes.md", "# 로그인 세션 메모\n".as_bytes());
    write(&root, "src/main.rs", b"fn main() {}\n");
    write(&root, "logo.png", &[0x89, b'P', b'N', b'G', 0, 1, 2, 3]);
    let ws = c.call::<ProjectsOpen>(OpenFolder { path: root.to_string_lossy().into() }).unwrap().workspace.id;

    let top = c.call::<FilesList>(ListDir { workspace_id: ws, dir: String::new(), show_hidden: false }).unwrap();
    let names: Vec<&str> = top.entries.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, ["docs", "src", "logo.png"]);

    let doc =
        c.call::<FilesRead>(ReadFile { workspace_id: ws, path: "docs/auth-notes.md".into(), max_bytes: None }).unwrap();
    assert!(matches!(doc.body, FileBody::Text { ref text, .. } if text.starts_with("# 로그인")));
    let base = doc.hash.clone();
    let png = c.call::<FilesRead>(ReadFile { workspace_id: ws, path: "logo.png".into(), max_bytes: None }).unwrap();
    assert_eq!(png.body, FileBody::Binary);

    // A CLI changes the file while the GUI has unsaved edits: the save is refused.
    write(&root, "docs/auth-notes.md", "# 로그인 세션 메모\n- 테스트 통과\n".as_bytes());
    let out = c
        .call::<FilesWrite>(WriteFile {
            workspace_id: ws,
            path: "docs/auth-notes.md".into(),
            text: "# 로그인 세션 메모\n- 내 편집\n".into(),
            base_hash: base,
        })
        .unwrap();
    let WriteOutcome::Conflict { disk_hash, disk_text, .. } = out else { panic!("expected a conflict") };
    assert_eq!(disk_text.as_deref(), Some("# 로그인 세션 메모\n- 테스트 통과\n"));
    assert_eq!(
        std::fs::read_to_string(root.join("docs/auth-notes.md")).unwrap(),
        "# 로그인 세션 메모\n- 테스트 통과\n"
    );
    // Saving over the version now seen works and is reported by hash.
    let merged = "# 로그인 세션 메모\n- 테스트 통과\n- 내 편집\n";
    let out = c
        .call::<FilesWrite>(WriteFile {
            workspace_id: ws,
            path: "docs/auth-notes.md".into(),
            text: merged.into(),
            base_hash: disk_hash,
        })
        .unwrap();
    let WriteOutcome::Saved { hash, .. } = out else { panic!("expected saved") };
    let st = c
        .call::<FilesStat>(StatFiles { workspace_id: ws, paths: vec!["docs/auth-notes.md".into(), "nope.md".into()] })
        .unwrap();
    assert_eq!(st[0].hash.as_deref(), Some(hash.as_str()));
    assert!(!st[1].exists);

    let created = c
        .call::<FilesCreate>(CreateFile {
            workspace_id: ws,
            rel_dir: "docs".into(),
            name: "회의 메모".into(),
            kind: NewDocKind::Md,
        })
        .unwrap();
    assert_eq!(created.path, "docs/회의 메모.md");
    let found = c.call::<FilesFind>(FindFiles { workspace_id: ws, query: "ㅎㅇ".into(), limit: 5 }).unwrap();
    assert_eq!(found.hits[0].path, "docs/회의 메모.md");

    // Confinement holds over the wire too.
    let e =
        c.call::<FilesRead>(ReadFile { workspace_id: ws, path: "../outside.txt".into(), max_bytes: None }).unwrap_err();
    assert_eq!(e.code, codes::DENIED);
    let e = c
        .call::<FilesWrite>(WriteFile { workspace_id: ws, path: "/tmp/x".into(), text: "x".into(), base_hash: None })
        .unwrap_err();
    assert_eq!(e.code, codes::DENIED);
    let e = c
        .call::<FilesList>(ListDir { workspace_id: uuid::Uuid::new_v4(), dir: String::new(), show_hidden: false })
        .unwrap_err();
    assert_eq!(e.code, codes::NOT_FOUND);
}
