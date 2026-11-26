//! The user lists sessions, writes to them and reads their replies on the
//! admin page (REQ-019). Unix only: sessions are fake Claude inboxes.
#![cfg(unix)]
use serde_json::{Value, json};
use session_bridge::{
    protocol::{Reply, Request},
    transport::{self, PeerContent, PeerFrame},
};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::{
    net::{UnixListener, UnixStream},
    time::timeout,
};
use uuid::Uuid;

#[path = "lan/common.rs"]
mod common;
use common::{AdminPage, Node, PROJECT, eventually};

struct Inbox {
    listener: UnixListener,
    path: PathBuf,
}
fn inbox(dir: &Path, name: &str) -> Inbox {
    let path = dir.join(name);
    let listener = UnixListener::bind(&path).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    Inbox { listener, path }
}
fn private_dir() -> (tempfile::TempDir, PathBuf) {
    let sockets = tempfile::Builder::new()
        .prefix("sb-ad-")
        .tempdir_in("/tmp")
        .unwrap();
    let dir = sockets.path().join("inboxes");
    session_bridge::store::ensure_private_dir(&dir).unwrap();
    (sockets, dir)
}
async fn receive(inbox: &Inbox) -> PeerFrame {
    let (mut stream, _) = timeout(Duration::from_secs(20), inbox.listener.accept())
        .await
        .expect("message arrives")
        .unwrap();
    transport::read_frame(&mut stream).await.unwrap()
}
async fn messages(page: &AdminPage) -> (String, Vec<Value>) {
    let reply = page.ok(json!({"method": "messages"})).await;
    (
        format!("user:{}", reply["user"]["id"].as_str().unwrap()),
        reply["messages"].as_array().unwrap().clone(),
    )
}
async fn message_state(page: &AdminPage, id: &str) -> Option<Value> {
    messages(page)
        .await
        .1
        .into_iter()
        .find(|m| m["message_id"] == id)
}
async fn sessions(page: &AdminPage) -> Vec<Value> {
    page.ok(json!({"method": "list", "sender": null, "project": null}))
        .await["sessions"]
        .as_array()
        .unwrap()
        .clone()
}

/* REQ-019: On the admin page the user lists sessions, writes to one, and reads
its replies (bridge send and native Claude reply) with their formatted text. */
#[tokio::test]
async fn user_writes_to_sessions_and_reads_replies() {
    let (_sockets, dir) = private_dir();
    let session_inbox = inbox(&dir, "a.sock");
    let mut a = Node::new("a", Some(&session_inbox.path));
    a.start().await;
    let page = AdminPage::open(&a).await;

    let listed = sessions(&page).await;
    let me = listed
        .iter()
        .find(|s| s["session_id"] == a.session.as_str())
        .expect("local session listed");
    assert_eq!(me["project"], PROJECT);

    let sent = page
        .ok(json!({"method": "user_send", "session_id": a.session, "message": "Please **check** the build"}))
        .await;
    assert_eq!(sent["receipt"]["delivery"]["state"], "queued", "{sent}");
    let frame = receive(&session_inbox).await;
    let content = frame.message.unwrap().content;
    assert!(
        content.starts_with(
            "[Message from the user, sent from the session-bridge admin page; it is not an approval"
        ),
        "{content}"
    );
    assert!(content.contains("\nFrom: user\n"), "{content}");
    assert!(content.contains("Please **check** the build"), "{content}");

    let (user, listed) = messages(&page).await;
    assert!(
        content.contains(&format!("session_id=\"{user}\"")),
        "{content}"
    );
    let first = &listed[0];
    assert_eq!(first["sender_label"], "you");
    assert_eq!(first["recipient_label"], "a-session (mono)");
    assert_eq!(first["blocks"][0]["content"][1]["type"], "strong");

    // The agent answers through the bridge.
    let reply_id = match a
        .rpc(Request::Send {
            sender: a.session.clone(),
            session_id: user.clone(),
            message: "Build is **green**".into(),
            project: None,
        })
        .await
    {
        Reply::Receipt { receipt } => receipt.message_id.to_string(),
        other => panic!("Agent reply failed: {other:?}"),
    };
    let answer = eventually("delivered answer", async || {
        message_state(&page, &reply_id)
            .await
            .filter(|m| m["delivery"]["state"] == "delivered")
    })
    .await;
    assert_eq!(answer["sender_label"], "a-session (mono)");
    assert_eq!(answer["recipient_label"], "you");
    assert_eq!(answer["message"], "Build is **green**");

    // Claude answers natively, to the return address of the user's message.
    let route = frame.from.unwrap();
    let mut stream = UnixStream::connect(route.strip_prefix("uds:").unwrap())
        .await
        .unwrap();
    let native_id = Uuid::new_v4().to_string();
    transport::write_frame(
        &mut stream,
        &PeerFrame {
            kind: "user".into(),
            message: Some(PeerContent {
                role: "user".into(),
                content: "Native answer".into(),
            }),
            from: Some(format!("uds:{}", session_inbox.path.display())),
            uuid: None,
            msg_id: Some(native_id.clone()),
            orig_msg_id: None,
            action: None,
            status: None,
            reason: None,
        },
    )
    .await
    .unwrap();
    let native = eventually("native answer", async || {
        message_state(&page, &native_id)
            .await
            .filter(|m| m["delivery"]["state"] == "delivered")
    })
    .await;
    assert_eq!(native["recipient_label"], "you");
    a.stop().await;
}

