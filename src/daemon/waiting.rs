//! The waiting messages of this laptop's sessions (REQ-032).
use super::{Control, Event};
use crate::{
    codex_queue,
    domain::{Provider, SessionId},
    protocol::{Reply, Request},
    waiting::{self, Place, WaitingItem},
};
use anyhow::Result;
use std::{collections::BTreeMap, time::Duration};

/// All Codex queue reads of one poll together.
const CODEX_READS: Duration = Duration::from_secs(5);
/// Codex queue reads at once (each is a short sqlite3 process).
const CODEX_READERS: usize = 4;

impl Control {
    /// Per local session with anything waiting: bridge messages not handed
    /// over yet (from the event loop, all of them), and bridge messages still
    /// unread in Codex's queue (read concurrently, under one time limit).
    /// Uses the last discovery; it runs none.
    pub(super) async fn waiting(&self) -> Result<Reply> {
        let sessions = self.last_sessions();
        let Reply::Waiting { waiting: unsent } = self
            .ask(|reply| Event::Rpc(Request::Waiting, reply))
            .await?
        else {
            anyhow::bail!("Could not read the waiting messages");
        };
        let mut items: BTreeMap<String, Vec<WaitingItem>> = unsent
            .into_iter()
            .map(|(id, view)| (id, view.items))
            .collect();
        let readers = std::sync::Arc::new(tokio::sync::Semaphore::new(CODEX_READERS));
        let mut reads = tokio::task::JoinSet::new();
        for session in sessions.iter().filter(|s| s.agent == Provider::Codex) {
            let Ok(id) = SessionId::parse(&session.session_id) else {
                continue;
            };
            let home = self.codex_home.clone();
            let key = session.session_id.clone();
            let readers = readers.clone();
            reads.spawn(async move {
                let _turn = readers.acquire_owned().await;
                (key, codex_queue::waiting(&home, &id.id).await)
            });
        }
        // Reads finished in time are kept; the rest are dropped (aborted).
        let deadline = tokio::time::Instant::now() + CODEX_READS;
        let mut gathered = Vec::new();
        loop {
            match tokio::time::timeout_at(deadline, reads.join_next()).await {
                Ok(Some(Ok(result))) => gathered.push(result),
                Ok(Some(Err(e))) => eprintln!("Codex queue read failed: {e}"),
                Ok(None) => break,
                Err(_) => {
                    eprintln!("Some Codex queue reads took longer than {CODEX_READS:?}");
                    break;
                }
            }
        }
        for (key, result) in gathered {
            match result {
                Ok(queued) => items.entry(key).or_default().extend(
                    queued
                        .into_iter()
                        .filter(|q| q.message_id.is_some())
                        .map(|q| WaitingItem {
                            created_at: q.created_at_ms / 1000,
                            preview: waiting::preview(&q.text),
                            place: Place::Codex,
                        }),
                ),
                Err(e) => eprintln!("Could not read Codex's queue for {key}: {e:#}"),
            }
        }
        let live: std::collections::BTreeSet<&str> =
            sessions.iter().map(|s| s.session_id.as_str()).collect();
        Ok(Reply::Waiting {
            waiting: items
                .into_iter()
                .filter(|(id, list)| !list.is_empty() && live.contains(id.as_str()))
                .map(|(id, list)| (id, waiting::view(list)))
                .collect(),
        })
    }
}
