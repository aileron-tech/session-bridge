//! A remote bridge reached through an SSH tunnel (REQ-027). A stand-in `ssh`
//! does what the real one would for `-L` and `-R` on one machine: it relays
//! the forwarded ports, so two real bridges talk exactly as through SSH.
#![cfg(unix)]
use session_bridge::{
    protocol::{Reply, Request},
    transport::{self, PeerFrame},
};
use std::{fs, os::unix::fs::PermissionsExt, path::Path, time::Duration};
use tokio::{net::UnixListener, time::timeout};

#[path = "lan/common.rs"]
mod common;
use common::{Node, PROJECT, eventually};

const FAKE_SSH: &str = r#"#!/usr/bin/env python3
# Stand-in for `ssh -N -L l:127.0.0.1:r -R b:127.0.0.1:o ... -- host` on one machine.
import socket, sys, threading
args = sys.argv[1:]
forwards = []
for flag, spec in zip(args, args[1:]):
    if flag in ("-L", "-R"):
        _, listen, _, target = spec.split(":")
        forwards.append((int(listen), int(target)))
def pipe(a, b):
    try:
        while True:
            data = a.recv(65536)
            if not data:
                break
            b.sendall(data)
    except OSError:
        pass
    finally:
        for s in (a, b):
            try:
                s.shutdown(socket.SHUT_RDWR)
            except OSError:
                pass
def serve(listen, target):
    server = socket.socket()
    server.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    server.bind(("127.0.0.1", listen))
    server.listen()
    while True:
        client, _ = server.accept()
        upstream = socket.create_connection(("127.0.0.1", target))
        threading.Thread(target=pipe, args=(client, upstream), daemon=True).start()
        threading.Thread(target=pipe, args=(upstream, client), daemon=True).start()
threads = [threading.Thread(target=serve, args=f, daemon=True) for f in forwards]
for t in threads:
    t.start()
for t in threads:
    t.join()
"#;

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

/* REQ-027: A bridge listening on loopback only is reached through an SSH
tunnel: it pairs with the one-time code, its sessions are listed, messages
go there, and replies come back through the reverse forward. */
#[tokio::test]
async fn remote_bridge_through_an_ssh_tunnel() {
    let sockets = tempfile::Builder::new()
        .prefix("sb-rm-")
        .tempdir_in("/tmp")
        .unwrap();
    let dir = sockets.path().join("inboxes");
    session_bridge::store::ensure_private_dir(&dir).unwrap();
    let ssh = sockets.path().join("ssh");
    fs::write(&ssh, FAKE_SSH).unwrap();
    fs::set_permissions(&ssh, fs::Permissions::from_mode(0o755)).unwrap();
    let (a_inbox, a_path) = inbox(&dir, "a.sock");
    let (b_inbox, b_path) = inbox(&dir, "b.sock");
    let (mut a, mut b) = (Node::new("a", Some(&a_path)), Node::new("b", Some(&b_path)));
    a.env
        .push(("SESSION_BRIDGE_SSH".into(), ssh.display().to_string()));
    a.start().await;
    b.start().await;

    let refused = a
        .rpc(Request::AddRemote {
            host: "box.example".into(),
            port: None,
        })
        .await;
    assert!(
        matches!(&refused, Reply::Error { message } if message.contains("Turn networking on first")),
        "{refused:?}"
    );
    let port_b = match b
        .rpc(Request::Network {
            enabled: true,
            port: Some(0),
            name: Some(b.label.clone()),
            local_only: Some(true),
        })
        .await
    {
        Reply::Lan { lan } => lan.network.port.unwrap(),
        other => panic!("{other:?}"),
    };
    a.network_on(0).await;
    let bad = a
        .rpc(Request::AddRemote {
            host: "-oProxyCommand=sh".into(),
            port: None,
        })
        .await;
    assert!(matches!(bad, Reply::Error { .. }), "{bad:?}");
    match a
        .rpc(Request::AddRemote {
            host: "box.example".into(),
            port: Some(port_b),
        })
        .await
    {
        Reply::Remotes { remotes } => assert_eq!(remotes[0].host, "box.example"),
        other => panic!("{other:?}"),
    }
    eventually("tunnel connected", async || {
        match a.rpc(Request::Remotes).await {
            Reply::Remotes { remotes } => (remotes[0].state == "connected").then_some(()),
            _ => None,
        }
    })
    .await;

    let id = match a
        .rpc(Request::Pair {
            host: "box.example".into(),
        })
        .await
    {
        Reply::Pairing { id, .. } => id,
        other => panic!("{other:?}"),
    };
    let code = b.wait_code().await;
    match a.rpc(Request::PairCode { id, code }).await {
        Reply::Paired { host, .. } => assert_eq!(host, b.label),
        other => panic!("{other:?}"),
    }

    let peer = a
        .lan()
        .await
        .hosts
        .into_iter()
        .find(|h| h.host == b.label)
        .expect("paired remote listed");
    assert!(
        peer.paired && peer.online,
        "a connected tunnel means online: {peer:?}"
    );

    let (sessions, unavailable) = a.sessions().await;
    assert!(unavailable.is_empty(), "{unavailable:?}");
    let remote = sessions
        .iter()
        .find(|s| s.session_id == b.session)
        .expect("remote session listed through the tunnel");
    assert_eq!(remote.host.as_deref(), Some(b.label.as_str()));

    let sent = a
        .rpc(Request::Send {
            sender: a.session.clone(),
            session_id: b.session.clone(),
            message: "Over the tunnel".into(),
            project: Some(PROJECT.into()),
        })
        .await;
    assert!(matches!(sent, Reply::Receipt { .. }), "{sent:?}");
    let there = receive(&b_inbox).await;
    assert!(there.contains("Over the tunnel"), "{there}");
    assert!(there.contains(&format!("From: {} /", a.label)), "{there}");

    let back = b
        .rpc(Request::Send {
            sender: b.session.clone(),
            session_id: a.session.clone(),
            message: "And back through the reverse forward".into(),
            project: Some(PROJECT.into()),
        })
        .await;
    assert!(matches!(back, Reply::Receipt { .. }), "{back:?}");
    let here = receive(&a_inbox).await;
    assert!(
        here.contains("And back through the reverse forward"),
        "{here}"
    );

    match a
        .rpc(Request::RemoveRemote {
            host: "box.example".into(),
        })
        .await
    {
        Reply::Remotes { remotes } => assert!(remotes.is_empty()),
        other => panic!("{other:?}"),
    }
    a.stop().await;
    b.stop().await;
}
