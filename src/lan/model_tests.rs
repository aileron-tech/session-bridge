use super::*;
use proptest::prelude::*;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

fn fp(byte: u8) -> Fingerprint {
    Fingerprint::of_public_key(&[byte; 32])
}
fn label(value: &str) -> HostLabel {
    HostLabel::parse(value).unwrap()
}
fn ip(last: u8) -> IpAddr {
    IpAddr::V4(Ipv4Addr::new(192, 168, 1, last))
}
fn code(value: u32) -> PairingCode {
    PairingCode::from_random(value)
}
fn open_gate(now: u64) -> PairingGate {
    PairingGate::default()
        .request(now, label("laptop-a"), fp(1), ip(31), code(482_913))
        .unwrap()
}

/* REQ-017: A one-time code is six digits shown as 482-913 and typed with or without separators. */
#[test]
fn pairing_code_formats_and_parses() {
    assert_eq!(code(482_913).to_string(), "482-913");
    assert_eq!(code(7).to_string(), "000-007");
    assert_eq!(code(1_482_913).to_string(), "482-913");
    for typed in ["482-913", "482913", " 482 913 ", "482 - 913"] {
        assert_eq!(PairingCode::parse(typed).unwrap(), code(482_913));
    }
    for bad in ["", "48291", "4829134", "48a-913", "482_913", "--"] {
        assert_eq!(PairingCode::parse(bad), Err(LanError::InvalidCode));
    }
    assert_eq!(code(7).password(), "000007");
}

/* REQ-017: Host labels are DNS-safe and derived from any machine name. */
#[test]
fn host_labels_are_dns_safe() {
    assert_eq!(label("laptop-b").as_str(), "laptop-b");
    for bad in ["", "-a", "a-", "a b", "a.b", &"x".repeat(64), "caf\u{e9}"] {
        assert_eq!(HostLabel::parse(bad), Err(LanError::InvalidLabel));
    }
    for (raw, expected) in [
        ("Alexeis-MacBook-Pro.local", "Alexeis-MacBook-Pro"),
        ("My Laptop", "My-Laptop"),
        ("--weird..name--", "weird-name"),
        ("", "session-bridge"),
        ("...", "session-bridge"),
    ] {
        assert_eq!(HostLabel::from_machine_name(raw).as_str(), expected);
    }
    assert_eq!(
        HostLabel::from_machine_name(&"a".repeat(80)).as_str().len(),
        63
    );
    let json = serde_json::to_string(&label("laptop-b")).unwrap();
    assert_eq!(json, "\"laptop-b\"");
    assert!(serde_json::from_str::<HostLabel>("\"bad name\"").is_err());
}

/* REQ-017: A key fingerprint round-trips through its text form and has a short display form. */
#[test]
fn fingerprints_round_trip() {
    let value = fp(9);
    let text = value.to_string();
    assert_eq!(text.len(), 52);
    assert_eq!(Fingerprint::parse(&text).unwrap(), value);
    assert_eq!(Fingerprint::parse(&text.to_lowercase()).unwrap(), value);
    assert_eq!(value.short(), format!("{}-{}", &text[..4], &text[4..8]));
    assert!(value.matches_prefix(&value.short()));
    assert!(value.matches_prefix(&text[..6].to_lowercase()));
    assert!(!value.matches_prefix(&text[..3]));
    assert!(!fp(10).matches_prefix(&value.short()));
    for bad in ["", "abc", &text[..50]] {
        assert_eq!(Fingerprint::parse(bad), Err(LanError::InvalidFingerprint));
    }
    let json = serde_json::to_string(&value).unwrap();
    assert_eq!(serde_json::from_str::<Fingerprint>(&json).unwrap(), value);
    assert!(serde_json::from_str::<Fingerprint>("\"nope\"").is_err());
    assert_eq!(value.as_bytes(), &fp(9).as_bytes().clone());
}

