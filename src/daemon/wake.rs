//! Every 30 s, starts the user's posts that wait in the queue of a live
//! Codex thread on this laptop (REQ-015), so a kickoff is not left behind an
//! interrupted turn. When a turn keeps running, a post that has waited two
//! minutes is handed to that turn as the user's own mid-turn input and taken
//! out of Codex's queue, once. Only the user's own posts, each by its queued
//! id: an agent's message never overrides the user's interrupt, and other
//! queued input is never started. Older copies of a scheduled post still
//! queued behind a newer copy of the same post are removed from Codex's
//! queue first, so waiting kickoffs never flood a session. Off the event
//! loop; never for a paired laptop's session; Codex refuses unloaded threads
//! and review turns.
use super::{Control, Event, now};
use crate::{
    codex_queue::QueuedItem,
    codex_wake::{self, Answer, Steer},
    domain::Provider,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::Duration,
};
use uuid::Uuid;

const SWEEP: Duration = Duration::from_secs(30);
/// Why the bridge removed an older queued copy of a scheduled post.
const SUPERSEDED: &str = "superseded by the newer post of the same schedule";

/// A bridge message that Codex queued and has not run yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct QueuedPost {
    pub thread: Uuid,
    pub message_id: Uuid,
    pub created_at: u64,
    /// Sent by the user (admin page, channel post, schedule), not an agent.
    pub from_user: bool,
    pub scheduled: bool,
    pub text: String,
}

/// One thread's queued bridge messages: the user's own (id, created at),
/// which may be started, and all of them, which are watched for delivery.
type Watched = (Vec<(Uuid, u64)>, Vec<Uuid>);

/// The bridge messages (`posts`, one thread's) that were seen in Codex's
/// queue before (`seen`) and are gone now: Codex took them into the session
/// (REQ-032). A message never seen in the queue is not counted: absence alone
/// proves nothing (no queue yet, removed elsewhere). Updates `seen`.
pub(super) fn taken(
    posts: &[Uuid],
    seen: &mut BTreeSet<Uuid>,
    queued: &[crate::codex_queue::QueuedItem],
) -> Vec<Uuid> {
    let present = |id: &Uuid| queued.iter().any(|q| q.message_id == Some(*id));
    let gone: Vec<Uuid> = posts
        .iter()
        .filter(|id| seen.contains(id) && !present(id))
        .copied()
        .collect();
    for id in &gone {
        seen.remove(id);
    }
    seen.extend(posts.iter().filter(|id| present(id)).copied());
    gone
}

/// Scheduled posts of the user superseded by a newer post of the same
/// schedule (same thread, same text) still queued: only the newest stays.
/// Posts the user typed, and agents' posts, are never collapsed.
fn superseded(posts: &[QueuedPost]) -> BTreeSet<Uuid> {
    let mut newest: BTreeMap<(Uuid, &str), (u64, Uuid)> = BTreeMap::new();
    for post in posts.iter().filter(|p| p.from_user && p.scheduled) {
        let key = (post.thread, post.text.as_str());
        let candidate = (post.created_at, post.message_id);
        if newest.get(&key).is_none_or(|kept| candidate > *kept) {
            newest.insert(key, candidate);
        }
    }
    posts
        .iter()
        .filter(|p| p.from_user && p.scheduled)
        .filter(|p| {
            newest.get(&(p.thread, p.text.as_str())).map(|(_, id)| *id) != Some(p.message_id)
        })
        .map(|p| p.message_id)
        .collect()
}
/// All queue reads of one sweep together, and how many run at once.
const READS: Duration = Duration::from_secs(5);
const READERS: usize = 4;

/// A queue item handed to a running turn. `Tried`: the hand-over was sent
/// and its outcome is unknown, so nothing more happens to it (never sent
/// again, never removed). `Accepted`: the turn took it; only its removal from
/// Codex's queue is retried until it is gone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Handed {
    Tried,
    Accepted,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct HandedOver {
    pub thread: Uuid,
    pub state: Handed,
}
/// Handed-over items by queued id, kept in `steered.json`.
type Steered = BTreeMap<String, HandedOver>;

