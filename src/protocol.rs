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
    /// The project channel the message belongs to (REQ-020).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    /// Posted by one of the user's schedules (REQ-025).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub scheduled: bool,
}

/// A remote host and its tunnel: "connected", "connecting", "down: <why>",
/// or "networking off" (REQ-027).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteView {
    pub host: String,
    pub remote_port: u16,
    pub local_port: u16,
    pub back_port: u16,
    pub state: String,
}

/// A schedule and whether the crontab runs it (REQ-025).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduleView {
    #[serde(flatten)]
    pub schedule: crate::schedule::Schedule,
    pub installed: bool,
}

/// Settings of a project channel (REQ-024).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelSettings {
    /// Session name that receives posts without a mention.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_recipient: Option<String>,
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
        /// Listen on 127.0.0.1 only (for SSH tunnels); left out keeps the saved choice.
        #[serde(default)]
        local_only: Option<bool>,
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
    /// With `channel`, it is posted in that project's channel (REQ-020).
    UserSend {
        session_id: String,
        message: String,
        #[serde(default)]
        channel: Option<String>,
        #[serde(default)]
        scheduled: bool,
    },
    /// Stop this bridge (control socket only; REQ-022).
    Shutdown,
    /// Settings of every channel that has any.
    Channels,
    /// Scheduled posts and the user's other crontab lines (REQ-025).
    Schedules,
    /// Add (no id) or change a schedule; admin page only.
    SaveSchedule {
        #[serde(default)]
        id: Option<uuid::Uuid>,
        cron: String,
        project: String,
        #[serde(default)]
        to: Option<String>,
        message: String,
    },
    /// Remove a schedule and its crontab line; admin page only.
    DeleteSchedule {
        id: uuid::Uuid,
    },
    /// Post a schedule's message now (cron runs this).
    RunSchedule {
        id: uuid::Uuid,
    },
    /// Roles and which session holds which (REQ-026).
    Roles,
    /// Remote hosts reached over SSH and their tunnels (REQ-027).
    Remotes,
    /// Reach the bridge on `host` (an SSH destination) through an SSH tunnel;
    /// `port` is its port on that host's loopback (default 47820).
    AddRemote {
        host: String,
        #[serde(default)]
        port: Option<u16>,
    },
    RemoveRemote {
        host: String,
    },
    /// Add a role, or change `previous`; admin page only.
    SaveRole {
        name: String,
        prompt: String,
        #[serde(default)]
        previous: Option<String>,
    },
    /// Remove a role and its assignments; admin page only.
    DeleteRole {
        name: String,
    },
    /// Give a session a role (its prompt is sent once) or none; admin page only.
    AssignRole {
        session_id: String,
        #[serde(default)]
        role: Option<String>,
    },
    /// Set or clear (`name: null`) a channel's default recipient; admin page only.
    ChannelDefault {
        project: String,
        #[serde(default)]
        name: Option<String>,
    },
    /// A post in a project's channel to the sessions it mentions
    /// (`@channel`, `@name`); accepted from the admin page only.
    ChannelSend {
        project: String,
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
    ShuttingDown {
        pid: u32,
    },
    Channels {
        channels: std::collections::BTreeMap<String, ChannelSettings>,
    },
    Remotes {
        remotes: Vec<RemoteView>,
    },
    Roles {
        roles: Vec<crate::roles::Role>,
        /// Session name to role name.
        assignments: std::collections::BTreeMap<String, String>,
    },
    Schedules {
        schedules: Vec<ScheduleView>,
        /// The user's own crontab lines, shown read-only.
        other_lines: Vec<String>,
        /// False where there is no crontab (Windows).
        available: bool,
        /// Why the crontab could not be read; the schedules are still listed.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        problem: Option<String>,
    },
    /// A channel post: one receipt per reached session, and who could not be reached.
    Posted {
        project: String,
        receipts: Vec<Receipt>,
        failed: Vec<String>,
    },
    Error {
        message: String,
    },
}