/* REQ-019: Only the admin page writes as the user: the control socket refuses
user sends, agents cannot name the user as their sender, and the page cannot
send as an agent. */
#[tokio::test]
async fn only_the_admin_page_writes_as_the_user() {
    let (_sockets, dir) = private_dir();
    let session_inbox = inbox(&dir, "a.sock");
    let mut a = Node::new("a", Some(&session_inbox.path));
    a.start().await;
    let page = AdminPage::open(&a).await;
    let (user, _) = messages(&page).await;

    let reply = a
        .rpc(Request::UserSend {
            session_id: a.session.clone(),
            message: "hi".into(),
            channel: None,
            scheduled: false,
        })
        .await;
    assert!(
        matches!(&reply, Reply::Error { message } if message.contains("Only the admin page")),
        "{reply:?}"
    );
    let reply = a
        .rpc(Request::Send {
            sender: user.clone(),
            session_id: a.session.clone(),
            message: "hi".into(),
            project: None,
        })
        .await;
    assert!(
        matches!(&reply, Reply::Error { message } if message.contains("Only the admin page")),
        "{reply:?}"
    );
    let (status, _) = page
        .call(json!({"method": "send", "sender": a.session, "session_id": user, "message": "x"}))
        .await;
    assert_eq!(status, 403);
    for (body, error) in [
        (
            json!({"method": "user_send", "session_id": user, "message": "x"}),
            "Send to an agent session",
        ),
        (
            json!({"method": "user_send", "session_id": a.session, "message": " "}),
            "Message must be nonempty",
        ),
        (
            json!({"method": "user_send", "session_id": format!("claude:{}", Uuid::new_v4()), "message": "x"}),
            "Recipient session is unknown",
        ),
    ] {
        let (status, reply) = page.call(body).await;
        assert_eq!(status, 200);
        assert_eq!(reply["result"], "error");
        assert!(
            reply["message"].as_str().unwrap().contains(error),
            "{reply}"
        );
    }
    a.stop().await;
}

