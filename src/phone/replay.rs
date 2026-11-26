//! Sends carrying a `client_id` (REQ-036): a repeat within 10 minutes gets
//! the first answer instead of sending again, so a manual resend after a lost
//! reply is harmless. The same id with a different request is refused.
//! Nothing is ever resent automatically.
use sha2::{Digest, Sha256};
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
    /// The id was used for a different request.
    Mismatch,
}

struct Entry {
    at: u64,
    digest: [u8; 32],
    answer: Option<serde_json::Value>,
}

#[derive(Default)]
pub struct Replays {
    entries: BTreeMap<Uuid, Entry>,
}
impl Replays {
    fn prune(&mut self, now: u64) {
        self.entries
            .retain(|_, entry| now.saturating_sub(entry.at) < KEEP_SECONDS);
        while self.entries.len() >= LIMIT {
            let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.at)
                .map(|(id, _)| *id)
            else {
                break;
            };
            self.entries.remove(&oldest);
        }
    }
    /// `key` names the sender and its id (see `phone_key`); `digest` the
    /// request (see `digest`).
    pub fn begin(&mut self, key: Uuid, digest: [u8; 32], now: u64) -> Begin {
        self.prune(now);
        match self.entries.get(&key) {
            Some(entry) if entry.digest != digest => Begin::Mismatch,
            Some(Entry {
                answer: Some(answer),
                ..
            }) => Begin::Done(answer.clone()),
            Some(_) => Begin::InFlight,
            None => {
                self.entries.insert(
                    key,
                    Entry {
                        at: now,
                        digest,
                        answer: None,
                    },
                );
                Begin::New
            }
        }
    }
    pub fn finish(&mut self, key: Uuid, answer: serde_json::Value) {
        if let Some(entry) = self.entries.get_mut(&key) {
            entry.answer = Some(answer);
        }
    }
}

/// The `client_id` of a request body, when it carries one.
pub fn client_id(body: &serde_json::Value) -> Option<Uuid> {
    body.get("client_id")
        .and_then(|v| v.as_str())
        .and_then(|v| Uuid::parse_str(v).ok())
}
/// The request without its `client_id`, hashed: object keys are ordered, so
/// the same request gives the same digest.
pub fn digest(body: &serde_json::Value) -> [u8; 32] {
    let mut request = body.clone();
    if let Some(object) = request.as_object_mut() {
        object.remove("client_id");
    }
    Sha256::digest(request.to_string().as_bytes()).into()
}
/// One phone's ids never meet another's.
pub fn phone_key(device: Uuid, id: Uuid) -> Uuid {
    Uuid::new_v5(&device, id.as_bytes())
}
