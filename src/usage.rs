//! Host and session usage (REQ-029): the pure model. Reading the system is
//! [`sampler`]'s job; this module chooses, aggregates and describes.
pub mod sampler;

use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

/// One host's load. CPU is the average over all cores (0 to 100); sizes are bytes.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct HostUsage {
    pub cpu_percent: f32,
    pub memory_used: u64,
    pub memory_total: u64,
    /// The filesystem that holds the user's home.
    pub disk_used: u64,
    pub disk_total: u64,
}

/// A mounted filesystem as the system reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mount {
    pub path: PathBuf,
    pub total: u64,
    pub available: u64,
}

/// One process's load. CPU is per core, so it can exceed 100 on several cores.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProcessSample {
    pub cpu_percent: f32,
    pub memory: u64,
    /// The process is a Codex app server, which serves every `codex --remote` session.
    pub app_server: bool,
}

/// What a session's process costs.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SessionUsage {
    /// The session has the process to itself.
    Own { cpu_percent: f32, memory: u64 },
    /// The process carries `sessions` live sessions; the numbers are the
    /// whole process's, never a share.
    Shared {
        cpu_percent: f32,
        memory: u64,
        sessions: usize,
        app_server: bool,
    },
    /// No process could be read for the session.
    Unavailable,
}

/// A host's usage, or why it is missing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum HostState {
    Available { usage: HostUsage },
    Unavailable { reason: String },
}

/// A host on the admin page: this laptop (`local`) or a paired bridge.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HostUsageView {
    pub host: String,
    pub local: bool,
    #[serde(flatten)]
    pub state: HostState,
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionUsageView {
    pub session_id: String,
    pub usage: SessionUsage,
    pub summary: String,
}

/// The used and total bytes of the mount that holds `home`: the longest
/// mount path that contains it.
pub fn home_disk(home: &Path, mounts: &[Mount]) -> Option<(u64, u64)> {
    mounts
        .iter()
        .filter(|m| home.starts_with(&m.path))
        .max_by_key(|m| m.path.components().count())
        .map(|m| (m.total.saturating_sub(m.available), m.total))
}

/// A Codex app server runs as `codex app-server ...`.
pub fn is_app_server(command: &[String]) -> bool {
    command.iter().skip(1).any(|arg| arg == "app-server")
}

/// Each session's usage from its process. A process that several sessions
/// share, or a Codex app server, is reported whole as shared.
pub fn session_usage(
    sessions: &[(String, u32)],
    samples: &BTreeMap<u32, ProcessSample>,
) -> Vec<SessionUsageView> {
    let mut carried: BTreeMap<u32, usize> = BTreeMap::new();
    for (_, pid) in sessions {
        *carried.entry(*pid).or_default() += 1;
    }
    sessions
        .iter()
        .map(|(session_id, pid)| {
            let usage = match samples.get(pid) {
                Some(s) if *pid > 0 => {
                    let count = carried.get(pid).copied().unwrap_or(1);
                    if s.app_server || count > 1 {
                        SessionUsage::Shared {
                            cpu_percent: s.cpu_percent,
                            memory: s.memory,
                            sessions: count,
                            app_server: s.app_server,
                        }
                    } else {
                        SessionUsage::Own {
                            cpu_percent: s.cpu_percent,
                            memory: s.memory,
                        }
                    }
                }
                _ => SessionUsage::Unavailable,
            };
            SessionUsageView {
                session_id: session_id.clone(),
                summary: describe_session(&usage),
                usage,
            }
        })
        .collect()
}

pub fn host_view(host: String, local: bool, state: HostState) -> HostUsageView {
    let summary = match &state {
        HostState::Available { usage } => describe_host(usage),
        HostState::Unavailable { .. } => "usage unavailable".to_owned(),
    };
    HostUsageView {
        host,
        local,
        state,
        summary,
    }
}

/// "CPU 12%, memory 9.1 of 16.0 GB, disk 210 of 494 GB".
pub fn describe_host(usage: &HostUsage) -> String {
    format!(
        "CPU {}, memory {}, disk {}",
        percent(usage.cpu_percent),
        of(usage.memory_used, usage.memory_total),
        of(usage.disk_used, usage.disk_total),
    )
}

/// "CPU 3%, memory 412 MB", prefixed "shared app server: " when shared.
pub fn describe_session(usage: &SessionUsage) -> String {
    match usage {
        SessionUsage::Own {
            cpu_percent,
            memory,
        } => format!("CPU {}, memory {}", percent(*cpu_percent), bytes(*memory)),
        SessionUsage::Shared {
            cpu_percent,
            memory,
            sessions,
            app_server,
        } => format!(
            "{} ({sessions} session{}): CPU {}, memory {}",
            if *app_server {
                "shared app server"
            } else {
                "shared process"
            },
            if *sessions == 1 { "" } else { "s" },
            percent(*cpu_percent),
            bytes(*memory)
        ),
        SessionUsage::Unavailable => "usage unavailable".to_owned(),
    }
}

fn percent(value: f32) -> String {
    let value = if value.is_finite() {
        value.max(0.0)
    } else {
        0.0
    };
    format!("{value:.0}%")
}

const KB: u64 = 1024;
const MB: u64 = KB * 1024;
const GB: u64 = MB * 1024;
const TB: u64 = GB * 1024;

/// Binary units with the familiar names, as Activity Monitor and Task Manager show them.
fn bytes(value: u64) -> String {
    let (unit, name) = unit_for(value);
    format!("{} {name}", scaled(value, unit))
}
/// "9.1 of 16.0 GB": both in the unit of the total.
fn of(used: u64, total: u64) -> String {
    let (unit, name) = unit_for(total);
    format!("{} of {} {name}", scaled(used, unit), scaled(total, unit))
}
fn unit_for(value: u64) -> (u64, &'static str) {
    match value {
        v if v >= TB => (TB, "TB"),
        v if v >= GB => (GB, "GB"),
        v if v >= MB => (MB, "MB"),
        v if v >= KB => (KB, "KB"),
        _ => (1, "B"),
    }
}
/// One decimal below 100 units, whole numbers above.
fn scaled(value: u64, unit: u64) -> String {
    let amount = value as f64 / unit as f64;
    if unit == 1 || amount >= 100.0 {
        format!("{amount:.0}")
    } else {
        format!("{amount:.1}")
    }
}

#[cfg(test)]
#[path = "usage_tests.rs"]
mod tests;
