//! Connections to hosts (this computer and registered devices) and a small query cache for the
//! immediate-mode UI.
//!
//! - `hub.query::<M>(host, &params)` returns cached data and fetches in the background when
//!   missing or invalidated by a `changed` event (stale-while-revalidate).
//! - `hub.call::<M>(host, params, |app, result| ...)` runs a request and hands the typed result
//!   to a closure that runs on the UI thread with `&mut App`.
//! - Terminal output for a session goes straight to a registered sink on the reader thread.

use crate::App;
use runwick_core::api::host::{Hello, HostHello, HostInfo};
use runwick_core::rpc::{EVENT_CHANGED, Message, Method, ResBody, RpcError, codes};
use runwick_core::wire::{Decoder, Frame};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::any::Any;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use uuid::Uuid;

/// Which host a request goes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
pub enum HostKey {
    Local,
    /// A registered device (id from the device list).
    Device(Uuid),
}

pub type UiTask = Box<dyn FnOnce(&mut App) + Send>;
pub type OutputSink = Box<dyn FnMut(&[u8]) + Send>;
type AnyArc = Arc<dyn Any + Send + Sync>;
type Pending = Box<dyn FnOnce(Result<Value, RpcError>) + Send>;

#[derive(Debug, Clone)]
pub enum ConnState {
    Connecting,
    Connected(HostInfo),
    Disconnected { since: Instant, reason: String },
}

impl ConnState {
    pub fn info(&self) -> Option<&HostInfo> {
        match self {
            ConnState::Connected(i) => Some(i),
            _ => None,
        }
    }
    pub fn is_connected(&self) -> bool {
        matches!(self, ConnState::Connected(_))
    }
}

/// How to reach a host. Returns a reader and a writer for the framed protocol.
pub trait Transport: Send + Sync + 'static {
    fn open(&self) -> std::io::Result<(Box<dyn Read + Send>, Box<dyn Write + Send>)>;
    /// Delay before reconnecting after a failure.
    fn retry_delay(&self, attempt: u32) -> Duration {
        Duration::from_millis((300 * 2u64.saturating_pow(attempt.min(4))).min(5000))
    }
    /// Short description for status lines.
    fn describe(&self) -> String;
}

/// This computer's host over its Unix socket; started on demand.
pub struct LocalTransport {
    pub exe: PathBuf,
}

impl Transport for LocalTransport {
    fn open(&self) -> std::io::Result<(Box<dyn Read + Send>, Box<dyn Write + Send>)> {
        let s = runwick_core::launch::connect_or_spawn(&self.exe)?;
        let w = s.try_clone()?;
        Ok((Box::new(s), Box::new(w)))
    }
    fn describe(&self) -> String {
        "이 컴퓨터".to_string()
    }
}

struct Inbox {
    results: Vec<(QKey, Result<AnyArc, RpcError>)>,
    events: Vec<(HostKey, String, Value)>,
    tasks: Vec<UiTask>,
}

/// One host connection with its own reader thread and reconnect loop.
pub struct HostConn {
    pub key: HostKey,
    writer: Arc<Mutex<Option<Box<dyn Write + Send>>>>,
    pending: Arc<Mutex<HashMap<u64, Pending>>>,
    sinks: Arc<Mutex<HashMap<Uuid, OutputSink>>>,
    state: Arc<Mutex<ConnState>>,
    next_id: Arc<AtomicU64>,
    /// Increments on every successful (re)connect so views can re-attach.
    generation: Arc<AtomicU64>,
    stop: Arc<std::sync::atomic::AtomicBool>,
}

impl HostConn {
    fn start(key: HostKey, transport: Arc<dyn Transport>, inbox: Arc<Mutex<Inbox>>, ctx: egui::Context) -> Self {
        let conn = HostConn {
            key,
            writer: Arc::new(Mutex::new(None)),
            pending: Arc::new(Mutex::new(HashMap::new())),
            sinks: Arc::new(Mutex::new(HashMap::new())),
            state: Arc::new(Mutex::new(ConnState::Connecting)),
            next_id: Arc::new(AtomicU64::new(1)),
            generation: Arc::new(AtomicU64::new(0)),
            stop: Arc::new(Default::default()),
        };
        let c = conn.clone_handles();
        std::thread::Builder::new()
            .name(format!("runwick-conn-{key:?}"))
            .spawn(move || c.run(transport, inbox, ctx))
            .expect("spawn connection thread");
        conn
    }

    fn clone_handles(&self) -> HostConn {
        HostConn {
            key: self.key,
            writer: self.writer.clone(),
            pending: self.pending.clone(),
            sinks: self.sinks.clone(),
            state: self.state.clone(),
            next_id: self.next_id.clone(),
            generation: self.generation.clone(),
            stop: self.stop.clone(),
        }
    }

