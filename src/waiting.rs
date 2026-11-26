//! What waits for a session (REQ-032): bridge messages not yet handed over
//! (pending, held) and bridge messages still in Codex's own queue. Pure.
use serde::{Deserialize, Serialize};

/// Where a waiting message sits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Place {
    /// In the bridge: not handed to the session yet (pending or held).
    Bridge,
    /// Handed to Codex, still unread in the thread's queue.
    Codex,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WaitingItem {
    /// Unix seconds.
    pub created_at: u64,
    pub preview: String,
    pub place: Place,
    /// The bridge message, which the user can remove (REQ-035).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<uuid::Uuid>,
    /// Codex's queued submission id, for an item in Codex's queue.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queued_id: Option<String>,
}

/// Whether the user may remove a bridge message in this state: not handed
/// over yet (pending, held), or handed to Codex's queue and just taken out
/// of it (`from_codex`).
pub fn removable(delivery: &crate::domain::Delivery, from_codex: bool) -> bool {
    use crate::domain::Delivery;
    match delivery {
        Delivery::Pending { .. } | Delivery::Held { .. } => true,
        Delivery::Queued => from_codex,
        _ => false,
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WaitingView {
    pub count: usize,
    /// Oldest first.
    pub items: Vec<WaitingItem>,
}

/// At most `PREVIEW` characters of the message itself: the `Content (JSON
/// string)` line of a delivered frame when there is one, else the first line.
pub const PREVIEW: usize = 120;
pub fn preview(text: &str) -> String {
    let content = text
        .lines()
        .find_map(|line| line.strip_prefix("Content (JSON string): "))
        .and_then(|json| serde_json::from_str::<String>(json).ok());
    let line = content
        .as_deref()
        .unwrap_or(text)
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    let mut out: String = line.chars().take(PREVIEW).collect();
    if line.chars().count() > PREVIEW {
        out.push_str("...");
    }
    out
}

/// Bridge messages not handed over yet (pending, held), by recipient.
pub fn unsent<'a>(
    messages: impl IntoIterator<Item = &'a crate::store::Envelope>,
) -> std::collections::BTreeMap<String, Vec<WaitingItem>> {
    let mut out: std::collections::BTreeMap<String, Vec<WaitingItem>> = Default::default();
    for m in messages {
        if matches!(
            m.delivery,
            crate::domain::Delivery::Pending { .. } | crate::domain::Delivery::Held { .. }
        ) {
            out.entry(m.recipient.to_string())
                .or_default()
                .push(WaitingItem {
                    created_at: m.created_at,
                    preview: preview(&m.message),
                    place: Place::Bridge,
                    message_id: Some(m.message_id),
                    queued_id: None,
                });
        }
    }
    out
}

/// One session's waiting messages, oldest first.
pub fn view(mut items: Vec<WaitingItem>) -> WaitingView {
    items.sort_by_key(|i| i.created_at);
    WaitingView {
        count: items.len(),
        items,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /* REQ-032: A waiting message is previewed by its own text, not by the
    delivery frame around it, and long text is cut. */
    #[test]
    fn previews_the_message_itself() {
        let frame = "[Message from the user]\nFrom: user in #mono\nMessage ID: x\nContent (JSON string): \"kick off\\nsecond line\"\nAnswer in #mono";
        assert_eq!(preview(frame), "kick off");
        assert_eq!(preview("\n  plain text  \nmore"), "plain text");
        let long = "x".repeat(PREVIEW + 5);
        assert_eq!(preview(&long).len(), PREVIEW + 3);
    }

    /* REQ-035: The user removes a bridge message that waits in the bridge, or
    one just taken out of Codex's queue; never one already handed over. */
    #[test]
    fn decides_what_can_be_removed() {
        use crate::domain::Delivery;
        assert!(removable(&Delivery::Pending { reason: "x".into() }, false));
        assert!(removable(&Delivery::Held { reason: "x".into() }, false));
        assert!(!removable(&Delivery::Queued, false));
        assert!(removable(&Delivery::Queued, true));
        assert!(!removable(&Delivery::Delivered, true));
        assert!(!removable(&Delivery::Dispatching, true));
    }

    /* REQ-032: Items are counted and listed oldest first. */
    #[test]
    fn orders_and_counts() {
        let item = |at, place| WaitingItem {
            created_at: at,
            preview: String::new(),
            place,
            message_id: None,
            queued_id: None,
        };
        let v = view(vec![item(5, Place::Codex), item(1, Place::Bridge)]);
        assert_eq!(v.count, 2);
        assert_eq!(v.items[0].created_at, 1);
        assert_eq!(view(Vec::new()), WaitingView::default());
    }
}
