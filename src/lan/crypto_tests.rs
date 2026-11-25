use super::channel::{Channel, StaticKey};
use super::model::{Fingerprint, PairingCode};
use super::pake::{self, Role};
use super::wire::{PeerReply, PeerRequest};
use serde_json::json;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt, duplex};

const WAIT: Duration = Duration::from_secs(5);

async fn pair_of_channels(
    a: &StaticKey,
    b: &StaticKey,
) -> (
    Channel<tokio::io::DuplexStream>,
    Channel<tokio::io::DuplexStream>,
) {
    let (left, right) = duplex(1 << 20);
    let (initiator, responder) =
        tokio::join!(Channel::initiate(left, a), Channel::respond(right, b));
    (initiator.unwrap(), responder.unwrap())
}

/* REQ-017: The Noise handshake tells each side the other's key fingerprint. */
#[tokio::test]
async fn handshake_identifies_both_keys() {
    let (a, b) = (
        StaticKey::generate().unwrap(),
        StaticKey::generate().unwrap(),
    );
    assert_ne!(a.fingerprint(), b.fingerprint());
    let (left, right) = pair_of_channels(&a, &b).await;
    assert_eq!(left.remote(), &b.fingerprint());
    assert_eq!(right.remote(), &a.fingerprint());
}

/* REQ-017: Frames larger than one Noise message arrive intact in both directions. */
#[tokio::test]
async fn frames_round_trip_across_noise_messages() {
    let (a, b) = (
        StaticKey::generate().unwrap(),
        StaticKey::generate().unwrap(),
    );
    let (mut left, mut right) = pair_of_channels(&a, &b).await;
    let big = "x".repeat(200_000);
    left.send(&json!({"text": big})).await.unwrap();
    let got: serde_json::Value = right.recv(WAIT).await.unwrap();
    assert_eq!(got["text"].as_str().unwrap().len(), 200_000);
    right.send(&json!({"ok": true})).await.unwrap();
    let back: serde_json::Value = left.recv(WAIT).await.unwrap();
    assert_eq!(back, json!({"ok": true}));
}

/* REQ-017: Oversized frames are refused before sending. */
#[tokio::test]
async fn refuses_oversized_frames() {
    let (a, b) = (
        StaticKey::generate().unwrap(),
        StaticKey::generate().unwrap(),
    );
    let (mut left, _right) = pair_of_channels(&a, &b).await;
    let huge = "x".repeat(crate::transport::FRAME_LIMIT as usize + 1);
    assert!(left.send(&json!({"text": huge})).await.is_err());
}

/* REQ-017: A receive gives up after its deadline and on a closed connection. */
#[tokio::test]
async fn receive_times_out_and_detects_close() {
    let (a, b) = (
        StaticKey::generate().unwrap(),
        StaticKey::generate().unwrap(),
    );
    let (mut left, right) = pair_of_channels(&a, &b).await;
    let waited = left
        .recv::<serde_json::Value>(Duration::from_millis(50))
        .await;
    assert!(waited.unwrap_err().to_string().contains("timed out"));
    drop(right);
    assert!(left.recv::<serde_json::Value>(WAIT).await.is_err());
}

