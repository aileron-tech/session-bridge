pub mod admin;
pub mod client;
pub mod codex_sessions;
pub mod daemon;
pub mod discovery;
pub mod domain;
pub mod install;
pub mod lan;
pub mod mcp;
pub mod mentions;
#[cfg(unix)]
pub mod native_peers;
#[cfg(windows)]
#[path = "native_peers_windows.rs"]
pub mod native_peers;
pub mod platform;
pub mod protocol;
pub mod store;
pub mod transport;
