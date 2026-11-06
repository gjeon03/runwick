//! `runwick delegate|workers|wait|result|send|report|status`: the delegation commands an agent
//! CLI (or a person) uses inside a Runwick terminal. They talk to this computer's host over its
//! socket (`RUNWICK_SOCKET`) as the session in `RUNWICK_SESSION_ID`, and never start a host.
//!
//! Output for programs goes to stdout (ids, answers, `--json`); explanations go to stderr.

use clap::{Args, Subcommand, ValueEnum};
use runwick_core::api::collab::*;
use runwick_core::api::host::{Hello, HostHello};
use runwick_core::api::projects::ById;
use runwick_core::api::sessions::{SessionId, SessionsGet, Tool};
use runwick_core::paths;
use runwick_core::rpc::{Message, Method, ResBody, RpcError};
use runwick_core::wire::{Decoder, Frame};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;
use uuid::Uuid;

/// Exit codes (also listed in `runwick help <command>`).
pub mod exit {
    pub const OK: i32 = 0;
    /// The host refused or failed the request (message on stderr).
    pub const ERROR: i32 = 1;
    /// Not inside a Runwick terminal (no `RUNWICK_SESSION_ID`).
    pub const NO_SESSION: i32 = 3;
    /// Cannot reach the Runwick host.
    pub const NO_HOST: i32 = 4;
    /// `wait` ran out of time; the work is still open (not a failure).
    pub const TIMEOUT: i32 = 5;
    /// The work ended without an answer: failed, cancelled, or delivery uncertain.
    pub const NOT_ANSWERED: i32 = 6;
}

const CODES: &str = "종료 코드 / exit codes: 0 성공 ok · 1 요청 실패 error · 3 Runwick 터미널 밖 not in a Runwick session · 4 호스트 연결 실패 host unreachable · 5 대기 시간 초과 wait timed out · 6 답 없이 끝남 ended without an answer";

#[derive(Clone, Copy, ValueEnum)]
pub enum ToolArg {
    Claude,
    Codex,
}

impl From<ToolArg> for Tool {
    fn from(t: ToolArg) -> Tool {
        match t {
            ToolArg::Claude => Tool::Claude,
            ToolArg::Codex => Tool::Codex,
        }
    }
}

#[derive(Subcommand)]
pub enum AgentCommand {
    /// 다른 CLI에 일을 맡기고 작업자 ID를 출력해요 / Hand work to another CLI and print the worker id
    #[command(after_help = CODES)]
    Delegate(DelegateArgs),
    /// 이 세션이 맡긴 작업자 목록 / List the workers this session delegated to
    #[command(after_help = CODES)]
    Workers {
        /// 끝난 일도 보여요 / Include finished work
        #[arg(long)]
        all: bool,
        #[arg(long)]
        json: bool,
    },
    /// 작업자가 이번 차례를 마칠 때까지 기다리고 답을 출력해요 / Wait until the worker finished its turn and print its answer
    #[command(after_help = CODES)]
    Wait {
        /// 작업자 ID 또는 접수 ID (앞부분만 써도 돼요) / Worker or request id (a prefix is enough)
        id: String,
        /// 최대 대기 초 (기본 600, 최대 3600) / Seconds to wait (default 600, at most 3600)
        #[arg(long, default_value_t = 600)]
        timeout: u64,
        #[arg(long)]
        json: bool,
    },
    /// 맡긴 일의 결과: 요약, 바뀐 파일, 검증, 남은 일 / Result: summary, changed files, checks, not done
    #[command(after_help = CODES)]
    Result {
        id: String,
        #[arg(long)]
        json: bool,
    },
    /// 작업자에게 후속 지시를 보내요 (응답 중이면 대기열) / Send a follow-up (queued while the worker is busy)
    #[command(after_help = CODES)]
    Send {
        id: String,
        /// 보낼 내용 (`-`이면 표준 입력) / Message (`-` reads stdin)
        message: String,
        /// 허용될 때 현재 응답을 멈추고 보내요 / Interrupt the current answer first, when allowed
        #[arg(long)]
        now: bool,
    },
    /// 맡은 일의 결과를 남겨요 (작업자가 사용) / Report your result for the work you were given
    #[command(after_help = CODES)]
    Report(ReportArgs),
    /// 이 세션의 협업 상태와 자동 배정 한도 / This session's collaboration state and limits
    #[command(after_help = CODES)]
    Status {
        #[arg(long)]
        json: bool,
    },
}

