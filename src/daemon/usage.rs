//! Host and session usage for the admin page (REQ-029).
use super::Control;
use crate::{
    protocol::Reply,
    usage::{self, HostState},
};
use anyhow::Result;

impl Control {
    /// This laptop and every paired bridge, and each local session's
    /// process. Sessions come from the last discovery the event loop
    /// published, so a usage poll never runs discovery or waits on messages.
    pub(super) async fn usage(&self) -> Result<Reply> {
        let local: Vec<(String, u32)> = self
            .live
            .borrow()
            .iter()
            .filter(|s| s.host.is_none() && s.pid > 0)
            .map(|s| (s.session_id.clone(), s.pid))
            .collect();
        let (own, remote, table) = tokio::join!(
            self.usage.host(),
            self.lan.usage_remote(),
            self.usage.processes()
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
        match remote {
            Ok(remote) => hosts.extend(
                remote
                    .into_iter()
                    .map(|(host, state)| usage::host_view(host, false, state)),
            ),
            Err(e) => eprintln!("Paired bridges' usage unavailable: {e:#}"),
        }
        let sessions = match table {
            Ok(table) => usage::session_usage(&local, &table),
            Err(e) => {
                eprintln!("Session usage unavailable: {e:#}");
                usage::session_usage(&local, &[])
            }
        };
        Ok(Reply::Usage { hosts, sessions })
    }
}
