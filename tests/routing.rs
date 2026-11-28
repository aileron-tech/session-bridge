// Unix only: these tests drive Unix sockets and the native Unix adapters.
#![cfg(unix)]
use serde_json::json;
use session_bridge::{
    domain::Delivery,
    protocol::{Receipt, Reply, Request},
    store::{self, Store},
    transport::{self, PeerContent, PeerFrame},
};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{
    net::UnixListener,
    process::{Child, Command},
    time::{sleep, timeout},
};
use uuid::Uuid;

#[path = "routing/scoping.rs"]
mod scoping;

struct Harness {
    home: tempfile::TempDir,
    control: PathBuf,
    sender_id: String,
    target_id: String,
    sender: UnixListener,
    target: Option<UnixListener>,
    target_path: PathBuf,
}
impl Harness {
    fn new() -> Self {
        let home = tempfile::Builder::new()
            .prefix("sb-test-")
            .tempdir_in("/tmp")
            .unwrap();
        let inbox = home.path().join("inboxes");
        store::ensure_private_dir(&inbox).unwrap();
        let records = home.path().join(".claude/sessions");
        fs::create_dir_all(&records).unwrap();
        let sender_uuid = Uuid::new_v4();
        let target_uuid = Uuid::new_v4();
        let sender_path = inbox.join("sender.sock");
        let target_path = inbox.join("target.sock");
        let sender = bind(&sender_path);
        let target = bind(&target_path);
        for (name, id, path) in [
            ("sender", sender_uuid, &sender_path),
            ("target", target_uuid, &target_path),
        ] {
            fs::write(records.join(format!("{name}.json")),serde_json::to_vec(&json!({"pid":std::process::id(),"sessionId":id,"cwd":"/work/mono","messagingSocketPath":path,"name":name,"status":"idle","kind":"interactive"})).unwrap()).unwrap();
        }
        Self {
            control: home.path().join(".local/state/session-bridge/control.sock"),
            home,
            sender_id: format!("claude:{sender_uuid}"),
            target_id: format!("claude:{target_uuid}"),
            sender,
            target: Some(target),
            target_path,
        }
    }
    /// Config and history (REQ-039).
    fn data(&self) -> PathBuf {
        self.home.path().join(".session-bridge")
    }
    /// The daemon command; with `state`, a bridge on its own state folder
    /// (SESSION_BRIDGE_STATE, REQ-021).
    fn command(&self, state: Option<&Path>) -> Command {
        fs::create_dir_all(self.home.path().join(".codex")).unwrap();
        let mut command = Command::new(env!("CARGO_BIN_EXE_session-bridge"));
        command
            .arg("daemon")
            .env("HOME", self.home.path())
            .env("CODEX_HOME", self.home.path().join(".codex"))
            .env("CLAUDE_CONFIG_DIR", self.home.path().join(".claude"))
            .env_remove("SESSION_BRIDGE_STATE")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .kill_on_drop(true);
        if let Some(state) = state {
            command.env("SESSION_BRIDGE_STATE", state);
        }
        command
    }
    async fn start(&self) -> Child {
        self.start_with(None).await
    }
    async fn start_with(&self, state: Option<&Path>) -> Child {
        let control = state.map_or_else(|| self.control.clone(), |s| s.join("control.sock"));
        let mut child = self
            .command(state)
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        for _ in 0..100 {
            if let Ok(mut stream) = transport::connect(&control).await {
                transport::write_frame(&mut stream, &Request::Health)
                    .await
                    .unwrap();
                let reply: Reply = transport::read_frame(&mut stream).await.unwrap();
                assert!(matches!(reply, Reply::Health { .. }));
                return child;
            }
            if let Some(status) = child.try_wait().unwrap() {
                panic!("Daemon exited: {status}");
            }
            sleep(Duration::from_millis(50)).await;
        }
        panic!("Daemon did not start")
    }
    async fn rpc(&self, request: Request) -> Reply {
        let mut stream = transport::connect(&self.control).await.unwrap();
        transport::write_frame(&mut stream, &request).await.unwrap();
        transport::read_frame(&mut stream).await.unwrap()
    }
    async fn send(&self, text: &str) -> Receipt {
        match self
            .rpc(Request::Send {
                sender: self.sender_id.clone(),
                session_id: self.target_id.clone(),
                message: text.into(),
                project: None,
            })
            .await
        {
            Reply::Receipt { receipt } => receipt,
            other => panic!("Unexpected reply {other:?}"),
        }
    }
    async fn wait_delivery(&self, id: Uuid, predicate: fn(&Delivery) -> bool) -> Delivery {
        for _ in 0..100 {
            if let Reply::Status { messages, .. } = self.rpc(Request::Status).await
                && let Some(message) = messages.iter().find(|m| m.message_id == id)
                && predicate(&message.delivery)
            {
                return message.delivery.clone();
            }
            sleep(Duration::from_millis(50)).await;
        }
        panic!("Expected delivery status did not arrive")
    }
}
fn bind(path: &Path) -> UnixListener {
    let listener = UnixListener::bind(path).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    listener
}
/* REQ-011: Allow native App Server startup/RPC drain before requiring process exit. */
async fn stop(child: &mut Child) {
    unsafe {
        libc::kill(child.id().unwrap() as i32, libc::SIGTERM);
    }
    timeout(Duration::from_secs(45), child.wait())
        .await
        .unwrap()
        .unwrap();
}
async fn receive(listener: &UnixListener) -> PeerFrame {
    let (mut stream, _) = timeout(Duration::from_secs(10), listener.accept())
        .await
        .unwrap()
        .unwrap();
    transport::read_frame(&mut stream).await.unwrap()
}

