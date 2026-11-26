use crate::{discovery::Session, store::Envelope};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Receipt {
    pub message_id: uuid::Uuid,
    pub sender: crate::domain::SessionId,
    pub recipient: crate::domain::SessionId,
    pub delivery: crate::domain::Delivery,
    pub created_at: u64,
}
impl From<&Envelope> for Receipt {
    fn from(e: &Envelope) -> Self {
        Self {
            message_id: e.message_id,
            sender: e.sender.clone(),
            recipient: e.recipient.clone(),
            delivery: e.delivery.clone(),
            created_at: e.created_at,
        }
    }
}

/// A message with its text and readable parties, for the admin page (REQ-019).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageView {
    pub message_id: uuid::Uuid,
    pub sender: crate::domain::SessionId,
    pub sender_label: String,
    pub recipient: crate::domain::SessionId,
    pub recipient_label: String,
    pub message: String,
    pub delivery: crate::domain::Delivery,
    pub created_at: u64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case")]
pub enum Request {
    Health,
    List {
        sender: Option<String>,
        project: Option<String>,
    },
    Send {
        sender: String,
        session_id: String,
        message: String,
        project: Option<String>,
    },
    Status,
    NativePeer {
        recipient: String,
        frame: Box<crate::transport::PeerFrame>,
    },
    /// Switch networking; a port or name left out keeps the saved one.
    Network {
        enabled: bool,
        #[serde(default)]
        port: Option<u16>,
        #[serde(default)]
        name: Option<String>,
    },
    /// This bridge's network state, other bridges, and pairing.
    Lan,
    /// Ask a discovered bridge (name, key prefix or ip:port) to show a code.
    Pair {
        host: String,
    },
    /// Try the code shown on the other laptop.
    PairCode {
        id: uuid::Uuid,
        code: String,
    },
    Unpair {
        host: String,
    },
    /// The address of the local setup page.
    Admin,
    /// Recent messages with their text, newest first.
    Messages,
    /// A message from the user to a session; accepted from the admin page only.
    UserSend {
        session_id: String,
        message: String,
    },
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum Reply {
    Health {
        pid: u32,
    },
    Sessions {
        sessions: Vec<Session>,
        recent_messages: Vec<Receipt>,
        /// Paired bridges that did not answer the listing.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        unavailable_hosts: Vec<String>,
    },
    Receipt {
        receipt: Receipt,
    },
    Status {
        pid: u32,
        sessions: usize,
        messages: Vec<Receipt>,
    },
    Lan {
        lan: crate::lan::model::LanView,
    },
    Pairing {
        id: uuid::Uuid,
        host: String,
        expires_in: u64,
    },
    Paired {
        host: String,
        key: String,
    },
    WrongCode {
        attempts_left: u8,
    },
    Unpaired {
        host: String,
    },
    Admin {
        url: String,
    },
    Messages {
        /// This laptop's user address: messages to it are replies to the user.
        user: crate::domain::SessionId,
        messages: Vec<MessageView>,
    },
    Error {
        message: String,
    },
}
