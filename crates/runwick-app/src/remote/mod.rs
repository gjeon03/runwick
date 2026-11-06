//! Registered devices as connections of this GUI: every device runs its own host; this computer
//! reaches it over SSH (`ssh` module) and addresses it as `HostKey::Device(id)`, so sessions,
//! files, records and refresh go to that host unchanged.
//!
//! - Connections follow the device list of this computer's host (`devices.list`).
//! - `연결 끊기` closes only this GUI's connection; the device keeps its runs.
//! - A connection that drops is remembered with the time and cause, so screens can keep the
//!   last state and say why ("원인 · 네트워크") instead of treating it as a failed AI run.
//! - A host that answers with another Runwick version, or as another installation than the
//!   one registered, is not used until checked again.

pub mod envsync;
pub mod probe;
pub mod proc;
pub mod ssh;
pub mod tailscale;
pub mod unsent;

use crate::App;
use crate::client::{ConnState, EVENT_CONN_LOST, EVENT_CONN_READY, HostKey};
use runwick_core::Ts;
use runwick_core::api::devices::{Device, DeviceId, DeviceSeen, DevicesList, DevicesSeen};
use runwick_core::api::host::HostInfo;
use runwick_core::api::sessions::{Phase, SessionInfo};
use runwick_core::rpc::{EVENT_CHANGED, topic};
use serde_json::Value;
use ssh::{Failure, Route, SshTransport};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

#[derive(Default)]
pub struct State {
    links: HashMap<DeviceId, Link>,
    /// Disconnected on purpose from this GUI (runs continue on the device).
    off: HashSet<DeviceId>,
    /// Devices as last listed by this computer's host.
    devices: Vec<Device>,
    pub unsent: unsent::Unsent,
}

struct Link {
    transport: Option<Arc<SshTransport>>,
    /// Why there is no transport (for example no Runwick path recorded yet).
    route_error: Option<String>,
    refused: Option<Refusal>,
    connected_since: Option<Instant>,
    last_info: Option<HostInfo>,
    lost: Option<Lost>,
    back: Option<Back>,
}

/// The device's host answered, but it is not used.
#[derive(Debug, Clone, PartialEq)]
pub enum Refusal {
    Version {
        remote: String,
    },
    /// Another installation (or computer) answered at the registered address.
    OtherHost,
    ThisComputer,
}

/// An established connection dropped.
#[derive(Debug, Clone)]
pub struct Lost {
    pub at: Instant,
    pub at_ts: Ts,
    /// Runs seen on the device just before (None when unknown).
    pub running: Option<usize>,
}

/// Connected again after a drop.
#[derive(Debug, Clone)]
pub struct Back {
    pub after: std::time::Duration,
}

/// How a device is reachable right now, as the person should see it.
#[derive(Debug, Clone)]
pub enum Status {
    /// Not in the device list (yet).
    Unknown,
    Connecting {
        lost: Option<Lost>,
        failure: Option<Failure>,
    },
    Connected(HostInfo),
    /// `연결 끊기` from this GUI.
    Off,
    Refused(Refusal),
    /// Could not connect or dropped; retrying in the background.
    Unreachable {
        lost: Option<Lost>,
        failure: Option<Failure>,
        reason: String,
    },
    /// Missing setup, such as the Runwick path.
    Setup(String),
}

impl Status {
    pub fn is_connected(&self) -> bool {
        matches!(self, Status::Connected(_))
    }
}

pub fn key(id: DeviceId) -> HostKey {
    HostKey::Device(id)
}

fn st(app: &mut App) -> &mut State {
    &mut app.feat.devices.remote
}

/// Registered devices (cached; refreshed by the `devices` topic).
pub fn devices(app: &mut App) -> Vec<Device> {
    match app.hub.query::<DevicesList>(HostKey::Local, &()).data {
        Some(l) => l.as_ref().clone(),
        None => st(app).devices.clone(),
    }
}

pub fn device(app: &mut App, id: DeviceId) -> Option<Device> {
    devices(app).into_iter().find(|d| d.id == id)
}

/// The name the person gave a device (`집 Mac mini`).
pub fn device_name(app: &mut App, id: DeviceId) -> String {
    device(app, id).map(|d| d.name).unwrap_or_else(|| "원격 기기".into())
}

/// The registered name from the last device list, for code that only has `&App`.
pub fn device_name_cached(app: &App, id: DeviceId) -> Option<String> {
    app.feat.devices.remote.devices.iter().find(|d| d.id == id).map(|d| d.name.clone())
}