/* REQ-002: Native Claude replies reach the original sender through a stable route. */
/* REQ-004: Held delivery survives restart and is not retried or treated as approval. */
#[tokio::test]
async fn held_messages_and_reply_routes_survive_daemon_restart() {
    let h = Harness::new();
    let mut child = h.start().await;
    let receipt = h.send("Coordinate deploy timing").await;
    assert_eq!(receipt.delivery, Delivery::Queued);
    let sent = receive(h.target.as_ref().unwrap()).await;
    let reply_path = PathBuf::from(sent.from.as_ref().unwrap().strip_prefix("uds:").unwrap());
    let mut held = sent.clone();
    held.kind = "control".into();
    held.action = Some("peer_message_status".into());
    held.status = Some("held".into());
    held.reason = Some("Recipient approval".into());
    held.orig_msg_id = Some(receipt.message_id.to_string());
    held.from = Some(format!("uds:{}", h.target_path.display()));
    held.message = None;
    let mut stream = transport::connect(&reply_path).await.unwrap();
    transport::write_frame(&mut stream, &held).await.unwrap();
    drop(stream);
    h.wait_delivery(receipt.message_id, |d| matches!(d, Delivery::Held { .. }))
        .await;
    stop(&mut child).await;
    child = h.start().await;
    h.wait_delivery(receipt.message_id, |d| matches!(d, Delivery::Held { .. }))
        .await;
    assert!(
        timeout(
            Duration::from_millis(250),
            h.target.as_ref().unwrap().accept()
        )
        .await
        .is_err()
    );
    let reply = PeerFrame {
        kind: "user".into(),
        message: Some(PeerContent {
            role: "user".into(),
            content: "ACK: deploy after tests finish".into(),
        }),
        from: Some(format!("uds:{}", h.target_path.display())),
        uuid: Some(Uuid::new_v4().to_string()),
        msg_id: None,
        orig_msg_id: None,
        action: None,
        status: None,
        reason: None,
    };
    let mut stream = transport::connect(&reply_path).await.unwrap();
    transport::write_frame(&mut stream, &reply).await.unwrap();
    let delivered = receive(&h.sender).await;
    assert!(
        delivered
            .message
            .unwrap()
            .content
            .contains("ACK: deploy after tests finish")
    );
    stop(&mut child).await;
}

/* REQ-003: Unsent messages resume automatically after destination and daemon restart. */
#[tokio::test]
async fn pending_messages_recover_when_destination_returns() {
    let mut h = Harness::new();
    let mut child = h.start().await;
    drop(h.target.take());
    let receipt = h.send("Preserve this pending finding").await;
    assert!(matches!(receipt.delivery, Delivery::Pending { .. }));
    stop(&mut child).await;
    child = h.start().await;
    fs::remove_file(&h.target_path).unwrap();
    h.target = Some(bind(&h.target_path));
    let delivered = receive(h.target.as_ref().unwrap()).await;
    assert!(
        delivered
            .message
            .unwrap()
            .content
            .contains("Preserve this pending finding")
    );
    h.wait_delivery(receipt.message_id, |d| matches!(d, Delivery::Queued))
        .await;
    assert!(
        timeout(
            Duration::from_millis(250),
            h.target.as_ref().unwrap().accept()
        )
        .await
        .is_err()
    );
    stop(&mut child).await;
}

