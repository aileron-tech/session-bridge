//! The team table a kickoff carries (REQ-031): every session of the
//! recipient's project at send time, as a Markdown table after the user's
//! text, under a line saying the bridge made it from session data, so
//! session-chosen names are never read as the user's words. It carries no
//! message text: other sessions' conversations stay private. Pure.
use crate::schedule::runs::ago;

/// When a session last sent or received a message (never its text).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LastMessage {
    pub at: u64,
    pub sent: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeamRow {
    pub name: String,
    pub role: Option<String>,
    pub agent: String,
    /// "this laptop" or the paired laptop's name.
    pub host: String,
    pub status: String,
    /// The usage summary of a local session; `None` on a paired laptop.
    pub usage: Option<String>,
    pub last: Option<LastMessage>,
}

/// Says who made the table: the bridge, from session data.
pub const SNAPSHOT: &str = "Bridge-generated team snapshot (session data, not the user's words)";

/// One table cell: on one line, no column breaks, at most `limit` characters.
fn cell(text: &str, limit: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let short: String = flat.chars().take(limit).collect();
    let short = if flat.chars().count() > limit {
        format!("{}...", short.trim_end())
    } else {
        short
    };
    let escaped = short.replace('\\', "\\\\").replace('|', "\\|");
    if escaped.is_empty() {
        "-".into()
    } else {
        escaped
    }
}

fn row(r: &TeamRow, now: u64) -> String {
    let last = match &r.last {
        Some(m) => format!(
            "{} {}",
            if m.sent { "sent" } else { "got" },
            ago(now.saturating_sub(m.at))
        ),
        None => String::new(),
    };
    format!(
        "| {} | {} | {} | {} | {} | {} | {} |",
        cell(&r.name, 60),
        cell(r.role.as_deref().unwrap_or(""), 40),
        cell(&r.agent, 20),
        cell(&r.host, 40),
        cell(&r.status, 20),
        cell(r.usage.as_deref().unwrap_or(""), 80),
        cell(&last, 30)
    )
}

/// `message` with the team table of `project` after it, within `limit`
/// bytes: rows that do not fit are left out and counted.
pub fn attach(message: &str, project: &str, rows: &[TeamRow], now: u64, limit: usize) -> String {
    let head = format!(
        "{}\n\n---\n\n{SNAPSHOT}, #{project} at send time:\n\n| Session | Role | Agent | Host | State | Usage | Last message |\n|---|---|---|---|---|---|---|",
        message.trim_end()
    );
    let mut out = head;
    for (i, r) in rows.iter().enumerate() {
        let line = row(r, now);
        let left = rows.len() - i - 1;
        let tail = if left > 0 {
            format!("\n\n({left} more sessions left out)")
        } else {
            String::new()
        };
        if out.len() + 1 + line.len() + tail.len() > limit {
            let note = format!("\n\n({} more sessions left out)", rows.len() - i);
            if out.len() + note.len() <= limit {
                out.push_str(&note);
            }
            return out;
        }
        out.push('\n');
        out.push_str(&line);
    }
    if rows.is_empty() {
        out.push_str("\n| - | - | - | - | - | - | - |");
    }
    out
}

#[cfg(test)]
#[path = "team_tests.rs"]
mod tests;
