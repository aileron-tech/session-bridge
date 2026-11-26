//! Phone admin app (REQ-036): a web app the bridge serves over HTTPS on the
//! local network while the user has turned "Phone access" on. A phone pairs
//! with a one-time code the admin page shows and the user confirms on the
//! laptop; every call is then authenticated by that phone's pinned key.
//! The pure parts (pairing, allowlist, limits, source filter) live here;
//! the listener, certificate and signatures are infrastructure.
pub mod allow;
pub mod idle;
pub mod limits;
pub mod model;
pub mod net;
pub mod profile;
pub mod replay;
pub mod service;
pub mod view;

#[cfg(not(windows))]
pub mod api;
#[cfg(not(windows))]
pub mod crypto;
#[cfg(not(windows))]
pub mod server;

#[cfg(test)]
mod allow_tests;
#[cfg(test)]
mod model_tests;
#[cfg(test)]
mod pure_tests;
