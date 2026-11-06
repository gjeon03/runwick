use crate::Ts;
use crate::method;
use crate::rpc::topic;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Hello {
    /// Human label of the connecting client, e.g. "MacBook Pro · Runwick 0.1.0".
    pub client: String,
    pub version: String,
    /// GUI clients are interactive; agent CLIs and bridges are not. The host believes the claim
    /// only together with a matching [`Hello::token`].
    #[serde(default)]
    pub interactive: bool,
    /// The host's per-launch window token ([`crate::paths::gui_token_path`]); only a Runwick
    /// window on the same computer sends it. Never sent to another device's host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HostInfo {
    /// Stable id of this host (device), created on first start.
    pub host_id: Uuid,
    pub device_name: String,
    pub version: String,
    pub os: String,
    pub arch: String,
    /// Data folder, for display.
    pub home: String,
    pub started_at: Ts,
    pub pid: u32,
    /// Id the host assigned to this connection.
    pub client_id: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ShutdownParams {
    /// Stop managed runs too. When false the host refuses to exit while runs are alive.
    pub stop_runs: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ShutdownReport {
    /// Sessions whose processes have all ended.
    pub stopped: Vec<Uuid>,
    /// Of those, the ones that did not end in time and were killed.
    pub killed: Vec<Uuid>,
    /// Sessions with processes still alive after being killed: not stopped.
    #[serde(default)]
    pub leftovers: Vec<Leftover>,
}

/// A session that could not be stopped completely.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Leftover {
    pub session: Uuid,
    /// "name (pid N)" of each process still alive.
    pub processes: Vec<String>,
}

method!(HostHello, "host.hello", Hello => HostInfo, [topic::HOST]);
method!(HostPing, "host.ping", () => Ts);
method!(HostShutdown, "host.shutdown", ShutdownParams => ShutdownReport);

/// Why a connection that asked to be a window is not one.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WindowRefusal {
    /// No valid window token (another data folder, or the host restarted meanwhile).
    NoToken,
    /// The socket did not say which process connected.
    UnknownProcess,
    /// Opened from inside a Runwick terminal (a managed run).
    InsideRun,
    /// The host runs another Runwick build than this window (moved app kept running, update).
    DifferentBinary,
    /// The window was started with verification hooks.
    Hooks,
    /// A `runwick host --stdio` bridge outside an SSH login.
    NoSsh,
}

/// A running session that would stop if the host restarts.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RunningRun {
    pub session: Uuid,
    pub title: String,
    /// CLI label, e.g. "Claude Code".
    pub tool: String,
}

/// What this connection is, as the host decided at `host.hello`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct WindowStatus {
    pub window: bool,
    /// Set when the connection asked to be a window and is not one.
    #[serde(default)]
    pub refusal: Option<WindowRefusal>,
    /// The host's executable (display path), for [`WindowRefusal::DifferentBinary`].
    #[serde(default)]
    pub host_exe: Option<String>,
    /// Whether this connection may ask the host to restart for it ([`HostRestartForUpdate`]).
    #[serde(default)]
    pub may_restart: bool,
    /// Runs that would stop, when it may.
    #[serde(default)]
    pub running: Vec<RunningRun>,
}

method!(HostWindowStatus, "host.window_status", () => WindowStatus, [topic::HOST]);
// Stop every run and exit, so a window of another Runwick build can start the host again from
// its own executable. Only for a connection refused for exactly that reason (see the host).
method!(HostRestartForUpdate, "host.restart_for_update", () => ShutdownReport);

/// Key/value storage for small client state such as the GUI layout.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct KvKey {
    pub key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct KvSet {
    pub key: String,
    pub value: serde_json::Value,
}

method!(KvGet, "kv.get", KvKey => Option<serde_json::Value>, [topic::SETTINGS]);
method!(KvPut, "kv.put", KvSet => ());
