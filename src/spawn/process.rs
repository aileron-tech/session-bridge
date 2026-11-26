//! Processes of started sessions (REQ-028): parents, command lines, ending
//! a process, and a launch becoming its agent.
use anyhow::{Context, Result};
#[cfg(unix)]
use std::time::Duration;
use std::{collections::BTreeMap, path::Path};
use tokio::process::Command;
#[cfg(unix)]
use tokio::time::timeout;

#[cfg(unix)]
async fn ps(args: &[&str]) -> Result<String> {
    let output = timeout(
        Duration::from_secs(5),
        Command::new("/bin/ps")
            .args(args)
            .kill_on_drop(true)
            .output(),
    )
    .await
    .context("ps timed out")??;
    anyhow::ensure!(output.status.success(), "ps found no such process");
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Every process's parent: pid to parent pid.
#[cfg(unix)]
pub async fn parent_pids() -> Result<BTreeMap<u32, u32>> {
    Ok(ps(&["-axo", "pid=,ppid="])
        .await?
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            Some((fields.next()?.parse().ok()?, fields.next()?.parse().ok()?))
        })
        .collect())
}
#[cfg(not(unix))]
pub async fn parent_pids() -> Result<BTreeMap<u32, u32>> {
    Ok(BTreeMap::new())
}

/// The command line of `pid`.
#[cfg(unix)]
pub async fn command_line(pid: u32) -> Result<String> {
    Ok(ps(&["-o", "args=", "-p", &pid.to_string()])
        .await
        .with_context(|| format!("Process {pid} has ended"))?
        .trim()
        .to_owned())
}
#[cfg(not(unix))]
pub async fn command_line(_pid: u32) -> Result<String> {
    Ok(String::new())
}

/// Asks `pid` to end (SIGTERM; on Windows, taskkill).
#[cfg(unix)]
pub async fn terminate(pid: u32) -> Result<()> {
    let pid = libc::pid_t::try_from(pid).context("Invalid process id")?;
    anyhow::ensure!(pid > 1, "Refusing to signal process {pid}");
    // SAFETY: kill(2) only sends a signal to a validated process id.
    if unsafe { libc::kill(pid, libc::SIGTERM) } != 0 {
        return Err(std::io::Error::last_os_error()).context("Could not end the session");
    }
    Ok(())
}
#[cfg(not(unix))]
pub async fn terminate(pid: u32) -> Result<()> {
    let output = Command::new("taskkill")
        .args(["/PID", &pid.to_string()])
        .output()
        .await?;
    anyhow::ensure!(
        output.status.success(),
        "Could not end the session: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(())
}

/// Becomes `program args` in `cwd` (Unix: exec, so the process id stays the
/// one the bridge bound); on Windows runs it and exits with its code.
pub fn become_agent(program: &Path, args: &[String], cwd: &Path) -> Result<()> {
    let mut command = std::process::Command::new(program);
    command.args(args).current_dir(cwd);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let error = command.exec();
        Err(error).with_context(|| format!("Could not start {}", program.display()))
    }
    #[cfg(not(unix))]
    {
        let status = command
            .status()
            .with_context(|| format!("Could not start {}", program.display()))?;
        std::process::exit(status.code().unwrap_or(1));
    }
}