/* REQ-038: A message waiting for a session that ends is refused, and is not delivered when the session id returns. */
#[tokio::test]
async fn pending_message_to_an_ended_session_is_refused() {
    let mut h = Harness::new();
    let mut child = h.start().await;
    drop(h.target.take());
    let receipt = h.send("Nobody reads this").await;
    assert!(matches!(receipt.delivery, Delivery::Pending { .. }));
    let record = h.home.path().join(".claude/sessions/target.json");
    let saved = fs::read(&record).unwrap();
    fs::remove_file(&record).unwrap();
    // Discovery and the retry tick both run every 3 s.
    let mut refused = None;
    for _ in 0..200 {
        if let Reply::Status { messages, .. } = h.rpc(Request::Status).await
            && let Some(m) = messages.iter().find(|m| m.message_id == receipt.message_id)
            && let Delivery::Refused { reason } = &m.delivery
        {
            refused = Some(reason.clone());
            break;
        }
        sleep(Duration::from_millis(50)).await;
    }
    let reason = refused.expect("the message was not refused");
    assert!(reason.contains("target has ended"), "{reason}");
    // The same session id returns: the refused message stays undelivered.
    fs::remove_file(&h.target_path).unwrap();
    h.target = Some(bind(&h.target_path));
    fs::write(&record, saved).unwrap();
    assert!(
        timeout(Duration::from_secs(7), h.target.as_ref().unwrap().accept())
            .await
            .is_err()
    );
    stop(&mut child).await;
}

/* REQ-039: The first start of the installed bridge copies the config from the
old state folder into ~/.session-bridge and delivers the message that was still
pending there; the old folder is left as it was. */
#[tokio::test]
async fn first_start_carries_config_and_pending_work_to_the_data_folder() {
    let h = Harness::new();
    let old = h.home.path().join(".local/state/session-bridge");
    fs::create_dir_all(old.parent().unwrap()).unwrap();
    store::ensure_private_dir(&old).unwrap();
    store::ensure_private_dir(&old.join("messages")).unwrap();
    let user = json!({"provider": "user", "id": Uuid::new_v4()});
    store::write_json(&old.join("user.json"), &user).unwrap();
    let id = Uuid::new_v4();
    let pending = json!({
        "message_id": id,
        "sender": {"provider": "claude", "id": h.sender_id.trim_start_matches("claude:")},
        "recipient": {"provider": "claude", "id": h.target_id.trim_start_matches("claude:")},
        "message": "Carried across the move",
        "delivery": {"state": "pending", "reason": "offline"},
        "created_at": 1,
    });
    let old_file = old.join("messages").join(format!("{id}.json"));
    store::write_json(&old_file, &pending).unwrap();
    let sessions = json!({
        h.sender_id.clone(): {"session_id": h.sender_id, "agent": "claude", "project": "mono",
            "cwd": "/work/mono", "name": "sender", "status": "idle", "kind": "interactive"},
    });
    store::write_json(&old.join("sessions.json"), &sessions).unwrap();
    let mut child = h.start().await;
    let delivered = receive(h.target.as_ref().unwrap()).await;
    assert!(
        delivered
            .message
            .unwrap()
            .content
            .contains("Carried across the move")
    );
    let data = h.data();
    let kept: serde_json::Value =
        serde_json::from_slice(&fs::read(data.join("user.json")).unwrap()).unwrap();
    assert_eq!(kept, user);
    assert!(data.join(store::HISTORY).is_file());
    assert!(old_file.is_file(), "the old folder was changed");
    assert!(!data.join("messages").exists());
    stop(&mut child).await;
}

