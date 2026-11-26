//! Starts a queued message in a live Codex thread (REQ-015): Codex runs a
//! queued message when its thread goes idle, but not after the user
//! interrupted the last turn. The bridge then asks the user's Codex app
//! server (its control socket, as the TUI uses it) for `thread/queue/start`.
//! Codex itself refuses an unloaded thread (no headless run) and a thread
//! with an active turn; approvals stay in Codex. Building and reading the
//! messages is pure; the socket is infra.
use anyhow::Result;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// The queued submission id `codex queue` prints:
/// "Queued message <id> for thread <thread>."
pub fn queued_submission(stdout: &str) -> Option<String> {
    stdout.lines().find_map(|line| {
        let rest = line.trim().strip_prefix("Queued message ")?;
        let (id, _) = rest.split_once(" for thread ")?;
        (!id.is_empty() && !id.contains(char::is_whitespace)).then(|| id.to_owned())
    })
}

/// The control socket of the user's Codex app server.
pub fn control_socket(codex_home: &Path) -> PathBuf {
    codex_home
        .join("app-server-control")
        .join("app-server-control.sock")
}

pub fn initialize(version: &str) -> Value {
    json!({"id": 1, "method": "initialize", "params": {
        "clientInfo": {"name": "session-bridge", "title": null, "version": version},
        "capabilities": {"experimentalApi": true}
    }})
}
pub fn initialized() -> Value {
    json!({"method": "initialized"})
}
/// Starts `item`, or the head of the queue when `None`.
pub fn queue_start(thread: &Uuid, item: Option<&str>) -> Value {
    json!({"id": 2, "method": "thread/queue/start", "params": {
        "threadId": thread.to_string(), "queuedSubmissionId": item
    }})
}

/// What Codex answered to a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    Ok,
    Refused(String),
}
/// The answer to request `id` in `message`, if it is that answer.
pub fn answer_to(id: u64, message: &Value) -> Option<Answer> {
    if message.get("id").and_then(Value::as_u64) != Some(id) {
        return None;
    }
    Some(match message.get("error") {
        Some(error) => Answer::Refused(
            error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("refused")
                .to_owned(),
        ),
        None => Answer::Ok,
    })
}

/// Asks the app server on `socket` to start a queued message of `thread`.
#[cfg(unix)]
pub async fn start(socket: &Path, thread: &Uuid, item: Option<&str>) -> Result<Answer> {
    use anyhow::Context;
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;
    let run = async {
        let stream = tokio::net::UnixStream::connect(socket)
            .await
            .with_context(|| format!("No Codex app server at {}", socket.display()))?;
        let (mut ws, _) = tokio_tungstenite::client_async("ws://localhost/", stream)
            .await
            .context("Codex app server refused the connection")?;
        let mut ask = async |message: Value, id: Option<u64>| -> Result<Option<Answer>> {
            ws.send(Message::text(message.to_string())).await?;
            let Some(id) = id else { return Ok(None) };
            while let Some(frame) = ws.next().await {
                if let Message::Text(text) = frame?
                    && let Ok(value) = serde_json::from_str::<Value>(&text)
                    && let Some(answer) = answer_to(id, &value)
                {
                    return Ok(Some(answer));
                }
            }
            anyhow::bail!("Codex app server closed the connection")
        };
        if let Some(Answer::Refused(why)) =
            ask(initialize(env!("CARGO_PKG_VERSION")), Some(1)).await?
        {
            anyhow::bail!("Codex app server refused the bridge: {why}");
        }
        ask(initialized(), None).await?;
        ask(queue_start(thread, item), Some(2))
            .await?
            .context("No answer from the Codex app server")
    };
    tokio::time::timeout(std::time::Duration::from_secs(5), run)
        .await
        .context("Codex app server did not answer in time")?
}
#[cfg(not(unix))]
pub async fn start(_socket: &Path, _thread: &Uuid, _item: Option<&str>) -> Result<Answer> {
    anyhow::bail!("Waking Codex threads is not available on this system")
}

#[cfg(test)]
mod tests {
    use super::*;

    /* REQ-015: The bridge reads the queued id `codex queue` prints and asks
    for exactly that item, or the head; Codex's answers read as started or
    refused with its reason. */
    #[test]
    fn builds_and_reads_app_server_messages() {
        assert_eq!(
            queued_submission("Queued message 0199-abc for thread 01a2.\n").as_deref(),
            Some("0199-abc")
        );
        assert_eq!(
            queued_submission("Warning: x\nQueued message q1 for thread t."),
            Some("q1".into())
        );
        assert_eq!(queued_submission("nothing"), None);
        let thread = Uuid::nil();
        let start = queue_start(&thread, Some("q1"));
        assert_eq!(start["method"], "thread/queue/start");
        assert_eq!(start["params"]["threadId"], thread.to_string());
        assert_eq!(start["params"]["queuedSubmissionId"], "q1");
        assert_eq!(
            queue_start(&thread, None)["params"]["queuedSubmissionId"],
            Value::Null
        );
        assert_eq!(
            initialize("1")["params"]["capabilities"]["experimentalApi"],
            true
        );
        assert_eq!(
            answer_to(2, &json!({"id": 2, "result": {"turn": {}}})),
            Some(Answer::Ok)
        );
        assert_eq!(
            answer_to(
                2,
                &json!({"id": 2, "error": {"code": -32600, "message": "resume the thread before starting a queued message"}})
            ),
            Some(Answer::Refused(
                "resume the thread before starting a queued message".into()
            ))
        );
        assert_eq!(
            answer_to(2, &json!({"method": "thread/queue/changed"})),
            None
        );
        assert_eq!(answer_to(2, &json!({"id": 1, "result": {}})), None);
        assert_eq!(
            control_socket(Path::new("/h/.codex")),
            PathBuf::from("/h/.codex/app-server-control/app-server-control.sock")
        );
    }
}
