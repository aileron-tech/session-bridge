//! Host and session usage on the admin page, across paired bridges (REQ-029).
use serde_json::{Value, json};
use session_bridge::lan::{
    channel::{Channel, StaticKey},
    model::{HostLabel, PairedPeer},
    state::LanStore,
};
use std::time::Duration;
use tokio::net::TcpListener;

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

/* REQ-029: The admin page shows this laptop's usage and a paired bridge's,
asked over the paired channel; a bridge that stops answering is "usage
unavailable" and the page call still succeeds. */
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
    let reply = page.ok(json!({"method": "usage"})).await;
    let peer = host(&reply, &b.label);
    assert_eq!(peer["state"], "unavailable", "{peer}");
    assert_eq!(peer["summary"], "usage unavailable");
    assert_available(host(&reply, &a.label));
    a.stop().await;
}

/* REQ-029: A paired bridge that predates the usage request closes the
connection on it (it cannot read the request); it reads as "usage
unavailable", not as an error. */
#[tokio::test]
async fn an_older_bridge_is_usage_unavailable() {
    let mut a = Node::new("a", None);
    a.start().await;
    a.network_on(0).await;
    let older = StaticKey::generate().unwrap();
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let address = listener.local_addr().unwrap();
    LanStore::open(&a.state())
        .unwrap()
        .save_peer(&PairedPeer {
            label: HostLabel::parse("older").unwrap(),
            fingerprint: older.fingerprint(),
            address: Some(address),
            paired_at: 1,
        })
        .unwrap();
    let served = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut channel = Channel::respond(stream, &older).await.unwrap();
        // What an older bridge does: the request does not parse, so it drops
        // the connection without an answer.
        let request: Value = channel.recv(Duration::from_secs(10)).await.unwrap();
        request["method"].as_str().unwrap().to_owned()
    });
    let page = AdminPage::open(&a).await;
    let reply = page.ok(json!({"method": "usage"})).await;
    assert_eq!(served.await.unwrap(), "usage");
    let peer = host(&reply, "older");
    assert_eq!(peer["state"], "unavailable", "{peer}");
    assert_eq!(peer["summary"], "usage unavailable");
    assert_available(host(&reply, &a.label));
    a.stop().await;
}

/* REQ-029: A live local session shows the CPU and memory of its process. */
#[cfg(unix)]
#[tokio::test]
async fn shows_a_local_sessions_process() {
    let sockets = tempfile::Builder::new()
        .prefix("sb-us-")
        .tempdir_in("/tmp")
        .unwrap();
    let path = sockets.path().join("a.sock");
    let _inbox = tokio::net::UnixListener::bind(&path).unwrap();
    let mut a = Node::new("a", Some(&path));
    a.start().await;
    let page = AdminPage::open(&a).await;
    let reply = page.ok(json!({"method": "usage"})).await;
    let session = reply["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["session_id"] == a.session.as_str())
        .unwrap_or_else(|| panic!("session missing from {reply}"));
    // The fake session's record names this test process.
    assert_eq!(session["usage"]["kind"], "own", "{session}");
    assert!(session["usage"]["memory"].as_u64().unwrap() > 0);
    let summary = session["summary"].as_str().unwrap();
    assert!(
        summary.starts_with("CPU ") && summary.contains(", memory "),
        "{summary}"
    );
    a.stop().await;
}