/* REQ-039: A bridge on its own state folder (SESSION_BRIDGE_STATE) carries
the pending message an older version left in messages/ into its history in
place and delivers it; messages/ is left as it is. */
#[tokio::test]
async fn own_state_folder_carries_old_pending_work_in_place() {
    let h = Harness::new();
    let state = h.home.path().join("own-state");
    store::ensure_private_dir(&state).unwrap();
    store::ensure_private_dir(&state.join("messages")).unwrap();
    let id = Uuid::new_v4();
    let pending = json!({
        "message_id": id,
        "sender": {"provider": "claude", "id": h.sender_id.trim_start_matches("claude:")},
        "recipient": {"provider": "claude", "id": h.target_id.trim_start_matches("claude:")},
        "message": "Carried in place",
        "delivery": {"state": "pending", "reason": "offline"},
        "created_at": 1,
    });
    let old_file = state.join("messages").join(format!("{id}.json"));
    store::write_json(&old_file, &pending).unwrap();
    let before = fs::read(&old_file).unwrap();
    let mut child = h.start_with(Some(&state)).await;
    let delivered = receive(h.target.as_ref().unwrap()).await;
    assert!(
        delivered
            .message
            .unwrap()
            .content
            .contains("Carried in place")
    );
    assert!(state.join(store::HISTORY).is_file());
    assert_eq!(
        fs::read(&old_file).unwrap(),
        before,
        "messages/ was changed"
    );
    assert!(!h.data().exists(), "the home data folder was used");
    stop(&mut child).await;
}

/* REQ-039: A history line that is not a message (other than a torn last line)
stops the start with its line number, so no message falls back to an older
state and is resent. */
#[tokio::test]
async fn unreadable_history_line_fails_the_start() {
    let h = Harness::new();
    let data = h.data();
    store::ensure_private_dir(&data).unwrap();
    let message = |state: serde_json::Value| {
        json!({
            "message_id": Uuid::nil(),
            "sender": {"provider": "claude", "id": h.sender_id.trim_start_matches("claude:")},
            "recipient": {"provider": "claude", "id": h.target_id.trim_start_matches("claude:")},
            "message": "Sent once",
            "delivery": state,
            "created_at": 1,
        })
    };
    let history = format!(
        "{}\n{{\"broken\": true}}\n{}\n",
        message(json!({"state": "pending", "reason": "offline"})),
        message(json!({"state": "delivered"})),
    );
    let file = data.join(store::HISTORY);
    fs::write(&file, &history).unwrap();
    fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
    let output = timeout(
        Duration::from_secs(20),
        h.command(None).stderr(Stdio::piped()).output(),
    )
    .await
    .expect("the daemon did not stop")
    .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("history.jsonl line 2"), "{stderr}");
    assert_eq!(fs::read_to_string(&file).unwrap(), history);
}

/* REQ-006: Malformed send requests produce errors without creating durable work. */
#[tokio::test]
async fn invalid_message_does_not_enter_outbox() {
    let h = Harness::new();
    let mut child = h.start().await;
    let response = h
        .rpc(Request::Send {
            sender: h.sender_id.clone(),
            session_id: h.target_id.clone(),
            message: "\0".into(),
            project: None,
        })
        .await;
    assert!(matches!(response, Reply::Error { .. }));
    assert!(
        Store::open(h.data())
            .unwrap()
            .messages()
            .unwrap()
            .is_empty()
    );
    stop(&mut child).await;
}