/* REQ-017: The right code pairs once; the request closes after success. */
#[test]
fn gate_pairs_with_the_right_code() {
    let gate = open_gate(100);
    let view = gate.view(130);
    let incoming = view.incoming.unwrap();
    assert_eq!(incoming.host, "laptop-a");
    assert_eq!(incoming.code, "482-913");
    assert_eq!(incoming.address, ip(31));
    assert_eq!(incoming.expires_in, CODE_LIFETIME - 30);
    assert_eq!(gate.code(&fp(1)), Some(code(482_913)));
    assert_eq!(gate.code(&fp(2)), None);
    let (gate, attempt) = gate.attempt(110, &fp(1), true).unwrap();
    assert_eq!(attempt, Attempt::Paired);
    let view = gate.view(110);
    assert!(view.incoming.is_none());
    assert_eq!(
        view.last,
        Some(PairingOutcome::Paired {
            host: "laptop-a".into(),
            key: fp(1).short()
        })
    );
    assert_eq!(gate.attempt(111, &fp(1), true), Err(GateError::NoRequest));
}

/* REQ-017: Three wrong codes close the request and cool down that address. */
#[test]
fn gate_closes_after_three_wrong_codes() {
    let gate = open_gate(100);
    let (gate, first) = gate.attempt(101, &fp(1), false).unwrap();
    assert_eq!(first, Attempt::Wrong { attempts_left: 2 });
    let (gate, second) = gate.attempt(102, &fp(1), false).unwrap();
    assert_eq!(second, Attempt::Wrong { attempts_left: 1 });
    let (gate, third) = gate.attempt(103, &fp(1), false).unwrap();
    assert_eq!(third, Attempt::Exhausted);
    assert!(gate.view(103).incoming.is_none());
    assert!(matches!(
        gate.view(103).last,
        Some(PairingOutcome::Failed { .. })
    ));
    let again = gate.request(110, label("laptop-a"), fp(1), ip(31), code(1));
    assert_eq!(again.err(), Some(GateError::CoolingDown { seconds: 23 }));
    assert!(
        gate.request(110, label("laptop-c"), fp(3), ip(40), code(1))
            .is_ok(),
        "Another address is not cooled down"
    );
    assert!(
        gate.request(133, label("laptop-a"), fp(1), ip(31), code(1))
            .is_ok()
    );
}

/* REQ-017: One request at a time; the same host may restart it; expired codes fail. */
#[test]
fn gate_holds_one_request_and_expires_it() {
    let gate = open_gate(100);
    assert_eq!(
        gate.request(101, label("laptop-c"), fp(3), ip(40), code(5))
            .err(),
        Some(GateError::Busy)
    );
    let restarted = gate
        .request(101, label("laptop-a"), fp(1), ip(31), code(5))
        .unwrap();
    assert_eq!(restarted.code(&fp(1)), Some(code(5)));
    let after_expiry = 100 + CODE_LIFETIME;
    assert!(gate.view(after_expiry).incoming.is_none());
    assert_eq!(
        gate.attempt(after_expiry, &fp(1), true).err(),
        Some(GateError::Expired)
    );
    assert!(
        gate.request(after_expiry, label("laptop-c"), fp(3), ip(40), code(5))
            .is_ok()
    );
    assert_eq!(
        gate.attempt(101, &fp(3), true).err(),
        Some(GateError::NoRequest)
    );
    let abandoned = gate.abandon(&fp(1));
    assert!(abandoned.view(101).incoming.is_none());
    assert_eq!(
        gate.abandon(&fp(3)).view(101).incoming.unwrap().host,
        "laptop-a"
    );
}

/* REQ-017: Gate errors explain what the user can do. */
#[test]
fn gate_errors_are_actionable() {
    for (error, text) in [
        (GateError::Busy, "Another laptop is pairing"),
        (GateError::CoolingDown { seconds: 5 }, "5 s"),
        (GateError::NoRequest, "No pairing request"),
        (GateError::Expired, "expired"),
        (GateError::Exhausted, "Too many wrong codes"),
    ] {
        assert!(error.to_string().contains(text), "{error}");
    }
}

fn peer(byte: u8, name: &str, address: Option<SocketAddr>) -> PairedPeer {
    PairedPeer {
        label: label(name),
        fingerprint: fp(byte),
        address,
        paired_at: 1,
    }
}
fn socket(last: u8, port: u16) -> SocketAddr {
    SocketAddr::new(ip(last), port)
}

