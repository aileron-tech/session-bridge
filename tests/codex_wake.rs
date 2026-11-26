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

/// How a stand-in app server answers one request: its `result`, or an
/// error message.
type Handler = fn(&Value) -> Result<Value, &'static str>;

/// Answers every request with an empty result.
fn agreeing(_: &Value) -> Result<Value, &'static str> {
    Ok(json!({}))
}

/// A stand-in Codex app server: answers each request with `handler` and
/// reports every message it gets.
fn app_server(socket: &Path, handler: Handler) -> mpsc::UnboundedReceiver<Value> {
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
                if request.get("id").is_none() {
                    continue;
                }
                let reply = match handler(&request) {
                    Ok(result) => json!({"id": request["id"], "result": result}),
                    Err(why) => {
                        json!({"id": request["id"], "error": {"code": -32600, "message": why}})
                    }
                };
                ws.send(Message::text(reply.to_string())).await.unwrap();
            }
        }
    });
    received
}
fn methods(requests: &mut mpsc::UnboundedReceiver<Value>) -> Vec<Value> {
    let mut all = Vec::new();
    while let Ok(request) = requests.try_recv() {
        all.push(request);
    }
    all
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

/// Codex's queue holding (queued id, bridge message id) pairs for `thread`.
fn codex_queue_ids(codex_home: &Path, thread: &Uuid, items: &[(&str, Uuid)]) {
    let database = codex_home.join("queue_1.sqlite");
    let _ = fs::remove_file(&database);
    let mut sql = "create table queued_items(id text, thread_id text, payload_json text, queue_order integer, created_at_ms integer);".to_owned();
    for (i, (queued, id)) in items.iter().enumerate() {
        let payload = json!({"UserInput": {"content": [{"type": "text", "text": format!("From: user\nMessage ID: {id}\nContent: x")}]}});
        sql.push_str(&format!(
            "insert into queued_items values('{queued}','{thread}','{}',{i},{i});",
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
        agreeing,
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
        agreeing,
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
    let mut requests = app_server(&socket, |request| match request["method"].as_str() {
        Some("thread/queue/start") => Err("resume the thread before starting a queued message"),
        _ => Ok(json!({})),
    });
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
    let seen: Vec<Value> = methods(&mut requests)
        .into_iter()
        .map(|r| r["method"].clone())
        .collect();
    assert_eq!(seen, ["initialize", "initialized", "thread/queue/start"]);
    let missing =
        session_bridge::codex_wake::start(&dir.path().join("none.sock"), &thread, None).await;
    assert!(missing.is_err());
}

/// A thread whose turn `t-run` is running; `turn/steer` answers per `steer`.
fn running_turn(
    request: &Value,
    steer: Result<Value, &'static str>,
) -> Result<Value, &'static str> {
    match request["method"].as_str() {
        Some("thread/turns/list") => {
            Ok(json!({"data": [{"id": "t-run", "status": "inProgress"}], "nextCursor": null}))
        }
        Some("turn/steer") => steer,
        Some("thread/queue/start") => Err("thread already has an active or pending turn"),
        _ => Ok(json!({})),
    }
}

/* REQ-015: Handing the user's post to a running turn names that turn, carries
the post's text, then removes the post from Codex's queue by its id. */
#[tokio::test]
async fn steers_into_the_running_turn_then_deletes() {
    let dir = tempfile::Builder::new()
        .prefix("sb-wk-")
        .tempdir_in("/tmp")
        .unwrap();
    let socket = dir.path().join("control.sock");
    let mut requests = app_server(&socket, |r| running_turn(r, Ok(json!({"turnId": "t-run"}))));
    let thread = Uuid::new_v4();
    let done = session_bridge::codex_wake::steer(&socket, &thread, "q1", "Kick off")
        .await
        .unwrap();
    assert_eq!(
        done,
        session_bridge::codex_wake::Steer::Steered { deleted: Ok(()) }
    );
    let seen = methods(&mut requests);
    let names: Vec<&str> = seen.iter().map(|r| r["method"].as_str().unwrap()).collect();
    assert_eq!(
        names,
        [
            "initialize",
            "initialized",
            "thread/turns/list",
            "turn/steer",
            "thread/queue/delete"
        ]
    );
    assert_eq!(seen[2]["params"]["threadId"], thread.to_string());
    assert_eq!(seen[3]["params"]["expectedTurnId"], "t-run");
    assert_eq!(seen[3]["params"]["input"][0]["text"], "Kick off");
    assert_eq!(seen[4]["params"]["queuedSubmissionId"], "q1");
}

/* REQ-015: A refused hand-over (for example a review turn) leaves the post in
Codex's queue: nothing is deleted. Without a running turn nothing is sent. */
#[tokio::test]
async fn a_refused_steer_leaves_the_post_queued() {
    let dir = tempfile::Builder::new()
        .prefix("sb-wk-")
        .tempdir_in("/tmp")
        .unwrap();
    let socket = dir.path().join("control.sock");
    let mut requests = app_server(&socket, |r| {
        running_turn(r, Err("cannot steer a review turn"))
    });
    let thread = Uuid::new_v4();
    let done = session_bridge::codex_wake::steer(&socket, &thread, "q1", "Kick off")
        .await
        .unwrap();
    assert_eq!(
        done,
        session_bridge::codex_wake::Steer::Refused("cannot steer a review turn".into())
    );
    let names: Vec<String> = methods(&mut requests)
        .iter()
        .map(|r| r["method"].as_str().unwrap().to_owned())
        .collect();
    assert!(
        !names.contains(&"thread/queue/delete".to_owned()),
        "{names:?}"
    );

    let idle_socket = dir.path().join("idle.sock");
    let mut idle = app_server(&idle_socket, |r| match r["method"].as_str() {
        Some("thread/turns/list") => Ok(json!({"data": [{"id": "t-old", "status": "completed"}]})),
        _ => Ok(json!({})),
    });
    let done = session_bridge::codex_wake::steer(&idle_socket, &thread, "q1", "Kick off")
        .await
        .unwrap();
    assert_eq!(done, session_bridge::codex_wake::Steer::NoActiveTurn);
    let names: Vec<String> = methods(&mut idle)
        .iter()
        .map(|r| r["method"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(names, ["initialize", "initialized", "thread/turns/list"]);
}

/* REQ-015: The 30 s sweep asks Codex to start the user's post by its queued
id and never an agent's post; while a turn runs, a post younger than two
minutes is not handed to that turn. */
#[tokio::test]
async fn the_sweep_starts_user_posts_and_waits_two_minutes_to_steer() {
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
        |r| running_turn(r, Ok(json!({"turnId": "t-run"}))),
    );
    s.node.start().await;
    let page = AdminPage::open(&s.node).await;
    let session = format!("codex:{}", s.thread);
    wait_for_session(&page, &session).await;
    let user = page
        .ok(json!({"method": "user_send", "session_id": session, "message": "Kick off"}))
        .await;
    let user_post: Uuid = user["receipt"]["message_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let agent = s
        .node
        .rpc(Request::Send {
            sender: s.node.session.clone(),
            session_id: session.clone(),
            message: "From a peer agent".into(),
            project: None,
        })
        .await;
    let Reply::Receipt { receipt } = agent else {
        panic!("{agent:?}")
    };
    // Codex's queue holds the agent's post first, then the user's.
    codex_queue_ids(
        &s.codex_home,
        &s.thread,
        &[("q-agent", receipt.message_id), ("q-user", user_post)],
    );
    // The sweep runs 30 s after the bridge started.
    tokio::time::sleep(Duration::from_secs(36)).await;
    let seen = methods(&mut requests);
    let starts: Vec<&Value> = seen
        .iter()
        .filter(|r| r["method"] == "thread/queue/start")
        .collect();
    assert!(!starts.is_empty(), "the sweep must ask: {seen:?}");
    for start in &starts {
        assert_ne!(start["params"]["queuedSubmissionId"], "q-agent", "{seen:?}");
    }
    assert!(
        starts
            .iter()
            .any(|r| r["params"]["queuedSubmissionId"] == "q-user"),
        "{seen:?}"
    );
    assert!(
        !seen.iter().any(|r| r["method"] == "turn/steer"),
        "nothing is handed over before two minutes: {seen:?}"
    );
    s.node.stop().await;
}
