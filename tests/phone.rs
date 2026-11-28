//! The phone admin app (REQ-036) against a real bridge: the admin page turns
//! phone access on and pairs a phone, and a phone client (rustls pinned to
//! the bridge certificate, a ring P-256 key like WebCrypto's) signs in and
//! calls the bridge over HTTPS from this machine's LAN address.
//! Not on Windows: phone access is macOS and Linux only.
#![cfg(unix)]
use data_encoding::BASE64URL_NOPAD;
use ring::{
    rand::SystemRandom,
    signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair, KeyPair},
};
use rustls::pki_types::{CertificateDer, ServerName};
use serde_json::{Value, json};
use session_bridge::{
    protocol::{Reply, Request},
    transport::{self, PeerFrame},
};
use sha2::{Digest, Sha256};
use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpStream, UnixListener},
    time::timeout,
};

#[path = "lan/common.rs"]
mod common;
use common::{AdminPage, Node};

/// This machine's first private IPv4 address: the phone connects to it.
fn lan_address() -> Option<Ipv4Addr> {
    if_addrs::get_if_addrs()
        .ok()?
        .into_iter()
        .filter_map(|i| match i.addr {
            if_addrs::IfAddr::V4(v4) if v4.ip.is_private() => Some(v4.ip),
            _ => None,
        })
        .next()
}
/// A port whose next port is free too.
fn free_port_pair() -> u16 {
    loop {
        let first = std::net::TcpListener::bind(("0.0.0.0", 0)).unwrap();
        let port = first.local_addr().unwrap().port();
        if port < u16::MAX && std::net::TcpListener::bind(("0.0.0.0", port + 1)).is_ok() {
            return port;
        }
    }
}

fn split(response: &[u8]) -> (u16, Value, String) {
    let text = String::from_utf8_lossy(response).into_owned();
    let status = text.get(9..12).and_then(|s| s.parse().ok()).unwrap_or(0);
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    (
        status,
        serde_json::from_str(body).unwrap_or(Value::Null),
        head.to_owned(),
    )
}

struct PhoneClient {
    address: SocketAddr,
    host: String,
    port: u16,
    tls: tokio_rustls::TlsConnector,
    key: EcdsaKeyPair,
    rng: SystemRandom,
    token: Option<String>,
    device: Option<String>,
}
impl PhoneClient {
    fn new(address: SocketAddr, host: &str, port: u16, cert: Vec<u8>) -> Self {
        let mut roots = rustls::RootCertStore::empty();
        roots.add(CertificateDer::from(cert)).unwrap();
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let config = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let rng = SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng).unwrap();
        let key = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8.as_ref(), &rng)
            .unwrap();
        Self {
            address,
            host: host.to_owned(),
            port,
            tls: tokio_rustls::TlsConnector::from(Arc::new(config)),
            key,
            rng,
            token: None,
            device: None,
        }
    }
    fn public_key(&self) -> String {
        BASE64URL_NOPAD.encode(self.key.public_key().as_ref())
    }
    async fn raw(
        &self,
        method: &str,
        path: &str,
        host: &str,
        body: &Value,
    ) -> (u16, Value, String) {
        let stream = TcpStream::connect(self.address).await.unwrap();
        let name = ServerName::try_from(self.host.clone()).unwrap();
        let mut tls = self
            .tls
            .connect(name, stream)
            .await
            .expect("TLS with the pinned certificate");
        let body = if method == "GET" {
            String::new()
        } else {
            body.to_string()
        };
        let auth = self
            .token
            .as_ref()
            .map(|t| format!("Authorization: Bearer {t}\r\n"))
            .unwrap_or_default();
        let request = format!(
            "{method} {path} HTTP/1.1\r\nHost: {host}\r\n{auth}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        tls.write_all(request.as_bytes()).await.unwrap();
        let mut response = Vec::new();
        timeout(Duration::from_secs(10), tls.read_to_end(&mut response))
            .await
            .expect("response")
            .ok();
        split(&response)
    }
    async fn post(&self, path: &str, body: Value) -> (u16, Value) {
        let host = format!("{}:{}", self.host, self.port);
        let (status, json, _) = self.raw("POST", path, &host, &body).await;
        (status, json)
    }
    async fn sign_in(&mut self) -> u16 {
        let (_, challenge) = self.post("/api/challenge", json!({})).await;
        let nonce = challenge["nonce"].as_str().unwrap().to_owned();
        let message = format!("session-bridge phone v1\n{}\n{nonce}", self.host);
        let signature = self.key.sign(&self.rng, message.as_bytes()).unwrap();
        let (status, reply) = self
            .post(
                "/api/session",
                json!({"device_id": self.device, "nonce": nonce, "signature": BASE64URL_NOPAD.encode(signature.as_ref())}),
            )
            .await;
        self.token = reply["token"].as_str().map(str::to_owned);
        status
    }
}

