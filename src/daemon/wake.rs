//! Every 30 s, starts the user's posts that wait in the queue of a live
//! Codex thread on this laptop (REQ-015), so a kickoff is not left behind an
//! interrupted turn. When a turn keeps running, a post that has waited two
//! minutes is handed to that turn as the user's own mid-turn input and taken
//! out of Codex's queue, once. Only the user's own posts, each by its queued
//! id: an agent's message never overrides the user's interrupt, and other
//! queued input is never started. Off the event loop; never for a paired
//! laptop's session; Codex refuses unloaded threads and review turns.
use super::{Control, Event, now};
use crate::{
    codex_queue::QueuedItem,
    codex_wake::{self, Answer, Steer},
    domain::Provider,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::Duration,
};
use uuid::Uuid;

const SWEEP: Duration = Duration::from_secs(30);
/// All queue reads of one sweep together, and how many run at once.
const READS: Duration = Duration::from_secs(5);
const READERS: usize = 4;

impl Control {
    pub(super) async fn wake_codex_queues(self) {
        let mut interval = tokio::time::interval(SWEEP);
        // Queued ids already handed to a running turn: never handed twice.
        let mut steered = BTreeSet::new();
        interval.tick().await;
        loop {
            interval.tick().await;
            if let Err(e) = self.sweep_codex_queues(&mut steered).await {
                eprintln!("Codex queue check failed: {e:#}");
            }
        }
    }

    async fn sweep_codex_queues(&self, steered: &mut BTreeSet<String>) -> anyhow::Result<()> {
        let posts = self.ask(Event::QueuedUserPosts).await?;
        let live: Vec<Uuid> = self
            .live
            .borrow()
            .iter()
            .filter(|s| s.host.is_none() && s.agent == Provider::Codex)
            .filter_map(|s| crate::domain::SessionId::parse(&s.session_id).ok())
            .map(|id| id.id)
            .collect();
        let mut ours: BTreeMap<Uuid, Vec<(Uuid, u64)>> = BTreeMap::new();
        for (thread, message, at) in posts.into_iter().filter(|(t, _, _)| live.contains(t)) {
            ours.entry(thread).or_default().push((message, at));
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
        let mut found = Vec::new();
        loop {
            match tokio::time::timeout_at(deadline, reads.join_next()).await {
                Ok(Some(Ok((thread, messages, Ok(queued))))) => {
                    if let Some((item, at)) = first_of_ours(&queued, &messages) {
                        found.push((thread, item.clone(), at));
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
        let socket = codex_wake::control_socket(&self.codex_home);
        let at_now = now()?;
        for (thread, item, at) in found {
            match codex_wake::start(&socket, &thread, Some(&item.id)).await {
                Ok(Answer::Ok) => {
                    eprintln!("Started the user's queued post in Codex thread {thread}")
                }
                Ok(Answer::Refused(why))
                    if codex_wake::steer_due(
                        &why,
                        at_now.saturating_sub(at),
                        steered.contains(&item.id),
                    ) =>
                {
                    match codex_wake::steer(&socket, &thread, &item.id, &item.text).await {
                        Ok(Steer::Steered { deleted }) => {
                            steered.insert(item.id.clone());
                            eprintln!(
                                "Handed the user's post to the running turn of Codex thread {thread}"
                            );
                            if let Err(why) = deleted {
                                eprintln!(
                                    "Codex thread {thread} keeps the handed-over post queued: {why}"
                                );
                            }
                        }
                        Ok(Steer::NoActiveTurn) => {
                            eprintln!(
                                "Codex thread {thread} has no running turn to hand the post to"
                            );
                        }
                        Ok(Steer::Refused(why)) => {
                            eprintln!(
                                "Codex thread {thread} refused the post in its running turn: {why}"
                            );
                        }
                        Err(e) => eprintln!("Cannot hand the post to Codex thread {thread}: {e:#}"),
                    }
                }
                Ok(Answer::Refused(why)) => {
                    eprintln!("Codex thread {thread} keeps its queue: {why}")
                }
                Err(e) => eprintln!("Cannot ask Codex to start thread {thread}: {e:#}"),
            }
        }
        Ok(())
    }
}

/// The oldest of the user's posts still in a thread's queue and when the
/// bridge took it, if any; never another item.
fn first_of_ours<'a>(
    queued: &'a [QueuedItem],
    ours: &[(Uuid, u64)],
) -> Option<(&'a QueuedItem, u64)> {
    queued.iter().find_map(|item| {
        let id = item.message_id?;
        ours.iter()
            .find(|(m, _)| *m == id)
            .map(|(_, at)| (item, *at))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /* REQ-015: The sweep picks the oldest of the user's own posts in a
    thread's queue, with its queued id and when the bridge took it, also
    behind other queued input; never another item, and nothing when none of
    ours waits. */
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
        let (found, at) = first_of_ours(&queue, &[(ours, 77)]).unwrap();
        assert_eq!((found.id.as_str(), at), ("kick", 77));
        assert!(first_of_ours(&queue[..2], &[(ours, 77)]).is_none());
        assert!(first_of_ours(&[], &[(ours, 77)]).is_none());
    }
}
