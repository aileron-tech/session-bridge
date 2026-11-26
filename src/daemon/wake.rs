//! Every 30 s, starts the user's posts that wait in the queue of a live
//! Codex thread on this laptop (REQ-015), so a kickoff is not left behind an
//! interrupted turn. Only the user's own posts, each by its queued id: an
//! agent's message never overrides the user's interrupt, and other queued
//! input is never started. Off the event loop; never for a paired laptop's
//! session; Codex refuses unloaded and busy threads.
use super::{Control, Event};
use crate::{codex_queue::QueuedItem, domain::Provider};
use std::{collections::BTreeMap, sync::Arc, time::Duration};
use uuid::Uuid;

const SWEEP: Duration = Duration::from_secs(30);
/// All queue reads of one sweep together, and how many run at once.
const READS: Duration = Duration::from_secs(5);
const READERS: usize = 4;

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
        let posts = self.ask(Event::QueuedUserPosts).await?;
        let live: Vec<Uuid> = self
            .live
            .borrow()
            .iter()
            .filter(|s| s.host.is_none() && s.agent == Provider::Codex)
            .filter_map(|s| crate::domain::SessionId::parse(&s.session_id).ok())
            .map(|id| id.id)
            .collect();
        let mut ours: BTreeMap<Uuid, Vec<Uuid>> = BTreeMap::new();
        for (thread, message) in posts.into_iter().filter(|(t, _)| live.contains(t)) {
            ours.entry(thread).or_default().push(message);
        }
        let readers = Arc::new(tokio::sync::Semaphore::new(READERS));
        let mut reads = tokio::task::JoinSet::new();
        for (thread, messages) in ours {
            let (home, readers) = (self.codex_home.clone(), readers.clone());
            reads.spawn(async move {
                let _turn = readers.acquire_owned().await;
                let queued = crate::codex_queue::waiting(&home, &thread).await;
                (thread, messages, queued)
            });
        }
        let deadline = tokio::time::Instant::now() + READS;
        loop {
            match tokio::time::timeout_at(deadline, reads.join_next()).await {
                Ok(Some(Ok((thread, messages, Ok(queued))))) => {
                    if let Some(item) = first_of_ours(&queued, &messages) {
                        crate::codex_sessions::start_queued(&self.codex_home, &thread, &item).await;
                    }
                }
                Ok(Some(Ok((thread, _, Err(e))))) => {
                    eprintln!("Could not read Codex's queue for {thread}: {e:#}");
                }
                Ok(Some(Err(e))) => eprintln!("Codex queue read failed: {e}"),
                Ok(None) => break,
                Err(_) => {
                    eprintln!("Some Codex queue reads took longer than {READS:?}");
                    break;
                }
            }
        }
        Ok(())
    }
}

/// The queued id of the oldest of the user's posts still in a thread's
/// queue, if any; never another item.
fn first_of_ours(queued: &[QueuedItem], ours: &[Uuid]) -> Option<String> {
    queued
        .iter()
        .find(|item| item.message_id.is_some_and(|id| ours.contains(&id)))
        .map(|item| item.id.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    /* REQ-015: The sweep starts the oldest of the user's own posts in a
    thread's queue, by its queued id, also behind other queued input; never
    another item, and nothing when none of ours waits. */
    #[test]
    fn starts_only_the_users_own_post() {
        let (ours, other) = (Uuid::new_v4(), Uuid::new_v4());
        let item = |id: &str, message: Option<Uuid>| QueuedItem {
            id: id.into(),
            created_at_ms: 1,
            text: String::new(),
            message_id: message,
        };
        let queue = [
            item("typed", None),
            item("peer", Some(other)),
            item("kick", Some(ours)),
        ];
        assert_eq!(first_of_ours(&queue, &[ours]).as_deref(), Some("kick"));
        assert_eq!(first_of_ours(&queue[..2], &[ours]), None);
        assert_eq!(first_of_ours(&[], &[ours]), None);
    }
}
