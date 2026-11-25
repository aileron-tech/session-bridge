//! LAN discovery, pairing and forwarding between the user's own bridges (REQ-017).
pub mod channel;
pub mod mdns;
pub mod model;
pub mod notify;
pub mod pake;
pub mod service;
pub mod state;
pub mod wire;

#[cfg(test)]
#[path = "lan/crypto_tests.rs"]
mod crypto_tests;
