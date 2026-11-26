//! Messages between paired bridges, sent over [`super::channel::Channel`].
//! Pairing is a lockstep exchange on one connection; the other requests use
//! one connection each.
use crate::{
    discovery::Session,
    domain::{Delivery, Provider, SessionId},
    usage::HostUsage,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case")]
pub enum PeerRequest {
    /// Ask the other bridge to show a one-time code. `port` is where this bridge listens.
    PairRequest {
        name: String,
        port: u16,
    },
    /// SPAKE2 message, base64.
    Pair {
        spake: String,
    },
    /// Key confirmation, base64.
    PairConfirm {
        mac: String,
    },
    List {
        project: Option<String>,
    },
    Forward {
        message: ForwardedMessage,
    },
    Status {
        message_id: Uuid,
        delivery: Delivery,
    },
    /// This bridge's host usage (REQ-029). A bridge that predates it cannot
    /// read it and closes the connection, which reads as usage unavailable.
    Usage,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum PeerReply {
    CodeShown { expires_in: u64 },
    Pair { spake: String },
    WrongCode { attempts_left: u8 },
    Paired { name: String, mac: String },
    Sessions { sessions: Vec<Session> },
    Accepted { delivery: Delivery },
    Usage { usage: HostUsage },
    Ok,
    Error { message: String },
}

/// The sending session as the receiving bridge renders it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteSender {
    pub session_id: SessionId,
    pub agent: Provider,
    pub project: String,
    pub cwd: PathBuf,
    pub name: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForwardedMessage {
    pub message_id: Uuid,
    pub sender: RemoteSender,
    pub recipient: SessionId,
    pub message: String,
    pub project: Option<String>,
    #[serde(default)]
    pub scheduled: bool,
}
