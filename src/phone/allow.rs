//! Which control requests a paired phone may make (REQ-036). The match has
//! no catch-all arm, so a new request does not compile until its phone
//! policy is decided here.
use crate::protocol::Request;

/// Sessions and messages, chat and channel posts as the user, schedules and
/// roles. Never: agent traffic, networking and pairing, remote hosts,
/// starting or closing sessions, run settings and approvals, or phones.
pub fn phone_allowed(request: &Request) -> bool {
    match request {
        Request::List { sender, .. } => sender.is_none(),
        // The phone writes as the user, never as a schedule.
        Request::UserSend { scheduled, .. } => !scheduled,
        Request::Messages
        | Request::ChannelSend { .. }
        | Request::Channels
        | Request::Projects
        | Request::Waiting
        | Request::Schedules
        | Request::SaveSchedule { .. }
        | Request::DeleteSchedule { .. }
        | Request::Roles
        | Request::SaveRole { .. }
        | Request::DeleteRole { .. }
        | Request::AssignRole { .. } => true,
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
        | Request::PhoneUnpair { .. } => false,
    }
}

/// The phone's view of a reply: the user's other crontab lines stay on the
/// laptop.
pub fn phone_reply(reply: crate::protocol::Reply) -> crate::protocol::Reply {
    match reply {
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
        other => other,
    }
}
