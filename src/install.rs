use crate::{client::LABEL, daemon::Config, store};
use anyhow::{Context, Result};
use std::{fs, os::unix::fs::PermissionsExt, path::Path};
use tokio::process::Command;

fn xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
async fn run(program: &Path, args: &[&str]) -> Result<()> {
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
async fn bootstrap(domain: &str, plist: &Path, replacing: bool) -> Result<()> {
    for attempt in 0..10 {
        let output = Command::new("/bin/launchctl")
            .args(["bootstrap", domain, &plist.to_string_lossy()])
            .output()
            .await?;
        if output.status.success() {
            return Ok(());
        }
        let error = String::from_utf8_lossy(&output.stderr);
        // bootout can return before launchd finishes removing the old job.
        if replacing && output.status.code() == Some(5) && attempt < 9 {
            eprintln!("Waiting for launchd teardown before registration: {error}");
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        } else {
            anyhow::bail!("launchctl bootstrap failed: {error}");
        }
    }
    unreachable!("the final bootstrap attempt returns its result")
}
pub async fn install(config: &Config, home: &Path, claude: &Path) -> Result<()> {
    store::Store::open(config.state.clone())?;
    let bin = home.join(".local/bin");
    fs::create_dir_all(&bin)?;
    let executable = bin.join("session-bridge");
    let source = std::env::current_exe()?;
    if source != executable {
        let temporary = bin.join("session-bridge.installing");
        fs::copy(&source, &temporary)?;
        fs::set_permissions(&temporary, fs::Permissions::from_mode(0o755))?;
        fs::rename(temporary, &executable)?;
    }
    let agents = home.join("Library/LaunchAgents");
    fs::create_dir_all(&agents)?;
    let plist = agents.join(format!("{LABEL}.plist"));
    let backup = config.state.join("install-backups");
    store::ensure_private_dir(&backup)?;
    for path in [
        config.sources.codex_home.join("config.toml"),
        home.join(".claude.json"),
        plist.clone(),
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
            fs::set_permissions(dest, fs::Permissions::from_mode(0o600))?;
        }
    }
    let text = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>{LABEL}</string>
<key>ProgramArguments</key><array><string>{exe}</string><string>daemon</string></array>
<key>RunAtLoad</key><true/><key>KeepAlive</key><true/>
<key>ThrottleInterval</key><integer>5</integer><key>Umask</key><integer>63</integer>
<key>WorkingDirectory</key><string>{home}</string>
<key>EnvironmentVariables</key><dict><key>HOME</key><string>{home}</string><key>PATH</key><string>{path}</string><key>CODEX_HOME</key><string>{codex_home}</string><key>CLAUDE_CONFIG_DIR</key><string>{claude_home}</string><key>SESSION_BRIDGE_CODEX_BIN</key><string>{codex}</string></dict>
<key>StandardOutPath</key><string>{state}/daemon.stdout.log</string>
<key>StandardErrorPath</key><string>{state}/daemon.stderr.log</string>
</dict></plist>
"#,
        exe = xml(&executable.to_string_lossy()),
        home = xml(&home.to_string_lossy()),
        path = xml(&std::env::var("PATH")?),
        codex_home = xml(&config.sources.codex_home.to_string_lossy()),
        claude_home = xml(&config.sources.claude_home.to_string_lossy()),
        codex = xml(&config.codex.to_string_lossy()),
        state = xml(&config.state.to_string_lossy())
    );
    fs::write(&plist, text)?;
    fs::set_permissions(&plist, fs::Permissions::from_mode(0o600))?;
    let uid = unsafe { libc::geteuid() };
    let domain = format!("gui/{uid}");
    let service = format!("{domain}/{LABEL}");
    let installed = Command::new("/bin/launchctl")
        .args(["print", &service])
        .output()
        .await?
        .status
        .success();
    if installed {
        run(Path::new("/bin/launchctl"), &["bootout", &service]).await?;
    }
    bootstrap(&domain, &plist, installed).await?;
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
    let codex_config_path = config.sources.codex_home.join("config.toml");
    let mut codex_config =
        fs::read_to_string(&codex_config_path)?.parse::<toml_edit::DocumentMut>()?;
    codex_config["mcp_servers"]["session-bridge"]["required"] = toml_edit::value(true);
    fs::write(&codex_config_path, codex_config.to_string())?;
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
        "Installed {}. Global MCP loads at Codex/Claude startup; launchd supervises the queue service.",
        executable.display()
    );
    Ok(())
}
