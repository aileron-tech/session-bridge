//! Host and session usage (REQ-029): the pure model. Reading the system is
//! [`sampler`]'s job; this module chooses, aggregates and describes.
pub mod sampler;

use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::Duration,
};

/// How long one local sample serves every reader (pages, paired bridges):
/// CPU is a change between readings, and readings closer than about 200 ms
/// are wrong.
pub const LOCAL_SAMPLE_AGE: Duration = Duration::from_secs(2);
/// How long a paired bridge's usage is reused before it is asked again.
pub const REMOTE_SAMPLE_AGE: Duration = Duration::from_secs(30);
/// The longest wait before asking a paired bridge that failed again.
pub const REMOTE_BACKOFF_MAX: Duration = Duration::from_secs(300);

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

/// One process in the system's process table. CPU is per core, so it can
/// exceed 100 on several cores.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProcessRow {
    pub pid: u32,
    pub parent: Option<u32>,
    pub cpu_percent: f32,
    pub memory: u64,
    /// The process is a Codex app server, which serves every `codex --remote` session.
    pub app_server: bool,
}

/// What a session's process costs.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SessionUsage {
    /// The session has the process to itself: the numbers are its process
    /// tree (the process and its descendants, such as a build it runs).
    Own {
        cpu_percent: f32,
        memory: u64,
        processes: usize,
    },
    /// The process carries `sessions` live sessions; the numbers are the
    /// main process's alone (its children belong to different sessions),
    /// never a share or a sum.
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

/// Each session's usage from the process table. A session's own process
/// counts with its descendants, up to another session's process (a session
/// started from this one's shell is its own row); a process that several sessions share, or a
/// Codex app server, counts alone and is reported as shared.
pub fn session_usage(sessions: &[(String, u32)], table: &[ProcessRow]) -> Vec<SessionUsageView> {
    let rows: BTreeMap<u32, &ProcessRow> = table.iter().map(|r| (r.pid, r)).collect();
    let mut children: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
    for row in table {
        if let Some(parent) = row.parent.filter(|p| *p != row.pid) {
            children.entry(parent).or_default().push(row.pid);
        }
    }
    let mut carried: BTreeMap<u32, usize> = BTreeMap::new();
    for (_, pid) in sessions {
        *carried.entry(*pid).or_default() += 1;
    }
    sessions
        .iter()
        .map(|(session_id, pid)| {
            let usage = match rows.get(pid) {
                Some(root) if *pid > 0 => {
                    let count = carried.get(pid).copied().unwrap_or(1);
                    if root.app_server || count > 1 {
                        SessionUsage::Shared {
                            cpu_percent: root.cpu_percent,
                            memory: root.memory,
                            sessions: count,
                            app_server: root.app_server,
                        }
                    } else {
                        let tree = descendants(*pid, &children, &carried);
                        let members = tree.iter().filter_map(|p| rows.get(p));
                        SessionUsage::Own {
                            cpu_percent: members.clone().map(|r| r.cpu_percent).sum(),
                            memory: members.clone().map(|r| r.memory).sum(),
                            processes: members.count(),
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

/// `root` and every process below it, each once (a parent loop cannot
/// recur). The walk stops at another session's process: it and its children
/// belong to that session.
fn descendants(
    root: u32,
    children: &BTreeMap<u32, Vec<u32>>,
    sessions: &BTreeMap<u32, usize>,
) -> Vec<u32> {
    let mut seen = std::collections::BTreeSet::from([root]);
    let mut pending = vec![root];
    while let Some(pid) = pending.pop() {
        for child in children.get(&pid).into_iter().flatten() {
            if !sessions.contains_key(child) && seen.insert(*child) {
                pending.push(*child);
            }
        }
    }
    seen.into_iter().collect()
}

/// How long a started query to a paired bridge keeps others from asking it
/// again: longer than any query may take, so a lost one does not block forever.
pub const REMOTE_QUERY_HOLD: Duration = Duration::from_secs(10);

/// Whether a query that started `running_for` ago still stands for this
/// bridge, so a second poll does not ask it too.
pub fn query_in_flight(running_for: Option<Duration>) -> bool {
    running_for.is_some_and(|d| d < REMOTE_QUERY_HOLD)
}

/// A paired bridge's last answer and how many times in a row it failed.
#[derive(Debug, Clone, PartialEq)]
pub struct RemoteSample {
    pub state: HostState,
    pub failures: u32,
}

/// Whether to ask a paired bridge again, `age` after its last sample: every
/// [`REMOTE_SAMPLE_AGE`] while it answers; after failures, waits that double
/// from that up to [`REMOTE_BACKOFF_MAX`], so an offline or older bridge is
/// not asked on every page poll.
pub fn remote_due(previous: Option<&RemoteSample>, age: Duration) -> bool {
    let Some(previous) = previous else {
        return true;
    };
    let wait = match previous.failures {
        0 => REMOTE_SAMPLE_AGE,
        n => REMOTE_SAMPLE_AGE
            .saturating_mul(1u32 << (n - 1).min(16))
            .min(REMOTE_BACKOFF_MAX),
    };
    age >= wait
}

/// The sample after asking: a failure adds to the run of failures.
pub fn next_remote(previous: Option<&RemoteSample>, state: HostState) -> RemoteSample {
    let failures = match &state {
        HostState::Available { .. } => 0,
        HostState::Unavailable { .. } => previous.map_or(0, |p| p.failures).saturating_add(1),
    };
    RemoteSample { state, failures }
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

/// "CPU 12%, memory 9.1 of 16.0 GiB, disk 210 of 494 GiB".
pub fn describe_host(usage: &HostUsage) -> String {
    format!(
        "CPU {}, memory {}, disk {}",
        percent(usage.cpu_percent),
        of(usage.memory_used, usage.memory_total),
        of(usage.disk_used, usage.disk_total),
    )
}

/// "CPU 3%, memory 412 MiB, 2 processes" for a session's own process tree;
/// "shared app server (3 sessions, main process only): ..." when shared.
pub fn describe_session(usage: &SessionUsage) -> String {
    match usage {
        SessionUsage::Own {
            cpu_percent,
            memory,
            processes,
        } => format!(
            "CPU {}, memory {}, {processes} process{}",
            percent(*cpu_percent),
            bytes(*memory),
            if *processes == 1 { "" } else { "es" }
        ),
        SessionUsage::Shared {
            cpu_percent,
            memory,
            sessions,
            app_server,
        } => format!(
            "{} ({sessions} session{}, main process only): CPU {}, memory {}",
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

const KIB: u64 = 1024;
const MIB: u64 = KIB * 1024;
const GIB: u64 = MIB * 1024;
const TIB: u64 = GIB * 1024;

/// Binary units, named as such (GiB): memory and disk alike. Finder counts
/// disks in decimal GB, so a disk reads about 7% smaller here.
fn bytes(value: u64) -> String {
    let (unit, name) = unit_for(value);
    format!("{} {name}", scaled(value, unit))
}
/// "9.1 of 16.0 GiB": both in the unit of the total.
fn of(used: u64, total: u64) -> String {
    let (unit, name) = unit_for(total);
    format!("{} of {} {name}", scaled(used, unit), scaled(total, unit))
}
fn unit_for(value: u64) -> (u64, &'static str) {
    match value {
        v if v >= TIB => (TIB, "TiB"),
        v if v >= GIB => (GIB, "GiB"),
        v if v >= MIB => (MIB, "MiB"),
        v if v >= KIB => (KIB, "KiB"),
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
