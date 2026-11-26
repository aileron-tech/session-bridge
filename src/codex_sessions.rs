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
        Ok(Ok(output)) if output.status.success() => {
            let printed = String::from_utf8_lossy(&output.stdout);
            if let (Ok(thread), Some(item)) = (
                uuid::Uuid::parse_str(thread),
                crate::codex_wake::queued_submission(&printed),
            ) {
                wake(home, &thread, Some(&item)).await;
            }
            Delivery::Queued
        }
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

/// Asks Codex to start a queued message now when the live thread is idle,
/// also after the user interrupted it (REQ-015). Codex refuses an unloaded
/// thread or one with an active turn; the message then stays queued and
/// Codex runs it when the thread goes idle. Never changes the delivery state.
pub async fn wake(home: &Path, thread: &uuid::Uuid, item: Option<&str>) {
    let socket = crate::codex_wake::control_socket(home);
    match crate::codex_wake::start(&socket, thread, item).await {
        Ok(crate::codex_wake::Answer::Ok) => {
            eprintln!("Started the queued message in Codex thread {thread}");
        }
        Ok(crate::codex_wake::Answer::Refused(why)) => {
            eprintln!("Codex thread {thread} keeps its queue: {why}");
        }
        Err(e) => eprintln!("Cannot ask Codex to start thread {thread}: {e:#}"),
    }
}
