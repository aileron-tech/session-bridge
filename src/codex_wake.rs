//! Starts a queued message in a live Codex thread (REQ-015): Codex runs a
//! queued message when its thread goes idle, but not after the user
//! interrupted the last turn. The bridge then asks the user's Codex app
//! server (its control socket, as the TUI uses it) for `thread/queue/start`.
//! Codex itself refuses an unloaded thread (no headless run) and a thread
//! with an active turn; approvals stay in Codex. When a turn has kept running
//! for [`STEER_AFTER`], the user's post is handed to that turn as the user's
//! own mid-turn input (`turn/steer`, as the TUI sends typed text) and removed
//! from Codex's queue. Building and reading the messages is pure; the socket
//! is infra.
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

/// The newest turn of `thread`, to find the running one.
pub fn turns_list(id: u64, thread: &Uuid) -> Value {
    json!({"id": id, "method": "thread/turns/list", "params": {"threadId": thread.to_string(), "limit": 1}})
}
/// Hands `text` to the running turn `turn` as the user's mid-turn input.
pub fn turn_steer(id: u64, thread: &Uuid, turn: &str, text: &str) -> Value {
    json!({"id": id, "method": "turn/steer", "params": {
        "threadId": thread.to_string(), "expectedTurnId": turn,
        "input": [{"type": "text", "text": text, "text_elements": []}]
    }})
}
/// Removes `item` from the thread's queue.
pub fn queue_delete(id: u64, thread: &Uuid, item: &str) -> Value {
    json!({"id": id, "method": "thread/queue/delete", "params": {
        "threadId": thread.to_string(), "queuedSubmissionId": item
    }})
}
/// The running turn's id in a `thread/turns/list` result.
pub fn active_turn(result: &Value) -> Option<String> {
    let newest = result.get("data")?.as_array()?.first()?;
    (newest.get("status")?.as_str()? == "inProgress")
        .then(|| newest.get("id")?.as_str().map(str::to_owned))
        .flatten()
}

/// What Codex answered to a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    Ok,
    Refused(String),
}
/// The result of request `id` in `message`, if it is that answer: its
/// `result`, or the error's message.
pub fn result_of(id: u64, message: &Value) -> Option<std::result::Result<Value, String>> {
    if message.get("id").and_then(Value::as_u64) != Some(id) {
        return None;
    }
    Some(match message.get("error") {
        Some(error) => Err(error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("refused")
            .to_owned()),
        None => Ok(message.get("result").cloned().unwrap_or(Value::Null)),
    })
}
/// The answer to request `id` in `message`, if it is that answer.
pub fn answer_to(id: u64, message: &Value) -> Option<Answer> {
    result_of(id, message).map(|r| match r {
        Ok(_) => Answer::Ok,
        Err(why) => Answer::Refused(why),
    })
}

/// How long the user's post waits behind a running turn before it is handed
/// to that turn (REQ-015).
pub const STEER_AFTER: u64 = 120;
/// Whether to hand the user's post to the running turn: Codex refused to
/// start it because a turn runs, it has waited [`STEER_AFTER`] seconds, and
/// it was not handed over before.
pub fn steer_due(refusal: &str, waited: u64, steered_before: bool) -> bool {
    refusal.contains("active or pending turn") && waited >= STEER_AFTER && !steered_before
}

/// What handing a post to the running turn did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Steer {
    /// The turn took it; `deleted` says whether it left Codex's queue.
    Steered {
        deleted: std::result::Result<(), String>,
    },
    /// No turn runs any more (it ended, or the thread is not loaded).
    NoActiveTurn,
    Refused(String),
}

#[cfg(unix)]
mod session {
    use super::*;
    use anyhow::Context;
    use futures_util::{SinkExt, StreamExt};
    use tokio::net::UnixStream;
    use tokio_tungstenite::{WebSocketStream, tungstenite::Message};

    /// One connection to the app server, after `initialize`.
    pub(super) struct Session(WebSocketStream<UnixStream>);
    impl Session {
        pub(super) async fn open(socket: &Path) -> Result<Self> {
            let stream = UnixStream::connect(socket)
                .await
                .with_context(|| format!("No Codex app server at {}", socket.display()))?;
            let (ws, _) = tokio_tungstenite::client_async("ws://localhost/", stream)
                .await
                .context("Codex app server refused the connection")?;
            let mut session = Self(ws);
            if let Err(why) = session
                .call(1, initialize(env!("CARGO_PKG_VERSION")))
                .await?
            {
                anyhow::bail!("Codex app server refused the bridge: {why}");
            }
            session
                .0
                .send(Message::text(initialized().to_string()))
                .await?;
            Ok(session)
        }
        /// Sends request `id` and waits for its answer.
        pub(super) async fn call(
            &mut self,
            id: u64,
            request: Value,
        ) -> Result<std::result::Result<Value, String>> {
            self.0.send(Message::text(request.to_string())).await?;
            while let Some(frame) = self.0.next().await {
                if let Message::Text(text) = frame?
                    && let Ok(value) = serde_json::from_str::<Value>(&text)
                    && let Some(result) = result_of(id, &value)
                {
                    return Ok(result);
                }
            }
            anyhow::bail!("Codex app server closed the connection")
        }
    }
}

