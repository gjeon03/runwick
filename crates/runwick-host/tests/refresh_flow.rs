//! Global environment + session refresh end to end against an in-process host: a real shell run
//! in a PTY, a variable saved after it started, the session listed as waiting, a refresh that
//! restarts only that run, and the new value visible inside the new shell.

use runwick_core::api::env::*;
use runwick_core::api::host::HostInfo;
use runwick_core::api::projects::ById;
use runwick_core::api::refresh::*;
use runwick_core::api::sessions::*;
use runwick_core::rpc::{Method, RpcError, codes};
use runwick_host::host::{Client, Ctx, Host};
use runwick_host::store::Store;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

/// A Claude Code stand-in: `mcp list` answers like the real CLI (linear needs authentication
/// until an `authed` file exists in the folder); a run records its arguments and stays alive.
const FAKE_CLAUDE: &str = r#"#!/bin/sh
if [ "$1" = mcp ] && [ "$2" = list ]; then
  printf 'Checking MCP server health\n\n'
  printf 'github: https://gh.example/mcp (HTTP) - \342\234\224 Connected\n'
  if [ -f authed ]; then
    printf 'linear: https://mcp.linear.app/mcp (HTTP) - \342\234\224 Connected\n'
  else
    printf 'linear: https://mcp.linear.app/mcp (HTTP) - ! Needs authentication\n'
  fi
  exit 0
fi
printf '%s\n' "$*" >> launches.txt
printf 'fake claude ready\r\n'
exec sleep 600
"#;

fn home() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap())
}

fn setup_env() {
    static DONE: OnceLock<()> = OnceLock::new();
    DONE.get_or_init(|| {
        let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("refresh_flow");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let dir = tempfile::Builder::new().prefix("rw").tempdir_in(&root).unwrap().keep();
        let home = dir.join("home");
        std::fs::create_dir_all(&home).unwrap();
        // A fake `claude` first on the login shell's PATH (sh -l reads ~/.profile).
        let bin = dir.join("fakebin");
        std::fs::create_dir_all(&bin).unwrap();
        let claude = bin.join("claude");
        std::fs::write(&claude, FAKE_CLAUDE).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(dir.join(".profile"), format!("PATH=\"{}:$PATH\"\nexport PATH\n", bin.display())).unwrap();
        // Set before any host thread starts. HOME points at an empty folder so the login shell and
        // provider configuration are this test's own.
        unsafe {
            std::env::set_var("RUNWICK_HOME", &home);
            std::env::set_var("RUNWICK_SOCKET", dir.join("h.sock"));
            std::env::set_var("SHELL", "/bin/sh");
            std::env::set_var("HOME", &dir);
        }
    });
}

struct T {
    host: Arc<Host>,
    router: runwick_host::router::Router,
    ctx: Ctx,
    _frames: tokio::sync::mpsc::UnboundedReceiver<runwick_core::wire::Frame>,
}

impl T {
    fn new() -> T {
        setup_env();
        let info = HostInfo {
            host_id: uuid::Uuid::now_v7(),
            device_name: "test".into(),
            version: "test".into(),
            os: "test".into(),
            arch: "test".into(),
            home: String::new(),
            started_at: runwick_core::now(),
            pid: std::process::id(),
            client_id: 0,
        };
        let host = Host::new(info, Store::open_in_memory().unwrap());
        runwick_host::env::boot(&host);
        runwick_host::refresh::boot(&host);
        runwick_host::sessions::boot(&host);
        let (tx, frames) = tokio::sync::mpsc::unbounded_channel();
        let client = Arc::new(Client::new(host.next_client_id(), tx));
        client.interactive.store(true, std::sync::atomic::Ordering::Relaxed);
        T { ctx: Ctx { host: host.clone(), client }, host, router: runwick_host::router::build(), _frames: frames }
    }

    async fn call<M: Method>(&self, p: M::Params) -> Result<M::Output, RpcError> {
        let v = self.router.dispatch(self.ctx.clone(), M::NAME, serde_json::to_value(p).unwrap()).await?;
        Ok(serde_json::from_value(v).unwrap())
    }

    async fn shell(&self) -> SessionId {
        let d = tempfile::tempdir().unwrap().keep();
        self.start(Tool::Shell, &d).await
    }

