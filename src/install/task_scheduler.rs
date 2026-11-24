//! Windows supervision: a per-user Scheduled Task started at logon and
//! restarted after failure. It needs neither elevation nor a stored password.
use super::run;
use crate::{daemon::Config, platform};
use anyhow::Result;
use std::{
    fs,
    path::{Path, PathBuf},
};
use tokio::process::Command;

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
    home.join(".local/state/session-bridge/task.xml")
}
async fn registered() -> Result<bool> {
    Ok(Command::new(SCHTASKS)
        .args(["/Query", "/TN", LABEL])
        .output()
        .await?
        .status
        .success())
}
/// Stop the running service so that its executable can be replaced.
pub async fn prepare() -> Result<()> {
    if registered().await? {
        run(Path::new(SCHTASKS), &["/End", "/TN", LABEL]).await?;
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
pub async fn start() -> Result<()> {
    let output = Command::new(SCHTASKS)
        .args(["/Run", "/TN", LABEL])
        .output()
        .await?;
    anyhow::ensure!(
        output.status.success(),
        "Bridge is unavailable. Run session-bridge install. schtasks: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
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