#[derive(Args)]
pub struct DelegateArgs {
    /// 맡길 CLI / CLI to start
    #[arg(long, value_enum)]
    tool: ToolArg,
    /// 역할 표시명, 예: 검토 / Role shown to people, e.g. "review"
    #[arg(long)]
    role: Option<String>,
    /// 짧은 제목 (없으면 내용의 첫 줄) / Short title (default: first line of the task)
    #[arg(long)]
    title: Option<String>,
    /// 새 branch의 별도 worktree에서 일해요 / Work in a separate git worktree on this new branch
    #[arg(long, value_name = "BRANCH")]
    split: Option<String>,
    /// 같은 키로 다시 부르면 새로 만들지 않고 같은 일을 돌려줘요 / Idempotency key
    #[arg(long)]
    key: Option<String>,
    /// 맡길 내용 (`-`이면 표준 입력) / The task (`-` reads stdin)
    task: String,
}

#[derive(Args)]
pub struct ReportArgs {
    /// 한 일의 요약 / What you did
    #[arg(long)]
    summary: String,
    /// 바꾼 파일 (여러 번) / A file you changed (repeatable)
    #[arg(long = "file", value_name = "PATH")]
    files: Vec<String>,
    /// 실행한 검증: "명령=pass|fail[,log=경로]" (여러 번) / A check you ran (repeatable)
    #[arg(long = "check", value_name = "CMD=pass|fail")]
    checks: Vec<String>,
    /// 하지 못한 일 (여러 번) / Something not done (repeatable)
    #[arg(long = "not-done", value_name = "TEXT")]
    not_done: Vec<String>,
}

struct Fail(i32, String);

impl From<RpcError> for Fail {
    fn from(e: RpcError) -> Self {
        Fail(exit::ERROR, e.message)
    }
}

/// Run an agent command; returns the process exit code.
pub fn run(cmd: AgentCommand) -> i32 {
    match dispatch(cmd) {
        Ok(code) => code,
        Err(Fail(code, msg)) => {
            eprintln!("runwick: {msg}");
            code
        }
    }
}

fn dispatch(cmd: AgentCommand) -> Result<i32, Fail> {
    let me = session_id();
    match cmd {
        AgentCommand::Delegate(a) => delegate(me?, a),
        AgentCommand::Workers { all, json } => workers(me?, all, json),
        AgentCommand::Wait { id, timeout, json } => {
            let mut c = Client::connect(Some(Duration::from_secs(timeout.min(3600) + 60)))?;
            let id = c.resolve(me.ok(), &id)?;
            let w = c.call::<CollabWait>(WaitFor { id, timeout_secs: timeout })?;
            print_wait(&w, json);
            Ok(match (w.timed_out, w.request.status) {
                (true, _) => exit::TIMEOUT,
                (false, ReqStatus::Done) => exit::OK,
                _ => exit::NOT_ANSWERED,
            })
        }
        AgentCommand::Result { id, json } => {
            let mut c = Client::connect(None)?;
            let id = c.resolve(me.ok(), &id)?;
            let v = c.call::<CollabGet>(ById { id })?;
            print_result(&v, json);
            Ok(exit::OK)
        }
        AgentCommand::Send { id, message, now } => {
            let mut c = Client::connect(None)?;
            let me = me.ok();
            let worker = c.resolve_worker(me, &id)?;
            let prompt = read_arg(message)?;
            let r = c.call::<CollabSend>(SendTo {
                worker_id: worker,
                parent_id: me,
                prompt,
                now,
                key: None,
                source: Source::Agent,
            })?;
            println!("{}", r.id);
            if r.worker_id != worker {
                // Written to the session that gave this work: kept as a notice for the person.
                eprintln!(
                    "맡긴 세션에는 입력하지 않았어요. 사람이 볼 수 있게 {}에 메시지로 남겼어요 / not typed into the parent; left as a notice for the person",
                    r.receipt()
                );
                return Ok(exit::OK);
            }
            eprintln!(
                "{} 접수했어요 · {}",
                r.receipt(),
                r.reason.as_deref().unwrap_or("작업자가 쉬고 있으면 곧 전달해요")
            );
            Ok(exit::OK)
        }
        AgentCommand::Report(a) => report(me?, a),
        AgentCommand::Status { json } => status(me?, json),
    }
}

