//! Messages between sessions on two paired bridges. Unix only: the recipient
//! sessions are fake Claude inboxes on Unix sockets.
#![cfg(unix)]
use session_bridge::{
    domain::Delivery,
    protocol::{Reply, Request},
    transport::{self, PeerFrame},
};
use std::{fs, os::unix::fs::PermissionsExt, path::Path, time::Duration};
use tokio::{net::UnixListener, time::timeout};
use uuid::Uuid;

#[path = "lan/common.rs"]
mod common;
use common::{Node, PROJECT, eventually};

fn inbox(dir: &Path, name: &str) -> (UnixListener, std::path::PathBuf) {
    let path = dir.join(name);
    let listener = UnixListener::bind(&path).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    (listener, path)
}
async fn receive(listener: &UnixListener) -> String {
    let (mut stream, _) = timeout(Duration::from_secs(20), listener.accept())
        .await
        .expect("message arrives")
        .unwrap();
    let frame: PeerFrame = transport::read_frame(&mut stream).await.unwrap();
    frame.message.unwrap().content
}
async fn send(from: &Node, to: &str, text: &str) -> (Uuid, Delivery) {
    match from
        .rpc(Request::Send {
            sender: from.session.clone(),
            session_id: to.into(),
            message: text.into(),
            project: Some(PROJECT.into()),
        })
        .await
    {
        Reply::Receipt { receipt } => (receipt.message_id, receipt.delivery),
        other => panic!("Send failed: {other:?}"),
    }
}
async fn delivery(node: &Node, id: Uuid) -> Delivery {
    match node.rpc(Request::Status).await {
        Reply::Status { messages, .. } => {
            messages
                .into_iter()
                .find(|m| m.message_id == id)
                .expect("receipt")
                .delivery
        }
        other => panic!("{other:?}"),
    }
}
struct Pair {
    a: Node,
    b: Node,
    a_inbox: UnixListener,
    b_inbox: UnixListener,
    _sockets: tempfile::TempDir,
}
async fn paired(port_b: u16) -> Pair {
    let sockets = tempfile::Builder::new()
        .prefix("sb-in-")
        .tempdir_in("/tmp")
        .unwrap();
    // Claude's socket directory must be private, as in the routing harness.
    let dir = sockets.path().join("inboxes");
    session_bridge::store::ensure_private_dir(&dir).unwrap();
    let (a_inbox, a_path) = inbox(&dir, "a.sock");
    let (b_inbox, b_path) = inbox(&dir, "b.sock");
    let (mut a, mut b) = (Node::new("a", Some(&a_path)), Node::new("b", Some(&b_path)));
    a.start().await;
    b.start().await;
    a.network_on(0).await;
    let port_b = b.network_on(port_b).await;
    a.pair_with(&b, &format!("127.0.0.1:{port_b}")).await;
    Pair {
        a,
        b,
        a_inbox,
        b_inbox,
        _sockets: sockets,
    }
}

/* REQ-017: A session lists and messages a session on the paired laptop; the
reply comes back; delivery states travel back to the sender. */
#[tokio::test]
async fn sessions_message_across_paired_bridges() {
    let mut p = paired(0).await;
    let (sessions, unavailable) = p.a.sessions().await;
    assert!(unavailable.is_empty(), "{unavailable:?}");
    let remote = sessions
        .iter()
        .find(|s| s.session_id == p.b.session)
        .expect("remote session listed");
    assert_eq!(remote.host.as_deref(), Some(p.b.label.as_str()));
    let local = sessions
        .iter()
        .find(|s| s.session_id == p.a.session)
        .unwrap();
    assert_eq!(local.host, None);

    let (id, first) = send(&p.a, &p.b.session, "Cross-laptop finding").await;
    assert!(
        matches!(
            first,
            Delivery::Forwarded { .. } | Delivery::Queued | Delivery::Dispatching
        ),
        "{first:?}"
    );
    let content = receive(&p.b_inbox).await;
    assert!(content.contains("Cross-laptop finding"), "{content}");
    assert!(
        content.contains(&format!("From: {} / {PROJECT}", p.a.label)),
        "{content}"
    );
    eventually("queued status from the other host", async || {
        (delivery(&p.a, id).await == Delivery::Queued).then_some(())
    })
    .await;

    let (reply_id, _) = send(&p.b, &p.a.session, "Reply from b").await;
    let content = receive(&p.a_inbox).await;
    assert!(content.contains("Reply from b"), "{content}");
    assert!(
        content.contains(&format!("From: {} /", p.b.label)),
        "{content}"
    );
    eventually("queued reply", async || {
        (delivery(&p.b, reply_id).await == Delivery::Queued).then_some(())
    })
    .await;
    p.a.stop().await;
    p.b.stop().await;
}

