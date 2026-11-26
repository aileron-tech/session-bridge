//! Every 30 s, starts the queue of each live Codex thread on this laptop
//! where a bridge message still waits (REQ-015), so a post is not left
//! behind an interrupted turn. Off the event loop; never for a paired
//! laptop's session; Codex refuses unloaded and busy threads.
use super::{Control, Event};
use crate::{
    domain::{Delivery, Provider},
    protocol::{Reply, Request},
};
use std::{collections::BTreeMap, time::Duration};
use uuid::Uuid;

const SWEEP: Duration = Duration::from_secs(30);

impl Control {
    pub(super) async fn wake_codex_queues(self) {
        let mut interval = tokio::time::interval(SWEEP);
        interval.tick().await;
        loop {
            interval.tick().await;
            if let Err(e) = self.sweep_codex_queues().await {
                eprintln!("Codex queue check failed: {e:#}");
            }
        }
    }

    async fn sweep_codex_queues(&self) -> anyhow::Result<()> {
        let Reply::Messages { messages, .. } = self
            .ask(|reply| Event::Rpc(Request::Messages, reply))
            .await?
        else {
            anyhow::bail!("Could not read messages");
        };
        let live: Vec<String> = self
            .live
            .borrow()
            .iter()
            .filter(|s| s.host.is_none() && s.agent == Provider::Codex)
            .map(|s| s.session_id.clone())
            .collect();
        // Our queued messages, per live local Codex thread.
        let mut ours: BTreeMap<Uuid, Vec<Uuid>> = BTreeMap::new();
        for m in messages.iter().filter(|m| {
            m.delivery == Delivery::Queued
                && m.recipient.provider == Provider::Codex
                && live.contains(&m.recipient.to_string())
        }) {
            ours.entry(m.recipient.id).or_default().push(m.message_id);
        }
        for (thread, ids) in ours {
            let waiting = crate::codex_queue::waiting(&self.codex_home, &thread).await?;
            if holds_ours(&waiting, &ids) {
                crate::codex_sessions::wake(&self.codex_home, &thread, None).await;
            }
        }
        Ok(())
    }
}

/// Whether a thread's queue still holds one of the bridge's messages: then
/// its head is started, whoever queued it (it runs first either way).
fn holds_ours(waiting: &[crate::codex_queue::QueuedItem], ours: &[Uuid]) -> bool {
    waiting
        .iter()
        .any(|item| item.message_id.is_some_and(|id| ours.contains(&id)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codex_queue::QueuedItem;

    /* REQ-015: A thread is woken while any bridge message of ours waits in
    its queue, also behind the user's own queued input; not for a queue of
    others' messages only, nor an empty one. */
    #[test]
    fn wakes_threads_holding_our_messages() {
        let (ours, other) = (Uuid::new_v4(), Uuid::new_v4());
        let item = |id: Option<Uuid>| QueuedItem {
            created_at_ms: 1,
            text: String::new(),
            message_id: id,
        };
        assert!(holds_ours(&[item(None), item(Some(ours))], &[ours]));
        assert!(!holds_ours(&[item(None), item(Some(other))], &[ours]));
        assert!(!holds_ours(&[], &[ours]));
    }
}
