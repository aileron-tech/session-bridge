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
                            message_id: q.message_id,
                            queued_id: Some(q.id),
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

    /// Removes a waiting bridge message the user picked (REQ-035): from
    /// Codex's queue first when it sits there, then marks it removed so it
    /// is never retried. Admin page only.
    pub(super) async fn remove_waiting(
        &self,
        session_id: String,
        message_id: uuid::Uuid,
        queued_id: Option<String>,
    ) -> Result<Reply> {
        if let Some(queued_id) = &queued_id {
            let session = self
                .last_sessions()
                .into_iter()
                .find(|s| s.session_id == session_id && s.agent == Provider::Codex)
                .ok_or_else(|| anyhow::anyhow!("That Codex session is not live on this laptop"))?;
            let thread = SessionId::parse(&session.session_id)?.id;
            let queued = codex_queue::waiting(&self.codex_home, &thread).await?;
            anyhow::ensure!(
                queued
                    .iter()
                    .any(|q| &q.id == queued_id && q.message_id == Some(message_id)),
                "That message is no longer in the session's queue"
            );
            let socket = crate::codex_wake::control_socket(&self.codex_home);
            let answers =
                crate::codex_wake::delete(&socket, &thread, std::slice::from_ref(queued_id))
                    .await?;
            if let Some(Err(e)) = answers.into_iter().next() {
                anyhow::bail!("Codex did not remove it: {e}");
            }
        }
        let request = Request::RemoveWaiting {
            session_id,
            message_id,
            queued_id,
        };
        if let Reply::Error { message } = self.ask(|reply| Event::Rpc(request, reply)).await? {
            anyhow::bail!(message);
        }
        self.waiting().await
    }
}
