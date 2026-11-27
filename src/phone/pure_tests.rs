use super::{
    limits::{Bucket, CALLS, Failures, SENDS},
    net::{
        Interface, host_allowed, host_name, is_lan_source, lan_addresses, origin_allowed,
        request_host, site_names,
    },
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
}

/* REQ-036: The LAN addresses are read again at most once per interval, not on every request. */
#[test]
fn addresses_are_cached() {
    use super::net::Cached;
    use std::time::{Duration, Instant};
    let mut cached: Cached<u32> = Cached::default();
    let start = Instant::now();
    let ttl = Duration::from_secs(10);
    let mut reads = 0;
    let mut read = |value| {
        reads += 1;
        value
    };
    assert_eq!(cached.get(start, ttl, || read(1)), 1);
    assert_eq!(
        cached.get(start + Duration::from_secs(9), ttl, || read(2)),
        1
    );
    assert_eq!(cached.get(start + ttl, ttl, || read(3)), 3);
    assert_eq!(reads, 2);
}

/* REQ-036: Phones are offered this laptop's LAN addresses, the default route's first, then other private ones, then link-local; never loopback or public ones. */
#[test]
fn lan_addresses_default_route_first() {
    let interface = |a, b, c, d| Interface {
        ip: Ipv4Addr::new(a, b, c, d),
        netmask: Ipv4Addr::new(255, 255, 255, 0),
    };
    let interfaces = [
        interface(127, 0, 0, 1),
        interface(169, 254, 3, 4),
        interface(10, 8, 0, 2),
        interface(192, 168, 1, 123),
        interface(203, 0, 113, 9),
        interface(192, 168, 1, 123),
    ];
    assert_eq!(
        lan_addresses(&interfaces, Some(Ipv4Addr::new(192, 168, 1, 123))),
        vec![
            Ipv4Addr::new(192, 168, 1, 123),
            Ipv4Addr::new(10, 8, 0, 2),
            Ipv4Addr::new(169, 254, 3, 4),
        ]
    );
    assert_eq!(
        lan_addresses(&interfaces, None),
        vec![
            Ipv4Addr::new(10, 8, 0, 2),
            Ipv4Addr::new(192, 168, 1, 123),
            Ipv4Addr::new(169, 254, 3, 4),
        ]
    );
    // A default route over a public address adds nothing.
    assert_eq!(
        lan_addresses(&interfaces[..2], Some(Ipv4Addr::new(203, 0, 113, 9))),
        vec![Ipv4Addr::new(169, 254, 3, 4)]
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

/* REQ-036: Host and Origin must name this listener by a current LAN address or its sb-<label>.local name (lower case, within DNS limits) with its port; the phone signs that host. */
#[test]
fn host_names() {
    assert_eq!(host_name("Laptop-A"), "sb-laptop-a.local");
    assert!(host_name(&"a".repeat(63)).len() <= 63 + ".local".len());
    let names = site_names(
        &[Ipv4Addr::new(192, 168, 1, 123), Ipv4Addr::new(10, 8, 0, 2)],
        "sb-laptop-a.local",
    );
    assert!(host_allowed(Some("192.168.1.123:47821"), &names, 47821));
    assert!(host_allowed(Some("10.8.0.2:47821"), &names, 47821));
    assert!(host_allowed(Some("SB-laptop-a.local:47821"), &names, 47821));
    assert!(!host_allowed(Some("192.168.1.124:47821"), &names, 47821));
    assert!(!host_allowed(Some("192.168.1.123:47822"), &names, 47821));
    assert!(!host_allowed(Some("evil.example:47821"), &names, 47821));
    assert!(!host_allowed(None, &names, 47821));
    assert!(origin_allowed(None, &names, 47821));
    assert!(origin_allowed(
        Some("https://192.168.1.123:47821"),
        &names,
        47821
    ));
    assert!(!origin_allowed(
        Some("http://192.168.1.123:47821"),
        &names,
        47821
    ));
    assert!(!origin_allowed(Some("https://evil.example"), &names, 47821));
    assert_eq!(request_host("192.168.1.123:47821"), "192.168.1.123");
    assert_eq!(request_host("SB-a.local:47821"), "sb-a.local");
}

/* REQ-036: The pairing URL keeps the code in the fragment and draws as a square QR code. */
#[test]
fn pairing_url_and_qr() {
    let url = pair_url("192.168.1.123", 47821, "ABCDEFGHJKLMNPQR", b"cert");
    assert!(url.starts_with("https://192.168.1.123:47821/#pair=ABCDEFGHJKLMNPQR&fp="));
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

/* REQ-036: The admin view offers the profile and an open pairing code for each LAN address, the default route's first, and the .local name only when there is no address. */
#[test]
fn view_links_per_address() {
    use super::model::{Offer, OfferState, PairingCode, PhoneBook};
    use super::view::{Runtime, ViewInput, phones_view};
    let book = PhoneBook {
        enabled: true,
        port: Some(47821),
        devices: Vec::new(),
    };
    let runtime = Runtime {
        listening: true,
        problem: None,
        addresses: vec!["192.168.1.123".into(), "10.8.0.2".into()],
        cert_der: Some(b"root".to_vec()),
    };
    let offer = Offer::new(uuid::Uuid::new_v4(), PairingCode::from_bytes([1; 10]), 0);
    let view = |offer: &Offer, runtime: &Runtime| {
        phones_view(ViewInput {
            book: &book,
            offer: Some(offer),
            runtime,
            host: "sb-a.local",
            sessions: &|_| 0,
            now: 1,
        })
    };
    let shown = view(&offer, &runtime);
    assert_eq!(shown.address.as_deref(), Some("192.168.1.123"));
    assert_eq!(shown.addresses.len(), 2);
    assert_eq!(
        shown.profile_url.as_deref(),
        Some("http://192.168.1.123:47822/bridge.mobileconfig")
    );
    assert_eq!(shown.profile_links[1].address, "10.8.0.2");
    let open = shown.offer.unwrap();
    assert!(open.url.starts_with("https://192.168.1.123:47821/#pair="));
    assert_eq!(open.links.len(), 2);
    assert!(
        open.links[1]
            .url
            .starts_with("https://10.8.0.2:47821/#pair=")
    );
    assert!(!open.qr.is_empty());

    let ended = Offer {
        state: OfferState::Expired,
        ..offer.clone()
    };
    let closed = view(&ended, &runtime).offer.unwrap();
    assert!(closed.url.is_empty() && closed.links.is_empty());

    let nowhere = Runtime {
        addresses: Vec::new(),
        ..runtime.clone()
    };
    let shown = view(&offer, &nowhere);
    assert!(shown.profile_url.is_none());
    assert!(
        shown
            .offer
            .unwrap()
            .url
            .starts_with("https://sb-a.local:47821/")
    );
}