/* REQ-019, REQ-017: The user writes to a session on a paired laptop; that
laptop names the user and host, and the session's answer returns to this page. */
#[tokio::test]
async fn user_writes_across_paired_laptops() {
    let (_sockets, dir) = private_dir();
    let (a_inbox, b_inbox) = (inbox(&dir, "a.sock"), inbox(&dir, "b.sock"));
    let (mut a, mut b) = (
        Node::new("a", Some(&a_inbox.path)),
        Node::new("b", Some(&b_inbox.path)),
    );
    a.start().await;
    b.start().await;
    a.network_on(0).await;
    let port_b = b.network_on(0).await;
    a.pair_with(&b, &format!("127.0.0.1:{port_b}")).await;
    let page = AdminPage::open(&a).await;

    let listed = sessions(&page).await;
    let remote = listed
        .iter()
        .find(|s| s["session_id"] == b.session.as_str())
        .expect("remote session listed");
    assert_eq!(remote["host"], b.label.as_str());

    let sent = page
        .ok(json!({"method": "user_send", "session_id": b.session, "message": "Over there"}))
        .await;
    let sent_id = sent["receipt"]["message_id"].as_str().unwrap().to_owned();
    let content = receive(&b_inbox).await.message.unwrap().content;
    assert!(
        content.contains(&format!("\nFrom: user on {}\n", a.label)),
        "{content}"
    );
    assert!(content.contains("Over there"), "{content}");
    eventually("queued on the other laptop", async || {
        message_state(&page, &sent_id)
            .await
            .filter(|m| m["delivery"]["state"] == "queued")
    })
    .await;

    let (user, _) = messages(&page).await;
    let answer_id = match b
        .rpc(Request::Send {
            sender: b.session.clone(),
            session_id: user,
            message: "Answer from b".into(),
            project: None,
        })
        .await
    {
        Reply::Receipt { receipt } => receipt.message_id.to_string(),
        other => panic!("Remote answer failed: {other:?}"),
    };
    let answer = eventually("answer from the other laptop", async || {
        message_state(&page, &answer_id)
            .await
            .filter(|m| m["delivery"]["state"] == "delivered")
    })
    .await;
    assert_eq!(
        answer["sender_label"],
        format!("b-session ({PROJECT}) on {}", b.label)
    );
    assert_eq!(answer["recipient_label"], "you");

    // A channel spans the laptops: @channel reaches the project's sessions on both.
    let posted = page
        .ok(json!({"method": "channel_send", "project": PROJECT, "message": "@channel sync up"}))
        .await;
    assert_eq!(posted["receipts"].as_array().unwrap().len(), 2, "{posted}");
    let here = receive(&a_inbox).await.message.unwrap().content;
    assert!(here.contains("\nFrom: user in #mono\n"), "{here}");
    let there = receive(&b_inbox).await.message.unwrap().content;
    assert!(
        there.contains(&format!("\nFrom: user in #mono on {}\n", a.label)),
        "{there}"
    );
    let reply_id = match b
        .rpc(Request::Send {
            sender: b.session.clone(),
            session_id: answer_address(&there),
            message: "Synced".into(),
            project: None,
        })
        .await
    {
        Reply::Receipt { receipt } => receipt.message_id.to_string(),
        other => panic!("Channel answer failed: {other:?}"),
    };
    let reply = eventually("channel answer from the other laptop", async || {
        message_state(&page, &reply_id)
            .await
            .filter(|m| m["delivery"]["state"] == "delivered")
    })
    .await;
    assert_eq!(reply["channel"], PROJECT);
    a.stop().await;
    b.stop().await;
}

/// Adds another live (fake) Claude session to `node`'s home.
fn add_session(node: &Node, name: &str, cwd: &str, inbox: &Path) -> String {
    let id = Uuid::new_v4();
    let record = json!({
        "pid": std::process::id(),
        "sessionId": id,
        "cwd": cwd,
        "messagingSocketPath": inbox,
        "name": name,
        "status": "idle",
        "kind": "interactive"
    });
    fs::write(
        node.home
            .path()
            .join(format!(".claude/sessions/{name}.json")),
        record.to_string(),
    )
    .unwrap();
    format!("claude:{id}")
}
fn answer_address(content: &str) -> String {
    let start = content.find("send_message(session_id=\"").unwrap() + 25;
    content[start..start + 41].to_owned()
}
async fn nothing_arrives(inbox: &Inbox) {
    assert!(
        timeout(Duration::from_secs(1), inbox.listener.accept())
            .await
            .is_err(),
        "no message expected"
    );
}

