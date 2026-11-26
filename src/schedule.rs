//! Scheduled channel posts through the user's crontab (REQ-025). The bridge
//! manages only its own lines (`<cron> <command> scheduled <id> # MARKER`);
//! every other line is kept byte for byte and in order. Pure; `crontab`
//! reads and writes the table.
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, path::Path};
use thiserror::Error;
use uuid::Uuid;

pub mod crontab;

/// Ends every line the bridge manages.
pub const MARKER: &str = "# session-bridge schedule";
/// Precedes the managed lines.
pub const HEADER: &str = "# session-bridge: scheduled messages, edited on the admin page";

/// A message posted in a project channel on a cron schedule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Schedule {
    pub id: Uuid,
    /// Five cron fields, or a macro such as `@daily`.
    pub cron: String,
    pub project: String,
    /// A session name, `channel` for every session, or none for the
    /// channel's default recipient and the message's own mentions.
    #[serde(default)]
    pub to: Option<String>,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Line {
    Managed(Uuid),
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ScheduleError {
    #[error(
        "A schedule has five cron fields (minute hour day month weekday) or a macro such as @daily"
    )]
    Cron,
    #[error("A channel is a project name")]
    Project,
    #[error("Send to a session name or @channel")]
    Recipient,
    #[error("Message must be nonempty, at most 16000 bytes, and contain no NUL")]
    Message,
}

/// The schedule id of a managed line.
fn managed(line: &str) -> Option<Uuid> {
    let body = line.trim_end().strip_suffix(MARKER)?;
    let id = body.trim_end().rsplit(' ').next()?;
    body.contains(" scheduled ")
        .then(|| Uuid::parse_str(id).ok())
        .flatten()
}

/// Lines split on `\n` only, so a `\r` stays part of its line.
fn lines(crontab: &str) -> impl Iterator<Item = &str> {
    crontab
        .strip_suffix('\n')
        .unwrap_or(crontab)
        .split('\n')
        .filter(move |_| !crontab.is_empty())
}

pub fn parse(crontab: &str) -> Vec<Line> {
    lines(crontab)
        .map(|line| match managed(line) {
            Some(id) => Line::Managed(id),
            None => Line::Other(line.to_owned()),
        })
        .collect()
}

/// Managed lines of other bridges (ids this bridge does not own), verbatim.
pub fn foreign_lines(crontab: &str, owned: &BTreeSet<Uuid>) -> Vec<String> {
    lines(crontab)
        .filter(|line| managed(line).is_some_and(|id| !owned.contains(&id)))
        .map(str::to_owned)
        .collect()
}

/// The crontab with every line kept except this bridge's own managed lines
/// (`owned`: its schedules, including ones just deleted) and its header,
/// which come back as one line per schedule at the end.
pub fn render(
    crontab: &str,
    schedules: &[Schedule],
    command: &str,
    owned: &BTreeSet<Uuid>,
) -> String {
    let mut out = String::new();
    let ours = |id: Uuid| owned.contains(&id) || schedules.iter().any(|s| s.id == id);
    for line in lines(crontab) {
        let drop = line == HEADER || managed(line).is_some_and(ours);
        if !drop {
            out.push_str(line);
            out.push('\n');
        }
    }
    if !schedules.is_empty() {
        out.push_str(HEADER);
        out.push('\n');
        for schedule in schedules {
            out.push_str(&format!(
                "{} {command} scheduled {} {MARKER}\n",
                schedule.cron, schedule.id
            ));
        }
    }
    out
}

fn valid_cron(cron: &str) -> bool {
    const MACROS: [&str; 8] = [
        "@reboot",
        "@yearly",
        "@annually",
        "@monthly",
        "@weekly",
        "@daily",
        "@midnight",
        "@hourly",
    ];
    if cron.starts_with('@') {
        return MACROS.contains(&cron);
    }
    let fields: Vec<&str> = cron.split(' ').filter(|f| !f.is_empty()).collect();
    fields.len() == 5
        && fields.iter().all(|field| {
            field
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '*' | '/' | ',' | '-'))
        })
}

pub fn validate(schedule: &Schedule) -> Result<(), ScheduleError> {
    if !valid_cron(&schedule.cron) {
        return Err(ScheduleError::Cron);
    }
    if crate::domain::validate_project(&schedule.project).is_err() || schedule.project.contains('/')
    {
        return Err(ScheduleError::Project);
    }
    // A mention ends at a trailing '.' or '-', so a name cannot end with one.
    if let Some(to) = &schedule.to
        && (to.is_empty()
            || to.ends_with(['.', '-'])
            || !to
                .chars()
                .all(|c| c.is_alphanumeric() || matches!(c, '.' | '_' | '-')))
    {
        return Err(ScheduleError::Recipient);
    }
    crate::domain::validate_message(&schedule.message).map_err(|_| ScheduleError::Message)
}

/// Quotes `text` for a POSIX shell inside a crontab line, where cron would
/// turn an unescaped `%` into a newline.
fn shell(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''").replace('%', r"\%"))
}

/// The command cron runs for a schedule, without the id: the bridge
/// executable, and for a separate bridge its state directory.
pub fn command(executable: &Path, state: Option<&Path>) -> String {
    let program = shell(&executable.to_string_lossy());
    match state {
        Some(state) => format!(
            "SESSION_BRIDGE_STATE={} {program}",
            shell(&state.to_string_lossy())
        ),
        None => program,
    }
}

#[cfg(test)]
#[path = "schedule_tests.rs"]
mod tests;
