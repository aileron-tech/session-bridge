//! Reads and writes the user crontab with the configured program (`crontab`
//! by default; tests use a stand-in so the user's table is never touched).
use anyhow::{Context, Result};
use std::process::Stdio;
use tokio::{io::AsyncWriteExt, process::Command};

/// The current table; empty when the user has none.
pub async fn read(program: &str) -> Result<String> {
    let output = Command::new(program)
        .arg("-l")
        .stdin(Stdio::null())
        .output()
        .await
        .with_context(|| format!("Cannot run {program}"))?;
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
    }
    let error = String::from_utf8_lossy(&output.stderr);
    if error.to_lowercase().contains("no crontab") {
        return Ok(String::new());
    }
    anyhow::bail!("{program} -l failed: {}", error.trim())
}

/// Replaces the table with `text`.
pub async fn write(program: &str, text: &str) -> Result<()> {
    let mut child = Command::new(program)
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("Cannot run {program}"))?;
    let mut stdin = child.stdin.take().context("No stdin for crontab")?;
    stdin.write_all(text.as_bytes()).await?;
    drop(stdin);
    let output = child.wait_with_output().await?;
    anyhow::ensure!(
        output.status.success(),
        "{program} rejected the table: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(())
}
