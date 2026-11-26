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
        return String::from_utf8(output.stdout)
            .map_err(|_| anyhow::anyhow!("The crontab is not valid UTF-8; not changing it"));
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /* REQ-025: A table that is not valid UTF-8 is never rewritten; a missing table is empty. */
    #[tokio::test]
    async fn reads_only_utf8_tables() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("crontab.sh");
        std::fs::write(&script, "#!/bin/sh\nprintf '# caf\\351\\n'\n").unwrap();
        let none = dir.path().join("none.sh");
        std::fs::write(
            &none,
            "#!/bin/sh\necho 'crontab: no crontab for me' >&2\nexit 1\n",
        )
        .unwrap();
        for path in [&script, &none] {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let error = read(&script.to_string_lossy())
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("not valid UTF-8"), "{error}");
        assert_eq!(read(&none.to_string_lossy()).await.unwrap(), "");
    }
}