/// Copies bytes both ways and flips byte `corrupt_at` (1-based) of the
/// initiator-to-responder direction.
async fn relay(a: tokio::io::DuplexStream, b: tokio::io::DuplexStream, corrupt_at: usize) {
    let (mut a_read, mut a_write) = tokio::io::split(a);
    let (mut b_read, mut b_write) = tokio::io::split(b);
    let up = async move {
        let mut seen = 0usize;
        let mut buffer = [0u8; 4096];
        loop {
            let n = match a_read.read(&mut buffer).await {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            for byte in &mut buffer[..n] {
                seen += 1;
                if seen == corrupt_at {
                    *byte ^= 0xff;
                }
            }
            if b_write.write_all(&buffer[..n]).await.is_err() {
                break;
            }
        }
    };
    let down = async move {
        let _ = tokio::io::copy(&mut b_read, &mut a_write).await;
    };
    tokio::join!(up, down);
}

/* REQ-017: A garbage handshake and tampered ciphertext are rejected. */
#[tokio::test]
async fn rejects_garbage_and_tampering() {
    let (a, b) = (
        StaticKey::generate().unwrap(),
        StaticKey::generate().unwrap(),
    );
    let (mut client, server) = duplex(1 << 16);
    client.write_all(b"\x00\x05hello").await.unwrap();
    client.shutdown().await.unwrap();
    assert!(Channel::respond(server, &b).await.is_err());

    let (left, relay_a) = duplex(1 << 16);
    let (relay_b, right) = duplex(1 << 16);
    // The handshake sends 34 + 66 bytes this way; byte 110 is transport data.
    tokio::spawn(relay(relay_a, relay_b, 110));
    let (initiator, responder) =
        tokio::join!(Channel::initiate(left, &a), Channel::respond(right, &b));
    let (mut initiator, mut responder) = (initiator.unwrap(), responder.unwrap());
    initiator.send(&json!({"n": 1})).await.unwrap();
    assert!(responder.recv::<serde_json::Value>(WAIT).await.is_err());
}

/* REQ-017: SPAKE2 with the same code gives both sides matching confirmations. */
#[test]
fn spake2_confirms_matching_codes() {
    let (a, b) = (fp(1), fp(2));
    let code = PairingCode::from_random(482_913);
    let (initiator, first) = pake::start(Role::Initiator, &code, &a, &b);
    let (responder, second) = pake::start(Role::Responder, &code, &a, &b);
    let ka = initiator.finish(&second).unwrap();
    let kb = responder.finish(&first).unwrap();
    let mac_a = pake::confirmation(&ka, Role::Initiator, &a, &b);
    let mac_b = pake::confirmation(&kb, Role::Responder, &a, &b);
    assert!(pake::verify(&kb, Role::Initiator, &a, &b, &mac_a));
    assert!(pake::verify(&ka, Role::Responder, &a, &b, &mac_b));
    assert!(
        !pake::verify(&kb, Role::Responder, &a, &b, &mac_a),
        "roles are bound"
    );
}

/* REQ-017: A wrong code or a substituted key fails confirmation. */
#[test]
fn spake2_rejects_wrong_codes_and_keys() {
    let (a, b) = (fp(1), fp(2));
    let right = PairingCode::from_random(482_913);
    let wrong = PairingCode::from_random(482_914);
    let (initiator, first) = pake::start(Role::Initiator, &wrong, &a, &b);
    let (responder, second) = pake::start(Role::Responder, &right, &a, &b);
    let ka = initiator.finish(&second).unwrap();
    let kb = responder.finish(&first).unwrap();
    assert!(!pake::verify(
        &kb,
        Role::Initiator,
        &a,
        &b,
        &pake::confirmation(&ka, Role::Initiator, &a, &b)
    ));

    // A man in the middle shows the responder a different initiator key.
    let mitm = fp(9);
    let (initiator, first) = pake::start(Role::Initiator, &right, &a, &b);
    let (responder, second) = pake::start(Role::Responder, &right, &mitm, &b);
    let ka = initiator.finish(&second).unwrap();
    let kb = responder.finish(&first).unwrap();
    assert!(!pake::verify(
        &kb,
        Role::Initiator,
        &mitm,
        &b,
        &pake::confirmation(&ka, Role::Initiator, &a, &b)
    ));
    assert!(
        pake::start(Role::Initiator, &right, &a, &b)
            .0
            .finish(b"short")
            .is_err()
    );
}

fn fp(byte: u8) -> Fingerprint {
    Fingerprint::of_public_key(&[byte; 32])
}

/* REQ-017: Peer messages keep the documented wire format. */
#[test]
fn wire_format_is_stable() {
    let request = PeerRequest::PairRequest {
        name: "laptop-a".into(),
        port: 47820,
    };
    assert_eq!(
        serde_json::to_value(&request).unwrap(),
        json!({"method":"pair_request","name":"laptop-a","port":47820})
    );
    assert_eq!(
        serde_json::to_value(PeerRequest::List { project: None }).unwrap(),
        json!({"method":"list","project":null})
    );
    assert_eq!(
        serde_json::to_value(PeerReply::CodeShown { expires_in: 120 }).unwrap(),
        json!({"result":"code_shown","expires_in":120})
    );
    assert_eq!(
        serde_json::to_value(PeerReply::WrongCode { attempts_left: 2 }).unwrap(),
        json!({"result":"wrong_code","attempts_left":2})
    );
    let parsed: PeerRequest =
        serde_json::from_value(json!({"method":"pair_confirm","mac":"AAAA"})).unwrap();
    assert!(matches!(parsed, PeerRequest::PairConfirm { mac } if mac == "AAAA"));
}
