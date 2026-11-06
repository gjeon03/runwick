//! Shared contract between the Runwick host (which owns runs and records) and its clients
//! (the GUI, agent CLIs, and remote bridges).

pub mod api;
pub mod josa;
pub mod launch;
pub mod nfc;
pub mod paths;
pub mod rpc;
pub mod term_guard;
pub mod wire;

pub use rpc::{Message, Method, RpcError};
pub use wire::Frame;

pub type Ts = chrono::DateTime<chrono::Utc>;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn now() -> Ts {
    chrono::Utc::now()
}
