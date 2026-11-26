//! Host and session usage for the admin page (REQ-029).
use super::{Control, Event};
use crate::{
    protocol::Reply,
    usage::{self, HostState},
};
use anyhow::Result;

impl Control {
    /// This laptop and every paired bridge, and each local session's process.
    pub(super) async fn usage(&self) -> Result<Reply> {
        let Reply::Sessions { sessions, .. } = self
            .ask(|reply| Event::List {
                sender: None,
                project: None,
                reply,
            })
            .await??
            .0
        else {
            anyhow::bail!("Could not list the sessions");
        };
        let local: Vec<(String, u32)> = sessions
            .iter()
            .filter(|s| s.host.is_none() && s.pid > 0)
            .map(|s| (s.session_id.clone(), s.pid))
            .collect();
        let mut pids: Vec<u32> = local.iter().map(|(_, pid)| *pid).collect();
        pids.sort_unstable();
        pids.dedup();
        let (own, remote, samples) = tokio::join!(
            self.usage.host(),
            self.lan.usage_remote(),
            self.usage.processes(pids)
        );
        let own = match own {
            Ok(usage) => HostState::Available { usage },
            Err(e) => HostState::Unavailable {
                reason: format!("{e:#}"),
            },
        };
        let mut hosts = vec![usage::host_view(
            self.lan.view().await?.network.host,
            true,
            own,
        )];
        hosts.extend(
            remote?
                .into_iter()
                .map(|(host, state)| usage::host_view(host, false, state)),
        );
        Ok(Reply::Usage {
            hosts,
            sessions: usage::session_usage(&local, &samples?),
        })
    }
}
