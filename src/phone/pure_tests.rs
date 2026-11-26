use super::{
    limits::{Bucket, CALLS, Failures, SENDS},
    net::{Interface, host_matches, host_name, is_lan_source, profile_address},
    profile::mobileconfig,
    replay::{Begin, KEEP_SECONDS, Replays, client_id, digest, phone_key},
    view::{pair_url, qr_rows},
};
use proptest::prelude::*;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

fn home() -> Vec<Interface> {
    vec![
        Interface {
            ip: Ipv4Addr::new(127, 0, 0, 1),
            netmask: Ipv4Addr::new(255, 0, 0, 0),
        },
        Interface {
            ip: Ipv4Addr::new(192, 168, 1, 31),
            netmask: Ipv4Addr::new(255, 255, 255, 0),
        },
    ]
}

/* REQ-036: Only private or link-local IPv4 sources on a local subnet reach the phone listener; loopback and public addresses do not. */
#[test]
fn lan_sources_only() {
    let interfaces = home();
    assert!(is_lan_source("192.168.1.40".parse().unwrap(), &interfaces));
    assert!(is_lan_source(
        IpAddr::V6(Ipv4Addr::new(192, 168, 1, 40).to_ipv6_mapped()),
        &interfaces
    ));
    assert!(!is_lan_source("192.168.2.40".parse().unwrap(), &interfaces));
    assert!(!is_lan_source("127.0.0.1".parse().unwrap(), &interfaces));
    assert!(!is_lan_source("8.8.8.8".parse().unwrap(), &interfaces));
    assert!(!is_lan_source(IpAddr::V6(Ipv6Addr::LOCALHOST), &interfaces));
    assert_eq!(
        profile_address(&interfaces),
        Some(Ipv4Addr::new(192, 168, 1, 31))
    );
}

proptest! {
    /* REQ-036: The source filter equals a naive subnet check over random addresses. */
    #[test]
    fn source_filter_matches_naive_check(a in any::<u32>(), b in any::<u32>(), bits in 8u32..31) {
        let netmask = Ipv4Addr::from(u32::MAX << (32 - bits));
        let interface = Interface { ip: Ipv4Addr::from(a), netmask };
        let source = Ipv4Addr::from(b);
        let private = |ip: Ipv4Addr| ip.is_private() || ip.is_link_local();
        let naive = private(source) && private(interface.ip)
            && (a & u32::from(netmask)) == (b & u32::from(netmask));
        prop_assert_eq!(is_lan_source(IpAddr::V4(source), &[interface]), naive);
    }

    /* REQ-036: A bucket never lets more than its burst plus its refill through. */
    #[test]
    fn bucket_bounds_requests(gaps in proptest::collection::vec(0u64..2000, 1..200)) {
        let mut bucket = Bucket::full(SENDS, 0);
        let mut now = 0;
        let mut passed = 0u64;
        for gap in gaps {
            now += gap;
            let (next, ok) = bucket.take(SENDS, now);
            bucket = next;
            passed += u64::from(ok);
        }
        let bound = SENDS.burst + (now as f64 / 1000.0) * SENDS.per_second;
        prop_assert!(passed as f64 <= bound + 1e-9);
    }
}

/* REQ-036: A phone's calls are limited to a burst of 20 and 5 per second after. */
#[test]
fn bucket_refuses_then_refills() {
    let mut bucket = Bucket::full(CALLS, 0);
    for _ in 0..20 {
        let (next, ok) = bucket.take(CALLS, 0);
        assert!(ok);
        bucket = next;
    }
    let (empty, ok) = bucket.take(CALLS, 0);
    assert!(!ok);
    assert_eq!(empty.retry_after(CALLS), 1);
    let (_, ok) = empty.take(CALLS, 200);
    assert!(ok);
}

/* REQ-036: Five failures in a row make an address wait 60 s. */
#[test]
fn failures_block_an_address() {
    let mut failures = Failures::default();
    for _ in 0..4 {
        failures = failures.fail(0);
        assert!(!failures.blocked(0));
    }
    failures = failures.fail(0);
    assert!(failures.blocked(59_999));
    assert!(!failures.blocked(60_000));
}

/* REQ-036: A send repeated with the same client_id within 10 minutes gets the first answer and is not sent again; the same id with another message is refused. */
#[test]
fn repeated_client_id_is_answered_once() {
    let mut replays = Replays::default();
    let id = uuid::Uuid::new_v4();
    let send =
        serde_json::json!({"method": "user_send", "message": "hi", "client_id": id.to_string()});
    let other =
        serde_json::json!({"method": "user_send", "message": "bye", "client_id": id.to_string()});
    let same = digest(&send);
    assert_eq!(
        same,
        digest(&serde_json::json!({"message": "hi", "method": "user_send"}))
    );
    assert_eq!(replays.begin(id, same, 0), Begin::New);
    assert_eq!(replays.begin(id, same, 1), Begin::InFlight);
    replays.finish(id, serde_json::json!({"result": "receipt"}));
    assert_eq!(
        replays.begin(id, same, 2),
        Begin::Done(serde_json::json!({"result": "receipt"}))
    );
    assert_eq!(replays.begin(id, digest(&other), 3), Begin::Mismatch);
    assert_eq!(replays.begin(id, same, KEEP_SECONDS + 1), Begin::New);
    let (a, b) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
    assert_ne!(phone_key(a, id), phone_key(b, id));
    assert_eq!(
        client_id(&serde_json::json!({"client_id": id.to_string()})),
        Some(id)
    );
    assert_eq!(client_id(&serde_json::json!({"client_id": "x"})), None);
}

/* REQ-036: The phone host name is sb-<label>.local in lower case within DNS limits, and Host must name it. */
#[test]
fn host_names() {
    assert_eq!(host_name("Laptop-A"), "sb-laptop-a.local");
    assert!(host_name(&"a".repeat(63)).len() <= 63 + ".local".len());
    assert!(host_matches(
        Some("SB-laptop-a.local:47821"),
        "sb-laptop-a.local",
        47821
    ));
    assert!(!host_matches(
        Some("192.168.1.31:47821"),
        "sb-laptop-a.local",
        47821
    ));
    assert!(!host_matches(None, "sb-laptop-a.local", 47821));
}

/* REQ-036: The pairing URL keeps the code in the fragment and draws as a square QR code. */
#[test]
fn pairing_url_and_qr() {
    let url = pair_url("sb-a.local", 47821, "ABCDEFGHJKLMNPQR", b"cert");
    assert!(url.starts_with("https://sb-a.local:47821/#pair=ABCDEFGHJKLMNPQR&fp="));
    let rows = qr_rows(&url);
    assert!(!rows.is_empty());
    assert!(rows.iter().all(|r| r.len() == rows.len()));
    assert!(rows.iter().all(|r| r.chars().all(|c| c == '0' || c == '1')));
}

/* REQ-036: The iOS profile carries the certificate as a root payload with stable identifiers. */
#[test]
fn profile_carries_the_certificate() {
    let profile = mobileconfig(b"cert-bytes", "sb-a.local");
    assert!(profile.contains("<string>com.apple.security.root</string>"));
    assert!(profile.contains(&data_encoding::BASE64.encode(b"cert-bytes")));
    assert_eq!(profile, mobileconfig(b"cert-bytes", "sb-a.local"));
    assert_ne!(profile, mobileconfig(b"other", "sb-a.local"));
}
