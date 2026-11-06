//! Global environment variables: one set for every CLI that Runwick starts on a host (there
//! are no per-project or per-run sets; projects keep using `.env` and their own tools).
//!
//! Secret values stay in the OS secret store (macOS Keychain) and leave the host only through
//! the explicit [`EnvReveal`]. Every change of a value bumps the environment *version*; each run
//! records the version it started with (`Session::env_version`), so "saved" and "applied" can be
//! told apart and a session refresh knows what changed.

use crate::Ts;
use crate::method;
use crate::rpc::topic;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EnvVar {
    pub key: String,
    #[serde(default)]
    pub description: String,
    pub secret: bool,
    /// Value of a non-secret variable. Always `None` for secrets.
    #[serde(default)]
    pub value: Option<String>,
    /// Environment version in which the value last changed.
    pub changed_in: u64,
    pub updated_at: Ts,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SecretStoreKind {
    /// macOS Keychain (service `dev.runwick.env`, account = variable name).
    Keychain,
    /// A file only this user can read (mode 0600) under the Runwick data folder.
    File,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SecretStoreInfo {
    pub kind: SecretStoreKind,
    /// Where secrets are kept, for display (keychain service or file path).
    pub location: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EnvState {
    /// Current environment version (0 before the first change).
    pub version: u64,
    pub vars: Vec<EnvVar>,
    pub secrets: SecretStoreInfo,
    /// When the current version was saved.
    #[serde(default)]
    pub saved_at: Option<Ts>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SetVar {
    pub key: String,
    /// New value. `None` keeps the stored value (editing only the description or secret flag).
    #[serde(default)]
    pub value: Option<String>,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub secret: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct VarKey {
    pub key: String,
}

/// When the current value of a variable was stored as plain text.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PlainPeriod {
    pub key: String,
    /// When the variable was first saved (its value may have been plain since then).
    pub since: Ts,
    /// When it became secret, keeping that value; `None` while it is still plain.
    #[serde(default)]
    pub until: Option<Ts>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EnvChangeKind {
    Added,
    Changed,
    Removed,
    /// The secret could not be read when the run started, so the run does not have it.
    Unreadable,
}

/// What changed for one variable since a run started. Never carries values.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EnvChange {
    pub key: String,
    pub kind: EnvChangeKind,
}

method!(EnvList, "env.list", () => EnvState, [topic::ENV]);
method!(EnvSet, "env.set", SetVar => EnvVar);
method!(EnvRemove, "env.remove", VarKey => ());
// The value of one variable for an explicit reveal or copy by the person. Never logged.
method!(EnvReveal, "env.reveal", VarKey => String);
// Variables whose current value is or was a plain value, with that period, so the window can
// tell whether it left this computer as plain text (sync, backups). Names and times only.
method!(EnvPlainHistory, "env.plain_history", () => Vec<PlainPeriod>, [topic::ENV]);

/// Names Runwick sets itself in every managed terminal; they cannot be global variables.
pub const RESERVED: &[&str] =
    &["PATH", "HOME", "SHELL", "TERM", "COLORTERM", "TERM_PROGRAM", "TERM_PROGRAM_VERSION", "PWD"];

/// Why a variable name is not accepted (Korean, for display), or `None` when it is valid.
pub fn key_problem(key: &str) -> Option<String> {
    if key.is_empty() {
        return Some("이름을 입력해 주세요".into());
    }
    if key.len() > 128 {
        return Some("이름은 128자까지 쓸 수 있어요".into());
    }
    let mut chars = key.chars();
    let first_ok = chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_');
    if !first_ok || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Some("영문, 숫자, 밑줄(_)만 쓸 수 있고 숫자로 시작할 수 없어요".into());
    }
    if RESERVED.contains(&key) || key.starts_with("RUNWICK_") {
        return Some(format!("{key}는 Runwick이 터미널마다 직접 정해서 바꿀 수 없어요"));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variable_names_follow_shell_rules() {
        assert!(key_problem("GITHUB_TOKEN").is_none());
        assert!(key_problem("_x1").is_none());
        assert!(key_problem("").is_some());
        assert!(key_problem("1ABC").is_some());
        assert!(key_problem("A-B").is_some());
        assert!(key_problem("한글").is_some());
        assert!(key_problem("PATH").is_some());
        assert!(key_problem("RUNWICK_SESSION_ID").is_some());
    }
}