    fn run(self, transport: Arc<dyn Transport>, inbox: Arc<Mutex<Inbox>>, ctx: egui::Context) {
        let mut attempt = 0u32;
        while !self.stop.load(Ordering::Relaxed) {
            *self.state.lock().unwrap() = ConnState::Connecting;
            ctx.request_repaint();
            match transport.open() {
                Ok((reader, writer)) => {
                    *self.writer.lock().unwrap() = Some(writer);
                    self.hello(&inbox, &ctx);
                    let reason = self.read_loop(reader, &inbox, &ctx);
                    *self.writer.lock().unwrap() = None;
                    self.fail_pending();
                    let was_connected = self.state.lock().unwrap().is_connected();
                    *self.state.lock().unwrap() = ConnState::Disconnected { since: Instant::now(), reason };
                    if was_connected {
                        attempt = 0;
                    }
                    inbox.lock().unwrap().events.push((self.key, EVENT_CONN_LOST.into(), Value::Null));
                }
                Err(e) => {
                    *self.state.lock().unwrap() =
                        ConnState::Disconnected { since: Instant::now(), reason: e.to_string() };
                }
            }
            ctx.request_repaint();
            if self.stop.load(Ordering::Relaxed) {
                break;
            }
            std::thread::sleep(transport.retry_delay(attempt));
            attempt = attempt.saturating_add(1);
        }
    }

    fn hello(&self, inbox: &Arc<Mutex<Inbox>>, ctx: &egui::Context) {
        let state = self.state.clone();
        let generation = self.generation.clone();
        let inbox2 = inbox.clone();
        let key = self.key;
        let ctx2 = ctx.clone();
        let hello = Hello {
            client: format!("{} · Runwick {}", local_device_label(), runwick_core::VERSION),
            version: runwick_core::VERSION.to_string(),
            interactive: true,
            // Read on every (re)connect: a restarted host writes a new one. Only this computer's
            // host gets it; another device's settings are changed from that device's window.
            token: (key == HostKey::Local)
                .then(|| std::fs::read_to_string(runwick_core::paths::gui_token_path()).ok())
                .flatten()
                .map(|t| t.trim().to_string()),
        };
        self.request_raw(
            HostHello::NAME,
            serde_json::to_value(hello).unwrap_or(Value::Null),
            Box::new(move |res| {
                match res.and_then(|v| serde_json::from_value::<HostInfo>(v).map_err(RpcError::internal)) {
                    Ok(info) => {
                        *state.lock().unwrap() = ConnState::Connected(info);
                        generation.fetch_add(1, Ordering::Relaxed);
                        inbox2.lock().unwrap().events.push((key, EVENT_CONN_READY.into(), Value::Null));
                    }
                    Err(e) => {
                        *state.lock().unwrap() = ConnState::Disconnected { since: Instant::now(), reason: e.message };
                    }
                }
                ctx2.request_repaint();
            }),
        );
    }

    fn read_loop(&self, mut reader: Box<dyn Read + Send>, inbox: &Arc<Mutex<Inbox>>, ctx: &egui::Context) -> String {
        let mut dec = Decoder::new();
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = match reader.read(&mut buf) {
                Ok(0) => return "연결이 닫혔어요".to_string(),
                Ok(n) => n,
                Err(e) => return e.to_string(),
            };
            dec.push(&buf[..n]);
            loop {
                match dec.next_frame() {
                    Ok(Some(Frame::Output { session, data })) => {
                        if let Some(sink) = self.sinks.lock().unwrap().get_mut(&session) {
                            sink(&data);
                        }
                        ctx.request_repaint();
                    }
                    Ok(Some(Frame::Message(Message::Res { id, body }))) => {
                        let cb = self.pending.lock().unwrap().remove(&id);
                        if let Some(cb) = cb {
                            cb(match body {
                                ResBody::Ok(v) => Ok(v),
                                ResBody::Err(e) => Err(e),
                            });
                        }
                        ctx.request_repaint();
                    }
                    Ok(Some(Frame::Message(Message::Event { topic, data }))) => {
                        inbox.lock().unwrap().events.push((self.key, topic, data));
                        ctx.request_repaint();
                    }
                    Ok(Some(_)) => {}
                    Ok(None) => break,
                    Err(e) => return format!("잘못된 데이터: {e}"),
                }
            }
        }
    }

    fn fail_pending(&self) {
        let pending: Vec<Pending> = self.pending.lock().unwrap().drain().map(|(_, v)| v).collect();
        for cb in pending {
            cb(Err(RpcError::new(codes::DISCONNECTED, "연결이 끊겨 요청을 마치지 못했어요")));
        }
    }

    fn request_raw(&self, method: &str, params: Value, cb: Pending) {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.pending.lock().unwrap().insert(id, cb);
        let frame = Frame::Message(Message::Req { id, method: method.to_string(), params });
        if !self.write(&frame)
            && let Some(cb) = self.pending.lock().unwrap().remove(&id)
        {
            let text = match self.key {
                HostKey::Local => "이 컴퓨터의 Runwick에 연결되어 있지 않아요",
                HostKey::Device(_) => "그 기기에 연결되어 있지 않아요",
            };
            cb(Err(RpcError::new(codes::DISCONNECTED, text)));
        }
    }

    fn write(&self, frame: &Frame) -> bool {
        let Ok(bytes) = frame.encode() else { return false };
        let mut w = self.writer.lock().unwrap();
        match w.as_mut() {
            Some(w) => w.write_all(&bytes).and_then(|_| w.flush()).is_ok(),
            None => false,
        }
    }

    pub fn state(&self) -> ConnState {
        self.state.lock().unwrap().clone()
    }

    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Relaxed)
    }

    /// Send terminal input bytes. Returns false when not connected.
    pub fn send_input(&self, session: Uuid, data: &[u8]) -> bool {
        Frame::input_chunks(session, data).all(|f| self.write(&f))
    }

    pub fn set_output_sink(&self, session: Uuid, sink: OutputSink) {
        self.sinks.lock().unwrap().insert(session, sink);
    }

    pub fn remove_output_sink(&self, session: Uuid) {
        self.sinks.lock().unwrap().remove(&session);
    }

    pub fn shutdown(&self) {
        self.stop.store(true, Ordering::Relaxed);
        *self.writer.lock().unwrap() = None;
    }
}