fn session_id() -> Result<SessionId, Fail> {
    std::env::var(paths::ENV_SESSION_ID).ok().and_then(|v| v.parse().ok()).ok_or_else(|| {
        Fail(
            exit::NO_SESSION,
            "Runwick 터미널 안에서만 쓸 수 있어요 (RUNWICK_SESSION_ID 없음) / run this inside a Runwick terminal"
                .into(),
        )
    })
}

fn read_arg(v: String) -> Result<String, Fail> {
    if v != "-" {
        return Ok(v);
    }
    let mut s = String::new();
    std::io::stdin()
        .read_to_string(&mut s)
        .map_err(|e| Fail(exit::ERROR, format!("표준 입력을 읽지 못했어요: {e}")))?;
    Ok(s)
}

fn delegate(me: SessionId, a: DelegateArgs) -> Result<i32, Fail> {
    let mut c = Client::connect(None)?;
    let prompt = read_arg(a.task)?;
    let d = c.call::<CollabDelegate>(Delegate {
        parent_id: Some(me),
        workspace_id: None,
        tool: a.tool.into(),
        role: a.role,
        title: a.title,
        prompt,
        split: a.split,
        key: a.key,
        source: Source::Agent,
        cols: 120,
        rows: 32,
    })?;
    println!("{}", d.worker.session.id);
    let r = &d.request;
    let what = if d.reused { "같은 키로 맡긴 일이 이미 있어요" } else { "맡겼어요" };
    eprintln!(
        "{} 작업자에게 {what} ({} · {}){}",
        r.tool.label(),
        r.receipt(),
        r.status.label(),
        r.reason.as_deref().map(|x| format!(" · {x}")).unwrap_or_default()
    );
    eprintln!("기다리기 / wait: runwick wait {}", d.worker.session.id);
    Ok(if r.status == ReqStatus::Failed { exit::NOT_ANSWERED } else { exit::OK })
}

fn workers(me: SessionId, all: bool, json: bool) -> Result<i32, Fail> {
    let mut c = Client::connect(None)?;
    let list = c.call::<CollabList>(RequestQuery { parent_id: Some(me), open_only: !all, ..Default::default() })?;
    if json {
        println!("{}", serde_json::to_string_pretty(&list).unwrap_or_default());
        return Ok(exit::OK);
    }
    if list.is_empty() {
        eprintln!(
            "{}",
            if all {
                "맡긴 일이 없어요"
            } else {
                "진행 중인 맡긴 일이 없어요 (--all로 끝난 일도 보기)"
            }
        );
        return Ok(exit::OK);
    }
    for r in &list {
        println!(
            "{}\t{}\t{}\t{}\t{}{}",
            r.worker_id,
            r.receipt(),
            r.tool.label(),
            r.status.label(),
            r.role.as_deref().map(|x| format!("{x} · ")).unwrap_or_default(),
            r.title
        );
    }
    Ok(exit::OK)
}

fn print_wait(w: &Waited, json: bool) {
    if json {
        println!("{}", serde_json::to_string_pretty(w).unwrap_or_default());
        return;
    }
    let r = &w.request;
    if w.timed_out {
        eprintln!("아직 끝나지 않았어요 ({}) · 다시 기다리거나 runwick result {}로 보세요", r.status.label(), r.id);
        return;
    }
    let from = match w.answer_from {
        Some(SummaryFrom::Report) => "작업자 보고",
        Some(SummaryFrom::Transcript) => "대화 기록의 마지막 답",
        Some(SummaryFrom::Screen) => "화면의 마지막 부분 (대화 기록 없음)",
        None => "답 없음",
    };
    eprintln!(
        "{} · {} · {from}{}",
        r.receipt(),
        r.status.label(),
        r.reason.as_deref().map(|x| format!(" · {x}")).unwrap_or_default()
    );
    if let Some(a) = &w.answer {
        println!("{a}");
    }
    if r.status == ReqStatus::Done {
        eprintln!("응답 완료는 검증 통과가 아니에요. 바뀐 파일과 검증은 runwick result {}", r.id);
    }
}

