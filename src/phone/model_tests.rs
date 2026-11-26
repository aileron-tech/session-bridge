use super::model::*;
use data_encoding::BASE64URL_NOPAD;
use proptest::prelude::*;
use uuid::Uuid;

fn key(seed: u8) -> DeviceKey {
    let mut raw = vec![0x04];
    raw.extend(std::iter::repeat_n(seed, 64));
    DeviceKey::parse(&BASE64URL_NOPAD.encode(&raw)).unwrap()
}
fn code() -> PairingCode {
    PairingCode::from_bytes([7; CODE_BYTES])
}
fn candidate(seed: u8, at: u64) -> Candidate {
    Candidate {
        name: format!("phone {seed}"),
        key: key(seed),
        address: "192.168.1.40".into(),
        at,
    }
}
fn offer(now: u64) -> Offer {
    Offer::new(Uuid::new_v4(), code(), now)
}
fn book_with(count: u8) -> PhoneBook {
    let mut book = PhoneBook::default();
    for seed in 0..count {
        book = book
            .add(Device {
                id: Uuid::new_v4(),
                name: format!("p{seed}"),
                key: key(100 + seed),
                paired_at: 0,
                last_seen: None,
                last_address: None,
            })
            .unwrap();
    }
    book
}

/* REQ-036: A pairing code is 80 bits shown as ABCD-EFGH-JKLM-NPQR and typed with or without dashes, in any case. */
#[test]
fn pairing_code_round_trips_through_its_display_form() {
    let code = code();
    assert_eq!(code.as_str().len(), 16);
    assert_eq!(code.display().len(), 19);
    assert_eq!(PairingCode::parse(&code.display()).unwrap(), code);
    assert_eq!(
        PairingCode::parse(&code.display().to_lowercase().replace('-', " ")).unwrap(),
        code
    );
    assert_eq!(
        PairingCode::parse("ABCD-EFGH"),
        Err(PhoneError::InvalidCode)
    );
    assert_eq!(
        PairingCode::parse("ABCD-EFGH-JKLM-NPQ1"),
        Err(PhoneError::InvalidCode)
    );
    assert!(!format!("{code:?}").contains(code.as_str()));
}

/* REQ-036: A phone key is an uncompressed P-256 point; anything else is refused. */
#[test]
fn device_keys_must_be_uncompressed_points() {
    assert_eq!(key(1).raw().len(), 65);
    assert_eq!(key(1).short().len(), 9);
    assert_eq!(
        DeviceKey::parse(&BASE64URL_NOPAD.encode(&[0x02; 33])),
        Err(PhoneError::InvalidKey)
    );
    assert_eq!(DeviceKey::parse("not base64!"), Err(PhoneError::InvalidKey));
    let json = serde_json::to_string(&key(3)).unwrap();
    assert_eq!(serde_json::from_str::<DeviceKey>(&json).unwrap(), key(3));
}

/* REQ-036: Phone names are 1 to 40 printable characters. */
#[test]
fn phone_names_are_short_and_printable() {
    assert_eq!(validate_name("  iPhone  ").unwrap(), "iPhone");
    assert_eq!(validate_name(""), Err(PhoneError::InvalidName));
    assert_eq!(validate_name("a\nb"), Err(PhoneError::InvalidName));
    assert_eq!(validate_name(&"x".repeat(41)), Err(PhoneError::InvalidName));
}

/* REQ-036: The right code within 120 s makes the phone wait for the laptop; nothing is saved yet. */
#[test]
fn right_code_waits_for_laptop_confirmation() {
    let book = PhoneBook::default();
    let (next, result) = offer(1000).redeem(&code(), candidate(1, 1100), &book);
    assert_eq!(result, Ok(()));
    assert!(matches!(next.state, OfferState::Scanned(_)));
    assert_eq!(next.expires_in(1100), OFFER_SECONDS);
    assert_eq!(pair_status(Some(&next), &key(1), 1100), PairStatus::Waiting);
    assert_eq!(pair_status(Some(&next), &key(2), 1100), PairStatus::Expired);
}

/* REQ-036: A wrong code uses the offer up, so each code allows one guess. */
#[test]
fn wrong_code_uses_the_offer_up() {
    let book = PhoneBook::default();
    let other = PairingCode::from_bytes([8; CODE_BYTES]);
    let (failed, result) = offer(0).redeem(&other, candidate(1, 10), &book);
    assert_eq!(result, Err(PhoneError::WrongCode));
    assert_eq!(failed.state, OfferState::Failed);
    let (_, again) = failed.redeem(&code(), candidate(1, 11), &book);
    assert_eq!(again, Err(PhoneError::AlreadyUsed));
}

