//! Devices found through Tailscale (`tailscale status --json`), when Tailscale is installed.
//! Only shown as choices: adding still goes through SSH, and Tailscale is never required.

use super::proc;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::process::Command;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq)]
pub struct Peer {
    /// Short machine name (`home-mac-mini`).
    pub host: String,
    /// MagicDNS name without the trailing dot, used as the SSH address when present.
    pub dns: Option<String>,
    pub ip: Option<String>,
    /// As Tailscale reports it: `macOS`, `linux`, `windows`, `iOS`, `android`…
    pub os: String,
    pub online: bool,
    /// Direct connection (`CurAddr` set) versus relayed (DERP).
    pub direct: bool,
    /// This computer.
    pub is_self: bool,
}

impl Peer {
    /// SSH destination for this peer.
    pub fn address(&self) -> String {
        self.dns.clone().or_else(|| self.ip.clone()).unwrap_or_else(|| self.host.clone())
    }

    /// Phones and tablets cannot run CLIs for Runwick.
    pub fn is_phone(&self) -> bool {
        matches!(self.os.to_lowercase().as_str(), "ios" | "android" | "ipados")
    }

    pub fn os_label(&self) -> String {
        match self.os.to_lowercase().as_str() {
            "macos" => "macOS".into(),
            "linux" => "Linux".into(),
            "windows" => "Windows".into(),
            "ios" => "iOS".into(),
            "android" => "Android".into(),
            "" => "운영체제 확인 불가".into(),
            _ => self.os.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Found {
    /// Tailscale is not installed here (adding by SSH address still works).
    NotInstalled,
    /// Installed but not connected (`BackendState` other than `Running`).
    Stopped(String),
    Peers(Vec<Peer>),
    Failed(String),
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Status {
    #[serde(default)]
    backend_state: String,
    #[serde(rename = "Self", default)]
    me: Option<Node>,
    #[serde(default)]
    peer: BTreeMap<String, Node>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Node {
    #[serde(default)]
    host_name: String,
    #[serde(rename = "DNSName", default)]
    dns_name: String,
    #[serde(rename = "OS", default)]
    os: String,
    #[serde(default)]
    online: bool,
    #[serde(default)]
    cur_addr: String,
    #[serde(rename = "TailscaleIPs", default)]
    tailscale_ips: Vec<String>,
}

fn peer(n: Node, is_self: bool) -> Peer {
    let dns = n.dns_name.trim_end_matches('.').to_string();
    let ip = n.tailscale_ips.into_iter().next();
    let host = [n.host_name.as_str(), dns.split('.').next().unwrap_or(""), ip.as_deref().unwrap_or("")]
        .into_iter()
        .find(|s| !s.is_empty())
        .unwrap_or("")
        .to_string();
    Peer {
        host,
        dns: (!dns.is_empty()).then_some(dns),
        ip,
        os: n.os,
        online: n.online || is_self,
        direct: !n.cur_addr.is_empty(),
        is_self,
    }
}

/// Parse `tailscale status --json`: this computer first, then online peers, then by name.
pub fn parse(json: &str) -> Result<Found, String> {
    let st: Status = serde_json::from_str(json).map_err(|e| format!("Tailscale 상태를 읽지 못했어요: {e}"))?;
    if st.backend_state != "Running" {
        return Ok(Found::Stopped(st.backend_state));
    }
    let mut peers: Vec<Peer> = st.peer.into_values().map(|n| peer(n, false)).collect();
    peers.sort_by(|a, b| b.online.cmp(&a.online).then_with(|| a.host.to_lowercase().cmp(&b.host.to_lowercase())));
    if let Some(me) = st.me {
        peers.insert(0, peer(me, true));
    }
    Ok(Found::Peers(peers))
}

fn binary() -> Option<String> {
    let candidates = [
        "/Applications/Tailscale.app/Contents/MacOS/Tailscale",
        "/opt/homebrew/bin/tailscale",
        "/usr/local/bin/tailscale",
        "/usr/bin/tailscale",
    ];
    candidates.iter().find(|p| std::path::Path::new(p).is_file()).map(|p| p.to_string())
}

/// Ask the local Tailscale for its peers (blocking; run off the UI thread).
pub fn discover() -> Found {
    let Some(bin) = binary() else {
        return Found::NotInstalled;
    };
    let mut c = Command::new(bin);
    c.args(["status", "--json"]);
    match proc::run(c, None, Duration::from_secs(5)) {
        Ok(o) if o.ok() => parse(&o.stdout).unwrap_or_else(Found::Failed),
        Ok(o) if o.timed_out => Found::Failed("Tailscale이 응답하지 않아요".into()),
        Ok(o) => {
            let line = o.stderr.lines().next().unwrap_or("").trim().to_string();
            // The CLI exits non-zero while logged out or stopped.
            if o.stdout.trim_start().starts_with('{') {
                parse(&o.stdout).unwrap_or(Found::Failed(line))
            } else {
                Found::Stopped(line)
            }
        }
        Err(e) => Found::Failed(format!("Tailscale을 실행하지 못했어요: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
      "BackendState": "Running",
      "Self": {"HostName": "macbook-pro", "DNSName": "macbook-pro.tail3f2a.ts.net.", "OS": "macOS",
               "Online": true, "TailscaleIPs": ["100.64.0.1"]},
      "Peer": {
        "nodekey:a": {"HostName": "iphone", "DNSName": "iphone.tail3f2a.ts.net.", "OS": "iOS", "Online": true,
                      "TailscaleIPs": ["100.64.0.3"], "CurAddr": "", "Relay": "tok"},
        "nodekey:b": {"HostName": "home-mac-mini", "DNSName": "home-mac-mini.tail3f2a.ts.net.", "OS": "macOS",
                      "Online": true, "CurAddr": "192.168.0.12:41641", "TailscaleIPs": ["100.64.0.2", "fd7a::2"]},
        "nodekey:c": {"HostName": "dev-server", "DNSName": "dev-server.tail3f2a.ts.net.", "OS": "linux",
                      "Online": false, "CurAddr": "", "Relay": "tok", "TailscaleIPs": ["100.64.0.4"]},
        "nodekey:d": {"HostName": "", "DNSName": "", "OS": "linux", "Online": true, "TailscaleIPs": ["100.64.0.9"]}
      }
    }"#;

    #[test]
    fn peers_parse_with_self_first_and_phones_marked() {
        let Found::Peers(p) = parse(SAMPLE).unwrap() else { panic!("peers") };
        let names: Vec<_> = p.iter().map(|x| x.host.as_str()).collect();
        assert_eq!(names, vec!["macbook-pro", "100.64.0.9", "home-mac-mini", "iphone", "dev-server"]);
        assert!(p[0].is_self);
        let mini = &p[2];
        assert_eq!(mini.address(), "home-mac-mini.tail3f2a.ts.net");
        assert!(mini.direct && mini.online && !mini.is_phone());
        assert_eq!(p[1].address(), "100.64.0.9", "no MagicDNS name: use the Tailscale IP");
        assert!(p[3].is_phone());
        let dev = &p[4];
        assert!(!dev.online && !dev.direct);
        assert_eq!(dev.os_label(), "Linux");
    }

    #[test]
    fn stopped_and_broken_status() {
        assert_eq!(parse(r#"{"BackendState":"Stopped"}"#).unwrap(), Found::Stopped("Stopped".into()));
        assert!(parse("not json").is_err());
    }
}
