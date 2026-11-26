//! Host and session usage on the admin page, across paired bridges (REQ-029).
use serde_json::{Value, json};
use session_bridge::lan::{
    channel::{Channel, StaticKey},
    model::{HostLabel, PairedPeer},
    state::LanStore,
    wire::{PeerReply, PeerRequest},
};
use std::time::{Duration, Instant};
use tokio::{
    net::{TcpListener, TcpStream},
    time::timeout,
};

#[path = "lan/common.rs"]
mod common;
use common::{AdminPage, Node};

fn host<'a>(reply: &'a Value, name: &str) -> &'a Value {
    reply["hosts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|h| h["host"] == name)
        .unwrap_or_else(|| panic!("{name} missing from {reply}"))
}
fn assert_available(view: &Value) {
    assert_eq!(view["state"], "available", "{view}");
    let usage = &view["usage"];
    for (used, total) in [("memory_used", "memory_total"), ("disk_used", "disk_total")] {
        let (used, total) = (
            usage[used].as_u64().unwrap(),
            usage[total].as_u64().unwrap(),
        );
        assert!(total > 0 && used <= total, "{view}");
    }
    let summary = view["summary"].as_str().unwrap();
    assert!(
        summary.starts_with("CPU ") && summary.contains(", memory ") && summary.contains(", disk "),
        "{summary}"
    );
}
fn assert_unavailable(view: &Value) {
    assert_eq!(view["state"], "unavailable", "{view}");
    assert_eq!(view["summary"], "usage unavailable");
}
/// A paired bridge stand-in on this test's own key, listening on a port of its own.
async fn fake_peer(node: &Node, name: &str) -> (StaticKey, TcpListener) {
    let key = StaticKey::generate().unwrap();
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    LanStore::open(&node.state())
        .unwrap()
        .save_peer(&PairedPeer {
            label: HostLabel::parse(name).unwrap(),
            fingerprint: key.fingerprint(),
            address: Some(listener.local_addr().unwrap()),
            paired_at: 1,
        })
        .unwrap();
    (key, listener)
}

/* REQ-029: The admin page shows this laptop's usage and a paired bridge's,
asked over the paired channel. The answer is reused for a while, so a bridge
that just stopped still shows its last usage; asked again (here after a
restart), it is "usage unavailable" and the page call still succeeds. */
#[tokio::test]
async fn shows_usage_of_this_laptop_and_paired_bridges() {
    let (mut a, mut b) = (Node::new("a", None), Node::new("b", None));
    a.start().await;
    b.start().await;
    a.network_on(0).await;
    let port_b = b.network_on(0).await;
    a.pair_with(&b, &format!("127.0.0.1:{port_b}")).await;
    let page = AdminPage::open(&a).await;

    let reply = page.ok(json!({"method": "usage"})).await;
    assert_eq!(reply["result"], "usage");
    assert_eq!(reply["hosts"][0]["host"], a.label.as_str());
    assert_eq!(reply["hosts"][0]["local"], true);
    assert_available(host(&reply, &a.label));
    let peer = host(&reply, &b.label);
    assert_eq!(peer["local"], false);
    assert_available(peer);

    b.stop().await;
    let cached = page.ok(json!({"method": "usage"})).await;
    assert_eq!(host(&cached, &b.label), peer);

    a.stop().await;
    a.start().await;
    let page = AdminPage::open(&a).await;
    let reply = page.ok(json!({"method": "usage"})).await;
    assert_unavailable(host(&reply, &b.label));
    assert_available(host(&reply, &a.label));
    a.stop().await;
}

/* REQ-029: A paired bridge that predates the usage request closes the
connection on it (it cannot read the request); it reads as "usage
unavailable", not as an error, and is not asked again on the next poll. */
#[tokio::test]
async fn an_older_bridge_is_usage_unavailable_and_backed_off() {
    let mut a = Node::new("a", None);
    a.start().await;
    a.network_on(0).await;
    let (older, listener) = fake_peer(&a, "older").await;
    let served = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut channel = Channel::respond(stream, &older).await.unwrap();
        // What an older bridge does: the request does not parse, so it drops
        // the connection without an answer.
        let request: Value = channel.recv(Duration::from_secs(10)).await.unwrap();
        drop(channel);
        let again = timeout(Duration::from_secs(3), listener.accept())
            .await
            .is_ok();
        (request["method"].as_str().unwrap().to_owned(), again)
    });
    let page = AdminPage::open(&a).await;
    let reply = page.ok(json!({"method": "usage"})).await;
    assert_unavailable(host(&reply, "older"));
    assert_available(host(&reply, &a.label));
    let reply = page.ok(json!({"method": "usage"})).await;
    assert_unavailable(host(&reply, "older"));
    let (method, asked_again) = served.await.unwrap();
    assert_eq!(method, "usage");
    assert!(
        !asked_again,
        "a failed bridge must not be asked on every poll"
    );
    a.stop().await;
}

