//! macOS supervision: a per-user LaunchAgent started at login and kept alive.
use super::run;
use crate::daemon::Config;
use anyhow::Result;
use std::{
    fs,
    path::{Path, PathBuf},
};
use tokio::process::Command;

pub const LABEL: &str = "com.aileron-tech.session-bridge";
/// The label used before the public release; retired on install.
const LEGACY_LABEL: &str = "fr.aileron.session-bridge";
pub const EXECUTABLE: &str = "session-bridge";
pub const SUPERVISOR: &str = "launchd";
/// Codex reports a startup error when a required MCP server cannot connect.
pub const CODEX_REQUIRED: bool = true;

fn xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
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
pub fn definition(home: &Path) -> PathBuf {
    home.join("Library/LaunchAgents")
        .join(format!("{LABEL}.plist"))
}
pub async fn prepare(_config: &Config) -> Result<()> {
    Ok(())
}
pub fn replace_executable(temporary: &Path, executable: &Path) -> Result<()> {
    Ok(fs::rename(temporary, executable)?)
}
pub async fn register(config: &Config, home: &Path, executable: &Path) -> Result<()> {
    let plist = definition(home);
    fs::create_dir_all(plist.parent().expect("LaunchAgents directory"))?;
    let text = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>{LABEL}</string>
<key>ProgramArguments</key><array><string>{exe}</string><string>daemon</string></array>
<key>RunAtLoad</key><true/><key>KeepAlive</key><true/>
<key>ThrottleInterval</key><integer>5</integer><key>Umask</key><integer>63</integer>
<key>WorkingDirectory</key><string>{home}</string>
<key>EnvironmentVariables</key><dict><key>SESSION_BRIDGE_TRAY</key><string>1</string><key>HOME</key><string>{home}</string><key>PATH</key><string>{path}</string><key>CODEX_HOME</key><string>{codex_home}</string><key>CLAUDE_CONFIG_DIR</key><string>{claude_home}</string><key>SESSION_BRIDGE_CODEX_BIN</key><string>{codex}</string></dict>
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
    crate::platform::set_mode(&plist, 0o600)?;
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
    let retired = retire_legacy(config, home, &domain).await?;
    bootstrap(&domain, &plist, installed || retired).await?;
    Ok(())
}
/// Unload the agent registered under the previous label and remove its
/// definition, so that exactly one agent owns the state directory.
async fn retire_legacy(config: &Config, home: &Path, domain: &str) -> Result<bool> {
    let service = format!("{domain}/{LEGACY_LABEL}");
    let loaded = Command::new("/bin/launchctl")
        .args(["print", &service])
        .output()
        .await?
        .status
        .success();
    if loaded {
        let running = crate::client::service_pid(config).await.ok();
        run(Path::new("/bin/launchctl"), &["bootout", &service]).await?;
        // The new agent must not start while the old process holds the lock.
        if let Some(pid) = running {
            for _ in 0..100 {
                if !crate::platform::is_alive(pid) {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
            anyhow::ensure!(
                !crate::platform::is_alive(pid),
                "Previous service process {pid} did not stop"
            );
        }
    }
    let plist = home
        .join("Library/LaunchAgents")
        .join(format!("{LEGACY_LABEL}.plist"));
    match fs::remove_file(&plist) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(loaded)
}
/// The definition file of a loaded agent, from `launchctl print` output.
pub fn loaded_definition(print: &str) -> Option<PathBuf> {
    print
        .lines()
        .find_map(|line| line.trim().strip_prefix("path = "))
        .map(PathBuf::from)
}

fn service_target() -> String {
    format!("gui/{}/{LABEL}", unsafe { libc::geteuid() })
}

/// Whether launchd runs the agent defined for this home. An agent with this
/// label loaded from another home's file belongs to someone else.
pub async fn state(home: &Path) -> Result<super::ServiceState> {
    let print = Command::new("/bin/launchctl")
        .args(["print", &service_target()])
        .output()
        .await?;
    let loaded = print
        .status
        .success()
        .then(|| loaded_definition(&String::from_utf8_lossy(&print.stdout)))
        .flatten();
    Ok(match loaded {
        None => super::ServiceState::NotInstalled,
        Some(path) if path == definition(home) => super::ServiceState::Installed,
        Some(path) => super::ServiceState::Elsewhere(path.display().to_string()),
    })
}

/// Unloads this home's agent, which stops the bridge until the next login.
pub async fn stop(_config: &Config) -> Result<()> {
    run(Path::new("/bin/launchctl"), &["bootout", &service_target()]).await
}

/// Unloads (and so stops) the agent registered for this home and removes its
/// definition. An agent with this label loaded from another home's file is
/// left alone, so a test or a second account never stops the user's service.
pub async fn unregister(config: &Config, home: &Path) -> Result<super::Unregistered> {
    let loaded = match state(home).await? {
        super::ServiceState::Elsewhere(owner) => {
            return Ok(super::Unregistered::Elsewhere(owner));
        }
        super::ServiceState::Installed => true,
        super::ServiceState::NotInstalled => false,
    };
    if loaded {
        stop(config).await?;
    }
    let plist = definition(home);
    let existed = plist.exists();
    if existed {
        fs::remove_file(&plist)?;
    }
    Ok(if loaded || existed {
        super::Unregistered::Removed
    } else {
        super::Unregistered::NotInstalled
    })
}

pub async fn start() -> Result<()> {
    let service = format!("gui/{}/{}", unsafe { libc::geteuid() }, LABEL);
    let output = Command::new("/bin/launchctl")
        .args(["kickstart", &service])
        .output()
        .await?;
    anyhow::ensure!(
        output.status.success(),
        "Bridge is unavailable. Run session-bridge install, or start it in a shell with: session-bridge daemon. launchctl: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /* REQ-021: Uninstall recognizes which definition file the loaded agent came from. */
    #[test]
    fn reads_the_loaded_definition() {
        let print = "gui/501/com.aileron-tech.session-bridge = {\n\tactive count = 1\n\tpath = /Users/me/Library/LaunchAgents/com.aileron-tech.session-bridge.plist\n\ttype = LaunchAgent\n}";
        assert_eq!(
            loaded_definition(print),
            Some(PathBuf::from(
                "/Users/me/Library/LaunchAgents/com.aileron-tech.session-bridge.plist"
            ))
        );
        assert_eq!(loaded_definition("state = running"), None);
    }
}
