//! Shared host state passed to every handler.

use crate::store::Store;
use runwick_core::api::host::HostInfo;
use runwick_core::rpc::Message;
use runwick_core::wire::Frame;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use uuid::Uuid;

pub type ClientId = u64;

/// Terminal output a client may have waiting to be written before more is dropped. A client
/// that falls this far behind gets a fresh snapshot of each affected session once it caught
/// up (see `sessions::resync`), so a slow reader never makes the host hold unbounded output.
/// Room for one whole snapshot (at most 8 MiB) on an empty queue.
pub const MAX_QUEUED_OUTPUT: usize = 10 * 1024 * 1024;

/// One connected client (GUI window, agent CLI, remote bridge).
pub struct Client {
    pub id: ClientId,
    pub label: Mutex<String>,
    pub interactive: std::sync::atomic::AtomicBool,
    /// Process id of the peer on this computer's socket (0: unknown, e.g. a bridge or a test).
    /// Lets features tell which managed terminal a command really comes from.
    peer_pid: std::sync::atomic::AtomicU32,
    /// The managed session this connection proved to be (session token in `host.hello`).
    session: Mutex<Option<Uuid>>,
    /// Why a window claim at `host.hello` was refused, and whether it may ask the host to
    /// restart for it (see `gui_token`).
    window_refusal: Mutex<Option<(runwick_core::api::host::WindowRefusal, bool)>>,
    tx: mpsc::UnboundedSender<Frame>,
    /// Bytes of terminal output queued and not yet written.
    queued_output: AtomicUsize,
    /// Sessions whose output was dropped because the queue was full.
    lagging: Mutex<HashSet<Uuid>>,
}

impl Client {
    pub fn new(id: ClientId, tx: mpsc::UnboundedSender<Frame>) -> Self {
        Self {
            id,
            label: Mutex::new(format!("client-{id}")),
            interactive: Default::default(),
            peer_pid: Default::default(),
            session: Mutex::new(None),
            window_refusal: Mutex::new(None),
            tx,
            queued_output: AtomicUsize::new(0),
            lagging: Mutex::new(HashSet::new()),
        }
    }

    pub fn set_window_refusal(&self, refusal: Option<(runwick_core::api::host::WindowRefusal, bool)>) {
        if let Ok(mut r) = self.window_refusal.lock() {
            *r = refusal;
        }
    }

    pub fn window_refusal(&self) -> Option<(runwick_core::api::host::WindowRefusal, bool)> {
        self.window_refusal.lock().ok().and_then(|r| *r)
    }

    pub fn set_peer_pid(&self, pid: u32) {
        self.peer_pid.store(pid, Ordering::Relaxed);
    }

    /// The connecting process, when the socket told us.
    pub fn peer_pid(&self) -> Option<u32> {
        Some(self.peer_pid.load(Ordering::Relaxed)).filter(|p| *p > 0)
    }

    /// This connection proved to be a managed session (a run's session token).
    pub fn set_session(&self, id: Uuid) {
        *self.session.lock().unwrap_or_else(|e| e.into_inner()) = Some(id);
    }

    pub fn session(&self) -> Option<Uuid> {
        *self.session.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Queue a frame for this client. Returns false when the client is gone. Terminal output
    /// past [`MAX_QUEUED_OUTPUT`] is dropped and its session marked for a resync.
    pub fn send(&self, frame: Frame) -> bool {
        if let Frame::Output { session, data } = &frame {
            let mut lagging = self.lagging.lock().unwrap_or_else(|e| e.into_inner());
            if lagging.contains(session) {
                return !self.tx.is_closed();
            }
            if self.queued_output.load(Ordering::Relaxed) + data.len() > MAX_QUEUED_OUTPUT {
                tracing::debug!(client = self.id, %session, "client fell behind; output dropped until a resync");
                lagging.insert(*session);
                return !self.tx.is_closed();
            }
            self.queued_output.fetch_add(data.len(), Ordering::Relaxed);
        }
        self.tx.send(frame).is_ok()
    }

    /// Queued output was written. True when the queue is empty and sessions wait for a resync.
    pub fn output_written(&self, bytes: usize) -> bool {
        let before = self.queued_output.fetch_sub(bytes, Ordering::Relaxed);
        before == bytes && !self.lagging.lock().unwrap_or_else(|e| e.into_inner()).is_empty()
    }

    /// Sessions whose output was dropped (see [`Self::send`]).
    pub fn lagging(&self) -> Vec<Uuid> {
        self.lagging.lock().unwrap_or_else(|e| e.into_inner()).iter().copied().collect()
    }

    /// The session's output flows again (call under its lock, right before its snapshot).
    pub fn clear_lag(&self, session: Uuid) {
        self.lagging.lock().unwrap_or_else(|e| e.into_inner()).remove(&session);
    }

    pub fn label(&self) -> String {
        self.label.lock().unwrap().clone()
    }

    pub fn is_interactive(&self) -> bool {
        self.interactive.load(Ordering::Relaxed)
    }
}

/// Fan-out of events to all connected clients.
#[derive(Default)]
pub struct Bus {
    clients: Mutex<HashMap<ClientId, Arc<Client>>>,
}

impl Bus {
    pub fn add(&self, c: Arc<Client>) {
        self.clients.lock().unwrap().insert(c.id, c);
    }

    pub fn remove(&self, id: ClientId) {
        self.clients.lock().unwrap().remove(&id);
    }

    pub fn get(&self, id: ClientId) -> Option<Arc<Client>> {
        self.clients.lock().unwrap().get(&id).cloned()
    }

    pub fn all(&self) -> Vec<Arc<Client>> {
        self.clients.lock().unwrap().values().cloned().collect()
    }

    pub fn broadcast(&self, msg: Message) {
        for c in self.all() {
            c.send(Frame::Message(msg.clone()));
        }
    }

    /// Tell clients that cached results for these topics are stale.
    pub fn changed(&self, topics: &[&str]) {
        self.broadcast(Message::changed(topics));
    }

    pub fn event(&self, topic: &str, data: serde_json::Value) {
        self.broadcast(Message::event(topic, data));
    }
}

pub struct Host {
    pub info: HostInfo,
    pub store: Store,
    pub bus: Bus,
    pub sessions: crate::sessions::Manager,
    pub tools: crate::tools::ToolCache,
    next_client: AtomicU64,
    /// Set by `host.shutdown`; the server loop exits when notified.
    pub shutdown: tokio::sync::Notify,
}

impl Host {
    pub fn new(info: HostInfo, store: Store) -> Arc<Self> {
        Arc::new(Self {
            info,
            store,
            bus: Bus::default(),
            sessions: crate::sessions::Manager::default(),
            tools: Default::default(),
            next_client: AtomicU64::new(1),
            shutdown: tokio::sync::Notify::new(),
        })
    }

    pub fn next_client_id(&self) -> ClientId {
        self.next_client.fetch_add(1, Ordering::Relaxed)
    }

    pub fn host_id(&self) -> Uuid {
        self.info.host_id
    }
}

/// Per-request context.
#[derive(Clone)]
pub struct Ctx {
    pub host: Arc<Host>,
    pub client: Arc<Client>,
}