/* REQ-036: A code is valid 120 s and single use. */
#[test]
fn code_expires_and_is_single_use() {
    let book = PhoneBook::default();
    let (expired, result) = offer(0).redeem(&code(), candidate(1, OFFER_SECONDS), &book);
    assert_eq!(result, Err(PhoneError::Expired));
    assert_eq!(expired.state, OfferState::Expired);
    let (scanned, _) = offer(0).redeem(&code(), candidate(1, 5), &book);
    let (_, second) = scanned.redeem(&code(), candidate(2, 6), &book);
    assert_eq!(second, Err(PhoneError::AlreadyUsed));
}

/* REQ-036: Accepting on the laptop saves the phone; declining or waiting too long does not. */
#[test]
fn laptop_confirmation_decides() {
    let book = PhoneBook::default();
    let (scanned, _) = offer(0).redeem(&code(), candidate(1, 10), &book);
    let id = Uuid::new_v4();
    let (paired, next) = scanned.confirm(true, id, &book, 20).unwrap();
    assert_eq!(next.devices.len(), 1);
    assert_eq!(next.device(id).unwrap().key, key(1));
    assert_eq!(
        pair_status(Some(&paired), &key(1), 21),
        PairStatus::Paired { device_id: id }
    );
    assert_eq!(pair_status(Some(&paired), &key(2), 21), PairStatus::Expired);

    let (declined, same) = scanned.confirm(false, id, &book, 20).unwrap();
    assert!(same.devices.is_empty());
    assert_eq!(
        pair_status(Some(&declined), &key(1), 21),
        PairStatus::Declined
    );

    assert_eq!(
        scanned.confirm(true, id, &book, 10 + OFFER_SECONDS),
        Err(PhoneError::Expired)
    );
    assert_eq!(
        offer(0).confirm(true, id, &book, 1),
        Err(PhoneError::NotScanned)
    );
}

/* REQ-036: At most 5 phones are paired, each key once. */
#[test]
fn phone_limit_and_duplicates() {
    let full = book_with(MAX_DEVICES as u8);
    let (_, result) = offer(0).redeem(&code(), candidate(1, 1), &full);
    assert_eq!(result, Err(PhoneError::TooManyDevices));
    let one = book_with(1);
    let mut duplicate = candidate(0, 1);
    duplicate.key = key(100);
    let (_, result) = offer(0).redeem(&code(), duplicate, &one);
    assert_eq!(result, Err(PhoneError::AlreadyPaired));
}

/* REQ-036: Unpairing removes exactly that phone. */
#[test]
fn unpairing_removes_the_phone() {
    let book = book_with(2);
    let id = book.devices[0].id;
    let next = book.remove(id).unwrap();
    assert_eq!(next.devices.len(), 1);
    assert!(next.device(id).is_none());
    assert_eq!(next.remove(id), Err(PhoneError::NoSuchDevice));
}

/* REQ-036: A phone session ends after 15 minutes idle and 12 hours at most. */
#[test]
fn sessions_expire() {
    assert!(session_valid(0, 0, SESSION_IDLE - 1));
    assert!(!session_valid(0, 0, SESSION_IDLE));
    assert!(session_valid(0, SESSION_MAX - 10, SESSION_MAX - 1));
    assert!(!session_valid(0, SESSION_MAX - 10, SESSION_MAX));
}

/* REQ-036: Certificate fingerprints read like the iOS profile screen; the short form is its first 8 pairs. */
#[test]
fn certificate_fingerprints_match_the_ios_form() {
    let full = hex_fingerprint(b"cert");
    assert_eq!(full.split(' ').count(), 32);
    assert!(full.starts_with(&short_hex_fingerprint(b"cert")));
    assert_eq!(short_hex_fingerprint(b"cert").split(' ').count(), 8);
}

proptest! {
    /* REQ-036: Only the exact code redeems an offer. */
    #[test]
    fn only_the_exact_code_redeems(bytes in proptest::array::uniform10(any::<u8>())) {
        let typed = PairingCode::from_bytes(bytes);
        let (_, result) = offer(0).redeem(&typed, candidate(1, 1), &PhoneBook::default());
        prop_assert_eq!(result.is_ok(), typed == code());
    }
}