/// Synthetic events raised by the client itself.
pub const EVENT_CONN_READY: &str = "conn.ready";
pub const EVENT_CONN_LOST: &str = "conn.lost";

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct QKey {
    host: HostKey,
    method: &'static str,
    params: String,
}

struct Entry {
    data: Option<AnyArc>,
    error: Option<RpcError>,
    in_flight: bool,
    stale: bool,
    topics: &'static [&'static str],
}

/// Snapshot of a cached query for one frame.
pub struct Query<T> {
    pub data: Option<Arc<T>>,
    pub error: Option<RpcError>,
    /// A request is in flight (first load or refresh).
    pub loading: bool,
}

impl<T> Query<T> {
    pub fn is_first_load(&self) -> bool {
        self.data.is_none() && self.error.is_none()
    }
}

pub struct Hub {
    conns: HashMap<HostKey, HostConn>,
    entries: HashMap<QKey, Entry>,
    inbox: Arc<Mutex<Inbox>>,
    ctx: egui::Context,
}

impl Hub {
    pub fn new(ctx: egui::Context, exe: PathBuf) -> Self {
        let inbox = Arc::new(Mutex::new(Inbox { results: vec![], events: vec![], tasks: vec![] }));
        let mut hub = Hub { conns: HashMap::new(), entries: HashMap::new(), inbox, ctx };
        hub.connect(HostKey::Local, Arc::new(LocalTransport { exe }));
        hub
    }

    pub fn connect(&mut self, key: HostKey, transport: Arc<dyn Transport>) {
        if let Some(old) = self.conns.remove(&key) {
            old.shutdown();
        }
        let conn = HostConn::start(key, transport, self.inbox.clone(), self.ctx.clone());
        self.conns.insert(key, conn);
    }

    pub fn disconnect(&mut self, key: HostKey) {
        if let Some(c) = self.conns.remove(&key) {
            c.shutdown();
        }
        self.entries.retain(|k, _| k.host != key);
    }

    pub fn conn(&self, key: HostKey) -> Option<&HostConn> {
        self.conns.get(&key)
    }

    pub fn hosts(&self) -> impl Iterator<Item = &HostConn> {
        self.conns.values()
    }

    pub fn state(&self, key: HostKey) -> ConnState {
        self.conns
            .get(&key)
            .map(|c| c.state())
            .unwrap_or(ConnState::Disconnected { since: Instant::now(), reason: "등록되지 않은 기기".into() })
    }

    pub fn local_info(&self) -> Option<HostInfo> {
        self.state(HostKey::Local).info().cloned()
    }

