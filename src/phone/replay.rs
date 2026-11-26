//! Sends carrying a `client_id` (REQ-036): a repeat within 10 minutes gets
//! the first answer instead of sending again, so a manual resend after a lost
//! reply is harmless. Nothing is ever resent automatically.
use std::collections::BTreeMap;
use uuid::Uuid;

pub const KEEP_SECONDS: u64 = 600;
const LIMIT: usize = 1000;

#[derive(Debug, Clone, PartialEq)]
pub enum Begin {
    /// Not seen: send it.
    New,
    /// The first copy is still being sent.
    InFlight,
    /// Already answered: this is the answer.
    Done(serde_json::Value),
}

#[derive(Debug, Default, Clone)]
pub struct Replays {
    entries: BTreeMap<Uuid, (u64, Option<serde_json::Value>)>,
}
impl Replays {
    fn prune(&mut self, now: u64) {
        self.entries
            .retain(|_, (at, _)| now.saturating_sub(*at) < KEEP_SECONDS);
        while self.entries.len() >= LIMIT {
            let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, (at, _))| *at)
                .map(|(id, _)| *id)
            else {
                break;
            };
            self.entries.remove(&oldest);
        }
    }
    pub fn begin(&mut self, id: Uuid, now: u64) -> Begin {
        self.prune(now);
        match self.entries.get(&id) {
            Some((_, Some(answer))) => Begin::Done(answer.clone()),
            Some((_, None)) => Begin::InFlight,
            None => {
                self.entries.insert(id, (now, None));
                Begin::New
            }
        }
    }
    pub fn finish(&mut self, id: Uuid, answer: serde_json::Value) {
        if let Some(entry) = self.entries.get_mut(&id) {
            entry.1 = Some(answer);
        }
    }
}

/// The `client_id` of a request body, when it carries one.
pub fn client_id(body: &serde_json::Value) -> Option<Uuid> {
    body.get("client_id")
        .and_then(|v| v.as_str())
        .and_then(|v| Uuid::parse_str(v).ok())
}