/* REQ-017: A message to a laptop that is away stays pending and is delivered
once that bridge returns; the receiving bridge never delivers it twice. */
#[tokio::test]
async fn pending_until_the_other_laptop_returns() {
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let mut p = paired(port).await;
    p.a.sessions().await;
    p.b.stop().await;
    let (_, unavailable) = p.a.sessions().await;
    assert_eq!(unavailable, [p.b.label.clone()]);
    let (id, first) = send(&p.a, &p.b.session, "While you were away").await;
    assert!(
        matches!(first, Delivery::Pending { .. } | Delivery::Dispatching),
        "{first:?}"
    );
    eventually("pending reason", async || {
        matches!(delivery(&p.a, id).await, Delivery::Pending { ref reason } if reason.contains("unreachable"))
            .then_some(())
    })
    .await;
    p.b.start().await;
    let content = receive(&p.b_inbox).await;
    assert!(content.contains("While you were away"));
    eventually("queued after return", async || {
        (delivery(&p.a, id).await == Delivery::Queued).then_some(())
    })
    .await;
    assert!(
        timeout(Duration::from_secs(4), p.b_inbox.accept())
            .await
            .is_err(),
        "delivered once"
    );
    p.a.stop().await;
    p.b.stop().await;
}

/* REQ-017: After the other laptop unpairs, its sessions disappear and a send is
refused there, not retried. */
#[tokio::test]
async fn unpaired_host_refuses_messages() {
    let mut p = paired(0).await;
    p.a.sessions().await;
    match p
        .b
        .rpc(Request::Unpair {
            host: p.a.label.clone(),
        })
        .await
    {
        Reply::Unpaired { host } => assert_eq!(host, p.a.label),
        other => panic!("{other:?}"),
    }
    let (sessions, unavailable) = p.a.sessions().await;
    assert!(!sessions.iter().any(|s| s.session_id == p.b.session));
    assert_eq!(unavailable, [p.b.label.clone()]);
    let (id, _) = send(&p.a, &p.b.session, "Should be refused").await;
    eventually("refused", async || {
        matches!(delivery(&p.a, id).await, Delivery::Refused { ref reason } if reason.contains("not paired"))
            .then_some(())
    })
    .await;
    p.a.stop().await;
    p.b.stop().await;
}

/* REQ-017: With networking off a message waits and goes out once it is back on;
after this laptop unpairs the other one, a send is refused here. */
#[tokio::test]
async fn network_off_waits_and_local_unpair_refuses() {
    let mut p = paired(0).await;
    p.a.sessions().await;
    p.a.rpc(Request::Network {
        enabled: false,
        port: None,
        name: None,
    })
    .await;
    let (id, _) = send(&p.a, &p.b.session, "Sent while offline").await;
    eventually("pending while networking is off", async || {
        matches!(delivery(&p.a, id).await, Delivery::Pending { ref reason } if reason.contains("Networking is off"))
            .then_some(())
    })
    .await;
    p.a.network_on(0).await;
    assert!(receive(&p.b_inbox).await.contains("Sent while offline"));
    match p
        .a
        .rpc(Request::Unpair {
            host: p.b.label.clone(),
        })
        .await
    {
        Reply::Unpaired { host } => assert_eq!(host, p.b.label),
        other => panic!("{other:?}"),
    }
    let (id, _) = send(&p.a, &p.b.session, "After unpairing").await;
    eventually("refused after unpairing", async || {
        matches!(delivery(&p.a, id).await, Delivery::Refused { ref reason } if reason.contains("not paired with this laptop"))
            .then_some(())
    })
    .await;
    p.a.stop().await;
    p.b.stop().await;
}