/* REQ-017: Discovery lists other bridges, marks paired ones, and keeps offline paired hosts. */
#[test]
fn merges_discovered_and_paired_hosts() {
    let discovered = vec![
        DiscoveredHost {
            label: label("self"),
            fingerprint: fp(0),
            addresses: vec![socket(1, 1)],
        },
        DiscoveredHost {
            label: label("laptop-c"),
            fingerprint: fp(3),
            addresses: vec![socket(40, 47820)],
        },
        DiscoveredHost {
            label: label("laptop-b"),
            fingerprint: fp(2),
            addresses: vec![socket(20, 47820)],
        },
    ];
    let paired = vec![
        peer(2, "laptop-b", Some(socket(19, 47820))),
        peer(4, "laptop-d", Some(socket(50, 47820))),
    ];
    let hosts = merge_hosts(&fp(0), &discovered, &paired);
    let names: Vec<_> = hosts.iter().map(|h| h.host.as_str()).collect();
    assert_eq!(names, ["laptop-b", "laptop-c", "laptop-d"]);
    assert!(hosts[0].paired && hosts[0].online);
    assert_eq!(hosts[0].addresses, [socket(20, 47820), socket(19, 47820)]);
    assert!(!hosts[1].paired && hosts[1].online);
    assert!(hosts[2].paired && !hosts[2].online);
    assert_eq!(hosts[2].key, fp(4).short());
    assert_eq!(hosts[2].fingerprint, fp(4).to_string());
}

/* REQ-017: A host is chosen by name, key prefix, or address; unknown and ambiguous names fail. */
#[test]
fn resolves_pairing_targets() {
    let discovered = vec![
        DiscoveredHost {
            label: label("laptop-b"),
            fingerprint: fp(2),
            addresses: vec![socket(20, 47820)],
        },
        DiscoveredHost {
            label: label("twin"),
            fingerprint: fp(5),
            addresses: vec![socket(60, 1)],
        },
        DiscoveredHost {
            label: label("twin"),
            fingerprint: fp(6),
            addresses: vec![socket(61, 1)],
        },
    ];
    let hosts = merge_hosts(&fp(0), &discovered, &[]);
    assert_eq!(
        resolve_target("laptop-b", &hosts).unwrap(),
        Target::Peer {
            fingerprint: fp(2),
            addresses: vec![socket(20, 47820)]
        }
    );
    assert_eq!(
        resolve_target(&fp(5).short(), &hosts).unwrap(),
        Target::Peer {
            fingerprint: fp(5),
            addresses: vec![socket(60, 1)]
        }
    );
    assert_eq!(
        resolve_target("127.0.0.1:5000", &hosts).unwrap(),
        Target::Address("127.0.0.1:5000".parse().unwrap())
    );
    assert_eq!(
        resolve_target("twin", &hosts),
        Err(LanError::AmbiguousHost("twin".into()))
    );
    assert_eq!(
        resolve_target("nobody", &hosts),
        Err(LanError::UnknownHost("nobody".into()))
    );
}

/* REQ-017: Addresses prefer routable IPv4, drop duplicates, and keep IPv6 last. */
#[test]
fn orders_addresses() {
    let v6 = SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 1);
    let link_local = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(169, 254, 1, 1)), 1);
    let ordered = order_addresses(vec![
        v6,
        link_local,
        socket(2, 1),
        socket(2, 1),
        socket(1, 1),
    ]);
    assert_eq!(ordered, [socket(2, 1), socket(1, 1), link_local, v6]);
}

/* REQ-017: A label already used by another paired key is a conflict; re-pairing the same key is not. */
#[test]
fn detects_label_conflicts() {
    let paired = vec![peer(2, "laptop-b", None)];
    assert!(label_conflict(&paired, &label("laptop-b"), &fp(3)));
    assert!(!label_conflict(&paired, &label("laptop-b"), &fp(2)));
    assert!(!label_conflict(&paired, &label("laptop-c"), &fp(3)));
}

