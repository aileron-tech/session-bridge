//! A message to a live Codex session also asks the user's Codex app server
//! to start it (REQ-015): stand-in `codex`, a stand-in rollout holder, and a
//! stand-in app server on the control socket. Unix only.
#![cfg(unix)]
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{fs, os::unix::fs::PermissionsExt, path::Path, time::Duration};
use tokio::{net::UnixListener, sync::mpsc, time::timeout};
use tokio_tungstenite::tungstenite::Message;
use uuid::Uuid;

#[path = "lan/common.rs"]
mod common;
use common::{AdminPage, Node};

const CODEX: &str = r#"#!/bin/sh
# Stand-in for `codex queue --thread T --message M`: records and confirms.
echo "$@" >> "$CODEX_HOME/queue-calls.txt"
echo "Queued message q-1 for thread $3."
"#;

/// A stand-in Codex app server: answers initialize and thread/queue/start
/// (refusing with `refuse` when set) and reports every request it gets.
fn app_server(socket: &Path, refuse: Option<&'static str>) -> mpsc::UnboundedReceiver<Value> {
    fs::create_dir_all(socket.parent().unwrap()).unwrap();
    let listener = UnixListener::bind(socket).unwrap();
    let (seen, received) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            while let Some(Ok(Message::Text(text))) = ws.next().await {
                let request: Value = serde_json::from_str(&text).unwrap();
                let _ = seen.send(request.clone());
                let reply = match (request["method"].as_str(), refuse) {
                    (Some("thread/queue/start"), Some(why)) => {
                        json!({"id": request["id"], "error": {"code": -32600, "message": why}})
                    }
                    _ if request.get("id").is_some() => json!({"id": request["id"], "result": {}}),
                    _ => continue,
                };
                ws.send(Message::text(reply.to_string())).await.unwrap();
            }
        }
    });
    received
}

/* REQ-015: A message from the user to a live Codex session is queued with
`codex queue` and Codex's app server is then asked to start exactly that
queued item (experimental API, after initialize); a refusal (an unloaded or
busy thread) leaves the delivery queued. */
#[tokio::test]
async fn queued_codex_messages_are_started() {
    let mut a = Node::new("a", None);
    let codex_home = a.home.path().join(".codex");
    let tools = a.home.path().join("tools");
    fs::create_dir_all(&tools).unwrap();
    let codex = tools.join("codex");
    fs::write(&codex, CODEX).unwrap();
    fs::set_permissions(&codex, fs::Permissions::from_mode(0o755)).unwrap();
    a.env.push((
        "SESSION_BRIDGE_CODEX_BIN".into(),
        codex.display().to_string(),
    ));
    // A live Codex thread: a process holds its rollout open, as Codex does.
    let thread = Uuid::new_v4();
    let sessions = codex_home.join("sessions");
    fs::create_dir_all(&sessions).unwrap();
    let rollout = sessions.join(format!("rollout-{thread}.jsonl"));
    fs::write(
        &rollout,
        format!("{}\n", json!({"type": "session_meta", "payload": {"id": thread, "cwd": "/work/mono", "source": "cli"}})),
    )
    .unwrap();
    let mut holder = tokio::process::Command::new("/bin/sh")
        .args([
            "-c",
            "exec 3<\"$0\"; sleep 120",
            &rollout.display().to_string(),
        ])
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut requests = app_server(
        &codex_home.join("app-server-control/app-server-control.sock"),
        None,
    );
    a.start().await;
    let page = AdminPage::open(&a).await;
    let session = format!("codex:{thread}");
    common::eventually("the Codex session", async || {
        let listed = page
            .ok(json!({"method": "list", "sender": null, "project": null}))
            .await;
        listed["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["session_id"] == session.as_str())
            .then_some(())
    })
    .await;

    let sent = page
        .ok(json!({"method": "user_send", "session_id": session, "message": "Kick off"}))
        .await;
    assert_eq!(sent["receipt"]["delivery"]["state"], "queued", "{sent}");
    let calls = fs::read_to_string(codex_home.join("queue-calls.txt")).unwrap();
    assert!(
        calls.starts_with(&format!("queue --thread {thread} --message ")),
        "{calls}"
    );
    assert!(!calls.contains("--resume"), "{calls}");
    let mut next = async || {
        timeout(Duration::from_secs(10), requests.recv())
            .await
            .unwrap()
            .unwrap()
    };
    let init = next().await;
    assert_eq!(init["method"], "initialize");
    assert_eq!(init["params"]["capabilities"]["experimentalApi"], true);
    assert_eq!(next().await["method"], "initialized");
    let start = next().await;
    assert_eq!(start["method"], "thread/queue/start");
    assert_eq!(start["params"]["threadId"], thread.to_string());
    assert_eq!(start["params"]["queuedSubmissionId"], "q-1");
    a.stop().await;
    holder.kill().await.unwrap();
}

/* REQ-015: When Codex refuses to start (an unloaded thread: no headless
run), the message stays queued and nothing else happens. */
#[tokio::test]
async fn a_refused_start_keeps_the_message_queued() {
    let dir = tempfile::Builder::new()
        .prefix("sb-wk-")
        .tempdir_in("/tmp")
        .unwrap();
    let socket = dir.path().join("control.sock");
    let mut requests = app_server(
        &socket,
        Some("resume the thread before starting a queued message"),
    );
    let thread = Uuid::new_v4();
    let answer = session_bridge::codex_wake::start(&socket, &thread, None)
        .await
        .unwrap();
    assert_eq!(
        answer,
        session_bridge::codex_wake::Answer::Refused(
            "resume the thread before starting a queued message".into()
        )
    );
    let mut methods = Vec::new();
    while let Ok(request) = requests.try_recv() {
        methods.push(request["method"].as_str().unwrap().to_owned());
    }
    assert_eq!(methods, ["initialize", "initialized", "thread/queue/start"]);
    let missing =
        session_bridge::codex_wake::start(&dir.path().join("none.sock"), &thread, None).await;
    assert!(missing.is_err());
}
