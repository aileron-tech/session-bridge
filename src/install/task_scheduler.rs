//! Windows supervision: a per-user Scheduled Task started at logon and
//! restarted after failure. It needs neither elevation nor a stored password.
use super::run;
use crate::{client, daemon::Config, platform};
use anyhow::Result;
use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::{process::Command, time::sleep};

pub const LABEL: &str = "session-bridge";
pub const EXECUTABLE: &str = "session-bridge.exe";
pub const SUPERVISOR: &str = "Task Scheduler";
/// Not required until native delivery is verified on Windows: a required
/// server that fails would block every new Codex session.
pub const CODEX_REQUIRED: bool = false;
const SCHTASKS: &str = "schtasks.exe";

fn xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
/// The task definition; pure so that its content can be tested.
pub fn task_definition(user_sid: &str, executable: &Path, home: &Path) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo><Description>session-bridge messaging service</Description></RegistrationInfo>
  <Triggers><LogonTrigger><Enabled>true</Enabled><UserId>{user}</UserId></LogonTrigger></Triggers>
  <Principals><Principal id="Author"><UserId>{user}</UserId><LogonType>InteractiveToken</LogonType><RunLevel>LeastPrivilege</RunLevel></Principal></Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <AllowHardTerminate>true</AllowHardTerminate>
    <StartWhenAvailable>true</StartWhenAvailable>
    <AllowStartOnDemand>true</AllowStartOnDemand>
    <Enabled>true</Enabled>
    <Hidden>false</Hidden>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <RestartOnFailure><Interval>PT1M</Interval><Count>999</Count></RestartOnFailure>
  </Settings>
  <Actions Context="Author"><Exec><Command>{exe}</Command><Arguments>daemon --background</Arguments><WorkingDirectory>{home}</WorkingDirectory></Exec></Actions>
</Task>
"#,
        user = xml(user_sid),
        exe = xml(&executable.to_string_lossy()),
        home = xml(&home.to_string_lossy())
    )
}
pub fn definition(home: &Path) -> PathBuf {
    home.join(".local")
        .join("state")
        .join("session-bridge")
        .join("task.xml")
}
async fn registered() -> Result<bool> {
    Ok(Command::new(SCHTASKS)
        .args(["/Query", "/TN", LABEL])
        .output()
        .await?
        .status
        .success())
}
async fn exited(pid: u32) -> bool {
    for _ in 0..50 {
        if !platform::is_alive(pid) {
            return true;
        }
        sleep(Duration::from_millis(100)).await;
    }
    false
}
/// Stop the running service so that the new executable serves the next start.
/// Ending the task does not reliably end a windowless process, so the process
/// the control endpoint reports is awaited and then terminated.
pub async fn prepare(config: &Config) -> Result<()> {
    let running = client::service_pid(config).await.ok();
    if registered().await? {
        run(Path::new(SCHTASKS), &["/End", "/TN", LABEL]).await?;
    }
    if let Some(pid) = running
        && !exited(pid).await
    {
        run(Path::new("taskkill.exe"), &["/PID", &pid.to_string(), "/F"]).await?;
        anyhow::ensure!(exited(pid).await, "Service process {pid} did not stop");
    }
    Ok(())
}
/// A running executable cannot be overwritten on Windows, but it can be
/// renamed: move it aside, then move the new one into place.
pub fn replace_executable(temporary: &Path, executable: &Path) -> Result<()> {
    if let Some(directory) = executable.parent() {
        for entry in fs::read_dir(directory)? {
            let path = entry?.path();
            let stale = path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("session-bridge.old-"));
            if stale && let Err(error) = fs::remove_file(&path) {
                eprintln!(
                    "Previous executable still in use {}: {error}",
                    path.display()
                );
            }
        }
    }
    if executable.exists() {
        fs::rename(
            executable,
            executable.with_extension(format!("old-{}.exe", uuid::Uuid::new_v4())),
        )?;
    }
    Ok(fs::rename(temporary, executable)?)
}
pub async fn register(_config: &Config, home: &Path, executable: &Path) -> Result<()> {
    let text = task_definition(&platform::current_user_sid()?, executable, home);
    // schtasks reads UTF-16 with a byte-order mark.
    let bytes: Vec<u8> = std::iter::once(0xFEFF)
        .chain(text.encode_utf16())
        .flat_map(u16::to_le_bytes)
        .collect();
    let path = definition(home);
    fs::write(&path, bytes)?;
    run(
        Path::new(SCHTASKS),
        &[
            "/Create",
            "/F",
            "/TN",
            LABEL,
            "/XML",
            &path.to_string_lossy(),
        ],
    )
    .await
}
/// Task XML from `schtasks /Query /XML`, which may be UTF-16.
fn decode(bytes: &[u8]) -> String {
    if bytes.len() >= 2 && bytes.iter().skip(1).step_by(2).take(8).all(|b| *b == 0) {
        let units: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    }
}
/// Whether a registered task runs the bridge for `home`.
pub fn task_belongs_to(task_xml: &str, home: &Path) -> bool {
    task_xml.contains(&format!(
        "<WorkingDirectory>{}</WorkingDirectory>",
        xml(&home.to_string_lossy())
    ))
}