#[cfg(unix)]
const LIMIT: std::time::Duration = std::time::Duration::from_secs(5);

/// Asks the app server on `socket` to start a queued message of `thread`.
#[cfg(unix)]
pub async fn start(socket: &Path, thread: &Uuid, item: Option<&str>) -> Result<Answer> {
    use anyhow::Context;
    let run = async {
        let mut session = session::Session::open(socket).await?;
        Ok(match session.call(2, queue_start(thread, item)).await? {
            Ok(_) => Answer::Ok,
            Err(why) => Answer::Refused(why),
        })
    };
    tokio::time::timeout(LIMIT, run)
        .await
        .context("Codex app server did not answer in time")?
}

/// Hands the queued `item` (whose text is `text`) to the running turn of
/// `thread`, then removes it from Codex's queue.
#[cfg(unix)]
pub async fn steer(socket: &Path, thread: &Uuid, item: &str, text: &str) -> Result<Steer> {
    use anyhow::Context;
    let run = async {
        let mut session = session::Session::open(socket).await?;
        let turns = match session.call(2, turns_list(2, thread)).await? {
            Ok(turns) => turns,
            Err(why) => return Ok(Steer::Refused(why)),
        };
        let Some(turn) = active_turn(&turns) else {
            return Ok(Steer::NoActiveTurn);
        };
        if let Err(why) = session.call(3, turn_steer(3, thread, &turn, text)).await? {
            return Ok(Steer::Refused(why));
        }
        let deleted = session
            .call(4, queue_delete(4, thread, item))
            .await?
            .map(|_| ());
        Ok(Steer::Steered { deleted })
    };
    tokio::time::timeout(LIMIT, run)
        .await
        .context("Codex app server did not answer in time")?
}

#[cfg(not(unix))]
pub async fn start(_socket: &Path, _thread: &Uuid, _item: Option<&str>) -> Result<Answer> {
    anyhow::bail!("Waking Codex threads is not available on this system")
}
#[cfg(not(unix))]
pub async fn steer(_socket: &Path, _thread: &Uuid, _item: &str, _text: &str) -> Result<Steer> {
    anyhow::bail!("Waking Codex threads is not available on this system")
}

#[cfg(test)]
mod tests {
    use super::*;

    /* REQ-015: The running turn is found from `thread/turns/list`; steering
    names it and carries the post's text; deleting names the queued id; the
    post is handed over only after a busy refusal, once it has waited two
    minutes, and only once. */
    #[test]
    fn builds_steering_and_decides_when() {
        let thread = Uuid::nil();
        let running = json!({"data": [{"id": "t9", "status": "inProgress"}], "nextCursor": null});
        assert_eq!(active_turn(&running).as_deref(), Some("t9"));
        assert_eq!(
            active_turn(&json!({"data": [{"id": "t8", "status": "completed"}]})),
            None
        );
        assert_eq!(active_turn(&json!({"data": []})), None);
        let list = turns_list(2, &thread);
        assert_eq!(list["method"], "thread/turns/list");
        assert_eq!(list["params"]["limit"], 1);
        let steer = turn_steer(3, &thread, "t9", "kick off");
        assert_eq!(steer["method"], "turn/steer");
        assert_eq!(steer["params"]["expectedTurnId"], "t9");
        assert_eq!(steer["params"]["input"][0]["text"], "kick off");
        let delete = queue_delete(4, &thread, "q1");
        assert_eq!(delete["method"], "thread/queue/delete");
        assert_eq!(delete["params"]["queuedSubmissionId"], "q1");
        assert_eq!(
            result_of(3, &json!({"id": 3, "result": {"turnId": "t9"}})),
            Some(Ok(json!({"turnId": "t9"})))
        );
        let busy = "thread already has an active or pending turn";
        assert!(steer_due(busy, STEER_AFTER, false));
        assert!(!steer_due(busy, STEER_AFTER - 1, false));
        assert!(!steer_due(busy, STEER_AFTER, true));
        assert!(!steer_due(
            "resume the thread before starting a queued message",
            600,
            false
        ));
    }

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
