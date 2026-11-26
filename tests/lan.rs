//! LAN discovery, pairing, admin and access control between real bridges.
use session_bridge::{
    lan::{
        channel::{Channel, StaticKey},
        model::PairingOutcome,
        wire::{PeerReply, PeerRequest},
    },
    protocol::{Reply, Request},
};
use std::time::Duration;
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::TcpStream,
};

#[path = "lan/common.rs"]
mod common;
use common::{Node, eventually, wrong};

/* REQ-017: The printer flow from the CLI: the chosen laptop shows a one-time
code, the other types it (one wrong try first), and both now trust each other. */
#[tokio::test]
async fn cli_pairs_with_a_one_time_code() {
    let (mut a, mut b) = (Node::new("a", None), Node::new("b", None));
    a.start().await;
    b.start().await;
    let on = a
        .cli(&["network", "on", "--port", "0", "--name", &a.label])
        .await;
    assert!(
        on.status.success(),
        "{}",
        String::from_utf8_lossy(&on.stderr)
    );
    let port_b = b.network_on(0).await;

    let mut shower = b
        .cli_command(&["pair"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut chooser = a
        .cli_command(&["pair", &format!("127.0.0.1:{port_b}")])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(shower.stdout.take().unwrap()).lines();
    let code = loop {
        let line = tokio::time::timeout(Duration::from_secs(20), lines.next_line())
            .await
            .expect("code line")
            .unwrap()
            .expect("output");
        if let Some(rest) = line.split("Code ").nth(1) {
            assert!(line.contains(&a.label), "{line}");
            break rest[..7].to_owned();
        }
    };
    let mut input = chooser.stdin.take().unwrap();
    input
        .write_all(format!("{}\n{code}\n", wrong(&code)).as_bytes())
        .await
        .unwrap();
    drop(input);
    let chosen = tokio::time::timeout(Duration::from_secs(20), chooser.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    let stdout = String::from_utf8_lossy(&chosen.stdout);
    let stderr = String::from_utf8_lossy(&chosen.stderr);
    assert!(chosen.status.success(), "{stdout}{stderr}");
    assert!(stderr.contains("Wrong code; 2 attempts left"), "{stderr}");
    assert!(
        stdout.contains(&format!("Paired with {}", b.label)),
        "{stdout}"
    );
    let mut rest = String::new();
    while let Ok(Some(line)) = lines.next_line().await {
        rest.push_str(&line);
    }
    assert!(rest.contains(&format!("Paired with {}", a.label)), "{rest}");
    assert!(shower.wait().await.unwrap().success());

    let a_view = a.lan().await;
    assert!(a_view.hosts.iter().any(|h| h.host == b.label && h.paired));
    let b_view = b.lan().await;
    assert!(b_view.hosts.iter().any(|h| h.host == a.label && h.paired));
    assert!(matches!(
        b_view.pairing.last,
        Some(PairingOutcome::Paired { ref host, .. }) if host == &a.label
    ));
    let discover = a.cli(&["discover"]).await;
    assert!(String::from_utf8_lossy(&discover.stdout).contains(&b.label));
    let unpair = a.cli(&["unpair", &b.label]).await;
    assert!(unpair.status.success());
    assert!(
        !a.lan()
            .await
            .hosts
            .iter()
            .any(|h| h.host == b.label && h.paired)
    );
    a.stop().await;
    b.stop().await;
}

/* REQ-017: Three wrong codes close the request; that address then waits. */
#[tokio::test]
async fn wrong_codes_close_the_request() {
    let (mut a, mut b) = (Node::new("a", None), Node::new("b", None));
    a.start().await;
    b.start().await;
    a.network_on(0).await;
    let port_b = b.network_on(0).await;
    let host = format!("127.0.0.1:{port_b}");
    let id = match a.rpc(Request::Pair { host: host.clone() }).await {
        Reply::Pairing { id, expires_in, .. } => {
            assert!(expires_in > 100);
            id
        }
        other => panic!("{other:?}"),
    };
    let code = b.wait_code().await;
    for left in [2, 1] {
        let reply = a
            .rpc(Request::PairCode {
                id,
                code: wrong(&code),
            })
            .await;
        assert!(
            matches!(reply, Reply::WrongCode { attempts_left } if attempts_left == left),
            "{reply:?}"
        );
    }
    let reply = a
        .rpc(Request::PairCode {
            id,
            code: code.clone(),
        })
        .await;
    assert!(
        matches!(reply, Reply::Paired { .. }),
        "third try is still allowed: {reply:?}"
    );

    let id = match a.rpc(Request::Pair { host: host.clone() }).await {
        Reply::Pairing { id, .. } => id,
        other => panic!("{other:?}"),
    };
    let code = b.wait_code().await;
    for _ in 0..2 {
        a.rpc(Request::PairCode {
            id,
            code: wrong(&code),
        })
        .await;
    }
    let reply = a
        .rpc(Request::PairCode {
            id,
            code: wrong(&code),
        })
        .await;
    assert!(
        matches!(&reply, Reply::Error { message } if message.contains("Too many wrong codes")),
        "{reply:?}"
    );
    let view = b.lan().await;
    assert!(view.pairing.incoming.is_none());
    assert!(matches!(
        view.pairing.last,
        Some(PairingOutcome::Failed { .. })
    ));
    let reply = a.rpc(Request::Pair { host }).await;
    assert!(
        matches!(&reply, Reply::Error { message } if message.contains("try again in")),
        "{reply:?}"
    );
    let reply = a.rpc(Request::PairCode { id, code }).await;
    assert!(
        matches!(reply, Reply::Error { .. }),
        "closed request: {reply:?}"
    );
    a.stop().await;
    b.stop().await;
}

/* REQ-017: A bridge that is not paired gets nothing; pairing needs networking;
a bad host or code format fails before any network use. */
#[tokio::test]
async fn unpaired_bridges_are_refused() {
    let mut b = Node::new("b", None);
    b.start().await;
    let reply = b
        .rpc(Request::Pair {
            host: "127.0.0.1:9".into(),
        })
        .await;
    assert!(
        matches!(&reply, Reply::Error { message } if message.contains("Networking is off")),
        "{reply:?}"
    );
    let port = b.network_on(0).await;
    let stranger = StaticKey::generate().unwrap();
    for request in [
        PeerRequest::List { project: None },
        PeerRequest::Pair {
            spake: "AAAA".into(),
        },
    ] {
        let stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        let mut channel = Channel::initiate(stream, &stranger).await.unwrap();
        channel.send(&request).await.unwrap();
        let reply: PeerReply = channel.recv(Duration::from_secs(5)).await.unwrap();
        assert!(matches!(reply, PeerReply::Error { .. }), "{reply:?}");
    }
    let stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let mut channel = Channel::initiate(stream, &stranger).await.unwrap();
    channel
        .send(&PeerRequest::PairRequest {
            name: "not a label".into(),
            port: 1,
        })
        .await
        .unwrap();
    let reply: PeerReply = channel.recv(Duration::from_secs(5)).await.unwrap();
    assert!(matches!(reply, PeerReply::Error { .. }), "{reply:?}");

    for host in ["nobody", "ABCD"] {
        let reply = b.rpc(Request::Pair { host: host.into() }).await;
        assert!(
            matches!(&reply, Reply::Error { message } if message.contains("No bridge named")),
            "{reply:?}"
        );
    }
    let reply = b
        .rpc(Request::Pair {
            host: format!("127.0.0.1:{port}"),
        })
        .await;
    assert!(
        matches!(&reply, Reply::Error { message } if message.contains("itself")),
        "{reply:?}"
    );
    let reply = b
        .rpc(Request::PairCode {
            id: uuid::Uuid::new_v4(),
            code: "12".into(),
        })
        .await;
    assert!(matches!(reply, Reply::Error { .. }));
    let reply = b
        .rpc(Request::Unpair {
            host: "nobody".into(),
        })
        .await;
    assert!(matches!(reply, Reply::Error { .. }));
    b.stop().await;
}

/* REQ-017: Turning networking off closes the port; on again reopens it, and the
setting and key survive a restart. */
#[tokio::test]
async fn network_switch_persists() {
    let mut a = Node::new("a", None);
    a.start().await;
    let initial = a.lan().await;
    assert!(!initial.network.enabled);
    assert!(initial.network.port.is_none());
    let port = free_port();
    assert_eq!(a.network_on(port).await, port);
    assert!(TcpStream::connect(("127.0.0.1", port)).await.is_ok());
    let off = a.cli(&["network", "off"]).await;
    assert!(off.status.success());
    eventually("closed port", async || {
        TcpStream::connect(("127.0.0.1", port))
            .await
            .is_err()
            .then_some(())
    })
    .await;
    assert!(!a.lan().await.network.enabled);
    assert_eq!(a.network_on(port).await, port);
    let key = a.lan().await.network.key;
    a.stop().await;
    a.start().await;
    let view = a.lan().await;
    assert!(view.network.enabled);
    assert_eq!(view.network.port, Some(port));
    assert_eq!(view.network.key, key);
    assert_eq!(view.network.host, a.label);
    let bad = a.cli(&["network", "on", "--name", "bad name"]).await;
    assert!(!bad.status.success());
    a.stop().await;
}

/* REQ-017: Each bridge announces itself; the other finds it by name and pairs. */
#[tokio::test]
async fn bridges_discover_each_other() {
    let (mut a, mut b) = (Node::new("a", None), Node::new("b", None));
    a.start().await;
    b.start().await;
    a.network_on(0).await;
    b.network_on(0).await;
    let found = eventually("mDNS announcement", async || {
        a.lan().await.hosts.into_iter().find(|h| h.host == b.label)
    })
    .await;
    assert!(found.online && !found.paired, "{found:?}");
    assert!(!found.addresses.is_empty());
    a.pair_with(&b, &b.label).await;
    let host = a
        .lan()
        .await
        .hosts
        .into_iter()
        .find(|h| h.host == b.label)
        .unwrap();
    assert!(host.paired && host.online);
    a.stop().await;
    b.stop().await;
}

async fn http(port: u16, request: String) -> (u16, String) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    let status = response[9..12].parse().unwrap();
    let body = response
        .split_once("\r\n\r\n")
        .map(|(_, b)| b.to_owned())
        .unwrap_or_default();
    (status, body)
}
fn call(host: &str, token: Option<&str>, body: &str) -> String {
    let auth = token
        .map(|t| format!("Authorization: Bearer {t}\r\n"))
        .unwrap_or_default();
    format!(
        "POST /api/call HTTP/1.1\r\nHost: {host}\r\n{auth}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

/* REQ-018: The admin page is local, needs the token from session-bridge admin,
and exposes setup actions only. */
#[tokio::test]
async fn admin_page_requires_its_token() {
    let mut a = Node::new("a", None);
    a.start().await;
    let url = match a.rpc(Request::Admin).await {
        Reply::Admin { url } => url,
        other => panic!("{other:?}"),
    };
    let again = match a.rpc(Request::Admin).await {
        Reply::Admin { url } => url,
        other => panic!("{other:?}"),
    };
    assert_eq!(url, again, "one admin server per service");
    let rest = url.strip_prefix("http://127.0.0.1:").unwrap();
    let (port, token) = rest.split_once("/#token=").unwrap();
    let port: u16 = port.parse().unwrap();
    let host = format!("127.0.0.1:{port}");

    let (status, page) = http(
        port,
        format!("GET / HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n"),
    )
    .await;
    assert_eq!(status, 200);
    assert!(page.contains("<title>session-bridge"));
    assert!(!page.contains(token), "the page never embeds the token");

    let lan = r#"{"method":"lan"}"#;
    assert_eq!(http(port, call(&host, None, lan)).await.0, 401);
    assert_eq!(http(port, call(&host, Some("wrong"), lan)).await.0, 401);
    assert_eq!(
        http(port, call("evil.example:80", Some(token), lan))
            .await
            .0,
        403
    );
    let send = r#"{"method":"send","sender":"x","session_id":"y","message":"z"}"#;
    assert_eq!(http(port, call(&host, Some(token), send)).await.0, 403);
    let (status, body) = http(port, call(&host, Some(token), lan)).await;
    assert_eq!(status, 200, "{body}");
    assert!(body.contains(r#""result":"lan""#), "{body}");
    let on = format!(
        r#"{{"method":"network","enabled":true,"port":0,"name":"{}"}}"#,
        a.label
    );
    let (status, body) = http(port, call(&host, Some(token), &on)).await;
    assert_eq!(status, 200, "{body}");
    assert!(body.contains(r#""enabled":true"#), "{body}");
    let (status, body) = http(port, call(&host, Some(token), r#"{"method":"status"}"#)).await;
    assert_eq!(status, 200, "{body}");
    let (status, _) = http(port, call(&host, Some(token), "not json")).await;
    assert!(status == 400 || status == 422, "{status}");
    let localhost = format!("localhost:{port}");
    assert_eq!(http(port, call(&localhost, Some(token), lan)).await.0, 200);
    a.stop().await;
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/* REQ-017: The CLI rejects bad input with a usable message, and the chosen
laptop's pair view reports a failed pairing. */
#[tokio::test]
async fn cli_reports_bad_input_and_failed_pairing() {
    let (mut a, mut b) = (Node::new("a", None), Node::new("b", None));
    a.start().await;
    b.start().await;
    let off = a.cli(&["pair"]).await;
    assert!(!off.status.success());
    assert!(String::from_utf8_lossy(&off.stderr).contains("Networking is off"));
    for args in [
        &["network", "maybe"][..],
        &["network", "on", "--port"],
        &["network", "on", "--port", "x"],
        &["network", "on", "--bogus", "1"],
        &["network", "off", "now"],
    ] {
        let output = a.cli(args).await;
        assert!(!output.status.success(), "{args:?}");
    }
    let usage = a.cli(&["help"]).await;
    assert!(String::from_utf8_lossy(&usage.stdout).contains("pair [<host>]"));
    let admin = a.cli(&["admin", "--no-open"]).await;
    assert!(String::from_utf8_lossy(&admin.stdout).starts_with("http://127.0.0.1:"));

    a.network_on(0).await;
    let port_b = b.network_on(0).await;
    let host = format!("127.0.0.1:{port_b}");
    let mut typo = a
        .cli_command(&["pair", &host])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = typo.stdin.take().unwrap();
    input.write_all(b"12\n").await.unwrap();
    drop(input);
    let typo = typo.wait_with_output().await.unwrap();
    let stderr = String::from_utf8_lossy(&typo.stderr);
    assert!(!typo.status.success());
    assert!(stderr.contains("Expected 6 digits"), "{stderr}");
    assert!(stderr.contains("No code entered"), "{stderr}");

    let mut shower = b
        .cli_command(&["pair"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let id = match a.rpc(Request::Pair { host }).await {
        Reply::Pairing { id, .. } => id,
        other => panic!("{other:?}"),
    };
    let code = b.wait_code().await;
    // Type wrong codes only once the chosen laptop shows the code, as a user would.
    let mut lines = BufReader::new(shower.stdout.take().unwrap()).lines();
    loop {
        let line = tokio::time::timeout(Duration::from_secs(20), lines.next_line())
            .await
            .expect("code line")
            .unwrap()
            .expect("output");
        if line.contains(&code) {
            break;
        }
    }
    for _ in 0..3 {
        a.rpc(Request::PairCode {
            id,
            code: wrong(&code),
        })
        .await;
    }
    let shown = tokio::time::timeout(Duration::from_secs(20), shower.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert!(!shown.status.success());
    assert!(
        String::from_utf8_lossy(&shown.stderr).contains("failed"),
        "{}",
        String::from_utf8_lossy(&shown.stderr)
    );
    a.stop().await;
    b.stop().await;
}

/* REQ-017: A paired bridge is still checked: no pairing messages out of order,
no invalid project, no forged sender, no unknown recipient. */
#[tokio::test]
async fn paired_peer_input_is_validated() {
    use session_bridge::{
        domain::{Delivery, Provider, SessionId},
        lan::{
            model::{HostLabel, PairedPeer},
            state::LanStore,
            wire::{ForwardedMessage, RemoteSender},
        },
    };
    let mut b = Node::new("b", None);
    b.start().await;
    let port = b.network_on(0).await;
    let key = StaticKey::generate().unwrap();
    LanStore::open(&b.state())
        .unwrap()
        .save_peer(&PairedPeer {
            label: HostLabel::parse("tester").unwrap(),
            fingerprint: key.fingerprint(),
            address: None,
            paired_at: 1,
        })
        .unwrap();
    let ask = async |request: PeerRequest| -> PeerReply {
        let stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        let mut channel = Channel::initiate(stream, &key).await.unwrap();
        channel.send(&request).await.unwrap();
        channel.recv(Duration::from_secs(10)).await.unwrap()
    };
    let error = |reply: PeerReply| match reply {
        PeerReply::Error { message } => message,
        other => panic!("Expected an error, got {other:?}"),
    };
    assert!(
        error(
            ask(PeerRequest::Pair {
                spake: "AAAA".into()
            })
            .await
        )
        .contains("pairing request first")
    );
    assert!(
        error(
            ask(PeerRequest::List {
                project: Some("relative/path".into())
            })
            .await
        )
        .contains("Project")
    );
    assert!(matches!(
        ask(PeerRequest::List { project: None }).await,
        PeerReply::Sessions { sessions } if sessions.is_empty()
    ));
    assert!(matches!(
        ask(PeerRequest::Status {
            message_id: uuid::Uuid::new_v4(),
            delivery: Delivery::Queued
        })
        .await,
        PeerReply::Ok
    ));
    let id = |provider| SessionId {
        provider,
        id: uuid::Uuid::new_v4(),
    };
    let message = |text: &str, agent, project: &str| ForwardedMessage {
        message_id: uuid::Uuid::new_v4(),
        sender: RemoteSender {
            session_id: id(Provider::Codex),
            agent,
            project: project.into(),
            cwd: "/work/mono".into(),
            name: "sender".into(),
        },
        recipient: id(Provider::Claude),
        message: text.into(),
        project: None,
        scheduled: false,
    };
    for (forward, expected) in [
        (
            message("", Provider::Codex, "mono"),
            "Message must be nonempty",
        ),
        (
            message("hi", Provider::Claude, "mono"),
            "does not match its session",
        ),
        (
            message("hi", Provider::Codex, " "),
            "Invalid sender metadata",
        ),
        (
            message("hi", Provider::Codex, "mono"),
            "Recipient session is unknown",
        ),
    ] {
        let text = error(ask(PeerRequest::Forward { message: forward }).await);
        assert!(text.contains(expected), "{text}");
    }
    b.stop().await;
}

/* REQ-027: With local_only the bridge listens on 127.0.0.1 only (for SSH
tunnels) and says so; it is not reachable on the machine's network address. */
#[tokio::test]
async fn local_only_listens_on_loopback() {
    let mut a = Node::new("a", None);
    a.start().await;
    let lan = match a
        .rpc(Request::Network {
            enabled: true,
            port: Some(0),
            name: Some(a.label.clone()),
            local_only: Some(true),
        })
        .await
    {
        Reply::Lan { lan } => lan,
        other => panic!("{other:?}"),
    };
    assert!(lan.network.local_only, "{lan:?}");
    let port = lan.network.port.unwrap();
    assert!(
        tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_ok()
    );
    // The address this machine would use on its network, without sending anything.
    let probe = std::net::UdpSocket::bind("0.0.0.0:0").unwrap();
    if probe.connect("192.0.2.1:9").is_ok()
        && let Ok(local) = probe.local_addr()
        && !local.ip().is_loopback()
        && !local.ip().is_unspecified()
    {
        assert!(
            tokio::net::TcpStream::connect((local.ip(), port))
                .await
                .is_err(),
            "reachable on {}",
            local.ip()
        );
    }
    // Saved: still loopback only after a restart.
    a.stop().await;
    a.start().await;
    assert!(a.lan().await.network.local_only);
    a.stop().await;
}

/* REQ-017: Turning networking on again, unchanged or with a new name, keeps
the bridge listening on its port (the old listener is closed before the new
one binds); it never ends up "enabled" with no listener. */
#[tokio::test]
async fn network_on_again_keeps_listening() {
    let mut a = Node::new("a", None);
    a.start().await;
    let port = free_port();
    for name in [
        a.label.clone(),
        a.label.clone(),
        format!("{}-renamed", a.label),
    ] {
        let lan = match a
            .rpc(Request::Network {
                enabled: true,
                port: Some(port),
                name: Some(name.clone()),
                local_only: None,
            })
            .await
        {
            Reply::Lan { lan } => lan,
            other => panic!("network on ({name}): {other:?}"),
        };
        assert_eq!(lan.network.port, Some(port), "{name}");
        assert!(
            tokio::net::TcpStream::connect(("127.0.0.1", port))
                .await
                .is_ok(),
            "listening after network on ({name})"
        );
    }
    assert_eq!(a.lan().await.network.port, Some(port));
    a.stop().await;
}

/* REQ-017: Two local-only bridges paired by address (no Zeroconf, no tunnel)
show each other online once one listed the other's sessions. */
#[tokio::test]
async fn address_paired_bridges_are_online_after_answering() {
    let (mut a, mut b) = (Node::new("a", None), Node::new("b", None));
    a.start().await;
    b.start().await;
    let mut ports = Vec::new();
    for node in [&a, &b] {
        match node
            .rpc(Request::Network {
                enabled: true,
                port: Some(0),
                name: Some(node.label.clone()),
                local_only: Some(true),
            })
            .await
        {
            Reply::Lan { lan } => ports.push(lan.network.port.unwrap()),
            other => panic!("{other:?}"),
        }
    }
    a.pair_with(&b, &format!("127.0.0.1:{}", ports[1])).await;
    let (_, unavailable) = a.sessions().await;
    assert!(unavailable.is_empty(), "{unavailable:?}");
    let peer = |lan: session_bridge::lan::model::LanView, label: &str| {
        lan.hosts
            .into_iter()
            .find(|h| h.host == label)
            .unwrap_or_else(|| panic!("{label} not listed"))
    };
    let seen_by_a = peer(a.lan().await, &b.label);
    assert!(seen_by_a.paired && seen_by_a.online, "{seen_by_a:?}");
    let seen_by_b = peer(b.lan().await, &a.label);
    assert!(seen_by_b.paired && seen_by_b.online, "{seen_by_b:?}");
    a.stop().await;
    b.stop().await;
}