/// Whether the logon task runs the bridge for this home.
pub async fn state(home: &Path) -> Result<super::ServiceState> {
    let query = Command::new(SCHTASKS)
        .args(["/Query", "/TN", LABEL, "/XML"])
        .output()
        .await?;
    Ok(if !query.status.success() {
        super::ServiceState::NotInstalled
    } else if task_belongs_to(&decode(&query.stdout), home) {
        super::ServiceState::Installed
    } else {
        super::ServiceState::Elsewhere("another user profile".into())
    })
}

/// Ends the task's process; the task stays registered and starts at logon.
pub async fn stop(config: &Config) -> Result<()> {
    prepare(config).await
}

/// Ends and deletes the logon task registered for this home and stops its
/// process. A task with this name registered for another home is left alone.
pub async fn unregister(config: &Config, home: &Path) -> Result<super::Unregistered> {
    let existed = definition(home).exists();
    match state(home).await? {
        super::ServiceState::NotInstalled => {
            if existed {
                fs::remove_file(definition(home))?;
            }
            return Ok(super::Unregistered::NotInstalled);
        }
        super::ServiceState::Elsewhere(owner) => {
            return Ok(super::Unregistered::Elsewhere(owner));
        }
        super::ServiceState::Installed => {}
    }
    prepare(config).await?;
    run(Path::new(SCHTASKS), &["/Delete", "/TN", LABEL, "/F"]).await?;
    if existed {
        fs::remove_file(definition(home))?;
    }
    Ok(super::Unregistered::Removed)
}

pub async fn start() -> Result<()> {
    let output = Command::new(SCHTASKS)
        .args(["/Run", "/TN", LABEL])
        .output()
        .await?;
    anyhow::ensure!(
        output.status.success(),
        "Bridge is unavailable. Run session-bridge install, or start it in a shell with: session-bridge daemon. schtasks: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /* REQ-021: Uninstall touches only the task registered for this home, in UTF-8 or UTF-16 output. */
    #[test]
    fn recognizes_this_homes_task() {
        let task = task_definition(
            "S-1",
            Path::new(r"C:\Users\R&D\.local\bin\session-bridge.exe"),
            Path::new(r"C:\Users\R&D"),
        );
        assert!(task_belongs_to(&task, Path::new(r"C:\Users\R&D")));
        assert!(!task_belongs_to(&task, Path::new(r"C:\Users\Other")));
        let wide: Vec<u8> = task.encode_utf16().flat_map(u16::to_le_bytes).collect();
        assert!(task_belongs_to(&decode(&wide), Path::new(r"C:\Users\R&D")));
        assert_eq!(decode(b"<Task/>"), "<Task/>");
    }
    /* REQ-016: The logon task runs the service as the installing user, restarts
    it after failure, and escapes paths in its definition. */
    #[test]
    fn task_definition_supervises_the_user_service() {
        let text = task_definition(
            "S-1-5-21-1-2-3-1001",
            Path::new(r"C:\Users\R&D\.local\bin\session-bridge.exe"),
            Path::new(r"C:\Users\R&D"),
        );
        assert_eq!(
            text.matches("<UserId>S-1-5-21-1-2-3-1001</UserId>").count(),
            2
        );
        assert!(
            text.contains(r"<Command>C:\Users\R&amp;D\.local\bin\session-bridge.exe</Command>")
        );
        assert!(text.contains("<Arguments>daemon --background</Arguments>"));
        assert!(text.contains("<LogonTrigger>"));
        assert!(text.contains("<RunLevel>LeastPrivilege</RunLevel>"));
        assert!(text.contains("<RestartOnFailure><Interval>PT1M</Interval>"));
        assert!(text.contains("<ExecutionTimeLimit>PT0S</ExecutionTimeLimit>"));
    }
}