/* REQ-017: Forward results map to delivery states without automatic resend of uncertain sends. */
#[test]
fn maps_forward_outcomes() {
    let dispatching = Delivery::Dispatching;
    let host = "laptop-b".to_string();
    let accepted = |remote| ForwardOutcome::Accepted {
        host: host.clone(),
        remote,
    };
    assert_eq!(
        complete_forward(
            &dispatching,
            &accepted(Delivery::Pending { reason: "x".into() })
        ),
        Some(Delivery::Forwarded { host: host.clone() })
    );
    assert_eq!(
        complete_forward(&dispatching, &accepted(Delivery::Queued)),
        Some(Delivery::Queued)
    );
    assert!(matches!(
        complete_forward(
            &dispatching,
            &ForwardOutcome::Unreachable {
                reason: "off".into()
            }
        ),
        Some(Delivery::Pending { .. })
    ));
    assert!(matches!(
        complete_forward(
            &dispatching,
            &ForwardOutcome::Rejected {
                reason: "no".into()
            }
        ),
        Some(Delivery::Refused { .. })
    ));
    assert!(matches!(
        complete_forward(
            &dispatching,
            &ForwardOutcome::Interrupted {
                reason: "cut".into()
            }
        ),
        Some(Delivery::Uncertain { .. })
    ));
    assert_eq!(
        complete_forward(
            &Delivery::Queued,
            &accepted(Delivery::Pending { reason: "x".into() })
        ),
        None,
        "A status that arrived first is kept"
    );
}

/* REQ-017: Status from the other host advances a forwarded message and never reopens it. */
#[test]
fn applies_remote_status() {
    let forwarded = Delivery::Forwarded {
        host: "laptop-b".into(),
    };
    assert_eq!(
        apply_remote_status(&forwarded, &Delivery::Queued),
        Some(Delivery::Queued)
    );
    assert_eq!(
        apply_remote_status(&Delivery::Queued, &Delivery::Delivered),
        Some(Delivery::Delivered)
    );
    assert_eq!(
        apply_remote_status(&Delivery::Held { reason: "a".into() }, &Delivery::Delivered),
        Some(Delivery::Delivered)
    );
    assert_eq!(
        apply_remote_status(&Delivery::Dispatching, &Delivery::Queued),
        Some(Delivery::Queued)
    );
    for ignored in [
        Delivery::Pending { reason: "x".into() },
        Delivery::Dispatching,
        Delivery::Forwarded { host: "x".into() },
    ] {
        assert_eq!(apply_remote_status(&forwarded, &ignored), None);
    }
    for terminal in [
        Delivery::Delivered,
        Delivery::Refused { reason: "x".into() },
        Delivery::Uncertain { reason: "x".into() },
        Delivery::Pending { reason: "x".into() },
    ] {
        assert_eq!(apply_remote_status(&terminal, &Delivery::Queued), None);
    }
}

proptest! {
    /* REQ-017: Every generated code prints and parses back to itself. */
    #[test]
    fn code_round_trips(value in any::<u32>()) {
        let code = PairingCode::from_random(value);
        prop_assert_eq!(PairingCode::parse(&code.to_string()).unwrap(), code);
        prop_assert_eq!(PairingCode::parse(&code.password()).unwrap(), code);
    }
    /* REQ-017: Any machine name yields a valid label. */
    #[test]
    fn machine_names_yield_valid_labels(raw in ".{0,100}") {
        let label = HostLabel::from_machine_name(&raw);
        prop_assert_eq!(HostLabel::parse(label.as_str()).unwrap(), label);
    }
    /* REQ-017: Any key fingerprint round-trips through text. */
    #[test]
    fn fingerprint_text_round_trips(bytes in any::<[u8; 32]>()) {
        let value = Fingerprint::of_public_key(&bytes);
        prop_assert_eq!(Fingerprint::parse(&value.to_string()).unwrap(), value);
    }
    /* REQ-017: A request never survives more than three wrong codes. */
    #[test]
    fn wrong_codes_never_exceed_the_limit(failures in 1usize..10) {
        let mut gate = open_gate(0);
        let mut closed = false;
        for n in 0..failures {
            match gate.attempt(1, &fp(1), false) {
                Ok((next, Attempt::Exhausted)) => { prop_assert_eq!(n, 2); gate = next; closed = true; }
                Ok((next, _)) => { prop_assert!(n < 2); gate = next; }
                Err(GateError::NoRequest) => prop_assert!(closed),
                Err(other) => prop_assert!(false, "unexpected {other:?}"),
            }
        }
        prop_assert_eq!(closed, failures >= 3);
    }
}
