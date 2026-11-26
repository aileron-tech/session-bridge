//! Runs and supervises the SSH connections of remote bridges (REQ-027): one
//! `ssh -N` per remote host, restarted with a growing delay when it ends.
use super::{Remote, ssh_args};
use std::{
    collections::BTreeMap,
    process::Stdio,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::{process::Command, sync::Mutex as AsyncMutex, task::JoinHandle};

struct Entry {
    remote: Remote,
    own_port: u16,
    /// "running", or why the last connection ended.
    status: Arc<Mutex<String>>,
    task: JoinHandle<()>,
}

/// The SSH connections of this bridge's remote hosts.
#[derive(Clone)]
pub struct Tunnels {
    ssh: String,
    entries: Arc<AsyncMutex<BTreeMap<String, Entry>>>,
}

impl Tunnels {
    pub fn new(ssh: String) -> Self {
        Self {
            ssh,
            entries: Arc::default(),
        }
    }
    /// Starts the connection for `remote`, or restarts it when its ports changed.
    pub async fn ensure(&self, remote: Remote, own_port: u16) {
        let mut entries = self.entries.lock().await;
        if let Some(entry) = entries.get(&remote.host) {
            if entry.remote == remote && entry.own_port == own_port {
                return;
            }
            entry.task.abort();
        }
        let status = Arc::new(Mutex::new("starting".to_owned()));
        let task = tokio::spawn(supervise(
            self.ssh.clone(),
            remote.clone(),
            own_port,
            status.clone(),
        ));
        entries.insert(
            remote.host.clone(),
            Entry {
                remote,
                own_port,
                status,
                task,
            },
        );
    }
    /// Ends the connection to `host` (its ssh process is killed).
    pub async fn remove(&self, host: &str) {
        if let Some(entry) = self.entries.lock().await.remove(host) {
            entry.task.abort();
        }
    }
    pub async fn stop_all(&self) {
        for (_, entry) in std::mem::take(&mut *self.entries.lock().await) {
            entry.task.abort();
        }
    }
    /// What each supervisor last reported, by host.
    pub async fn statuses(&self) -> BTreeMap<String, String> {
        self.entries
            .lock()
            .await
            .iter()
            .map(|(host, entry)| {
                let status = entry
                    .status
                    .lock()
                    .map(|s| s.clone())
                    .unwrap_or_else(|p| p.into_inner().clone());
                (host.clone(), status)
            })
            .collect()
    }
}

fn set(status: &Mutex<String>, text: String) {
    match status.lock() {
        Ok(mut current) => *current = text,
        Err(poisoned) => *poisoned.into_inner() = text,
    }
}

async fn supervise(ssh: String, remote: Remote, own_port: u16, status: Arc<Mutex<String>>) {
    let mut delay = Duration::from_secs(5);
    loop {
        let started = Instant::now();
        let child = Command::new(&ssh)
            .args(ssh_args(&remote, own_port))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn();
        let reason = match child {
            Err(e) => format!("cannot run {ssh}: {e}"),
            Ok(child) => {
                set(&status, "running".into());
                match child.wait_with_output().await {
                    Ok(output) => String::from_utf8_lossy(&output.stderr)
                        .lines()
                        .rfind(|line| !line.trim().is_empty())
                        .map(str::to_owned)
                        .unwrap_or_else(|| format!("ssh ended ({})", output.status)),
                    Err(e) => format!("ssh failed: {e}"),
                }
            }
        };
        eprintln!("SSH tunnel to {} ended: {reason}", remote.host);
        set(&status, reason);
        if started.elapsed() > Duration::from_secs(60) {
            delay = Duration::from_secs(5);
        }
        tokio::time::sleep(delay).await;
        delay = (delay * 2).min(Duration::from_secs(60));
    }
}