fn print_result(v: &RequestView, json: bool) {
    if json {
        println!("{}", serde_json::to_string_pretty(v).unwrap_or_default());
        return;
    }
    let r = &v.request;
    println!("{} · {} · {} · 시도 {}", r.title, r.receipt(), r.status.label(), r.attempt);
    if let Some(reason) = &r.reason {
        println!("  {reason}");
    }
    if let Some(a) = &v.answer {
        println!("\n요약 / summary:\n{a}");
    }
    let Some(res) = &r.result else {
        println!("\n아직 결과가 없어요");
        return;
    };
    if let (Some(b), Some(h)) = (&res.base_rev, &res.head_rev) {
        println!("\n기준 {} → 현재 {}", short(b), short(h));
    }
    if !res.files.is_empty() {
        println!("\n바뀐 파일 / changed files ({}):", res.files.len());
        for f in &res.files {
            let counts = match (f.added, f.removed) {
                (Some(a), Some(d)) => format!(" +{a} -{d}"),
                (Some(a), None) => format!(" +{a}"),
                _ => String::new(),
            };
            println!("  {} {}{counts}", f.status, f.path);
        }
    }
    if res.checks.is_empty() {
        println!("\n검증 / checks: 보고된 검증 없음");
    } else {
        println!("\n검증 / checks{}:", if v.checks_stale { " (이후 파일이 바뀌어 이전 버전 기준)" } else { "" });
        for c in &res.checks {
            let at = c.revision.as_deref().map(short).unwrap_or_else(|| "기준 없음".into());
            println!("  {} {} · {at} 기준", if c.passed { "통과" } else { "실패" }, c.command);
        }
    }
    if !res.not_done.is_empty() {
        println!("\n하지 못한 일 / not done:");
        for n in &res.not_done {
            println!("  - {n}");
        }
    }
}

fn short(rev: &str) -> String {
    rev.chars().take(7).collect()
}

/// `"cmd=pass"`, `"cmd=fail,log=path"`. The command may itself contain `=`.
fn parse_check(spec: &str) -> Result<CheckReport, Fail> {
    let (rest, log_path) = match spec.rsplit_once(",log=") {
        Some((r, l)) if !l.trim().is_empty() => (r, Some(l.trim().to_string())),
        _ => (spec, None),
    };
    let bad = || Fail(exit::ERROR, format!("--check는 \"명령=pass\" 또는 \"명령=fail\" 형식이에요: {spec}"));
    let (cmd, verdict) = rest.rsplit_once('=').ok_or_else(bad)?;
    let passed = match verdict.trim() {
        "pass" | "passed" | "ok" => true,
        "fail" | "failed" => false,
        _ => return Err(bad()),
    };
    if cmd.trim().is_empty() {
        return Err(bad());
    }
    Ok(CheckReport { command: cmd.trim().to_string(), passed, log_path, log: None })
}

/// Most of a check's log sent along (its end).
const MAX_LOG: u64 = 64 * 1024;

/// The end of a check's log. Only a regular file inside the worker's folder is read (no links
/// out of it, no devices or pipes), and only its last 64 KB; the host itself never opens paths.
/// A relative path is taken from the worker's folder.
fn read_log(path: &str, folder: &Path) -> Result<String, Fail> {
    let bad = |why: &str| Fail(exit::ERROR, format!("--check의 log를 읽지 않았어요 ({why}): {path}"));
    let given = Path::new(path);
    // Relative paths are read from the worker's folder, like the commands it ran there.
    let full = if given.is_absolute() { given.to_path_buf() } else { folder.join(given) };
    let regular = |p: &Path| std::fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_file());
    if !regular(&full) {
        return Err(bad("작업 폴더 안의 일반 파일이 아니에요"));
    }
    let real = full.canonicalize().map_err(|_| bad("파일을 찾을 수 없어요"))?;
    let root = folder.canonicalize().map_err(|_| bad("작업 폴더를 찾을 수 없어요"))?;
    if !real.starts_with(&root) || !regular(&real) {
        return Err(bad("작업 폴더 안의 일반 파일이 아니에요"));
    }
    let mut f = std::fs::File::open(&real).map_err(|_| bad("열 수 없어요"))?;
    let len = f.metadata().map_err(|_| bad("열 수 없어요"))?.len();
    f.seek(SeekFrom::Start(len.saturating_sub(MAX_LOG))).map_err(|_| bad("읽을 수 없어요"))?;
    let mut buf = Vec::new();
    f.take(MAX_LOG).read_to_end(&mut buf).map_err(|_| bad("읽을 수 없어요"))?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

