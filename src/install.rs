use crate::{daemon::Config, platform, store};
use anyhow::{Context, Result};
use std::{fs, path::Path};
use tokio::process::Command;

#[cfg(unix)]
#[path = "install/launchd.rs"]
mod service;
#[cfg(windows)]
#[path = "install/task_scheduler.rs"]
mod service;
pub use service::start as start_service;
pub use service::{EXECUTABLE, replace_executable};

/// Whether the background service runs the bridge for this home.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceState {
    Installed,
    NotInstalled,
    /// Registered for another home; never touched from here.
    Elsewhere(String),
}
pub use service::state as service_state;

/// Stops this home's bridge (REQ-022): the installed service through its
/// supervisor (so it is not restarted at once), otherwise with a shutdown
/// request. Returns what happened, for the user.
pub async fn stop(config: &Config, home: &Path) -> Result<String> {
    if config.supervised && service::state(home).await? == ServiceState::Installed {
        service::stop(config).await?;
        return Ok(format!(
            "Stopped the background service ({}). It starts again at next login, or now with: session-bridge install",
            service::SUPERVISOR
        ));
    }
    let Ok(mut stream) = crate::transport::connect(&config.socket()).await else {
        return Ok(format!(
            "No bridge is running for {}",
            config.state.display()
        ));
    };
    crate::transport::write_frame(&mut stream, &crate::protocol::Request::Shutdown).await?;
    let crate::protocol::Reply::ShuttingDown { pid } =
        crate::transport::read_frame(&mut stream).await?
    else {
        anyhow::bail!("The bridge did not accept the shutdown");
    };
    // The endpoint closes as the bridge finishes; a parent that has not yet
    // reaped the process would still report its pid as alive.
    for _ in 0..100 {
        if crate::transport::connect(&config.socket()).await.is_err() {
            return Ok(format!("Stopped the bridge for {}", config.state.display()));
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    anyhow::bail!("Bridge process {pid} is still running after 10 seconds")
}

/// Install may replace this home's service, never another home's: the label
/// (macOS) and task name (Windows) are global per user (REQ-022).
pub fn may_install(state: &ServiceState) -> Result<()> {
    match state {
        ServiceState::Elsewhere(owner) => anyhow::bail!(
            "A session-bridge service that belongs to another home is installed ({owner}); uninstall it there first"
        ),
        ServiceState::Installed | ServiceState::NotInstalled => Ok(()),
    }
}

/// What `uninstall` found for this home (REQ-021).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unregistered {
    Removed,
    NotInstalled,
    /// A service with this label is registered for another home; left alone.
    Elsewhere(String),
}

/// Removes the background service registered for this home. The executable,
/// MCP entries and state stay, so the bridge can still run from a shell.
pub async fn uninstall(config: &Config, home: &Path) -> Result<()> {
    default_state_only(config)?;
    match service::unregister(config, home).await? {
        Unregistered::Removed => println!(
            "Removed the background service ({}). The executable, MCP entries, pairing and messages stay. Start the bridge in a shell with: session-bridge daemon, or reinstall with: session-bridge install",
            service::SUPERVISOR
        ),
        Unregistered::NotInstalled => {
            println!("No background service is installed for this home.")
        }
        Unregistered::Elsewhere(owner) => println!(
            "No background service is installed for this home; the registered one belongs to {owner} and was left alone."
        ),
    }
    Ok(())
}

pub(crate) async fn run(program: &Path, args: &[&str]) -> Result<()> {
    let output = Command::new(program).args(args).output().await?;
    anyhow::ensure!(
        output.status.success(),
        "{} failed: {}{}",
        program.display(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    eprintln!("{}", String::from_utf8_lossy(&output.stdout).trim());
    Ok(())
}
fn has_matching_claude_server(path: &Path, executable: &Path) -> Result<bool> {
    let raw = match fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    let config: serde_json::Value = serde_json::from_str(&raw)?;
    let server = &config["mcpServers"]["session-bridge"];
    Ok(server["type"] == "stdio"
        && server["command"].as_str() == executable.to_str()
        && server["args"] == serde_json::json!(["mcp"]))
}
/// Install, uninstall and update act on the service, which serves the default
/// state; a separate bridge (SESSION_BRIDGE_STATE) never touches it.
pub fn default_state_only(config: &Config) -> Result<()> {
    anyhow::ensure!(
        config.supervised,
        "install, uninstall and update manage the service of the default state; unset SESSION_BRIDGE_STATE"
    );
    Ok(())
}

pub async fn install(config: &Config, home: &Path, claude: &Path) -> Result<()> {
    default_state_only(config)?;
    may_install(&service::state(home).await?)?;
    store::ensure_private_dir(&config.state)?;
    let bin = home.join(".local").join("bin");
    fs::create_dir_all(&bin)?;
    let executable = bin.join(service::EXECUTABLE);
    let source = std::env::current_exe()?;
    service::prepare(config).await?;
    if source != executable {
        let temporary = bin.join("session-bridge.installing");
        fs::copy(&source, &temporary)?;
        platform::set_mode(&temporary, 0o755)?;
        service::replace_executable(&temporary, &executable)?;
    }
    let backup = config.state.join("install-backups");
    store::ensure_private_dir(&backup)?;
    for path in [
        config.sources.codex_home.join("config.toml"),
        home.join(".claude.json"),
        service::definition(home),
    ] {
        if path.exists() {
            let dest = backup.join(format!(
                "{}-{}",
                uuid::Uuid::new_v4(),
                path.file_name()
                    .context("No config name")?
                    .to_string_lossy()
            ));
            fs::copy(&path, &dest)?;
            platform::set_mode(&dest, 0o600)?;
        }
    }
    service::register(config, home, &executable).await?;
    run(
        &config.codex,
        &[
            "mcp",
            "add",
            "session-bridge",
            "--",
            &executable.to_string_lossy(),
            "mcp",
        ],
    )
    .await?;
    if service::CODEX_REQUIRED {
        let codex_config_path = config.sources.codex_home.join("config.toml");
        let mut codex_config = fs::read_to_string(&codex_config_path)
            .with_context(|| format!("Reading {}", codex_config_path.display()))?
            .parse::<toml_edit::DocumentMut>()?;
        codex_config["mcp_servers"]["session-bridge"]["required"] = toml_edit::value(true);
        fs::write(&codex_config_path, codex_config.to_string())?;
    }
    if !has_matching_claude_server(&home.join(".claude.json"), &executable)? {
        run(
            claude,
            &[
                "mcp",
                "add",
                "--scope",
                "user",
                "session-bridge",
                "--",
                &executable.to_string_lossy(),
                "mcp",
            ],
        )
        .await?;
    }
    crate::client::ensure_running(config).await?;
    println!(
        "Installed {}. Global MCP loads at Codex/Claude startup; {} supervises the queue service.",
        executable.display(),
        service::SUPERVISOR
    );
    Ok(())
}

#[cfg(test)]
mod guard_tests {
    use super::*;

    /* REQ-022: Install replaces only this home's service; another home's is refused. */
    #[test]
    fn refuses_another_homes_service() {
        assert!(may_install(&ServiceState::NotInstalled).is_ok());
        assert!(may_install(&ServiceState::Installed).is_ok());
        let refused = may_install(&ServiceState::Elsewhere("/Users/other/x.plist".into()))
            .unwrap_err()
            .to_string();
        assert!(refused.contains("belongs to another home"), "{refused}");
    }
}