/// `이 컴퓨터` or the device name.
pub fn host_label(app: &mut App, host: HostKey) -> String {
    match host {
        HostKey::Local => "이 컴퓨터".into(),
        HostKey::Device(id) => device_name(app, id),
    }
}

pub fn status(app: &mut App, id: DeviceId) -> Status {
    if st(app).off.contains(&id) {
        return Status::Off;
    }
    let conn = app.hub.state(key(id));
    let Some(link) = st(app).links.get(&id) else {
        return Status::Unknown;
    };
    if let Some(r) = &link.refused {
        return Status::Refused(r.clone());
    }
    if let Some(e) = &link.route_error {
        return Status::Setup(e.clone());
    }
    let failure = link.transport.as_ref().and_then(|t| t.last_failure());
    match conn {
        ConnState::Connected(info) => Status::Connected(info),
        ConnState::Connecting => Status::Connecting { lost: link.lost.clone(), failure },
        ConnState::Disconnected { reason, .. } => Status::Unreachable { lost: link.lost.clone(), failure, reason },
    }
}

/// The note shown once after a connection came back (until dismissed).
pub fn back(app: &mut App, id: DeviceId) -> Option<Back> {
    st(app).links.get(&id).and_then(|l| l.back.clone())
}

pub fn dismiss_back(app: &mut App, id: DeviceId) {
    if let Some(l) = st(app).links.get_mut(&id) {
        l.back = None;
    }
}

/// Last connection time this GUI confirmed (from the device record).
pub fn last_seen(app: &mut App, id: DeviceId) -> Option<Ts> {
    device(app, id).and_then(|d| d.last_seen_at)
}

/// Connect (or reconnect now) to a registered device.
pub fn connect(app: &mut App, d: &Device) {
    st(app).off.remove(&d.id);
    if let Some(old) = st(app).links.get(&d.id).and_then(|l| l.transport.clone()) {
        old.close();
    }
    let prev = st(app).links.remove(&d.id);
    let mut link = Link {
        transport: None,
        route_error: None,
        refused: None,
        connected_since: None,
        last_info: prev.as_ref().and_then(|l| l.last_info.clone()),
        lost: prev.and_then(|l| l.lost),
        back: None,
    };
    match Route::for_device(d, &app.exe) {
        Ok(route) => {
            let t = Arc::new(SshTransport::new(route));
            link.transport = Some(t.clone());
            app.hub.connect(key(d.id), t);
        }
        Err(e) => {
            app.hub.disconnect(key(d.id));
            link.route_error = Some(e);
        }
    }
    st(app).links.insert(d.id, link);
}

/// Connect under a chosen id during the add flow (before the device is registered).
pub fn connect_route(app: &mut App, id: DeviceId, route: Route) {
    if let Some(old) = st(app).links.remove(&id).and_then(|l| l.transport) {
        old.close();
    }
    let t = Arc::new(SshTransport::new(route));
    st(app).links.insert(
        id,
        Link {
            transport: Some(t.clone()),
            route_error: None,
            refused: None,
            connected_since: None,
            last_info: None,
            lost: None,
            back: None,
        },
    );
    app.hub.connect(key(id), t);
}

/// `연결 끊기`: close this GUI's connection only.
pub fn disconnect(app: &mut App, id: DeviceId) {
    st(app).off.insert(id);
    drop_link(app, id);
}

/// Forget a connection entirely (device removed, or add flow cancelled).
pub fn forget(app: &mut App, id: DeviceId) {
    st(app).off.remove(&id);
    drop_link(app, id);
    app.hub.disconnect(key(id));
    st(app).links.remove(&id);
    st(app).unsent.forget_host(key(id));
}

fn drop_link(app: &mut App, id: DeviceId) {
    // Stop the connection but keep its cached data: screens keep showing the last state.
    if let Some(c) = app.hub.conn(key(id)) {
        c.shutdown();
    }
    if let Some(l) = st(app).links.get_mut(&id) {
        if let Some(t) = l.transport.take() {
            t.close();
        }
        l.connected_since = None;
        l.back = None;
    }
}

/// `다시 연결` / `연결 확인`: try now, also after a version or identity refusal.
pub fn reconnect(app: &mut App, id: DeviceId) {
    match device(app, id) {
        Some(d) => connect(app, &d),
        None => app.toast_error("등록된 기기를 찾을 수 없어요"),
    }
}

fn refresh_list(app: &mut App) {
    app.hub.call::<DevicesList>(HostKey::Local, (), |app, res| match res {
        Ok(list) => sync(app, list),
        Err(e) => tracing::warn!("devices.list failed: {}", e.message),
    });
}