fn report(me: SessionId, a: ReportArgs) -> Result<i32, Fail> {
    let mut checks = a.checks.iter().map(|s| parse_check(s)).collect::<Result<Vec<_>, _>>()?;
    let mut c = Client::connect(None)?;
    if checks.iter().any(|ch| ch.log_path.is_some()) {
        let folder = PathBuf::from(c.call::<SessionsGet>(ById { id: me })?.session.cwd);
        for ch in &mut checks {
            if let Some(path) = ch.log_path.take() {
                ch.log = Some(read_log(&path, &folder)?);
            }
        }
    }
    let r = c.call::<CollabReport>(Report {
        worker_id: me,
        summary: a.summary,
        files: a.files,
        checks,
        not_done: a.not_done,
    })?;
    eprintln!("{} 결과를 남겼어요 · 맡긴 세션이 runwick result로 볼 수 있어요", r.receipt());
    Ok(exit::OK)
}

fn status(me: SessionId, json: bool) -> Result<i32, Fail> {
    let mut c = Client::connect(None)?;
    let ov = c.call::<CollabOverview>(())?;
    let mine = c.call::<CollabList>(RequestQuery { parent_id: Some(me), open_only: true, ..Default::default() })?;
    let given = c.call::<CollabList>(RequestQuery { worker_id: Some(me), limit: 1, ..Default::default() })?;
    let session = c.call::<SessionsGet>(ById { id: me })?;
    if json {
        let v = serde_json::json!({
            "session": session.session,
            "open_workers": mine,
            "given_to_me": given.first(),
            "overview": ov,
        });
        println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
        return Ok(exit::OK);
    }
    println!("세션 / session: {} ({})", session.session.title, me);
    if ov.held_parents.contains(&me) {
        println!("새 배정 보류 중 / new assignments held: 대기 중인 일은 사람이 재개할 때까지 전달되지 않아요");
    }
    println!("진행 중인 맡긴 일 / open delegated work: {}", mine.len());
    for r in &mine {
        println!("  {} {} · {} · {}", r.worker_id, r.receipt(), r.status.label(), r.title);
    }
    if let Some(g) = given.first() {
        println!(
            "이 세션이 맡은 일 / work given to this session: {} · {} · {}",
            g.receipt(),
            g.status.label(),
            g.title
        );
    }
    println!("{} (관측할 수 없는 CLI 내장 작업은 제외)", runwick_core::api::activity::slots_line(&ov));
    Ok(exit::OK)
}

/// A blocking protocol client for one command.
struct Client {
    s: UnixStream,
    dec: Decoder,
    next: u64,
}

impl Client {
    fn connect(read_timeout: Option<Duration>) -> Result<Client, Fail> {
        let sock = paths::socket_path();
        let s = UnixStream::connect(&sock).map_err(|e| {
            Fail(
                exit::NO_HOST,
                format!("Runwick 호스트에 연결하지 못했어요 / cannot reach the Runwick host ({}): {e}", sock.display()),
            )
        })?;
        s.set_read_timeout(Some(read_timeout.unwrap_or(Duration::from_secs(120)))).ok();
        let mut c = Client { s, dec: Decoder::new(), next: 1 };
        let label =
            std::env::var(paths::ENV_SESSION_ID).map(|id| format!("runwick agent · {}", &id[..id.len().min(8)]));
        c.call::<HostHello>(Hello {
            client: label.unwrap_or_else(|_| "runwick agent".into()),
            version: runwick_core::VERSION.into(),
            interactive: false,
            token: None,
        })?;
        // Prove which session this terminal belongs to (its run's secret, never the window's).
        if let Some(token) = std::env::var(paths::ENV_SESSION_TOKEN).ok().filter(|t| !t.is_empty()) {
            c.call::<CollabIdentify>(Identify { token })?;
        }
        Ok(c)
    }

