//! Codex delivery on this laptop with stand-ins (REQ-015, REQ-025): a stand-in
//! `codex`, a process holding a rollout open (a live thread), a stand-in app
//! server on the control socket, and a Codex queue database. Unix only.
#![cfg(unix)]
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use session_bridge::protocol::{Reply, Request};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::{net::UnixListener, process::Child, sync::mpsc, time::timeout};
use tokio_tungstenite::tungstenite::Message;
use uuid::Uuid;

#[path = "lan/common.rs"]
mod common;
use common::{AdminPage, Node};

const CODEX: &str = r#"#!/bin/sh
# Stand-in for `codex queue --thread T --message M`: records and confirms.
n=$(wc -l < "$CODEX_HOME/queue-calls.txt" 2>/dev/null || echo 0)
echo "$@" >> "$CODEX_HOME/queue-calls.txt"
echo "Queued message q-$((n + 1)) for thread $3."
"#;
const CRONTAB: &str = "#!/bin/sh\nif [ \"$1\" = \"-l\" ]; then cat \"$CRONTAB_FILE\" 2>/dev/null; else cat > \"$CRONTAB_FILE\"; fi\n";

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

/// A bridge with a stand-in `codex` and crontab, one live Codex thread in
/// #mono, and (with `inbox`) a Claude session of its own.
struct Setup {
    node: Node,
    codex_home: PathBuf,
    thread: Uuid,
    _holder: Child,
}
fn setup(inbox: Option<&Path>) -> Setup {
    let mut node = Node::new("a", inbox);
    let codex_home = node.home.path().join(".codex");
    let tools = node.home.path().join("tools");
    fs::create_dir_all(&tools).unwrap();
    for (name, text) in [("codex", CODEX), ("crontab", CRONTAB)] {
        fs::write(tools.join(name), text).unwrap();
        fs::set_permissions(tools.join(name), fs::Permissions::from_mode(0o755)).unwrap();
    }
    fs::write(tools.join("crontab.txt"), "").unwrap();
    node.env.extend([
        (
            "SESSION_BRIDGE_CODEX_BIN".into(),
            tools.join("codex").display().to_string(),
        ),
        (
            "SESSION_BRIDGE_CRONTAB".into(),
            tools.join("crontab").display().to_string(),
        ),
        (
            "CRONTAB_FILE".into(),
            tools.join("crontab.txt").display().to_string(),
        ),
    ]);
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
    let holder = tokio::process::Command::new("/bin/sh")
        .args([
            "-c",
            "exec 3<\"$0\"; sleep 120",
            &rollout.display().to_string(),
        ])
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    Setup {
        node,
        codex_home,
        thread,
        _holder: holder,
    }
}
async fn wait_for_session(page: &AdminPage, session: &str) {
    common::eventually("the Codex session", async || {
        let listed = page
            .ok(json!({"method": "list", "sender": null, "project": null}))
            .await;
        listed["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["session_id"] == session)
            .then_some(())
    })
    .await;
}
/// Codex's queue holding `messages` (bridge message ids) for `thread`.
fn codex_queue(codex_home: &Path, thread: &Uuid, messages: &[Uuid]) {
    let database = codex_home.join("queue_1.sqlite");
    let _ = fs::remove_file(&database);
    let mut sql = "create table queued_items(id text, thread_id text, payload_json text, queue_order integer, created_at_ms integer);".to_owned();
    for (i, id) in messages.iter().enumerate() {
        let payload = json!({"UserInput": {"content": [{"type": "text", "text": format!("From: user\nMessage ID: {id}\nContent: x")}]}});
        sql.push_str(&format!(
            "insert into queued_items values('q{i}','{thread}','{}',{i},{i});",
            payload.to_string().replace('\'', "''")
        ));
    }
    let made = std::process::Command::new("/usr/bin/sqlite3")
        .arg(&database)
        .arg(sql)
        .output()
        .unwrap();
    assert!(made.status.success(), "{made:?}");
}

/* REQ-015: A message from the user to a live Codex session is queued with
`codex queue` and Codex's app server is then asked to start exactly that
queued item (experimental API, after initialize). */
#[tokio::test]
async fn the_users_codex_messages_are_started() {
    let mut s = setup(None);
    let mut requests = app_server(
        &s.codex_home
            .join("app-server-control/app-server-control.sock"),
        None,
    );
    s.node.start().await;
    let page = AdminPage::open(&s.node).await;
    let session = format!("codex:{}", s.thread);
    wait_for_session(&page, &session).await;

    let sent = page
        .ok(json!({"method": "user_send", "session_id": session, "message": "Kick off"}))
        .await;
    assert_eq!(sent["receipt"]["delivery"]["state"], "queued", "{sent}");
    let calls = fs::read_to_string(s.codex_home.join("queue-calls.txt")).unwrap();
    assert!(
        calls.starts_with(&format!("queue --thread {} --message ", s.thread)),
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
    assert_eq!(start["params"]["threadId"], s.thread.to_string());
    assert_eq!(start["params"]["queuedSubmissionId"], "q-1");
    s.node.stop().await;
}

/* REQ-015: An agent's message to a Codex session is only queued: the bridge
never asks Codex to start it, so it cannot override the user's interrupt. */
#[tokio::test]
async fn agent_messages_are_not_started() {
    let sockets = tempfile::Builder::new()
        .prefix("sb-wk-")
        .tempdir_in("/tmp")
        .unwrap();
    let dir = sockets.path().join("inboxes");
    session_bridge::store::ensure_private_dir(&dir).unwrap();
    let inbox = dir.join("a.sock");
    let _listener = std::os::unix::net::UnixListener::bind(&inbox).unwrap();
    fs::set_permissions(&inbox, fs::Permissions::from_mode(0o600)).unwrap();
    let mut s = setup(Some(&inbox));
    let mut requests = app_server(
        &s.codex_home
            .join("app-server-control/app-server-control.sock"),
        None,
    );
    s.node.start().await;
    let page = AdminPage::open(&s.node).await;
    let session = format!("codex:{}", s.thread);
    wait_for_session(&page, &session).await;
    let reply = s
        .node
        .rpc(Request::Send {
            sender: s.node.session.clone(),
            session_id: session,
            message: "From a peer agent".into(),
            project: None,
        })
        .await;
    assert!(
        matches!(&reply, Reply::Receipt { receipt } if receipt.delivery == session_bridge::domain::Delivery::Queued),
        "{reply:?}"
    );
    assert!(
        fs::read_to_string(s.codex_home.join("queue-calls.txt"))
            .unwrap()
            .contains("From a peer agent")
    );
    assert!(
        timeout(Duration::from_secs(2), requests.recv())
            .await
            .is_err(),
        "the app server must not be asked"
    );
    s.node.stop().await;
}

/* REQ-025: A scheduled run skips a Codex session while the schedule's
previous post to it is still in Codex's queue, says so, and posts again once
Codex took it. */
#[tokio::test]
async fn schedules_skip_posts_still_in_codexs_queue() {
    let mut s = setup(None);
    s.node.start().await;
    let page = AdminPage::open(&s.node).await;
    let session = format!("codex:{}", s.thread);
    wait_for_session(&page, &session).await;
    let saved = page
        .ok(json!({"method": "save_schedule", "cron": "@hourly", "project": "mono", "to": s.thread.to_string(), "message": "Status please"}))
        .await;
    let id = saved["schedules"][0]["id"].as_str().unwrap().to_owned();
    let run = async || -> Value {
        let out = s.node.cli(&["scheduled", &id]).await;
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).unwrap()
    };
    let first = run().await;
    let posted: Uuid = first["receipts"][0]["message_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    codex_queue(&s.codex_home, &s.thread, &[Uuid::new_v4(), posted]);
    let second = run().await;
    assert_eq!(second["receipts"], json!([]), "{second}");
    let skipped = second["skipped"][0].as_str().unwrap();
    assert!(
        skipped.ends_with("the previous post (just now) was not processed yet; sending skipped"),
        "{skipped}"
    );
    codex_queue(&s.codex_home, &s.thread, &[Uuid::new_v4()]);
    let third = run().await;
    assert_eq!(third["receipts"].as_array().unwrap().len(), 1, "{third}");
    s.node.stop().await;
}

/* REQ-015: When Codex refuses to start (an unloaded thread: no headless
run), the bridge reads the refusal; a missing app server is an error. */
#[tokio::test]
async fn a_refused_start_is_reported() {
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
    let answer = session_bridge::codex_wake::start(&socket, &thread, Some("q1"))
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