async fn plain_get(address: SocketAddr, path: &str) -> (u16, Vec<u8>) {
    let mut stream = TcpStream::connect(address).await.unwrap();
    let request = format!("GET {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await.unwrap();
    let split_at = response
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map_or(response.len(), |p| p + 4);
    let status = String::from_utf8_lossy(&response[9..12])
        .parse()
        .unwrap_or(0);
    (status, response[split_at..].to_vec())
}

fn inbox() -> (tempfile::TempDir, UnixListener, PathBuf) {
    let sockets = tempfile::Builder::new()
        .prefix("sb-ph-")
        .tempdir_in("/tmp")
        .unwrap();
    let dir = sockets.path().join("inboxes");
    session_bridge::store::ensure_private_dir(&dir).unwrap();
    let path = dir.join("a.sock");
    let listener = UnixListener::bind(&path).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    (sockets, listener, path)
}

/* REQ-036: Phone access is off by default, refused on the control socket, and when on serves
HTTPS by the laptop's LAN address under a pinned per-bridge root to LAN sources only, with the
root also downloadable over plain HTTP; a phone pairs with a one-time code and the laptop's confirmation, signs in with
its key, calls only the phone allowlist (never root, no local paths), has a repeated client_id
answered once and a reused one refused (also on the admin page), and is shut out after unpairing
and when phone access is turned off. */
#[tokio::test]
async fn phone_pairs_signs_in_and_calls_the_allowlist() {
    let Some(lan) = lan_address() else {
        eprintln!(
            "No private IPv4 address on this machine: phone access cannot be reached; skipped"
        );
        return;
    };
    let (_sockets, listener, inbox_path) = inbox();
    let mut a = Node::new("a", Some(&inbox_path));
    a.start().await;
    let page = AdminPage::open(&a).await;

    // Off by default; only the admin page manages phones.
    let phones = page.ok(json!({"method": "phones"})).await;
    assert_eq!(phones["phones"]["enabled"], false, "{phones}");
    assert_eq!(phones["phones"]["listening"], false);
    assert!(matches!(a.rpc(Request::Phones).await, Reply::Error { .. }));

    let port = free_port_pair();
    let on = page
        .ok(json!({"method": "phone_access", "enabled": true, "port": port}))
        .await["phones"]
        .clone();
    assert_eq!(on["listening"], true, "{on}");
    let host = on["host"].as_str().unwrap().to_owned();
    assert!(
        host.starts_with("sb-") && host.ends_with(".local"),
        "{host}"
    );
    let fingerprint = on["fingerprint"].as_str().unwrap().to_owned();
    // Phones reach the laptop by its LAN address, the default route's first.
    let first = on["address"].as_str().unwrap().to_owned();
    assert_eq!(on["addresses"][0], first.as_str());
    assert!(
        on["addresses"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a == &lan.to_string()),
        "{on}"
    );
    assert_eq!(
        on["profile_url"],
        format!("http://{first}:{}/bridge.mobileconfig", port + 1)
    );

    // The root certificate over plain HTTP matches the fingerprint the page shows.
    let plain = SocketAddr::new(IpAddr::V4(lan), port + 1);
    let (status, cert) = plain_get(plain, "/bridge.crt").await;
    assert_eq!(status, 200);
    let hex: Vec<String> = Sha256::digest(&cert)
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect();
    assert_eq!(hex.join(" "), fingerprint);
    let (status, profile) = plain_get(plain, "/bridge.mobileconfig").await;
    assert_eq!(status, 200);
    assert!(String::from_utf8_lossy(&profile).contains("com.apple.security.root"));

    // The phone trusts only that root and reaches the laptop by its LAN
    // address; loopback is not the local network: no TLS there.
    let mut phone = PhoneClient::new(
        SocketAddr::new(IpAddr::V4(lan), port),
        &lan.to_string(),
        port,
        cert.clone(),
    );
    {
        let stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        let name = ServerName::try_from(lan.to_string()).unwrap();
        assert!(phone.tls.connect(name, stream).await.is_err());
    }

    // The app loads with its own Host only (its address, or its .local name),
    // under the page's policy.
    let (status, _, head) = phone
        .raw("GET", "/", &format!("{lan}:{port}"), &Value::Null)
        .await;
    assert_eq!(status, 200);
    assert!(
        head.to_lowercase()
            .contains("content-security-policy: default-src 'none'"),
        "{head}"
    );
    let (status, _, _) = phone
        .raw("GET", "/", &format!("{host}:{port}"), &Value::Null)
        .await;
    assert_eq!(status, 200);
    let (status, _, _) = phone
        .raw("GET", "/", &format!("evil.example:{port}"), &Value::Null)
        .await;
    assert_eq!(status, 403);

    // A wrong code uses the offer up.
    let offer = page.ok(json!({"method": "phone_offer"})).await["phones"]["offer"].clone();
    let code = offer["code"].as_str().unwrap();
    let wrong = if code.starts_with('A') { "B" } else { "A" }.to_owned() + &code[1..];
    let (status, _) = phone
        .post(
            "/api/pair",
            json!({"code": wrong, "name": "Test phone", "public_key": phone.public_key()}),
        )
        .await;
    assert_eq!(status, 403);
    let after = page.ok(json!({"method": "phones"})).await;
    assert_eq!(
        after["phones"]["offer"]["state"]["state"], "failed",
        "{after}"
    );

    // The right code waits for the laptop; the laptop accepts.
    let offer = page.ok(json!({"method": "phone_offer"})).await["phones"]["offer"].clone();
    assert!(offer["url"].as_str().unwrap().contains(&format!(
        "#pair={}",
        offer["code"].as_str().unwrap().replace('-', "")
    )));
    let (status, scanned) = phone
        .post(
            "/api/pair",
            json!({"code": offer["code"], "name": "Test phone", "public_key": phone.public_key()}),
        )
        .await;
    assert_eq!(status, 200, "{scanned}");
    assert_eq!(scanned["fingerprint"], fingerprint.replace(' ', ""));
    let (_, waiting) = phone
        .post(
            "/api/pair/status",
            json!({"public_key": phone.public_key()}),
        )
        .await;
    assert_eq!(waiting["state"], "waiting");
    let shown = page.ok(json!({"method": "phones"})).await["phones"]["offer"].clone();
    assert_eq!(shown["state"]["state"], "scanned");
    assert_eq!(shown["state"]["key"], scanned["key"]);
    // No session before the laptop accepts: the phone has no device id yet.
    phone.device = Some(uuid::Uuid::new_v4().to_string());
    assert_eq!(phone.sign_in().await, 401);
    let confirmed = page
        .ok(json!({"method": "phone_confirm", "id": offer["id"], "accept": true}))
        .await["phones"]
        .clone();
    let device = confirmed["offer"]["state"]["device_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(confirmed["devices"][0]["id"], device.as_str());
    let (_, paired) = phone
        .post(
            "/api/pair/status",
            json!({"public_key": phone.public_key()}),
        )
        .await;
    assert_eq!(paired["device_id"], device.as_str());
    phone.device = Some(device.clone());
    assert_eq!(phone.sign_in().await, 200);

    // The allowlist: listing works, laptop actions do not.
    let (status, listed) = phone
        .post(
            "/api/call",
            json!({"method": "list", "sender": null, "project": null}),
        )
        .await;
    assert_eq!(status, 200, "{listed}");
    let mine = listed["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["session_id"] == a.session.as_str())
        .expect("local session listed")
        .clone();
    assert_eq!(mine["cwd"], "", "no local paths on the phone: {mine}");
    for refused in [
        json!({"method": "network", "enabled": false}),
        json!({"method": "phone_offer"}),
        json!({"method": "admin"}),
        json!({"method": "spawn", "agent": "claude", "role": "qa"}),
        json!({"method": "assign_role", "session_id": a.session, "role": "root"}),
        json!({"method": "save_role", "name": "root", "prompt": "anything"}),
        json!({"method": "open_roles_folder"}),
    ] {
        let (status, _) = phone.post("/api/call", refused.clone()).await;
        assert_eq!(status, 403, "{refused}");
    }
    let (_, schedules) = phone
        .post("/api/call", json!({"method": "schedules"}))
        .await;
    assert_eq!(schedules["other_lines"], json!([]), "{schedules}");
    // REQ-037: the phone sees the roles and their versions, not where the
    // files are on the laptop.
    let (_, roles) = phone.post("/api/call", json!({"method": "roles"})).await;
    assert_eq!(roles["folder"], "", "{roles}");
    assert_eq!(roles["files"]["master"]["path"], "", "{roles}");
    assert!(roles["files"]["master"]["version"].is_string(), "{roles}");
    // A phone save carries the version it read: without it, or after an
    // edit on the laptop, it is refused and the file is kept.
    let master = a.data().join("roles/master.md");
    let save = |version: serde_json::Value| json!({"method": "save_role", "name": "master", "prompt": "Lead from the phone.", "previous": "master", "version": version});
    let (status, refused) = phone.post("/api/call", save(json!(null))).await;
    assert_eq!(status, 409, "{refused}");
    let version = roles["files"]["master"]["version"].clone();
    std::fs::write(&master, "Edited on the laptop.").unwrap();
    let (status, refused) = phone.post("/api/call", save(version)).await;
    assert_eq!(status, 409, "{refused}");
    assert_eq!(
        std::fs::read_to_string(&master).unwrap(),
        "Edited on the laptop."
    );
    let current = json!(session_bridge::roles::version(b"Edited on the laptop."));
    let (status, saved) = phone.post("/api/call", save(current)).await;
    assert_eq!(status, 200, "{saved}");
    assert_eq!(
        std::fs::read_to_string(&master).unwrap(),
        "Lead from the phone."
    );

    // A send repeated with its client_id is sent once and answered the same.
    let client_id = uuid::Uuid::new_v4();
    let send = json!({"method": "user_send", "session_id": a.session, "message": "From the phone", "client_id": client_id});
    let (status, first) = phone.post("/api/call", send.clone()).await;
    assert_eq!(status, 200, "{first}");
    let (_, again) = phone.post("/api/call", send).await;
    assert_eq!(
        first["receipt"]["message_id"],
        again["receipt"]["message_id"]
    );
    let reused = json!({"method": "user_send", "session_id": a.session, "message": "Something else", "client_id": client_id});
    let (status, refused) = phone.post("/api/call", reused).await;
    assert_eq!(status, 409, "{refused}");
    let (mut stream, _) = timeout(Duration::from_secs(20), listener.accept())
        .await
        .unwrap()
        .unwrap();
    let frame: PeerFrame = transport::read_frame(&mut stream).await.unwrap();
    let content = frame.message.unwrap().content;
    assert!(content.contains("it is not an approval"), "{content}");
    let (_, messages) = phone.post("/api/call", json!({"method": "messages"})).await;
    let sent: Vec<&Value> = messages["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["message"] == "From the phone")
        .collect();
    assert_eq!(sent.len(), 1);

    // A nonce is single use.
    let (_, challenge) = phone.post("/api/challenge", json!({})).await;
    let nonce = challenge["nonce"].as_str().unwrap();
    let message = format!("session-bridge phone v1\n{lan}\n{nonce}");
    let signature = BASE64URL_NOPAD.encode(
        phone
            .key
            .sign(&phone.rng, message.as_bytes())
            .unwrap()
            .as_ref(),
    );
    let body = json!({"device_id": device, "nonce": nonce, "signature": signature});
    assert_eq!(phone.post("/api/session", body.clone()).await.0, 200);
    assert_eq!(phone.post("/api/session", body).await.0, 401);

    // Limited before sign-in: once the bucket refills one token, the phone
    // signs in (the challenge costs it, the session request does not).
    let mut retry_after = 0;
    for _ in 0..12 {
        let (status, _) = phone.post("/api/challenge", json!({})).await;
        if status == 429 {
            retry_after = 1;
            break;
        }
    }
    assert_eq!(retry_after, 1, "the challenge budget never ran out");
    let (status, refused) = phone.post("/api/challenge", json!({})).await;
    assert_eq!(status, 429, "{refused}");
    let wait: u64 = refused["message"]
        .as_str()
        .and_then(|m| m.split_whitespace().rev().nth(1))
        .and_then(|n| n.parse().ok())
        .expect("a wait in the message");
    tokio::time::sleep(Duration::from_secs(wait) + Duration::from_millis(300)).await;
    assert_eq!(
        phone.sign_in().await,
        200,
        "a limited phone did not sign in after the refill"
    );

    // Unpairing ends the phone's sessions at once.
    page.ok(json!({"method": "phone_unpair", "device_id": device}))
        .await;
    let (status, _) = phone.post("/api/call", json!({"method": "messages"})).await;
    assert_eq!(status, 401);

    // Off: nothing listens.
    page.ok(json!({"method": "phone_access", "enabled": false}))
        .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let connected = TcpStream::connect(SocketAddr::new(IpAddr::V4(lan), port)).await;
    assert!(connected.is_err(), "listener still accepts");

    // The admin page answers a repeated client_id once too.
    let client_id = uuid::Uuid::new_v4();
    let send = json!({"method": "user_send", "session_id": a.session, "message": "From the page", "client_id": client_id});
    let first = page.ok(send.clone()).await;
    let again = page.ok(send).await;
    assert_eq!(
        first["receipt"]["message_id"],
        again["receipt"]["message_id"]
    );
    let (status, _) = page
        .call(json!({"method": "user_send", "session_id": a.session, "message": "Other", "client_id": client_id}))
        .await;
    assert_eq!(status, 409);
    let listed = page.ok(json!({"method": "messages"})).await;
    let copies = listed["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["message"] == "From the page")
        .count();
    assert_eq!(copies, 1);
    a.stop().await;
}
