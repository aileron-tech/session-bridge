//! Reads host and process usage from the system with `sysinfo` (REQ-029).
//! One sampler lives as long as the bridge: CPU percentages are the change
//! since its previous reading, so the first reading primes them.
use super::{HostUsage, Mount, ProcessSample, home_disk, is_app_server};
use anyhow::{Context, Result, anyhow};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, Mutex, MutexGuard},
};
use sysinfo::{
    DiskRefreshKind, Disks, Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind,
};

struct State {
    system: System,
    disks: Disks,
}

#[derive(Clone)]
pub struct Sampler {
    state: Arc<Mutex<State>>,
    home: Arc<PathBuf>,
}

impl Sampler {
    /// `home` names the filesystem whose disk is reported.
    pub fn new(home: PathBuf) -> Result<Self> {
        let mut system = System::new().map_err(|e| anyhow!("Cannot read system usage: {e}"))?;
        system.refresh_cpu_usage();
        let disks = Disks::new().map_err(|e| anyhow!("Cannot read disks: {e}"))?;
        Ok(Self {
            state: Arc::new(Mutex::new(State { system, disks })),
            home: Arc::new(home),
        })
    }
    fn lock(state: &Mutex<State>) -> MutexGuard<'_, State> {
        match state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    pub async fn host(&self) -> Result<HostUsage> {
        let (state, home) = (self.state.clone(), self.home.clone());
        tokio::task::spawn_blocking(move || {
            let mut state = Self::lock(&state);
            state.system.refresh_cpu_usage();
            state.system.refresh_memory();
            state
                .disks
                .refresh_specifics(true, DiskRefreshKind::nothing().with_storage());
            let mounts: Vec<Mount> = state
                .disks
                .iter()
                .map(|d| Mount {
                    path: d.mount_point().to_path_buf(),
                    total: d.total_space(),
                    available: d.available_space(),
                })
                .collect();
            let (disk_used, disk_total) = home_disk(&home, &mounts)
                .with_context(|| format!("No disk holds {}", home.display()))?;
            Ok(HostUsage {
                cpu_percent: state.system.global_cpu_usage(),
                memory_used: state.system.used_memory(),
                memory_total: state.system.total_memory(),
                disk_used,
                disk_total,
            })
        })
        .await
        .context("Usage reading stopped")?
    }

    /// The processes in `pids` that exist; others are left out.
    pub async fn processes(&self, pids: Vec<u32>) -> Result<BTreeMap<u32, ProcessSample>> {
        let state = self.state.clone();
        tokio::task::spawn_blocking(move || {
            let mut state = Self::lock(&state);
            let wanted: Vec<Pid> = pids.iter().map(|p| Pid::from_u32(*p)).collect();
            state.system.refresh_processes_specifics(
                ProcessesToUpdate::Some(&wanted),
                true,
                ProcessRefreshKind::nothing()
                    .with_cpu()
                    .with_memory()
                    .with_cmd(UpdateKind::OnlyIfNotSet),
            );
            pids.iter()
                .filter_map(|pid| {
                    let process = state.system.process(Pid::from_u32(*pid))?;
                    let command: Vec<String> = process
                        .cmd()
                        .iter()
                        .map(|a| a.to_string_lossy().into_owned())
                        .collect();
                    Some((
                        *pid,
                        ProcessSample {
                            cpu_percent: process.cpu_usage(),
                            memory: process.memory(),
                            app_server: is_app_server(&command),
                        },
                    ))
                })
                .collect()
        })
        .await
        .context("Usage reading stopped")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /* REQ-029: This machine's usage is read: some memory and a home disk, and
    this test's own process with its memory. */
    #[tokio::test]
    async fn reads_this_machine_and_process() {
        let home = std::env::temp_dir();
        let sampler = Sampler::new(home).unwrap();
        let host = sampler.host().await.unwrap();
        assert!(host.memory_total > 0 && host.memory_used <= host.memory_total);
        assert!(host.disk_total > 0 && host.disk_used <= host.disk_total);
        assert!((0.0..=100.0).contains(&host.cpu_percent), "{host:?}");
        let own = std::process::id();
        let found = sampler.processes(vec![own, u32::MAX - 1]).await.unwrap();
        assert_eq!(found.keys().copied().collect::<Vec<_>>(), vec![own]);
        assert!(found[&own].memory > 0);
        assert!(!found[&own].app_server);
    }
}