/* REQ-005, REQ-014: MCP reconnects and scopes peers using its thread metadata, not its process directory. */
#[tokio::test]
async fn mcp_autoconnects_and_reconnects_to_the_queue() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let h = Harness::new();
    let raw = json!({"type":"session_meta","payload":{"id":"01a1d465-0b2b-76e2-ac61-2c442c669b71","cwd":"/work/mono","source":"cli"}});
    let caller =
        session_bridge::discovery::parse_codex_record(&raw.to_string(), std::process::id())
            .unwrap();
    let state = Store::open(h.data()).unwrap();
    store::write_json(
        &state.root.join("sessions.json"),
        &std::collections::BTreeMap::from([(caller.session_id.clone(), caller)]),
    )
    .unwrap();
    let target_path = h.home.path().join(".claude/sessions/target.json");
    let mut target: serde_json::Value =
        serde_json::from_slice(&fs::read(&target_path).unwrap()).unwrap();
    target["cwd"] = json!("/elsewhere/mono");
    fs::write(target_path, serde_json::to_vec(&target).unwrap()).unwrap();
    let mut daemon = h.start().await;
    let mut mcp = Command::new(env!("CARGO_BIN_EXE_session-bridge"))
        .arg("mcp")
        .env("HOME", h.home.path())
        .env("CODEX_HOME", h.home.path().join(".codex"))
        .env("CLAUDE_CONFIG_DIR", h.home.path().join(".claude"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut input = mcp.stdin.take().unwrap();
    let mut output = BufReader::new(mcp.stdout.take().unwrap()).lines();
    let requests = [
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"bridge-test","version":"1"}}}),
        json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}),
    ];
    for request in requests {
        input
            .write_all(format!("{request}\n").as_bytes())
            .await
            .unwrap();
    }
    let mut tools = Vec::new();
    loop {
        let line = timeout(Duration::from_secs(10), output.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let response: serde_json::Value = serde_json::from_str(&line).unwrap();
        if response["id"] == 2 {
            for tool in response["result"]["tools"].as_array().unwrap() {
                tools.push(tool["name"].as_str().unwrap().to_owned());
            }
            break;
        }
    }
    // Listing and sending, plus the root role's tools (REQ-028).
    tools.sort();
    assert_eq!(
        tools,
        [
            "close_session",
            "list_sessions",
            "send_message",
            "spawn_session"
        ]
    );
    for id in [3, 4] {
        if id == 4 {
            stop(&mut daemon).await;
            daemon = h.start().await;
        }
        let request = json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":"list_sessions","arguments":{},"_meta":{"threadId":"01a1d465-0b2b-76e2-ac61-2c442c669b71"}}});
        input
            .write_all(format!("{request}\n").as_bytes())
            .await
            .unwrap();
        loop {
            let line = timeout(Duration::from_secs(10), output.next_line())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            let response: serde_json::Value = serde_json::from_str(&line).unwrap();
            if response["id"] == id {
                assert!(response.get("error").is_none(), "{response}");
                let content = response["result"]["content"][0]["text"].as_str().unwrap();
                let result: serde_json::Value = serde_json::from_str(content).unwrap();
                assert_eq!(result["sessions"].as_array().unwrap().len(), 1);
                assert_eq!(result["sessions"][0]["session_id"], h.sender_id);
                break;
            }
        }
    }
    drop(input);
    timeout(Duration::from_secs(5), mcp.wait())
        .await
        .unwrap()
        .unwrap();
    stop(&mut daemon).await;
}

/* REQ-005: Claude can publish or change its session identity after MCP initialization. */
#[tokio::test]
async fn claude_identity_can_be_registered_after_mcp_startup() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let h = Harness::new();
    let mut daemon = h.start().await;
    let sessions = h.home.path().join(".claude/sessions");
    let parked = h.home.path().join(".claude/parked");
    fs::rename(&sessions, &parked).unwrap();
    let mut mcp = Command::new(env!("CARGO_BIN_EXE_session-bridge"))
        .arg("mcp")
        .env("HOME", h.home.path())
        .env("CODEX_HOME", h.home.path().join(".codex"))
        .env("CLAUDE_CONFIG_DIR", h.home.path().join(".claude"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut input = mcp.stdin.take().unwrap();
    let mut output = BufReader::new(mcp.stdout.take().unwrap()).lines();
    let initialize = json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"claude-startup-test","version":"1"}}});
    input
        .write_all(format!("{initialize}\n").as_bytes())
        .await
        .unwrap();
    timeout(Duration::from_secs(10), output.next_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    fs::rename(&parked, &sessions).unwrap();
    let notification = json!({"jsonrpc":"2.0","method":"notifications/initialized"});
    let call = json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"list_sessions","arguments":{}}});
    input
        .write_all(format!("{notification}\n{call}\n").as_bytes())
        .await
        .unwrap();
    loop {
        let line = timeout(Duration::from_secs(10), output.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let response: serde_json::Value = serde_json::from_str(&line).unwrap();
        if response["id"] == 2 {
            assert!(response.get("error").is_none(), "{response}");
            break;
        }
    }
    drop(input);
    timeout(Duration::from_secs(5), mcp.wait())
        .await
        .unwrap()
        .unwrap();
    stop(&mut daemon).await;
}
