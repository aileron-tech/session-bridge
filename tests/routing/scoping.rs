use super::*;
use session_bridge::{discovery, domain::SessionId, store::Envelope};

async fn scoped_rpc(h: &Harness, value: serde_json::Value) -> Reply {
    let mut stream = transport::connect(&h.control).await.unwrap();
    transport::write_frame(&mut stream, &value).await.unwrap();
    transport::read_frame(&mut stream).await.unwrap()
}

/* REQ-014: Defaults isolate equal project names in different directories; explicit scope permits coordination. */
#[tokio::test]
async fn caller_project_is_the_default_scope() {
    let h = Harness::new();
    let record_path = h.home.path().join(".claude/sessions/target.json");
    let mut record: serde_json::Value =
        serde_json::from_slice(&fs::read(&record_path).unwrap()).unwrap();
    record["cwd"] = json!("/elsewhere/mono");
    fs::write(&record_path, serde_json::to_vec(&record).unwrap()).unwrap();
    let mut daemon = h.start().await;
    let cli = Command::new(env!("CARGO_BIN_EXE_session-bridge"))
        .arg("list")
        .current_dir(h.home.path())
        .env("HOME", h.home.path())
        .output()
        .await
        .unwrap();
    assert!(
        cli.status.success(),
        "{}",
        String::from_utf8_lossy(&cli.stderr)
    );
    let Reply::Sessions { sessions, .. } = serde_json::from_slice(&cli.stdout).unwrap() else {
        panic!("Expected CLI sessions");
    };
    assert!(sessions.is_empty(), "CLI defaults to its current directory");
    for (project, expected) in [
        (None, &h.sender_id),
        (Some("/elsewhere/mono"), &h.target_id),
    ] {
        let reply = scoped_rpc(
            &h,
            json!({"method":"list","sender":h.sender_id,"project":project}),
        )
        .await;
        let Reply::Sessions { sessions, .. } = reply else {
            panic!("Expected sessions: {reply:?}");
        };
        assert_eq!(sessions.len(), 1);
        assert_eq!(&sessions[0].session_id, expected);
    }
    let reply = scoped_rpc(
        &h,
        json!({"method":"list","sender":format!("codex:{}",Uuid::new_v4())}),
    )
    .await;
    assert!(matches!(reply, Reply::Error { .. }));
    let reply = scoped_rpc(&h, json!({"method":"send","sender":h.sender_id,"session_id":h.target_id,"message":"Implicit cross-project send"})).await;
    assert!(matches!(reply, Reply::Error { .. }));
    let state = Store::open(h.control.parent().unwrap().to_owned()).unwrap();
    assert!(state.messages().unwrap().is_empty());
    let reply = scoped_rpc(&h, json!({"method":"send","sender":h.sender_id,"session_id":h.target_id,"message":"Explicit coordination","project":"/elsewhere/mono"})).await;
    assert!(matches!(reply, Reply::Receipt { .. }), "{reply:?}");
    let content = receive(h.target.as_ref().unwrap())
        .await
        .message
        .unwrap()
        .content;
    assert!(content.contains("Explicit coordination"));
    assert!(content.contains("project=\"/work/mono\""));
    let reply = scoped_rpc(&h, json!({"method":"send","sender":h.target_id,"session_id":h.sender_id,"message":"Explicit reply","project":"/work/mono"})).await;
    assert!(matches!(reply, Reply::Receipt { .. }), "{reply:?}");
    assert!(
        receive(&h.sender)
            .await
            .message
            .unwrap()
            .content
            .contains("Explicit reply")
    );
    let reply = scoped_rpc(&h, json!({"method":"list","sender":h.sender_id})).await;
    let Reply::Sessions {
        recent_messages, ..
    } = reply
    else {
        panic!("Expected sessions");
    };
    assert!(
        recent_messages.is_empty(),
        "Other-project receipts stay outside the default topic"
    );
    stop(&mut daemon).await;
}

