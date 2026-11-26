//! What a scheduled run did, and whether its previous post is still unread
//! (REQ-025). A run never stacks a post on an unread one: it skips that
//! recipient and says so. Pure.
use crate::domain::{Delivery, Provider};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use uuid::Uuid;

/// A post a run sent to one session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunPost {
    pub message_id: Uuid,
    pub at: u64,
}

/// One run: when, who got the post, who was skipped and why, who failed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduleRun {
    pub at: u64,
    /// Session names that got the post.
    #[serde(default)]
    pub posted: Vec<String>,
    /// One line per skipped session, ready to show.
    #[serde(default)]
    pub skipped: Vec<String>,
    #[serde(default)]
    pub failed: Vec<String>,
}

/// A schedule's last run and its last post to each session (by session id),
/// kept in `schedule_runs.json`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduleState {
    #[serde(default)]
    pub last_run: Option<ScheduleRun>,
    #[serde(default)]
    pub posts: BTreeMap<String, RunPost>,
}

/// Whether the recipient has not taken a post yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unread {
    Yes,
    No,
    /// Accepted by Codex's queue: unread while it is still in that queue.
    IfInCodexQueue,
}

/// From the post's delivery state. A Claude `queued` post has no receipt yet;
/// a local Codex `queued` post is read once Codex takes it from its queue. A
/// post on a paired laptop's Codex cannot be checked from here and counts as
/// read, so it never blocks a schedule for good.
pub fn unread(delivery: &Delivery, provider: &Provider, local: bool) -> Unread {
    match delivery {
        Delivery::Pending { .. }
        | Delivery::Dispatching
        | Delivery::Held { .. }
        | Delivery::Forwarded { .. } => Unread::Yes,
        Delivery::Queued => match provider {
            Provider::Codex if local => Unread::IfInCodexQueue,
            Provider::Codex => Unread::No,
            _ => Unread::Yes,
        },
        Delivery::Delivered | Delivery::Refused { .. } | Delivery::Uncertain { .. } => Unread::No,
    }
}

/// "3 min ago", "2 h ago".
pub fn ago(seconds: u64) -> String {
    match seconds {
        s if s < 60 => "just now".to_owned(),
        s if s < 2 * 3600 => format!("{} min ago", s / 60),
        s if s < 2 * 86400 => format!("{} h ago", s / 3600),
        s => format!("{} days ago", s / 86400),
    }
}

/// The line for a skipped session, in the user's words.
pub fn skip_line(name: &str, previous_at: u64, now: u64) -> String {
    format!(
        "@{name}: the previous post ({}) was not processed yet; sending skipped",
        ago(now.saturating_sub(previous_at))
    )
}

/// One line for the Cron tab and the run's reply.
pub fn summary(run: &ScheduleRun) -> String {
    let mut parts = Vec::new();
    if !run.posted.is_empty() {
        let names: Vec<String> = run.posted.iter().map(|n| format!("@{n}")).collect();
        parts.push(format!("posted to {}", names.join(", ")));
    }
    parts.extend(run.skipped.iter().map(|s| format!("skipped {s}")));
    parts.extend(run.failed.iter().map(|f| format!("failed {f}")));
    if parts.is_empty() {
        "nobody to post to".to_owned()
    } else {
        parts.join("; ")
    }
}

#[cfg(test)]
#[path = "runs_tests.rs"]
mod tests;