/// Follow the device list: connect new devices, drop removed ones.
fn sync(app: &mut App, list: Vec<Device>) {
    let ids: HashSet<DeviceId> = list.iter().map(|d| d.id).collect();
    let gone: Vec<DeviceId> = st(app).devices.iter().map(|d| d.id).filter(|id| !ids.contains(id)).collect();
    for id in gone {
        forget(app, id);
    }
    for d in &list {
        let known = st(app).links.contains_key(&d.id);
        let changed = st(app)
            .devices
            .iter()
            .find(|x| x.id == d.id)
            .is_some_and(|x| x.target != d.target || x.port != d.port || x.remote_exe != d.remote_exe);
        if (!known || changed) && !st(app).off.contains(&d.id) {
            connect(app, d);
        }
    }
    st(app).devices = list;
}

/// Runs seen on a host right now, from cached session lists.
fn running_on(app: &mut App, host: HostKey) -> Option<usize> {
    let list = crate::shell::data::all_sessions(app, host).data?;
    Some(list.iter().filter(|s| is_running(app, host, s)).count())
}

pub fn is_running(app: &App, host: HostKey, s: &SessionInfo) -> bool {
    let live = crate::terminal::live(app, host, s.session.id).or_else(|| s.live.clone());
    live.is_some_and(|l| matches!(l.phase, Phase::Starting | Phase::Running))
}

pub fn on_event(app: &mut App, host: HostKey, topic_name: &str, data: &Value) {
    match host {
        HostKey::Local if topic_name == EVENT_CONN_READY => refresh_list(app),
        HostKey::Local if topic_name == EVENT_CHANGED => {
            let devices_changed = data
                .get("topics")
                .and_then(|t| t.as_array())
                .is_some_and(|t| t.iter().any(|x| x.as_str() == Some(topic::DEVICES)));
            if devices_changed {
                refresh_list(app);
            }
        }
        HostKey::Device(id) if topic_name == EVENT_CONN_READY => on_ready(app, id),
        HostKey::Device(id) if topic_name == EVENT_CONN_LOST => on_lost(app, id),
        _ => {}
    }
}

fn on_ready(app: &mut App, id: DeviceId) {
    let Some(info) = app.hub.state(key(id)).info().cloned() else {
        return;
    };
    let local = app.hub.local_info().map(|i| i.host_id);
    let registered = device(app, id);
    let refusal = if info.version != runwick_core::VERSION {
        Some(Refusal::Version { remote: info.version.clone() })
    } else if local == Some(info.host_id) {
        Some(Refusal::ThisComputer)
    } else if registered.as_ref().and_then(|d| d.remote_host_id).is_some_and(|h| h != info.host_id) {
        Some(Refusal::OtherHost)
    } else {
        None
    };
    if let Some(r) = refusal {
        // Not used: stop retrying until the person checks again.
        drop_link(app, id);
        if let Some(l) = st(app).links.get_mut(&id) {
            l.refused = Some(r);
            l.last_info = Some(info);
        }
        return;
    }
    if let Some(l) = st(app).links.get_mut(&id) {
        l.back = l.lost.take().map(|lost| Back { after: lost.at.elapsed() });
        l.connected_since = Some(Instant::now());
        l.last_info = Some(info.clone());
    }
    if registered.is_some() {
        report_seen(app, id, &info);
    }
}

fn on_lost(app: &mut App, id: DeviceId) {
    // A replaced connection reports its end after the new one started.
    if !matches!(app.hub.state(key(id)), ConnState::Disconnected { .. }) || st(app).off.contains(&id) {
        return;
    }
    let was = st(app).links.get(&id).and_then(|l| l.connected_since);
    if was.is_none() {
        return;
    }
    let running = running_on(app, key(id));
    let info = st(app).links.get(&id).and_then(|l| l.last_info.clone());
    if let Some(l) = st(app).links.get_mut(&id) {
        l.connected_since = None;
        l.back = None;
        l.lost = Some(Lost { at: Instant::now(), at_ts: runwick_core::now(), running });
    }
    if let Some(info) = info
        && device(app, id).is_some()
    {
        report_seen(app, id, &info);
    }
}

/// Record on this computer's host that the device was reachable until now.
fn report_seen(app: &mut App, id: DeviceId, info: &HostInfo) {
    let p = DeviceSeen {
        id,
        remote_host_id: info.host_id,
        remote_version: info.version.clone(),
        os: info.os.clone(),
        arch: info.arch.clone(),
    };
    app.hub.call::<DevicesSeen>(HostKey::Local, p, |app, res| {
        if let Err(e) = res {
            app.toast_error(&format!("기기 연결 기록을 남기지 못했어요: {}", e.message));
        }
    });
}