    async fn start(&self, tool: Tool, folder: &std::path::Path) -> SessionId {
        let ws = runwick_host::projects::open_folder(&self.host.store, &folder.to_string_lossy()).unwrap().workspace.id;
        let p = StartSession {
            workspace_id: ws,
            tool,
            title: Some("셸".into()),
            initial_prompt: None,
            parent_id: None,
            role: None,
            cols: 100,
            rows: 30,
        };
        let id = self.call::<SessionsStart>(p).await.unwrap().session.id;
        self.call::<SessionsAttach>(Attach { id, cols: 100, rows: 30, interactive: true }).await.unwrap();
        id
    }

    async fn screen_until(&self, id: SessionId, needle: &str) -> String {
        for _ in 0..100 {
            let text = self.call::<SessionsScreenText>(ScreenQuery { id, scrollback: 50 }).await.unwrap();
            if text.contains(needle) {
                return text;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("screen never showed {needle:?}");
    }

    fn type_line(&self, id: SessionId, line: &str) {
        runwick_host::sessions::on_input(&self.host, &self.ctx.client, id, format!("{line}\r").into_bytes());
    }

    async fn job_until(&self, id: SessionId, done: impl Fn(&Job) -> bool) -> Job {
        for _ in 0..150 {
            if let Some(j) = self.call::<RefreshStatus>(Target { session: id }).await.unwrap()
                && done(&j)
            {
                return j;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!(
            "refresh did not reach the expected state: {:?}",
            self.call::<RefreshStatus>(Target { session: id }).await
        );
    }
}

fn set(key: &str, value: &str) -> SetVar {
    SetVar { key: key.into(), value: Some(value.into()), description: String::new(), secret: false }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn saved_variables_reach_a_running_shell_only_after_its_refresh() {
    let t = T::new();
    let a = t.shell().await;
    let other = t.shell().await;
    t.type_line(a, "echo before=[$RW_FLOW_VAR]");
    t.screen_until(a, "before=[]").await;

    // Saved, not applied: both running shells wait for a refresh; values never appear in lists.
    t.call::<EnvSet>(set("RW_FLOW_VAR", "hello world")).await.unwrap();
    let pending = t.call::<RefreshPending>(()).await.unwrap();
    assert_eq!(pending.len(), 2, "{pending:?}");
    let p = pending.iter().find(|p| p.session == a).unwrap();
    assert_eq!(p.env_changes, vec![EnvChange { key: "RW_FLOW_VAR".into(), kind: EnvChangeKind::Added }]);
    assert!(!serde_json::to_string(&pending).unwrap().contains("hello world"));

    let plan = t.call::<RefreshPlan>(Target { session: a }).await.unwrap();
    assert_eq!(plan.method, Some(runwick_core::api::refresh::Method::RestartShell));
    assert_eq!((plan.env_from, plan.env_to), (0, 1));
    assert!(plan.input_recent, "we just typed into it");

    let job = t
        .call::<RefreshStart>(StartParams { session: a, when: When::Now, verify_only: false, cols: 100, rows: 30 })
        .await
        .unwrap();
    assert_eq!(job.steps.iter().map(|s| s.kind).collect::<Vec<_>>(), [StepKind::Restart, StepKind::Verify]);
    // A second request for the same session is refused while the first runs.
    let again = t
        .call::<RefreshStart>(StartParams { session: a, when: When::Now, verify_only: false, cols: 100, rows: 30 })
        .await;
    assert_eq!(again.unwrap_err().code, codes::CONFLICT);
    let done = t.job_until(a, |j| !j.phase.is_active()).await;
    assert_eq!(done.phase, JobPhase::Done, "{done:?}");
    assert!(done.steps.iter().all(|s| s.state == StepState::Done), "{done:?}");
    assert_eq!(done.checks[0].label, "공통 환경변수 v1 적용");

    // The same session, a new shell with the value; the other session was not touched.
    let s = t.call::<SessionsGet>(ById { id: a }).await.unwrap().session;
    assert_eq!(s.env_version, 1);
    t.type_line(a, "echo after=[$RW_FLOW_VAR]");
    t.screen_until(a, "after=[hello world]").await;
    let pending = t.call::<RefreshPending>(()).await.unwrap();
    assert_eq!(pending.iter().map(|p| p.session).collect::<Vec<_>>(), vec![other]);
    let o = t.call::<SessionsGet>(ById { id: other }).await.unwrap().session;
    assert_eq!(o.env_version, 0);

    // "After the response": a busy shell keeps the reservation until it is cancelled.
    t.type_line(other, "while :; do echo tick; sleep 0.2; done");
    t.screen_until(other, "tick").await;
    let job = t
        .call::<RefreshStart>(StartParams {
            session: other,
            when: When::AfterResponse,
            verify_only: false,
            cols: 100,
            rows: 30,
        })
        .await
        .unwrap();
    assert_eq!(job.phase, JobPhase::Scheduled);
    tokio::time::sleep(Duration::from_millis(1600)).await;
    let still = t.call::<RefreshStatus>(Target { session: other }).await.unwrap().unwrap();
    assert_eq!(still.phase, JobPhase::Scheduled, "must not restart while output continues");
    assert_eq!(t.call::<RefreshPending>(()).await.unwrap()[0].job, Some(JobPhase::Scheduled));
    t.call::<RefreshCancel>(Target { session: other }).await.unwrap();
    let cancelled = t.call::<RefreshStatus>(Target { session: other }).await.unwrap().unwrap();
    assert_eq!(cancelled.phase, JobPhase::Cancelled);
    let o = t.call::<SessionsGet>(ById { id: other }).await.unwrap().session;
    assert_eq!(o.env_version, 0, "a cancelled reservation does not restart");

    // A stopped run has nothing to refresh.
    t.call::<SessionsStop>(Stop { id: other, force: true }).await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    let plan = t.call::<RefreshPlan>(Target { session: other }).await.unwrap();
    assert_eq!(plan.blockers.iter().map(|b| b.kind).collect::<Vec<_>>(), vec![BlockerKind::NotRunning]);
    for id in [a, other] {
        t.call::<SessionsStop>(Stop { id, force: true }).await.unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn only_runwick_windows_change_or_reveal_variables() {
    let t = T::new();
    t.ctx.client.interactive.store(false, std::sync::atomic::Ordering::Relaxed);
    let denied = t.call::<EnvSet>(set("RW_AGENT_SET", "x")).await.unwrap_err();
    assert_eq!(denied.code, codes::DENIED);
    assert_eq!(t.call::<EnvReveal>(VarKey { key: "RW_FLOW_VAR".into() }).await.unwrap_err().code, codes::DENIED);
    // (In-process dispatch is not gated by method; the socket's gate is in window_token.rs.)

    // Claiming to be a window is not enough: only this launch's window token counts.
    use runwick_core::api::host::{Hello, HostHello};
    use runwick_core::api::prefs::{PrefsSetEditor, SetEditor};
    let hello =
        |token: Option<String>| Hello { client: "raw client".into(), version: "0".into(), interactive: true, token };
    // The test itself is the connecting process: this binary, outside every managed run.
    t.ctx.client.set_peer_pid(std::process::id());
    let token_file = runwick_core::paths::gui_token_path();
    runwick_host::gui_token::issue(&token_file).unwrap();
    let token = std::fs::read_to_string(&token_file).unwrap();
    for claim in [None, Some("0".repeat(64)), Some(format!("{token}x"))] {
        t.call::<HostHello>(hello(claim)).await.unwrap();
        assert!(!t.ctx.client.is_interactive());
        let e = t.call::<EnvSet>(set("RW_AGENT_SET", "x")).await.unwrap_err();
        assert_eq!(e.code, codes::DENIED);
        let script = SetEditor { command: "/bin/sh -c id".into() };
        assert_eq!(t.call::<PrefsSetEditor>(script).await.unwrap_err().code, codes::DENIED);
    }
    t.call::<HostHello>(hello(Some(token.trim().to_string()))).await.unwrap();
    assert!(t.ctx.client.is_interactive());
    t.call::<EnvSet>(set("RW_WINDOW_SET", "x")).await.unwrap();
    t.call::<EnvRemove>(VarKey { key: "RW_WINDOW_SET".into() }).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn claude_refresh_resumes_the_same_conversation_and_reports_mcp_auth() {
    let t = T::new();
    let claude_json = home().join(".claude.json");
    std::fs::write(&claude_json, r#"{"mcpServers":{"github":{"type":"http","url":"https://gh.example/mcp"}}}"#)
        .unwrap();
    let folder = tempfile::tempdir().unwrap().keep();
    let id = t.start(Tool::Claude, &folder).await;
    t.screen_until(id, "fake claude ready").await;
    let conv = t.call::<SessionsGet>(ById { id }).await.unwrap().session.provider_conv_id.unwrap();

    // Nothing said yet (no transcript): a refresh could only start a new conversation.
    let plan = t.call::<RefreshPlan>(Target { session: id }).await.unwrap();
    assert_eq!(plan.method, Some(runwick_core::api::refresh::Method::StartFresh));
    // Claude saved the conversation; a server is added and a variable changes.
    let slug: String =
        folder.to_string_lossy().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    let projects = home().join(".claude").join("projects").join(slug);
    std::fs::create_dir_all(&projects).unwrap();
    std::fs::write(projects.join(format!("{conv}.jsonl")), "{}\n").unwrap();
    std::fs::write(
        &claude_json,
        r#"{"mcpServers":{"github":{"type":"http","url":"https://gh.example/mcp"},"linear":{"type":"http","url":"https://mcp.linear.app/mcp"}}}"#,
    )
    .unwrap();
    t.call::<EnvSet>(set("RW_CLAUDE_FLOW", "1")).await.unwrap();

    let plan = t.call::<RefreshPlan>(Target { session: id }).await.unwrap();
    assert_eq!(plan.method, Some(runwick_core::api::refresh::Method::Resume));
    let mcp = plan.mcp_changes.clone().unwrap();
    assert_eq!(mcp.len(), 1, "{mcp:?}");
    assert_eq!((mcp[0].name.as_str(), mcp[0].kind), ("linear", runwick_core::api::mcp::McpChangeKind::Added));
    assert!(plan.env_changes.iter().any(|c| c.key == "RW_CLAUDE_FLOW"));

    t.call::<RefreshStart>(StartParams { session: id, when: When::Now, verify_only: false, cols: 100, rows: 30 })
        .await
        .unwrap();
    let job = t.job_until(id, |j| !j.phase.is_active()).await;
    assert_eq!(job.phase, JobPhase::Partial, "{job:?}");
    assert!(job.steps.iter().all(|s| s.state == StepState::Done), "{job:?}");
    let reconnect = job.steps.iter().find(|s| s.kind == StepKind::Reconnect).unwrap();
    assert_eq!(reconnect.message.as_deref(), Some("대화 ID가 이전과 같음을 확인했어요"));
    let linear = job.checks.iter().find(|c| c.label == "MCP linear 인증 필요").expect("linear check");
    assert_eq!(linear.state, CheckState::NeedsAuth);
    assert!(job.message.as_deref().unwrap().starts_with("같은 대화로 다시 연결했어요"), "{job:?}");

    // The same Runwick session resumed the same provider conversation with the new settings.
    let s = t.call::<SessionsGet>(ById { id }).await.unwrap().session;
    assert_eq!(s.provider_conv_id.as_deref(), Some(conv.as_str()));
    assert_eq!(s.env_version, plan.env_to);
    let launches = std::fs::read_to_string(folder.join("launches.txt")).unwrap();
    let lines: Vec<&str> = launches.lines().collect();
    assert_eq!(lines.len(), 2, "{launches}");
    assert!(lines[0].starts_with(&format!("--session-id {conv}")), "{launches}");
    assert!(lines[1].starts_with(&format!("--resume {conv}")), "{launches}");
    assert!(t.call::<RefreshPending>(()).await.unwrap().iter().all(|p| p.session != id));

    // After signing in, "이 세션에서 다시 시도" only checks again; the run is not restarted.
    std::fs::write(folder.join("authed"), "").unwrap();
    t.call::<RefreshStart>(StartParams { session: id, when: When::Now, verify_only: true, cols: 100, rows: 30 })
        .await
        .unwrap();
    let job = t.job_until(id, |j| !j.phase.is_active()).await;
    assert_eq!(job.phase, JobPhase::Done, "{job:?}");
    assert_eq!(job.steps.iter().map(|s| s.kind).collect::<Vec<_>>(), [StepKind::Verify]);
    assert!(job.checks.iter().any(|c| c.label == "MCP linear 연결" && c.state == CheckState::Ok), "{job:?}");
    assert_eq!(std::fs::read_to_string(folder.join("launches.txt")).unwrap().lines().count(), 2);
    t.call::<SessionsStop>(Stop { id, force: true }).await.unwrap();
}
