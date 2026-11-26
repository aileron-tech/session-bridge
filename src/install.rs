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
pub async fn install(config: &Config, home: &Path, claude: &Path) -> Result<()> {
    store::Store::open(config.state.clone())?;
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
        let mut codex_config =
            fs::read_to_string(&codex_config_path)?.parse::<toml_edit::DocumentMut>()?;
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
