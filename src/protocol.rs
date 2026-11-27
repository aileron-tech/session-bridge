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
    /// What its last run did (REQ-025).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run: Option<LastRunView>,
}

/// A schedule's last run: when, and one line on what it did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LastRunView {
    pub at: u64,
    pub summary: String,
}

/// Settings of a project channel (REQ-024).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelSettings {
    /// Session name that receives posts without a mention.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_recipient: Option<String>,
    /// The project's master chosen by the user (REQ-030), by session name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub master: Option<String>,
    /// That master's session: only this session receives decisions (REQ-034).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub master_session: Option<String>,
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
        /// Attach the team table of the session's project (REQ-031).
        #[serde(default)]
        team_report: bool,
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
        /// Attach the team table to each post (REQ-031).
        #[serde(default)]
        team_report: bool,
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
        /// The version of the role's file the editor started from
        /// (REQ-037): the save is refused if the file changed since.
        #[serde(default)]
        version: Option<String>,
    },
    /// Show the roles folder in the file manager; admin page only.
    OpenRolesFolder,
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
        /// Attach the project's team table (REQ-031).
        #[serde(default)]
        team_report: bool,
    },
    /// Start a session with a role in a terminal window (REQ-028). Without
    /// `sender` it is the user's (admin page, CLI); a session (MCP) must hold
    /// root, and starts in its own project. `project` is a project name or
    /// an absolute directory.
    Spawn {
        #[serde(default)]
        sender: Option<String>,
        #[serde(default)]
        project: Option<String>,
        agent: crate::spawn::Agent,
        role: String,
        /// The user's own instructions for the new session, added to its
        /// first prompt (REQ-034); the user only, never a session.
        #[serde(default)]
        note: Option<String>,
    },
    /// End a local session's process; the same rules as `Spawn`.
    Close {
        #[serde(default)]
        sender: Option<String>,
        session_id: String,
    },
    /// A launched terminal claims what it starts (control socket only).
    ClaimSpawn {
        token: uuid::Uuid,
        pid: u32,
    },
    /// Short project names and the terminal for new sessions.
    SpawnSettings,
    /// Fill the run settings from the user's zsh aliases; admin page only.
    ImportAliases,
    /// This laptop's projects and their masters (REQ-030).
    Projects,
    /// What waits for each local session (REQ-032).
    Waiting,
    /// Remove a waiting bridge message (REQ-035); admin page only. With
    /// `queued_id`, it is first taken out of Codex's queue.
    RemoveWaiting {
        session_id: String,
        message_id: uuid::Uuid,
        #[serde(default)]
        queued_id: Option<String>,
    },
    /// Choose (by a live session's name) or clear a project's master; admin page only.
    ProjectMaster {
        project: String,
        #[serde(default)]
        name: Option<String>,
    },
    /// CPU, memory and disk of this laptop and paired bridges, and of each
    /// local session's process (REQ-029).
    Usage,
    /// Set or clear (`short_name: null`) a project's short name, and/or the
    /// terminal; admin page only.
    SaveSpawnSettings {
        #[serde(default)]
        project: Option<String>,
        #[serde(default)]
        short_name: Option<String>,
        #[serde(default)]
        terminal: Option<crate::spawn::Terminal>,
        /// How sessions the user starts run (REQ-034).
        #[serde(default)]
        claude: Option<crate::spawn::ClaudeRun>,
        #[serde(default)]
        codex: Option<crate::spawn::CodexRun>,
    },
    /// Phone access and paired phones (REQ-036); admin page only.
    Phones,
    /// Turn phone access on or off; a port left out keeps the saved one.
    PhoneAccess {
        enabled: bool,
        #[serde(default)]
        port: Option<u16>,
    },
    /// Show a new one-time pairing code; it replaces an open one.
    PhoneOffer,
    /// Accept or decline the phone that used the code `id`.
    PhoneConfirm {
        id: uuid::Uuid,
        accept: bool,
    },
    PhoneUnpair {
        device_id: uuid::Uuid,
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
        /// The roles folder (REQ-037); empty for the phone.
        #[serde(default)]
        folder: String,
        /// Each role's file, by role name.
        #[serde(default)]
        files: std::collections::BTreeMap<String, RoleFile>,
        /// Files in the folder that are not roles, and why.
        #[serde(default)]
        not_loaded: Vec<crate::roles::NotLoaded>,
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
    /// A session is starting in a terminal window under `name`.
    Spawned {
        name: String,
        project: String,
        agent: crate::spawn::Agent,
        role: String,
    },
    /// The session's process was asked to end.
    Closed {
        name: String,
    },
    /// What a launched terminal starts.
    Launch {
        agent: crate::spawn::Agent,
        cwd: std::path::PathBuf,
        name: String,
        prompt: String,
        /// Arguments before the session's own (REQ-034).
        #[serde(default)]
        run_args: Vec<String>,
    },
    SpawnSettings {
        #[serde(flatten)]
        settings: crate::spawn::SpawnSettings,
        /// The command a session the user starts would run, per agent.
        preview: std::collections::BTreeMap<String, String>,
    },
    Projects {
        projects: Vec<crate::projects::ProjectView>,
    },
    /// Session id to what waits for it; sessions with nothing waiting are left out.
    Waiting {
        waiting: std::collections::BTreeMap<String, crate::waiting::WaitingView>,
    },
    /// This laptop first, then paired bridges by name; local sessions only.
    Usage {
        hosts: Vec<crate::usage::HostUsageView>,
        sessions: Vec<crate::usage::SessionUsageView>,
    },
    /// A channel post: one receipt per reached session, and who could not be reached.
    Posted {
        project: String,
        receipts: Vec<Receipt>,
        failed: Vec<String>,
        /// Sessions a scheduled run left out because its previous post to
        /// them is still unread (REQ-025), one line each.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        skipped: Vec<String>,
    },
    Phones {
        phones: crate::phone::view::PhonesView,
    },
    Error {
        message: String,
    },
    /// The request was refused because what it changes changed since it was
    /// read (REQ-037): read again, then retry. HTTP 409 on the admin page
    /// and the phone.
    Conflict {
        message: String,
    },
}

/// A role's file (REQ-037).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoleFile {
    /// Where it is on this laptop; empty for the phone.
    pub path: String,
    /// Its content version as last read; none for the built-in root prompt
    /// used in place of a root.md that is not loaded.
    #[serde(default)]
    pub version: Option<String>,
}
