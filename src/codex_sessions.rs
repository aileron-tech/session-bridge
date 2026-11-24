//! Deliver through the native CLI; the bridge never owns a Codex runtime.
use crate::domain::Delivery;
use std::{path::Path, process::Stdio, time::Duration};
use tokio::{process::Command, time::timeout};

pub async fn queue(executable: &Path, home: &Path, thread: &str, text: &str) -> Delivery {
    let child = match Command::new(executable)
        .args(["queue", "--thread", thread, "--message", text])
        .env("CODEX_HOME", home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            return Delivery::Pending {
                reason: format!("Cannot start native Codex queue: {error}"),
            };
        }
    };
    match timeout(Duration::from_secs(20), child.wait_with_output()).await {
        Ok(Ok(output)) if output.status.success() => Delivery::Queued,
        Ok(Ok(output)) => Delivery::Uncertain {
            reason: format!(
                "Native Codex queue exited {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        },
        Ok(Err(error)) => Delivery::Uncertain {
            reason: format!("Cannot observe native Codex queue result: {error}"),
        },
        Err(error) => Delivery::Uncertain {
            reason: format!(
                "Native Codex queue timed out; inspect recipient before retrying: {error}"
            ),
        },
    }
}