    /// Cached read. Fetches when missing or stale; returns the last good data meanwhile.
    pub fn query<M: Method>(&mut self, host: HostKey, params: &M::Params) -> Query<M::Output> {
        let key = QKey { host, method: M::NAME, params: serde_json::to_string(params).unwrap_or_default() };
        let needs_fetch = match self.entries.get(&key) {
            None => true,
            Some(e) => e.stale && !e.in_flight,
        };
        if needs_fetch {
            let connected = self.conns.get(&host).is_some_and(|c| c.state().is_connected());
            let entry = self.entries.entry(key.clone()).or_insert(Entry {
                data: None,
                error: None,
                in_flight: false,
                stale: true,
                topics: M::TOPICS,
            });
            if connected {
                entry.in_flight = true;
                entry.stale = false;
                let inbox = self.inbox.clone();
                let k2 = key.clone();
                let params_v = serde_json::to_value(params).unwrap_or(Value::Null);
                if let Some(conn) = self.conns.get(&host) {
                    conn.request_raw(
                        M::NAME,
                        params_v,
                        Box::new(move |res| {
                            let typed = res.and_then(|v| {
                                serde_json::from_value::<M::Output>(v)
                                    .map(|o| Arc::new(o) as AnyArc)
                                    .map_err(|e| RpcError::internal(format!("{}: 응답 형식 오류 ({e})", M::NAME)))
                            });
                            inbox.lock().unwrap().results.push((k2, typed));
                        }),
                    );
                }
            }
        }
        let e = &self.entries[&key];
        Query {
            data: e.data.clone().and_then(|d| d.downcast::<M::Output>().ok()),
            error: e.error.clone(),
            loading: e.in_flight,
        }
    }

    /// Run a request; `done` runs on the UI thread with the app and the typed result.
    pub fn call<M: Method>(
        &self,
        host: HostKey,
        params: M::Params,
        done: impl FnOnce(&mut App, Result<M::Output, RpcError>) + Send + 'static,
    ) {
        let inbox = self.inbox.clone();
        let ctx = self.ctx.clone();
        let cb: Pending = Box::new(move |res| {
            let typed = res.and_then(|v| {
                serde_json::from_value::<M::Output>(v)
                    .map_err(|e| RpcError::internal(format!("{}: 응답 형식 오류 ({e})", M::NAME)))
            });
            inbox.lock().unwrap().tasks.push(Box::new(move |app: &mut App| done(app, typed)));
            ctx.request_repaint();
        });
        match self.conns.get(&host) {
            Some(conn) => conn.request_raw(M::NAME, serde_json::to_value(params).unwrap_or(Value::Null), cb),
            None => cb(Err(RpcError::new(codes::DISCONNECTED, "등록되지 않은 기기예요"))),
        }
    }

    /// Fire-and-forget request; failures become a toast.
    pub fn send<M: Method>(&self, host: HostKey, params: M::Params) {
        self.call::<M>(host, params, |app, res| {
            if let Err(e) = res {
                app.toast_error(&e.message);
            }
        });
    }

    /// Queue work for the UI thread (for example from a background thread).
    pub fn post(&self, task: UiTask) {
        self.inbox.lock().unwrap().tasks.push(task);
        self.ctx.request_repaint();
    }

    pub fn invalidate(&mut self, host: HostKey, topics: &[&str]) {
        for (k, e) in self.entries.iter_mut() {
            if k.host == host && e.topics.iter().any(|t| topics.contains(t)) {
                e.stale = true;
            }
        }
    }

    pub fn invalidate_host(&mut self, host: HostKey) {
        for (k, e) in self.entries.iter_mut() {
            if k.host == host {
                e.stale = true;
                e.in_flight = false;
            }
        }
    }

    /// Apply finished queries and return events and UI tasks gathered since the last frame.
    pub fn pump(&mut self) -> (Vec<(HostKey, String, Value)>, Vec<UiTask>) {
        let (results, events, tasks) = {
            let mut ib = self.inbox.lock().unwrap();
            (std::mem::take(&mut ib.results), std::mem::take(&mut ib.events), std::mem::take(&mut ib.tasks))
        };
        for (k, res) in results {
            if let Some(e) = self.entries.get_mut(&k) {
                e.in_flight = false;
                match res {
                    Ok(d) => {
                        e.data = Some(d);
                        e.error = None;
                    }
                    Err(err) => e.error = Some(err),
                }
            }
        }
        for (host, topic, data) in &events {
            if topic == EVENT_CHANGED {
                let topics: Vec<String> =
                    data.get("topics").and_then(|t| serde_json::from_value(t.clone()).ok()).unwrap_or_default();
                let refs: Vec<&str> = topics.iter().map(|s| s.as_str()).collect();
                self.invalidate(*host, &refs);
            } else if topic == EVENT_CONN_READY {
                self.invalidate_host(*host);
            }
        }
        (events, tasks)
    }
}

fn local_device_label() -> String {
    std::env::var("RUNWICK_CLIENT_LABEL").unwrap_or_else(|_| {
        #[cfg(target_os = "macos")]
        if let Ok(out) = std::process::Command::new("scutil").args(["--get", "ComputerName"]).output() {
            let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !s.is_empty() {
                return s;
            }
        }
        "이 컴퓨터".to_string()
    })
}
