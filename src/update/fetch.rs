//! Downloads and installation of an update (REQ-023), with the system curl
//! (macOS, Windows 10 and later). A binary whose SHA-256 differs from the
//! release's SHA256SUMS is deleted, never installed.
use super::{Plan, Release, checksum_for, plan};
use crate::{
    daemon::Config,
    install::{self, ServiceState},
    platform, store,
};
use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::{fs, path::Path};
use tokio::process::Command;

/// The GitHub API address of the latest release.
pub const LATEST: &str = "https://api.github.com/repos/aileron-tech/session-bridge/releases/latest";
const RUNNING: &str = env!("CARGO_PKG_VERSION");

async fn download(url: &str, to: &Path) -> Result<()> {
    let curl = if cfg!(windows) { "curl.exe" } else { "curl" };
    let output = Command::new(curl)
        .args(["-fsSL", "--max-time", "300", "-A", "session-bridge"])
        .args(["-H", "Accept: application/vnd.github+json", "-o"])
        .arg(to)
        .arg(url)
        .output()
        .await
        .context("curl is needed to download updates")?;
    anyhow::ensure!(
        output.status.success(),
        "Download failed ({url}): {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(())
}

fn workspace(config: &Config) -> Result<std::path::PathBuf> {
    let dir = config.state.join("update");
    store::ensure_private_dir(&dir)?;
    Ok(dir)
}

/// The latest release and what it means for the running version.
pub async fn check(config: &Config) -> Result<Plan> {
    let path = workspace(config)?.join("latest.json");
    download(&config.releases, &path).await?;
    let release: Release = serde_json::from_slice(&fs::read(&path)?)
        .context("The release list is not in the expected format")?;
    Ok(plan(
        RUNNING,
        &release,
        std::env::consts::OS,
        std::env::consts::ARCH,
    )?)
}

/// Installs the latest release when it is newer. With this home's service
/// installed, the new binary installs itself (executable, service definition,
/// MCP entries) and restarts the service; otherwise it replaces the installed
/// executable in `~/.local/bin`.
pub async fn update(config: &Config, home: &Path) -> Result<String> {
    install::default_state_only(config)?;
    let (version, binary, sums) = match check(config).await? {
        Plan::UpToDate { version } => return Ok(format!("session-bridge {version} is up to date")),
        Plan::Update {
            version,
            binary,
            sums,
        } => (version, binary, sums),
    };
    let dir = workspace(config)?;
    let new = dir.join(&binary.name);
    let list = dir.join("SHA256SUMS");
    download(&binary.browser_download_url, &new).await?;
    download(&sums.browser_download_url, &list).await?;
    let expected = checksum_for(&fs::read_to_string(&list)?, &binary.name)
        .with_context(|| format!("SHA256SUMS does not list {}", binary.name))?;
    let actual: [u8; 32] = Sha256::digest(fs::read(&new)?).into();
    if actual != expected {
        fs::remove_file(&new)?;
        anyhow::bail!("Checksum mismatch for {}: not installing it", binary.name);
    }
    platform::set_mode(&new, 0o755)?;
    if config.supervised && install::service_state(home).await? == ServiceState::Installed {
        let status = Command::new(&new).arg("install").status().await?;
        anyhow::ensure!(
            status.success(),
            "Installing {version} failed; the previous version still runs"
        );
        return Ok(format!("Updated to {version}; the service restarted"));
    }
    let bin = home.join(".local").join("bin");
    fs::create_dir_all(&bin)?;
    let executable = bin.join(install::EXECUTABLE);
    install::replace_executable(&new, &executable)?;
    Ok(format!(
        "Updated {} to {version}; restart a bridge that runs from a shell to use it",
        executable.display()
    ))
}
