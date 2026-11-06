//! Registered devices (other computers whose own host keeps their runs and records) and the
//! links that show their workspaces in this computer's sidebar.
//!
//! The GUI reaches a device's host over SSH (`ssh … '<runwick> host --stdio'`) and speaks the
//! same protocol as with the local host. This computer's host only stores the device list and
//! the workspace links; it never runs anything on a device by itself.
//!
//! `devices.browse_dirs` is answered by whichever host is asked: the add-device flow asks the
//! device's host to list folders under that computer's home.

use crate::Ts;
use crate::api::projects::{ById, ProjectId, Workspace, WorkspaceId};
use crate::method;
use crate::rpc::topic;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub type DeviceId = Uuid;

/// Prefix of a target that starts a local test host instead of SSH (development and tests
/// only; the app refuses it unless `RUNWICK_DEV_SIMULATED_DEVICES=1`).
pub const SIMULATED_PREFIX: &str = "sim:";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Device {
    pub id: DeviceId,
    /// Name the person gave it, e.g. "집 Mac mini".
    pub name: String,
    /// SSH destination as typed (`user@host`, `host`, or an alias from ~/.ssh/config).
    pub target: String,
    #[serde(default)]
    pub port: Option<u16>,
    /// `std::env::consts::OS` / `ARCH` of the device's host when last seen.
    #[serde(default)]
    pub os: Option<String>,
    #[serde(default)]
    pub arch: Option<String>,
    /// Stable id of the device's host; a different id at the same address is another install.
    #[serde(default)]
    pub remote_host_id: Option<Uuid>,
    #[serde(default)]
    pub remote_version: Option<String>,
    /// How Runwick is started there (path found during setup).
    #[serde(default)]
    pub remote_exe: Option<String>,
    /// Host key fingerprint the person confirmed (`SHA256:…`), for display.
    #[serde(default)]
    pub fingerprint: Option<String>,
    /// How it was found, e.g. "Tailscale".
    #[serde(default)]
    pub via: Option<String>,
    pub added_at: Ts,
    /// Last time this computer's GUI was connected to the device's host.
    #[serde(default)]
    pub last_seen_at: Option<Ts>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AddDevice {
    /// Id chosen by the client (it may already be connected under this id during setup).
    #[serde(default)]
    pub id: Option<DeviceId>,
    pub name: String,
    pub target: String,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub os: Option<String>,
    #[serde(default)]
    pub arch: Option<String>,
    #[serde(default)]
    pub remote_host_id: Option<Uuid>,
    #[serde(default)]
    pub remote_version: Option<String>,
    #[serde(default)]
    pub remote_exe: Option<String>,
    #[serde(default)]
    pub fingerprint: Option<String>,
    #[serde(default)]
    pub via: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DeviceUpdate {
    pub id: DeviceId,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub remote_exe: Option<String>,
}

/// Reported by the GUI after it connected to a device's host.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceSeen {
    pub id: DeviceId,
    pub remote_host_id: Uuid,
    pub remote_version: String,
    pub os: String,
    pub arch: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct DeviceRemoved {
    /// Workspace links removed from this computer's sidebar (the device keeps its records).
    pub unlinked: usize,
}

/// Which local project a remote workspace is shown under.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum LinkProject {
    /// Under an existing project of this computer (chosen by the person, never automatic).
    Existing { project_id: ProjectId },
    /// A separate project that only holds remote workspaces.
    New { name: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LinkRemote {
    pub device_id: DeviceId,
    /// Workspace id inside the device's host.
    pub remote_workspace_id: WorkspaceId,
    /// Workspace name shown in the sidebar.
    pub name: String,
    /// Folder on the device (absolute, as the device's host reported it).
    pub path: String,
    pub project: LinkProject,
    /// `origin` of the folder on the device, kept with a new project for later matching.
    #[serde(default)]
    pub remote_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BrowseDirs {
    /// Folder to list; `None` lists the home folder. Must be inside the home folder.
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub show_hidden: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DirInfo {
    pub name: String,
    /// Absolute path on the answering host.
    pub path: String,
    pub is_git: bool,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub remote_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DirListing {
    /// The listed folder (absolute).
    pub dir: DirInfo,
    /// Same folder for display (`~/work/x`).
    pub display: String,
    /// Parent folder when it is still inside the home folder.
    #[serde(default)]
    pub parent: Option<String>,
    /// Absolute home folder of the answering host.
    pub home: String,
    /// Sub-folders, sorted by name.
    pub dirs: Vec<DirInfo>,
    /// Folders left out because the list was capped.
    #[serde(default)]
    pub more: usize,
}

/// Most sub-folders `devices.browse_dirs` returns.
pub const BROWSE_LIMIT: usize = 400;

method!(DevicesList, "devices.list", () => Vec<Device>, [topic::DEVICES]);
method!(DevicesAdd, "devices.add", AddDevice => Device);
method!(DevicesUpdate, "devices.update", DeviceUpdate => Device);
method!(DevicesSeen, "devices.seen", DeviceSeen => Device);
method!(DevicesRemove, "devices.remove", ById => DeviceRemoved);
method!(WorkspacesLinkRemote, "workspaces.link_remote", LinkRemote => Workspace);
method!(WorkspacesUnlinkRemote, "workspaces.unlink_remote", ById => ());
method!(DevicesBrowseDirs, "devices.browse_dirs", BrowseDirs => DirListing);

/// SSH destination parts typed by the person: `user@host`, `host:port`, `user@host:port`,
/// `[v6]:port`. Returns `(destination, port)` or a Korean explanation.
pub fn parse_target(input: &str) -> Result<(String, Option<u16>), String> {
    let s = input.trim();
    if s.is_empty() {
        return Err("SSH 주소를 입력하세요".into());
    }
    if s.chars().any(|c| c.is_whitespace() || c == '\'' || c == '"' || c == '`' || c == ';' || c == '$') {
        return Err("주소에 공백이나 따옴표, 특수 문자는 쓸 수 없어요".into());
    }
    if s.starts_with('-') {
        return Err("주소는 ‘-’로 시작할 수 없어요".into());
    }
    let (user, rest) = match s.rsplit_once('@') {
        Some((u, r)) if !u.is_empty() && !r.is_empty() => (Some(u), r),
        Some(_) => return Err("‘사용자@주소’ 형식으로 입력하세요".into()),
        None => (None, s),
    };
    let (host, port) = if let Some(v6) = rest.strip_prefix('[') {
        let (h, tail) = v6.split_once(']').ok_or_else(|| "IPv6 주소의 ‘]’가 빠졌어요".to_string())?;
        let port = match tail.strip_prefix(':') {
            Some(p) => Some(parse_port(p)?),
            None if tail.is_empty() => None,
            None => return Err("주소 형식을 확인하세요".into()),
        };
        (h.to_string(), port)
    } else if rest.matches(':').count() == 1 {
        let (h, p) = rest.split_once(':').unwrap_or((rest, ""));
        (h.to_string(), Some(parse_port(p)?))
    } else {
        (rest.to_string(), None)
    };
    if host.is_empty() {
        return Err("주소를 입력하세요".into());
    }
    let dest = match user {
        Some(u) => format!("{u}@{host}"),
        None => host,
    };
    Ok((dest, port))
}

fn parse_port(p: &str) -> Result<u16, String> {
    p.parse::<u16>().ok().filter(|n| *n > 0).ok_or_else(|| format!("포트 번호가 올바르지 않아요: {p}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets_parse_user_host_and_port() {
        assert_eq!(parse_target("mini").unwrap(), ("mini".into(), None));
        assert_eq!(parse_target(" me@mini.tail3f2a.ts.net ").unwrap(), ("me@mini.tail3f2a.ts.net".into(), None));
        assert_eq!(parse_target("me@192.168.0.12:2222").unwrap(), ("me@192.168.0.12".into(), Some(2222)));
        assert_eq!(parse_target("[fd7a::1]:22").unwrap(), ("fd7a::1".into(), Some(22)));
        assert!(parse_target("").is_err());
        assert!(parse_target("-oProxyCommand=x").is_err());
        assert!(parse_target("me@host; rm").is_err());
        assert!(parse_target("@host").is_err());
        assert!(parse_target("host:0").is_err());
        assert!(parse_target("host:abc").is_err());
    }
}
