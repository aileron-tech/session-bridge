//! Which control requests a paired phone may make (REQ-036). The match has
//! no catch-all arm, so a new request does not compile until its phone
//! policy is decided here.
use crate::{protocol::Request, spawn::is_root};

/// Sessions and messages, chat and channel posts as the user, schedules and
/// roles other than root. Never: agent traffic, networking and pairing,
/// remote hosts, starting or closing sessions, run settings and approvals,
/// the root role, or phones.
pub fn phone_allowed(request: &Request) -> bool {
    match request {
        Request::List { sender, .. } => sender.is_none(),
        // The phone writes as the user, never as a schedule.
        Request::UserSend { scheduled, .. } => !scheduled,
        // Root starts and closes sessions (REQ-028): the phone may not give
        // it to a session, nor change or remove its instructions.
        Request::AssignRole { role, .. } => !is_root(role.as_deref()),
        Request::SaveRole { name, previous, .. } => {
            !is_root(Some(name)) && !is_root(previous.as_deref())
        }
        Request::DeleteRole { name } => !is_root(Some(name)),
        Request::Messages
        | Request::ChannelSend { .. }
        | Request::Channels
        | Request::Projects
        | Request::Waiting
        | Request::Schedules
        | Request::SaveSchedule { .. }
        | Request::DeleteSchedule { .. }
        | Request::Roles => true,
        Request::Health
        | Request::Send { .. }
        | Request::Status
        | Request::NativePeer { .. }
        | Request::Network { .. }
        | Request::Lan
        | Request::Pair { .. }
        | Request::PairCode { .. }
        | Request::Unpair { .. }
        | Request::Admin
        | Request::Shutdown
        | Request::RunSchedule { .. }
        | Request::Remotes
        | Request::AddRemote { .. }
        | Request::RemoveRemote { .. }
        | Request::ChannelDefault { .. }
        | Request::Spawn { .. }
        | Request::Close { .. }
        | Request::ClaimSpawn { .. }
        | Request::SpawnSettings
        | Request::SaveSpawnSettings { .. }
        | Request::ImportAliases
        | Request::RemoveWaiting { .. }
        | Request::ProjectMaster { .. }
        | Request::Usage
        | Request::Phones
        | Request::PhoneAccess { .. }
        | Request::PhoneOffer
        | Request::PhoneConfirm { .. }
        | Request::PhoneUnpair { .. }
        | Request::OpenRolesFolder => false,
    }
}

/// The phone's view of a reply: the user's other crontab lines and local
/// paths (session and project directories) stay on the laptop.
pub fn phone_reply(reply: crate::protocol::Reply) -> crate::protocol::Reply {
    match reply {
        crate::protocol::Reply::Sessions {
            sessions,
            recent_messages,
            unavailable_hosts,
        } => crate::protocol::Reply::Sessions {
            sessions: sessions
                .into_iter()
                .map(|session| crate::discovery::Session {
                    cwd: std::path::PathBuf::new(),
                    ..session
                })
                .collect(),
            recent_messages,
            unavailable_hosts,
        },
        crate::protocol::Reply::Projects { projects } => crate::protocol::Reply::Projects {
            projects: projects
                .into_iter()
                .map(|project| crate::projects::ProjectView {
                    dir: std::path::PathBuf::new(),
                    ..project
                })
                .collect(),
        },
        crate::protocol::Reply::Schedules {
            schedules,
            other_lines: _,
            available,
            problem,
        } => crate::protocol::Reply::Schedules {
            schedules,
            other_lines: Vec::new(),
            available,
            problem,
        },
        crate::protocol::Reply::Roles {
            roles,
            assignments,
            folder: _,
            files,
            not_loaded,
        } => crate::protocol::Reply::Roles {
            roles,
            assignments,
            folder: String::new(),
            files: files
                .into_iter()
                .map(|(name, file)| {
                    (
                        name,
                        crate::protocol::RoleFile {
                            path: String::new(),
                            ..file
                        },
                    )
                })
                .collect(),
            not_loaded,
        },
        other => other,
    }
}