/// The `원인 · …` line and a sentence for an unreachable device.
pub fn explain(status: &Status, name: &str) -> Option<(String, String)> {
    match status {
        Status::Unreachable { failure: Some(f), .. } | Status::Connecting { failure: Some(f), .. } => {
            Some((f.cause.label().to_string(), f.explain(name)))
        }
        Status::Unreachable { reason, .. } => Some(("확인 불가".into(), reason.clone())),
        Status::Refused(Refusal::Version { remote }) => Some((
            "버전 불일치".into(),
            format!(
                "{name} {remote} · 이 컴퓨터 {}. 기기에 닿았고 로그인도 됐어요. 네트워크나 로그인 문제는 아니에요.",
                runwick_core::VERSION
            ),
        )),
        Status::Refused(Refusal::OtherHost) => Some((
            "다른 Runwick".into(),
            "등록할 때와 다른 Runwick이 응답했어요. 같은 주소의 다른 기기이거나 다시 설치된 것 같아요. 기기를 다시 추가하세요."
                .into(),
        )),
        Status::Refused(Refusal::ThisComputer) => {
            Some(("이 컴퓨터".into(), "지금 쓰는 컴퓨터가 응답했어요. 이 컴퓨터는 등록하지 않아도 이미 쓰고 있어요.".into()))
        }
        Status::Setup(e) => Some(("준비 필요".into(), e.clone())),
        _ => None,
    }
}

/// Short state words with a tone, for lists and badges.
pub fn summary(status: &Status) -> (crate::theme::Tone, String) {
    use crate::theme::Tone;
    match status {
        Status::Unknown => (Tone::Waiting, "확인 중".into()),
        Status::Connecting { lost: Some(_), .. } => (Tone::Waiting, "다시 연결하는 중".into()),
        Status::Connecting { .. } => (Tone::Waiting, "연결하는 중".into()),
        Status::Connected(_) => (Tone::Running, "연결됨".into()),
        Status::Off => (Tone::Waiting, "직접 끊음 · 실행은 계속".into()),
        Status::Refused(Refusal::Version { .. }) => (Tone::Check, "연결 안 됨 · 버전 확인 필요".into()),
        Status::Refused(_) => (Tone::Check, "연결 안 됨 · 기기 확인 필요".into()),
        Status::Unreachable { failure: Some(f), .. } => (Tone::Waiting, format!("연결 안 됨 · {}", f.cause.label())),
        Status::Unreachable { .. } => (Tone::Waiting, "연결 안 됨".into()),
        Status::Setup(_) => (Tone::Check, "준비 필요".into()),
    }
}

/// `HH:MM` in local time.
pub fn clock(ts: Ts) -> String {
    ts.with_timezone(&chrono::Local).format("%H:%M").to_string()
}

/// `18분`, `2시간 5분`.
pub fn duration_words(d: std::time::Duration) -> String {
    let m = d.as_secs() / 60;
    match m {
        0 => "1분이 안 되는 시간".into(),
        1..=59 => format!("{m}분"),
        _ if m.is_multiple_of(60) => format!("{}시간", m / 60),
        _ => format!("{}시간 {}분", m / 60, m % 60),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_read_naturally() {
        use std::time::Duration;
        assert_eq!(duration_words(Duration::from_secs(20)), "1분이 안 되는 시간");
        assert_eq!(duration_words(Duration::from_secs(18 * 60)), "18분");
        assert_eq!(duration_words(Duration::from_secs(120 * 60)), "2시간");
        assert_eq!(duration_words(Duration::from_secs(125 * 60)), "2시간 5분");
    }

    #[test]
    fn statuses_never_claim_more_than_observed() {
        let s = Status::Unreachable {
            lost: None,
            failure: Some(Failure::new(ssh::Kind::Timeout, "")),
            reason: String::new(),
        };
        let (cause, text) = explain(&s, "집 Mac mini").unwrap();
        assert_eq!(cause, "응답 없음");
        assert!(text.contains("잠자기"));
        assert_eq!(summary(&s).1, "연결 안 됨 · 응답 없음");
        let v = Status::Refused(Refusal::Version { remote: "0.0.1".into() });
        assert!(explain(&v, "개발 서버").unwrap().1.contains("네트워크나 로그인 문제는 아니에요"));
        assert!(explain(&Status::Off, "x").is_none());
    }
}