/* REQ-020: A post in a project channel reaches every session of the project
with @channel and one session with @name; posts without a clear mention are
refused; answers to the channel come back to the channel. */
#[tokio::test]
async fn channel_posts_reach_mentioned_sessions() {
    let (_sockets, dir) = private_dir();
    let (first, second, other) = (
        inbox(&dir, "a.sock"),
        inbox(&dir, "b.sock"),
        inbox(&dir, "c.sock"),
    );
    let mut a = Node::new("a", Some(&first.path));
    add_session(&a, "second", &format!("/work/{PROJECT}"), &second.path);
    add_session(&a, "elsewhere", "/work/other", &other.path);
    a.start().await;
    let page = AdminPage::open(&a).await;

    for (message, error) in [
        (
            "hello all",
            "Mention @channel or a session (@name) to post in #mono",
        ),
        ("@nobody hi", "No session named @nobody in #mono"),
        ("@elsewhere hi", "No session named @elsewhere in #mono"),
    ] {
        let (status, reply) = page
            .call(json!({"method": "channel_send", "project": PROJECT, "message": message}))
            .await;
        assert_eq!(status, 200);
        assert_eq!(reply["result"], "error", "{reply}");
        assert_eq!(reply["message"], error);
    }

    let posted = page
        .ok(json!({"method": "channel_send", "project": PROJECT, "message": "@channel **stop** now"}))
        .await;
    assert_eq!(posted["result"], "posted");
    assert_eq!(posted["receipts"].as_array().unwrap().len(), 2, "{posted}");
    for inbox in [&first, &second] {
        let content = receive(inbox).await.message.unwrap().content;
        assert!(
            content.starts_with(
                "[Message from the user in #mono, sent from the session-bridge admin page"
            ),
            "{content}"
        );
        assert!(content.contains("\nFrom: user in #mono\n"), "{content}");
        assert!(content.contains("@channel **stop** now"), "{content}");
    }
    nothing_arrives(&other).await;

    page.ok(json!({"method": "channel_send", "project": PROJECT, "message": "@second only you"}))
        .await;
    let content = receive(&second).await.message.unwrap().content;
    assert!(content.contains("@second only you"), "{content}");
    nothing_arrives(&first).await;

    let channel = answer_address(&content);
    let (user, _) = messages(&page).await;
    assert!(channel.starts_with("user:") && channel != user, "{channel}");
    let answer_id = match a
        .rpc(Request::Send {
            sender: a.session.clone(),
            session_id: channel,
            message: "On it".into(),
            project: None,
        })
        .await
    {
        Reply::Receipt { receipt } => receipt.message_id.to_string(),
        other => panic!("Answer failed: {other:?}"),
    };
    let answer = eventually("answer in the channel", async || {
        message_state(&page, &answer_id)
            .await
            .filter(|m| m["delivery"]["state"] == "delivered")
    })
    .await;
    assert_eq!(answer["channel"], PROJECT);
    assert_eq!(answer["recipient_label"], "you");
    let (_, listed) = messages(&page).await;
    let post = listed
        .iter()
        .find(|m| m["message"] == "@second only you")
        .unwrap();
    assert_eq!(post["channel"], PROJECT);
    assert_eq!(post["sender_label"], "you");

    let reply = a
        .rpc(Request::ChannelSend {
            project: PROJECT.into(),
            message: "@channel hi".into(),
        })
        .await;
    assert!(
        matches!(&reply, Reply::Error { message } if message.contains("Only the admin page")),
        "{reply:?}"
    );
    a.stop().await;
}

/* REQ-024: The user sets a channel's default recipient on the admin page; a
post without a mention goes to it, a mention overrides it, it survives a
restart, and only the page can change it. */
#[tokio::test]
async fn channel_default_recipient() {
    let (_sockets, dir) = private_dir();
    let (first, second) = (inbox(&dir, "a.sock"), inbox(&dir, "b.sock"));
    let mut a = Node::new("a", Some(&first.path));
    add_session(&a, "second", &format!("/work/{PROJECT}"), &second.path);
    a.start().await;
    let page = AdminPage::open(&a).await;

    let (_, reply) = page
        .call(json!({"method": "channel_default", "project": PROJECT, "name": "nobody"}))
        .await;
    assert_eq!(reply["message"], "No session named @nobody in #mono");
    let set = page
        .ok(json!({"method": "channel_default", "project": PROJECT, "name": "SECOND"}))
        .await;
    assert_eq!(set["channels"][PROJECT]["default_recipient"], "second");

    page.ok(json!({"method": "channel_send", "project": PROJECT, "message": "status please"}))
        .await;
    let content = receive(&second).await.message.unwrap().content;
    assert!(content.contains("status please"), "{content}");
    nothing_arrives(&first).await;
    page.ok(
        json!({"method": "channel_send", "project": PROJECT, "message": "@a-session only you"}),
    )
    .await;
    assert!(
        receive(&first)
            .await
            .message
            .unwrap()
            .content
            .contains("only you")
    );
    nothing_arrives(&second).await;

    let refused = a
        .rpc(Request::ChannelDefault {
            project: PROJECT.into(),
            name: None,
        })
        .await;
    assert!(
        matches!(&refused, Reply::Error { message } if message.contains("Only the admin page")),
        "{refused:?}"
    );

    a.stop().await;
    a.start().await;
    let page = AdminPage::open(&a).await;
    let kept = page.ok(json!({"method": "channels"})).await;
    assert_eq!(kept["channels"][PROJECT]["default_recipient"], "second");

    let cleared = page
        .ok(json!({"method": "channel_default", "project": PROJECT, "name": null}))
        .await;
    assert!(cleared["channels"][PROJECT].is_null(), "{cleared}");
    let (_, reply) = page
        .call(json!({"method": "channel_send", "project": PROJECT, "message": "anyone?"}))
        .await;
    assert_eq!(
        reply["message"],
        "Mention @channel or a session (@name) to post in #mono"
    );
    a.stop().await;
}

