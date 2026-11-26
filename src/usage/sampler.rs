//! Reads host and process usage from the system with `sysinfo` (REQ-029).
//! One sampler lives as long as the bridge: CPU percentages are the change
//! since its previous reading, so the first reading primes them. Readings are
//! reused for [`LOCAL_SAMPLE_AGE`], so pages and paired bridges that ask at
//! once share one baseline instead of resetting it.
use super::{HostUsage, LOCAL_SAMPLE_AGE, Mount, ProcessRow, home_disk, is_app_server};
use anyhow::{Context, Result, anyhow};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex, MutexGuard},
    time::Instant,
};
use sysinfo::{DiskRefreshKind, Disks, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

struct State {
    system: System,
    disks: Disks,
    host: Option<(Instant, HostUsage)>,
    table: Option<(Instant, Arc<Vec<ProcessRow>>)>,
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
            state: Arc::new(Mutex::new(State {
                system,
                disks,
                host: None,
                table: None,
            })),
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
            if let Some((at, usage)) = state.host
                && at.elapsed() < LOCAL_SAMPLE_AGE
            {
                return Ok(usage);
            }
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
            let usage = HostUsage {
                cpu_percent: state.system.global_cpu_usage(),
                memory_used: state.system.used_memory(),
                memory_total: state.system.total_memory(),
                disk_used,
                disk_total,
            };
            state.host = Some((Instant::now(), usage));
            Ok(usage)
        })
        .await
        .context("Usage reading stopped")?
    }

    /// Every process of the system with its parent, CPU and memory.
    pub async fn processes(&self) -> Result<Arc<Vec<ProcessRow>>> {
        let state = self.state.clone();
        tokio::task::spawn_blocking(move || {
            let mut state = Self::lock(&state);
            if let Some((at, table)) = &state.table
                && at.elapsed() < LOCAL_SAMPLE_AGE
            {
                return table.clone();
            }
            // The command line is read once per process; dead processes are
            // dropped on each refresh, so a reused pid is read anew unless
            // it is reused between two refreshes (rare, and only the
            // app-server mark could then be stale).
            state.system.refresh_processes_specifics(
                ProcessesToUpdate::All,
                true,
                ProcessRefreshKind::nothing()
                    .with_cpu()
                    .with_memory()
                    .with_cmd(UpdateKind::OnlyIfNotSet),
            );
            let table: Arc<Vec<ProcessRow>> = Arc::new(
                state
                    .system
                    .processes()
                    .values()
                    .map(|process| {
                        let command: Vec<String> = process
                            .cmd()
                            .iter()
                            .map(|a| a.to_string_lossy().into_owned())
                            .collect();
                        ProcessRow {
                            pid: process.pid().as_u32(),
                            parent: process.parent().map(|p| p.as_u32()),
                            cpu_percent: process.cpu_usage(),
                            memory: process.memory(),
                            app_server: is_app_server(&command),
                        }
                    })
                    .collect(),
            );
            state.table = Some((Instant::now(), table.clone()));
            table
        })
        .await
        .context("Usage reading stopped")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /* REQ-029: This machine's usage is read: some memory and a home disk, and
    this test's own process with its memory and parent; a second reading
    within the sample age is the same reading. */
    #[tokio::test]
    async fn reads_this_machine_and_process() {
        let sampler = Sampler::new(std::env::temp_dir()).unwrap();
        let host = sampler.host().await.unwrap();
        assert!(host.memory_total > 0 && host.memory_used <= host.memory_total);
        assert!(host.disk_total > 0 && host.disk_used <= host.disk_total);
        assert!((0.0..=100.0).contains(&host.cpu_percent), "{host:?}");
        assert_eq!(sampler.host().await.unwrap(), host);
        let own = std::process::id();
        let table = sampler.processes().await.unwrap();
        let row = table.iter().find(|r| r.pid == own).expect("own process");
        assert!(row.memory > 0 && !row.app_server);
        assert!(row.parent.is_some());
        assert!(Arc::ptr_eq(&table, &sampler.processes().await.unwrap()));
    }
}
