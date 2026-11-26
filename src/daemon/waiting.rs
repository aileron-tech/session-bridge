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
        let mut reads = tokio::task::JoinSet::new();
        for session in sessions.iter().filter(|s| s.agent == Provider::Codex) {
            let Ok(id) = SessionId::parse(&session.session_id) else {
                continue;
            };
            let home = self.codex_home.clone();
            let key = session.session_id.clone();
            reads.spawn(async move { (key, codex_queue::waiting(&home, &id.id).await) });
        }
        let gathered = tokio::time::timeout(CODEX_READS, async {
            let mut done = Vec::new();
            while let Some(joined) = reads.join_next().await {
                match joined {
                    Ok(result) => done.push(result),
                    Err(e) => eprintln!("Codex queue read failed: {e}"),
                }
            }
            done
        })
        .await
        .unwrap_or_else(|_| {
            eprintln!(
                "Codex queue reads took longer than {CODEX_READS:?}; showing what the bridge holds"
            );
            Vec::new()
        });
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