/// A stand-in for `crontab` that keeps the table in `table`; never the user's.
fn fake_crontab(dir: &Path, table: &str) -> (PathBuf, PathBuf) {
    let file = dir.join("crontab.txt");
    fs::write(&file, table).unwrap();
    let script = dir.join("crontab.sh");
    fs::write(
        &script,
        format!(
            "#!/bin/sh\ncase \"$1\" in\n  -l) cat '{0}' ;;\n  -) cat > '{0}' ;;\n  *) exit 2 ;;\nesac\n",
            file.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    (script, file)
}

/* REQ-025: The user schedules a channel post on the admin page: the bridge
writes only its own crontab line, keeps the user's lines, refuses an invalid
schedule and any change from the control socket; what cron runs posts the
message, marked scheduled; deleting restores the table. */
#[tokio::test]
async fn schedules_post_through_crontab() {
    let (_sockets, dir) = private_dir();
    let (first, second) = (inbox(&dir, "a.sock"), inbox(&dir, "b.sock"));
    let original = "MAILTO=me\n15 3 * * * /usr/bin/backup --all\n";
    let (script, table) = fake_crontab(&dir, original);
    let mut a = Node::new("a", Some(&first.path));
    a.env.push((
        "SESSION_BRIDGE_CRONTAB".into(),
        script.display().to_string(),
    ));
    add_session(&a, "second", &format!("/work/{PROJECT}"), &second.path);
    a.start().await;
    let page = AdminPage::open(&a).await;

    let listed = page.ok(json!({"method": "schedules"})).await;
    assert_eq!(listed["available"], true);
    assert_eq!(
        listed["other_lines"],
        json!(["MAILTO=me", "15 3 * * * /usr/bin/backup --all"])
    );
    assert_eq!(listed["schedules"], json!([]));

    let (_, refused) = page
        .call(json!({"method": "save_schedule", "cron": "* * *", "project": PROJECT, "to": "second", "message": "x"}))
        .await;
    assert_eq!(
        refused["message"],
        "A schedule has five cron fields (minute hour day month weekday) or a macro such as @daily"
    );
    let text = "kick off with issues status and queued table in priority order";
    let saved = page
        .ok(json!({"method": "save_schedule", "cron": "*/10 * * * *", "project": PROJECT, "to": "second", "message": text}))
        .await;
    let entry = &saved["schedules"][0];
    assert_eq!(entry["installed"], true, "{saved}");
    let id = entry["id"].as_str().unwrap().to_owned();
    let written = fs::read_to_string(&table).unwrap();
    assert!(written.starts_with(original), "{written}");
    assert!(
        written.ends_with(&format!(" scheduled {id} # session-bridge schedule\n")),
        "{written}"
    );
    assert!(written.contains("\n*/10 * * * * '"), "{written}");
    assert_eq!(written.matches("# session-bridge schedule").count(), 1);

    let socket = a
        .rpc(Request::DeleteSchedule {
            id: Uuid::parse_str(&id).unwrap(),
        })
        .await;
    assert!(
        matches!(&socket, Reply::Error { message } if message.contains("Only the admin page")),
        "{socket:?}"
    );

    // What cron runs.
    let run = a.cli(&["scheduled", &id]).await;
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let content = receive(&second).await.message.unwrap().content;
    assert!(
        content.starts_with(
            "[Scheduled message from the user in #mono, sent by a schedule the user set"
        ),
        "{content}"
    );
    assert!(content.contains(text), "{content}");
    nothing_arrives(&first).await;
    let (_, listed) = messages(&page).await;
    let post = listed.iter().find(|m| m["message"] == text).unwrap();
    assert_eq!(post["scheduled"], true);
    assert_eq!(post["channel"], PROJECT);

    page.ok(json!({"method": "save_schedule", "id": id, "cron": "@hourly", "project": PROJECT, "to": "channel", "message": text}))
        .await;
    let written = fs::read_to_string(&table).unwrap();
    assert_eq!(
        written.matches("# session-bridge schedule").count(),
        1,
        "{written}"
    );
    assert!(written.contains("\n@hourly "), "{written}");
    assert!(a.cli(&["scheduled", &id]).await.status.success());
    for inbox in [&first, &second] {
        assert!(receive(inbox).await.message.unwrap().content.contains(text));
    }

    let deleted = page
        .ok(json!({"method": "delete_schedule", "id": id}))
        .await;
    assert_eq!(deleted["schedules"], json!([]));
    assert_eq!(fs::read_to_string(&table).unwrap(), original);
    let gone = a.cli(&["scheduled", &id]).await;
    assert!(!gone.status.success());
    assert!(String::from_utf8_lossy(&gone.stderr).contains("No schedule"));
    a.stop().await;
}