/* REQ-029: A paired bridge that accepts the request and never answers times
out to "usage unavailable" in a few seconds; the page is not held for the
general request timeout. */
#[tokio::test]
async fn a_slow_bridge_times_out_to_unavailable() {
    let mut a = Node::new("a", None);
    a.start().await;
    a.network_on(0).await;
    let (slow, listener) = fake_peer(&a, "slow").await;
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut channel = Channel::respond(stream, &slow).await.unwrap();
        let _: Value = channel.recv(Duration::from_secs(10)).await.unwrap();
        tokio::time::sleep(Duration::from_secs(30)).await;
    });
    let page = AdminPage::open(&a).await;
    let started = Instant::now();
    let reply = page.ok(json!({"method": "usage"})).await;
    assert!(
        started.elapsed() < Duration::from_secs(8),
        "{:?}",
        started.elapsed()
    );
    assert_unavailable(host(&reply, "slow"));
    assert_eq!(host(&reply, "slow")["reason"], "No answer in time");
    a.stop().await;
}

/* REQ-029: Two page polls at the same moment ask a paired bridge once: the
second sees the query in flight instead of opening another connection. */
#[tokio::test]
async fn concurrent_polls_ask_a_bridge_once() {
    let mut a = Node::new("a", None);
    a.start().await;
    a.network_on(0).await;
    let (slow, listener) = fake_peer(&a, "slow").await;
    let served = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut channel = Channel::respond(stream, &slow).await.unwrap();
        let _: Value = channel.recv(Duration::from_secs(10)).await.unwrap();
        // Hold the first query open while the second poll arrives.
        timeout(Duration::from_secs(4), listener.accept())
            .await
            .is_ok()
    });
    let page = AdminPage::open(&a).await;
    let (first, second) = tokio::join!(page.ok(json!({"method": "usage"})), async {
        tokio::time::sleep(Duration::from_millis(300)).await;
        page.ok(json!({"method": "usage"})).await
    });
    assert_unavailable(host(&first, "slow"));
    assert_unavailable(host(&second, "slow"));
    assert_eq!(
        host(&second, "slow")["reason"],
        "Waiting for its first answer"
    );
    assert!(!served.await.unwrap(), "the bridge was asked twice");
    a.stop().await;
}

/* REQ-029: Only a paired bridge gets this bridge's usage. */
#[tokio::test]
async fn an_unpaired_bridge_is_refused_usage() {
    let mut a = Node::new("a", None);
    a.start().await;
    let port = a.network_on(0).await;
    let stranger = StaticKey::generate().unwrap();
    let stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let mut channel = Channel::initiate(stream, &stranger).await.unwrap();
    channel.send(&PeerRequest::Usage).await.unwrap();
    match channel.recv(Duration::from_secs(10)).await.unwrap() {
        PeerReply::Error { message } => assert!(message.contains("not paired"), "{message}"),
        other => panic!("Expected a refusal, got {other:?}"),
    }
    a.stop().await;
}

#[cfg(unix)]
mod local {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    async fn session_usage(page: &AdminPage, session: &str) -> Value {
        let reply = page.ok(json!({"method": "usage"})).await;
        reply["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["session_id"] == session)
            .cloned()
            .unwrap_or_else(|| panic!("session missing from {reply}"))
    }

    /* REQ-029: A live local session shows the CPU and memory of its process
    tree; when discovery starts failing, usage still answers with the hosts
    and the sessions last discovered. */
    #[tokio::test]
    async fn shows_local_sessions_also_when_discovery_fails() {
        let sockets = tempfile::Builder::new()
            .prefix("sb-us-")
            .tempdir_in("/tmp")
            .unwrap();
        let path = sockets.path().join("a.sock");
        let _inbox = tokio::net::UnixListener::bind(&path).unwrap();
        let mut a = Node::new("a", Some(&path));
        a.start().await;
        let page = AdminPage::open(&a).await;
        // The fake session's record names this test process.
        let session = session_usage(&page, &a.session).await;
        assert_eq!(session["usage"]["kind"], "own", "{session}");
        assert!(session["usage"]["memory"].as_u64().unwrap() > 0);
        assert!(session["usage"]["processes"].as_u64().unwrap() >= 1);
        let summary = session["summary"].as_str().unwrap();
        assert!(
            summary.starts_with("CPU ")
                && summary.contains(", memory ")
                && summary.contains("process"),
            "{summary}"
        );

        let records = a.home.path().join(".claude/sessions");
        std::fs::set_permissions(&records, std::fs::Permissions::from_mode(0o000)).unwrap();
        // Discovery runs every 3 s; let it fail at least once.
        tokio::time::sleep(Duration::from_secs(4)).await;
        let (status, reply) = page
            .call(json!({"method": "list", "sender": null, "project": null}))
            .await;
        assert_eq!(status, 200);
        assert_eq!(
            reply["result"], "error",
            "discovery should be failing: {reply}"
        );
        let session = session_usage(&page, &a.session).await;
        assert_eq!(session["usage"]["kind"], "own", "{session}");
        let reply = page.ok(json!({"method": "usage"})).await;
        assert_eq!(reply["hosts"][0]["local"], true);
        assert_available(&reply["hosts"][0]);
        std::fs::set_permissions(&records, std::fs::Permissions::from_mode(0o700)).unwrap();
        a.stop().await;
    }
}