/* REQ-009, REQ-038: Native peer records name Codex, belong to the socket process, and route without duplicate discovery; a message for a remembered, not live session is refused. */
#[tokio::test]
async fn native_codex_peer_routes_claude_messages_and_cleans_up() {
    use tokio::io::AsyncWriteExt;
    let h = Harness::new();
    let codex_id = Uuid::new_v4();
    let raw =
        json!({"type":"session_meta","payload":{"id":codex_id,"cwd":"/work/mono","source":"cli"}});
    let codex = discovery::parse_codex_record(&raw.to_string(), std::process::id()).unwrap();
    let state = Store::open(h.control.parent().unwrap().to_owned()).unwrap();
    store::write_json(
        &state.root.join("sessions.json"),
        &std::collections::BTreeMap::from([(codex.session_id.clone(), codex.clone())]),
    )
    .unwrap();
    let mut daemon = h.start().await;
    let mut helper = Command::new(env!("CARGO_BIN_EXE_session-bridge"))
        .arg("native-peer")
        .env("HOME", h.home.path())
        .env("CODEX_HOME", h.home.path().join(".codex"))
        .env("CLAUDE_CONFIG_DIR", h.home.path().join(".claude"))
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let input = session_bridge::native_peers::PeerInput {
        session: codex.clone(),
        owner_pid: std::process::id(),
    };
    helper
        .stdin
        .take()
        .unwrap()
        .write_all(&serde_json::to_vec(&input).unwrap())
        .await
        .unwrap();
    let pid = helper.id().unwrap();
    let record_path = h.home.path().join(format!(".claude/sessions/{pid}.json"));
    timeout(Duration::from_secs(5), async {
        while !record_path.exists() {
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let record: serde_json::Value =
        serde_json::from_slice(&fs::read(&record_path).unwrap()).unwrap();
    assert_eq!(record["pid"], pid);
    assert_eq!(record["sessionId"], codex_id.to_string());
    assert!(record["name"].as_str().unwrap().starts_with("codex-mono-"));
    let socket = PathBuf::from(record["messagingSocketPath"].as_str().unwrap());
    let mut stream = transport::connect(&socket).await.unwrap();
    assert_eq!(stream.peer_cred().unwrap().pid(), Some(pid as i32));
    let sources = discovery::Sources {
        codex_home: h.home.path().join(".codex"),
        claude_home: h.home.path().join(".claude"),
    };
    assert_eq!(discovery::discover(&sources).await.unwrap().len(), 2);
    let message_id = Uuid::new_v4();
    let frame = json!({"type":"user","message":{"role":"user","content":"Native peer check"},"from":format!("uds:{}",h.sender.local_addr().unwrap().as_pathname().unwrap().display()),"msg_id":message_id});
    transport::write_frame(&mut stream, &frame).await.unwrap();
    // The Codex session is only remembered, not live: it has ended (REQ-038).
    h.wait_delivery(
        message_id,
        |d| matches!(d, Delivery::Refused { reason } if reason.contains("has ended")),
    )
    .await;
    let messages = state.messages().unwrap();
    assert_eq!(messages[0].recipient.to_string(), codex.session_id);
    assert_eq!(messages[0].sender.to_string(), h.sender_id);
    stop(&mut helper).await;
    assert!(!record_path.exists());
    assert!(!socket.exists());
    stop(&mut daemon).await;
}

/* REQ-008: Project scoping lists exact matches and rejects mismatched recipients before persistence. */
#[tokio::test]
async fn project_filter_scopes_listing_and_sending() {
    let h = Harness::new();
    let mut daemon = h.start().await;
    for (project, expected) in [
        ("mono", 2),
        ("/work/mono", 2),
        ("mon", 0),
        ("/elsewhere/mono", 0),
    ] {
        let reply = scoped_rpc(&h, json!({"method":"list","project":project})).await;
        let Reply::Sessions { sessions, .. } = reply else {
            panic!("Expected sessions");
        };
        assert_eq!(sessions.len(), expected, "{project}");
    }
    for project in ["other", "/elsewhere/mono", "", "work/mono"] {
        let reply = scoped_rpc(&h, json!({"method":"send","sender":h.sender_id,"session_id":h.target_id,"message":"Scoped message","project":project})).await;
        assert!(matches!(reply, Reply::Error { .. }), "{reply:?}");
    }
    let state = Store::open(h.control.parent().unwrap().to_owned()).unwrap();
    assert!(state.messages().unwrap().is_empty());
    let reply = scoped_rpc(&h, json!({"method":"send","sender":h.sender_id,"session_id":h.target_id,"message":"Scoped message","project":"/work/mono"})).await;
    assert!(matches!(reply, Reply::Receipt { .. }), "{reply:?}");
    assert!(
        receive(h.target.as_ref().unwrap())
            .await
            .message
            .unwrap()
            .content
            .contains("Scoped message")
    );
    stop(&mut daemon).await;
}

/* REQ-008, REQ-014: An implicitly project-scoped message cannot move to another project on retry. */
#[tokio::test]
async fn pending_delivery_preserves_project_filter_after_restart() {
    let mut h = Harness::new();
    let mut daemon = h.start().await;
    drop(h.target.take());
    fs::remove_file(&h.target_path).unwrap();
    let reply = scoped_rpc(&h, json!({"method":"send","sender":h.sender_id,"session_id":h.target_id,"message":"Stay in project"})).await;
    let Reply::Receipt { receipt } = reply else {
        panic!("Expected receipt");
    };
    assert!(matches!(receipt.delivery, Delivery::Pending { .. }));
    let state = Store::open(h.control.parent().unwrap().to_owned()).unwrap();
    assert_eq!(
        state.messages().unwrap()[0].project.as_deref(),
        Some("/work/mono")
    );
    stop(&mut daemon).await;
    let record_path = h.home.path().join(".claude/sessions/target.json");
    let mut record: serde_json::Value =
        serde_json::from_slice(&fs::read(&record_path).unwrap()).unwrap();
    record["cwd"] = json!("/elsewhere/mono");
    fs::write(&record_path, serde_json::to_vec(&record).unwrap()).unwrap();
    h.target = Some(bind(&h.target_path));
    daemon = h.start().await;
    h.wait_delivery(receipt.message_id, |d| {
        matches!(d, Delivery::Refused { .. })
    })
    .await;
    assert!(
        timeout(
            Duration::from_millis(100),
            h.target.as_ref().unwrap().accept()
        )
        .await
        .is_err()
    );
    stop(&mut daemon).await;
}

/* REQ-007: Subagents are hidden, cannot send or receive, and old pending work is refused. */
#[tokio::test]
async fn subagents_stay_under_parent_control() {
    let h = Harness::new();
    let record_path = h.home.path().join(".claude/sessions/target.json");
    let mut record: serde_json::Value =
        serde_json::from_slice(&fs::read(&record_path).unwrap()).unwrap();
    record["kind"] = json!("subagent");
    fs::write(&record_path, serde_json::to_vec(&record).unwrap()).unwrap();
    let sources = discovery::Sources {
        claude_home: h.home.path().join(".claude"),
        codex_home: h.home.path().join(".codex"),
    };
    let sessions = discovery::discover(&sources).await.unwrap();
    let known: std::collections::BTreeMap<_, _> = sessions
        .into_iter()
        .map(|s| (s.session_id.clone(), s))
        .collect();
    let state = Store::open(h.control.parent().unwrap().to_owned()).unwrap();
    store::write_json(&state.root.join("sessions.json"), &known).unwrap();
    let pending = Envelope {
        message_id: Uuid::new_v4(),
        sender: SessionId::parse(&h.sender_id).unwrap(),
        recipient: SessionId::parse(&h.target_id).unwrap(),
        message: "Old pending message".into(),
        project: None,
        delivery: Delivery::Pending {
            reason: "Offline".into(),
        },
        created_at: 1,
        scheduled: false,
    };
    state.save(&pending).unwrap();
    let mut daemon = h.start().await;
    let Reply::Sessions { sessions, .. } = h
        .rpc(Request::List {
            sender: None,
            project: None,
        })
        .await
    else {
        panic!("Expected sessions");
    };
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].session_id, h.sender_id);
    for (sender, recipient) in [(&h.sender_id, &h.target_id), (&h.target_id, &h.sender_id)] {
        let reply = h
            .rpc(Request::Send {
                sender: sender.clone(),
                session_id: recipient.clone(),
                message: "Blocked".into(),
                project: None,
            })
            .await;
        assert!(matches!(reply, Reply::Error { message } if message.contains("Subagents")));
    }
    h.wait_delivery(pending.message_id, |d| {
        matches!(d, Delivery::Refused { .. })
    })
    .await;
    assert_eq!(state.messages().unwrap().len(), 1);
    assert!(
        timeout(
            Duration::from_millis(100),
            h.target.as_ref().unwrap().accept()
        )
        .await
        .is_err()
    );
    stop(&mut daemon).await;
}
