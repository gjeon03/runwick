//! Request/response/event messages and the typed [`Method`] contract.
//!
//! Each feature declares its methods in `api/<feature>.rs` with [`crate::method!`]. The host
//! registers a handler per method; clients call them with typed params and get typed output.
//! After a mutation the host emits a `changed` event listing [`topic`]s so clients can refetch.

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Message {
    Req {
        id: u64,
        method: String,
        #[serde(default)]
        params: Value,
    },
    Res {
        id: u64,
        #[serde(flatten)]
        body: ResBody,
    },
    Event {
        topic: String,
        #[serde(default)]
        data: Value,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResBody {
    Ok(Value),
    Err(RpcError),
}

impl Message {
    pub fn event(topic: &str, data: Value) -> Self {
        Message::Event { topic: topic.to_string(), data }
    }

    pub fn changed(topics: &[&str]) -> Self {
        Message::event(EVENT_CHANGED, serde_json::json!({ "topics": topics }))
    }
}

/// Event topic emitted after any mutation: `{ "topics": ["projects", ...] }`.
pub const EVENT_CHANGED: &str = "changed";

/// Error returned by a method. `code` is stable for programmatic handling; `message` is
/// user-facing Korean text when the host can explain the failure.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, thiserror::Error)]
#[error("{message} ({code})")]
pub struct RpcError {
    pub code: String,
    pub message: String,
}

impl RpcError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self { code: code.into(), message: message.into() }
    }
    pub fn not_found(what: impl Into<String>) -> Self {
        Self::new(codes::NOT_FOUND, what)
    }
    pub fn invalid(what: impl Into<String>) -> Self {
        Self::new(codes::INVALID, what)
    }
    pub fn internal(what: impl std::fmt::Display) -> Self {
        Self::new(codes::INTERNAL, what.to_string())
    }
}

pub mod codes {
    pub const NOT_FOUND: &str = "not_found";
    pub const INVALID: &str = "invalid";
    pub const INTERNAL: &str = "internal";
    pub const UNKNOWN_METHOD: &str = "unknown_method";
    pub const CONFLICT: &str = "conflict";
    pub const UNSUPPORTED: &str = "unsupported";
    pub const DISCONNECTED: &str = "disconnected";
    pub const BUSY: &str = "busy";
    pub const DENIED: &str = "denied";
}

/// Topics used for cache invalidation. Features may add their own string constants.
pub mod topic {
    pub const HOST: &str = "host";
    pub const PROJECTS: &str = "projects";
    pub const WORKSPACES: &str = "workspaces";
    pub const SESSIONS: &str = "sessions";
    pub const MESSAGES: &str = "messages";
    pub const PINS: &str = "pins";
    pub const FILES: &str = "files";
    pub const WIKI: &str = "wiki";
    pub const TIMELINE: &str = "timeline";
    pub const SETTINGS: &str = "settings";
    pub const ENV: &str = "env";
    pub const TOOLS: &str = "tools";
    pub const JOBS: &str = "jobs";
    pub const COLLAB: &str = "collab";
    pub const DEVICES: &str = "devices";
    pub const SYNC: &str = "sync";
    pub const ACTIVITY: &str = "activity";
}

pub trait Method {
    const NAME: &'static str;
    /// Topics whose `changed` event invalidates a cached result of this method.
    const TOPICS: &'static [&'static str];
    type Params: Serialize + DeserializeOwned + Send + Sync + 'static;
    type Output: Serialize + DeserializeOwned + Send + Sync + 'static;
}

/// Declare a method type.
///
/// ```ignore
/// method!(ListProjects, "projects.list", () => Vec<Project>, [topic::PROJECTS]);
/// ```
#[macro_export]
macro_rules! method {
    ($ty:ident, $name:literal, $params:ty => $out:ty, [$($t:expr),* $(,)?]) => {
        pub struct $ty;
        impl $crate::rpc::Method for $ty {
            const NAME: &'static str = $name;
            const TOPICS: &'static [&'static str] = &[$($t),*];
            type Params = $params;
            type Output = $out;
        }
    };
    ($ty:ident, $name:literal, $params:ty => $out:ty) => {
        $crate::method!($ty, $name, $params => $out, []);
    };
}

pub fn to_value<T: Serialize>(v: &T) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_json_shape_is_stable() {
        let m = Message::Res { id: 7, body: ResBody::Err(RpcError::not_found("없음")) };
        let s = serde_json::to_string(&m).unwrap();
        assert!(s.contains("\"t\":\"res\""), "{s}");
        assert!(s.contains("\"err\""), "{s}");
        let back: Message = serde_json::from_str(&s).unwrap();
        assert_eq!(back, m);
        let ok = Message::Res { id: 1, body: ResBody::Ok(serde_json::json!([1, 2])) };
        let back: Message = serde_json::from_str(&serde_json::to_string(&ok).unwrap()).unwrap();
        assert_eq!(back, ok);
    }
}