    fn call<M: Method>(&mut self, params: M::Params) -> Result<M::Output, Fail> {
        let id = self.next;
        self.next += 1;
        let req = Message::Req { id, method: M::NAME.into(), params: serde_json::to_value(params).unwrap_or_default() };
        let bytes = Frame::Message(req).encode().map_err(|e| Fail(exit::ERROR, e.to_string()))?;
        self.s.write_all(&bytes).map_err(|e| Fail(exit::NO_HOST, format!("호스트에 보내지 못했어요: {e}")))?;
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            while let Some(f) = self.dec.next_frame().map_err(|e| Fail(exit::ERROR, e.to_string()))? {
                if let Frame::Message(Message::Res { id: rid, body }) = f
                    && rid == id
                {
                    return match body {
                        ResBody::Ok(v) => serde_json::from_value(v)
                            .map_err(|e| Fail(exit::ERROR, format!("{}: 응답 형식 오류 ({e})", M::NAME))),
                        ResBody::Err(e) => Err(e.into()),
                    };
                }
            }
            let n =
                self.s.read(&mut buf).map_err(|e| Fail(exit::NO_HOST, format!("호스트 응답을 받지 못했어요: {e}")))?;
            if n == 0 {
                return Err(Fail(exit::NO_HOST, "호스트 연결이 끊겼어요".into()));
            }
            self.dec.push(&buf[..n]);
        }
    }

    /// A full id, or a prefix of a worker or request id among this session's work.
    fn resolve(&mut self, me: Option<SessionId>, text: &str) -> Result<Uuid, Fail> {
        if let Ok(id) = text.parse::<Uuid>() {
            return Ok(id);
        }
        let needle = text.trim().trim_start_matches("r-").to_ascii_lowercase();
        if needle.len() < 4 {
            return Err(Fail(exit::ERROR, "ID는 네 글자 이상 써 주세요".into()));
        }
        let list = self.call::<CollabList>(RequestQuery { parent_id: me, limit: 500, ..Default::default() })?;
        let mut hits: Vec<Uuid> = Vec::new();
        // UUIDv7 ids of one moment share their start, so the end (as in receipts) is accepted too.
        let needle = needle.replace('-', "");
        let matches = |id: &Uuid| {
            let s = id.simple().to_string();
            s.starts_with(&needle) || s.ends_with(&needle)
        };
        for r in &list {
            if matches(&r.worker_id) && !hits.contains(&r.worker_id) {
                hits.push(r.worker_id);
            } else if matches(&r.id) && !hits.contains(&r.id) {
                hits.push(r.id);
            }
        }
        match hits.as_slice() {
            [one] => Ok(*one),
            [] => Err(Fail(exit::ERROR, format!("그 ID로 맡긴 일을 찾을 수 없어요: {text}"))),
            _ => Err(Fail(exit::ERROR, format!("여러 개와 맞아요. 더 길게 써 주세요: {text}"))),
        }
    }

    /// The worker behind an id (a request id resolves to its worker).
    fn resolve_worker(&mut self, me: Option<SessionId>, text: &str) -> Result<SessionId, Fail> {
        let id = self.resolve(me, text)?;
        match self.call::<CollabGet>(ById { id }) {
            Ok(v) => Ok(v.request.worker_id),
            Err(_) => Ok(id),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_specs() {
        let c = parse_check("pnpm test auth=pass").ok().unwrap();
        assert_eq!((c.command.as_str(), c.passed, c.log_path), ("pnpm test auth", true, None));
        let c = parse_check("FOO=1 make test=fail,log=out/test.log").ok().unwrap();
        assert_eq!((c.command.as_str(), c.passed), ("FOO=1 make test", false));
        assert_eq!(c.log_path.as_deref(), Some("out/test.log"));
        assert!(parse_check("make test").is_err());
        assert!(parse_check("make=maybe").is_err());
        assert!(parse_check("=pass").is_err());
    }

    #[test]
    fn logs_are_regular_files_inside_the_folder_and_only_their_end() {
        let base = std::env::temp_dir().join(format!("rw-log-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let folder = base.join("ws");
        std::fs::create_dir_all(&folder).unwrap();
        let log = folder.join("test.log");
        let mut text = "x".repeat(70 * 1024);
        text.push_str("\nlast line\n");
        std::fs::write(&log, &text).unwrap();
        let got = read_log(&log.to_string_lossy(), &folder).ok().unwrap();
        assert!(got.len() as u64 <= MAX_LOG && got.ends_with("last line\n"));
        let relative = read_log("test.log", &folder).ok().unwrap();
        assert_eq!(relative, got, "relative to the worker's folder");

        let outside = base.join("secret");
        std::fs::write(&outside, "TOPSECRET").unwrap();
        assert!(read_log(&outside.to_string_lossy(), &folder).is_err(), "outside the folder");
        let link = folder.join("link.log");
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        assert!(read_log(&link.to_string_lossy(), &folder).is_err(), "a link out of the folder");
        assert!(read_log("/dev/zero", &folder).is_err(), "a device");
        let fifo = folder.join("pipe.log");
        assert!(std::process::Command::new("mkfifo").arg(&fifo).status().unwrap().success());
        assert!(read_log(&fifo.to_string_lossy(), &folder).is_err(), "a pipe is never opened");
        assert!(read_log(&folder.to_string_lossy(), &folder).is_err(), "a folder");
        let _ = std::fs::remove_dir_all(&base);
    }
}