impl Control {
    pub(super) async fn wake_codex_queues(self) {
        // Bridge messages seen in a Codex queue, to tell when Codex took them.
        let mut seen = BTreeSet::new();
        let mut interval = tokio::time::interval(SWEEP);
        let mut steered: Steered = if self.steered_file.exists() {
            match crate::store::read_json(&self.steered_file) {
                Ok(saved) => saved,
                Err(e) => {
                    eprintln!("Ignoring unreadable {}: {e:#}", self.steered_file.display());
                    Steered::new()
                }
            }
        } else {
            Steered::new()
        };
        interval.tick().await;
        loop {
            interval.tick().await;
            if let Err(e) = self.sweep_codex_queues(&mut steered, &mut seen).await {
                eprintln!("Codex queue check failed: {e:#}");
            }
        }
    }

    fn save_steered(&self, steered: &Steered) {
        if let Err(e) = crate::store::write_json(&self.steered_file, steered) {
            eprintln!("Cannot save {}: {e:#}", self.steered_file.display());
        }
    }

    async fn sweep_codex_queues(
        &self,
        steered: &mut Steered,
        seen: &mut BTreeSet<Uuid>,
    ) -> anyhow::Result<()> {
        let posts = self.ask(Event::QueuedCodexPosts).await?;
        let stale = superseded(&posts);
        let live: Vec<Uuid> = self
            .live
            .borrow()
            .iter()
            .filter(|s| s.host.is_none() && s.agent == Provider::Codex)
            .filter_map(|s| crate::domain::SessionId::parse(&s.session_id).ok())
            .map(|id| id.id)
            .collect();
        // Every queued bridge message of a live thread is watched (to mark it
        // delivered once Codex takes it); the user's own may also be started.
        let mut watched: BTreeMap<Uuid, Watched> = BTreeMap::new();
        for post in posts.iter().filter(|p| live.contains(&p.thread)) {
            let entry = watched.entry(post.thread).or_default();
            entry.1.push(post.message_id);
            if post.from_user {
                entry.0.push((post.message_id, post.created_at));
            }
        }
        let readers = Arc::new(tokio::sync::Semaphore::new(READERS));
        let mut reads = tokio::task::JoinSet::new();
        for (thread, (messages, all)) in watched {
            let (home, readers) = (self.codex_home.clone(), readers.clone());
            reads.spawn(async move {
                let _turn = readers.acquire_owned().await;
                let queued = crate::codex_queue::queue(&home, &thread).await;
                (thread, (messages, all), queued)
            });
        }
        let deadline = tokio::time::Instant::now() + READS;
        let (mut found, mut removals, mut outcomes) = (Vec::new(), Vec::new(), Vec::new());
        let before = steered.clone();
        loop {
            match tokio::time::timeout_at(deadline, reads.join_next()).await {
                // Without a queue database nothing can be told about this thread.
                Ok(Some(Ok((_, _, Ok(None))))) => {}
                Ok(Some(Ok((thread, (messages, all), Ok(Some(queued)))))) => {
                    outcomes.extend(
                        taken(&all, seen, &queued)
                            .into_iter()
                            .map(|id| (id, crate::domain::Delivery::Delivered)),
                    );
                    // Items Codex no longer holds need no record.
                    steered
                        .retain(|id, h| h.thread != thread || queued.iter().any(|q| q.id == *id));
                    let mut remove = Vec::new();
                    let mut current = Vec::new();
                    for item in queued {
                        if item.message_id.is_some_and(|id| stale.contains(&id)) {
                            remove.push((item, SUPERSEDED));
                        } else if let Some(handed) = steered.get(&item.id) {
                            if handed.state == Handed::Accepted {
                                remove.push((item, "already handed to the running turn"));
                            }
                        } else {
                            current.push(item);
                        }
                    }
                    if !remove.is_empty() {
                        removals.push((thread, remove));
                    }
                    if let Some((item, at)) = first_of_ours(&current, &messages) {
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
        for (thread, items) in removals {
            let ids: Vec<String> = items.iter().map(|(item, _)| item.id.clone()).collect();
            match codex_wake::delete(&socket, &thread, &ids).await {
                Ok(answers) => {
                    for ((item, why), answer) in items.iter().zip(answers) {
                        match answer {
                            Ok(()) => {
                                steered.remove(&item.id);
                                eprintln!(
                                    "Removed queued post {} from Codex thread {thread}: {why}",
                                    item.id
                                );
                                // A dropped copy was never delivered; a copy the
                                // turn already took was.
                                if let Some(id) = item.message_id {
                                    seen.remove(&id);
                                    outcomes.push((
                                        id,
                                        if *why == SUPERSEDED {
                                            crate::domain::Delivery::Refused {
                                                reason: SUPERSEDED.into(),
                                            }
                                        } else {
                                            crate::domain::Delivery::Delivered
                                        },
                                    ));
                                }
                            }
                            Err(e) => eprintln!(
                                "Codex thread {thread} keeps queued post {}: {e}",
                                item.id
                            ),
                        }
                    }
                }
                Err(e) => eprintln!("Cannot remove queued posts from Codex thread {thread}: {e:#}"),
            }
        }
        if !outcomes.is_empty()
            && self
                .events
                .send(Event::CodexOutcomes(outcomes))
                .await
                .is_err()
        {
            eprintln!("Codex queue check stopped before recording outcomes");
        }
        if *steered != before {
            self.save_steered(steered);
        }
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
                        steered.contains_key(&item.id),
                    ) =>
                {
                    // Recorded before it is sent: an uncertain hand-over is never sent again.
                    steered.insert(
                        item.id.clone(),
                        HandedOver {
                            thread,
                            state: Handed::Tried,
                        },
                    );
                    self.save_steered(steered);
                    match codex_wake::steer(&socket, &thread, &item.id, &item.text).await {
                        Ok(Steer::Steered { deleted }) => {
                            eprintln!(
                                "Handed the user's post to the running turn of Codex thread {thread}"
                            );
                            match deleted {
                                Ok(()) => {
                                    steered.remove(&item.id);
                                }
                                Err(why) => {
                                    eprintln!(
                                        "Codex thread {thread} keeps the handed-over post queued; removing it again later: {why}"
                                    );
                                    steered.insert(
                                        item.id.clone(),
                                        HandedOver {
                                            thread,
                                            state: Handed::Accepted,
                                        },
                                    );
                                }
                            }
                        }
                        Ok(Steer::NoActiveTurn) => {
                            // Nothing was handed over: the post may be tried again.
                            steered.remove(&item.id);
                            eprintln!(
                                "Codex thread {thread} has no running turn to hand the post to"
                            );
                        }
                        Ok(Steer::Refused(why)) => {
                            steered.remove(&item.id);
                            eprintln!(
                                "Codex thread {thread} refused the post in its running turn: {why}"
                            );
                        }
                        Err(e) => eprintln!(
                            "Handing the post to Codex thread {thread} ended uncertain; it is not sent again: {e:#}"
                        ),
                    }
                    self.save_steered(steered);
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

    /* REQ-015: Of the user's scheduled posts with the same text in one
    thread, only the newest stays queued; posts the user typed, agents'
    posts and other threads are never collapsed. */
    /* REQ-032: A queued bridge message Codex no longer holds was taken into
    the session; one still in the queue, or Codex's own items, are not. */
    #[test]
    fn finds_messages_codex_took() {
        let (a, b, c) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let item = |id: &str, message: Option<Uuid>| crate::codex_queue::QueuedItem {
            id: id.into(),
            created_at_ms: 0,
            text: String::new(),
            message_id: message,
        };
        let mut seen = BTreeSet::new();
        // First look: a and b are queued, c never was: nothing is taken.
        let first = [item("q1", Some(a)), item("q2", Some(b)), item("q3", None)];
        assert!(taken(&[a, b, c], &mut seen, &first).is_empty());
        // Later a is gone: taken. c, never seen, is not.
        let later = [item("q2", Some(b))];
        assert_eq!(taken(&[a, b, c], &mut seen, &later), [a]);
        assert!(taken(&[b, c], &mut seen, &later).is_empty());
        assert_eq!(taken(&[b], &mut seen, &[]), [b]);
    }

    #[test]
    fn collapses_older_copies_of_a_scheduled_post() {
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let post = |thread, at, from_user, scheduled, text: &str| QueuedPost {
            thread,
            message_id: Uuid::new_v4(),
            created_at: at,
            from_user,
            scheduled,
            text: text.into(),
        };
        let posts = vec![
            post(a, 10, true, true, "Kick off"),
            post(a, 30, true, true, "Kick off"),
            post(a, 20, true, true, "Kick off"),
            post(a, 15, true, true, "Daily status"),
            post(a, 5, true, false, "Kick off"),
            post(a, 6, true, false, "Kick off"),
            post(a, 1, false, true, "Kick off"),
            post(a, 2, false, true, "Kick off"),
            post(b, 9, true, true, "Kick off"),
        ];
        let gone = superseded(&posts);
        let expected: BTreeSet<Uuid> = [posts[0].message_id, posts[2].message_id].into();
        assert_eq!(gone, expected);
        assert!(superseded(&[]).is_empty());
    }

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
