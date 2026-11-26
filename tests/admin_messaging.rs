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
    a.stop().await;
    b.stop().await;
}
