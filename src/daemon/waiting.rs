//! The waiting messages of this laptop's sessions (REQ-032).
use super::{Control, Event};
use crate::{
    codex_queue,
    domain::{Delivery, Provider, SessionId},
    protocol::{Reply, Request},
    waiting::{self, Place, WaitingItem},
};
use anyhow::Result;
use std::collections::BTreeMap;

impl Control {
    /// Per local session with anything waiting: bridge messages not handed
    /// over yet, and bridge messages still unread in Codex's queue.
    pub(super) async fn waiting(&self) -> Result<Reply> {
        let sessions = self.local_sessions().await?;
        let Reply::Messages { messages, .. } = self
            .ask(|reply| Event::Rpc(Request::Messages, reply))
            .await?
        else {
            anyhow::bail!("Could not read the messages");
        };
        let mut items: BTreeMap<String, Vec<WaitingItem>> = BTreeMap::new();
        for message in &messages {
            if matches!(
                message.delivery,
                Delivery::Pending { .. } | Delivery::Held { .. }
            ) {
                items
                    .entry(message.recipient.to_string())
                    .or_default()
                    .push(WaitingItem {
                        created_at: message.created_at,
                        preview: waiting::preview(&message.message),
                        place: Place::Bridge,
                    });
            }
        }
        for session in sessions.iter().filter(|s| s.agent == Provider::Codex) {
            let Ok(id) = SessionId::parse(&session.session_id) else {
                continue;
            };
            match codex_queue::waiting(&self.codex_home, &id.id).await {
                Ok(queued) => items.entry(session.session_id.clone()).or_default().extend(
                    queued
                        .into_iter()
                        .filter(|q| q.message_id.is_some())
                        .map(|q| WaitingItem {
                            created_at: q.created_at_ms / 1000,
                            preview: waiting::preview(&q.text),
                            place: Place::Codex,
                        }),
                ),
                Err(e) => eprintln!("Could not read Codex's queue for {}: {e:#}", session.name),
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
