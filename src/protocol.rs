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
    },
    Receipt {
        receipt: Receipt,
    },
    Status {
        pid: u32,
        sessions: usize,
        messages: Vec<Receipt>,
    },
    Error {
        message: String,
    },
}
