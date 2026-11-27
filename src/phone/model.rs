//! Phones, pairing offers and their transitions (REQ-036). Pure: callers pass
//! the time (unix seconds) and randomness in.
use data_encoding::{BASE32_NOPAD, BASE64URL_NOPAD};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// HTTPS port of the phone listener; the certificate profile is served over
/// plain HTTP on the next port.
pub const DEFAULT_PORT: u16 = 47821;
/// A pairing offer, and the laptop confirmation after the phone used it.
pub const OFFER_SECONDS: u64 = 120;
pub const MAX_DEVICES: usize = 5;
/// How often a phone's last use is saved while it makes calls.
pub const SEEN_EVERY: u64 = 300;
/// Random bytes of a pairing code: 80 bits, 16 base32 characters.
pub const CODE_BYTES: usize = 10;
const NAME_LIMIT: usize = 40;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PhoneError {
    #[error("Pairing code must be 16 letters and digits (ABCD-EFGH-JKLM-NPQR)")]
    InvalidCode,
    #[error("Phone name must be 1 to 40 printable characters")]
    InvalidName,
    #[error("Phone key must be an uncompressed P-256 public key")]
    InvalidKey,
    #[error("No pairing code is open: show a new one on the laptop")]
    NoOffer,
    #[error("Pairing code expired: show a new one on the laptop")]
    Expired,
    #[error("Wrong pairing code: show a new one on the laptop")]
    WrongCode,
    #[error("This pairing code was already used")]
    AlreadyUsed,
    #[error("No phone waits for confirmation")]
    NotScanned,
    #[error("At most 5 phones can be paired: unpair one first")]
    TooManyDevices,
    #[error("This phone is already paired")]
    AlreadyPaired,
    #[error("No such phone")]
    NoSuchDevice,
}

/// A one-time pairing code, kept as 16 base32 characters without dashes.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairingCode(String);
impl std::fmt::Debug for PairingCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PairingCode(..)")
    }
}
impl PairingCode {
    pub fn from_bytes(bytes: [u8; CODE_BYTES]) -> Self {
        Self(BASE32_NOPAD.encode(&bytes))
    }
    /// What the user typed: dashes and spaces are ignored, case does not matter.
    pub fn parse(input: &str) -> Result<Self, PhoneError> {
        let code: String = input
            .chars()
            .filter(|c| !matches!(c, '-' | ' '))
            .map(|c| c.to_ascii_uppercase())
            .collect();
        let valid = code.len() == 16
            && code
                .bytes()
                .all(|b| b.is_ascii_uppercase() || (b'2'..=b'7').contains(&b));
        valid.then_some(Self(code)).ok_or(PhoneError::InvalidCode)
    }
    /// `ABCD-EFGH-JKLM-NPQR`, as the admin page shows it.
    pub fn display(&self) -> String {
        group(&self.0, 4)
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    /// Constant-time comparison.
    pub fn matches(&self, other: &PairingCode) -> bool {
        self.0.len() == other.0.len()
            && self
                .0
                .bytes()
                .zip(other.0.bytes())
                .fold(0u8, |acc, (a, b)| acc | (a ^ b))
                == 0
    }
}

/// A phone's public key: an uncompressed P-256 point (65 bytes, WebCrypto
/// `raw` export), kept as base64url.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct DeviceKey(Vec<u8>);
impl DeviceKey {
    pub fn parse(encoded: &str) -> Result<Self, PhoneError> {
        let raw = BASE64URL_NOPAD
            .decode(encoded.trim_end_matches('=').as_bytes())
            .map_err(|_| PhoneError::InvalidKey)?;
        (raw.len() == 65 && raw[0] == 0x04)
            .then_some(Self(raw))
            .ok_or(PhoneError::InvalidKey)
    }
    pub fn raw(&self) -> &[u8] {
        &self.0
    }
    /// The short form both screens show while pairing: `ABCD-EFGH`.
    pub fn short(&self) -> String {
        short_fingerprint(&self.0)
    }
}
impl TryFrom<String> for DeviceKey {
    type Error = PhoneError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}
impl From<DeviceKey> for String {
    fn from(key: DeviceKey) -> Self {
        BASE64URL_NOPAD.encode(&key.0)
    }
}

/// `ABCD-EFGH`: the first 40 bits of SHA-256, base32.
pub fn short_fingerprint(data: &[u8]) -> String {
    let encoded = BASE32_NOPAD.encode(&Sha256::digest(data));
    group(&encoded[..8], 4)
}
/// SHA-256 as 32 uppercase hex pairs separated by spaces, as iOS shows a
/// certificate fingerprint.
pub fn hex_fingerprint(data: &[u8]) -> String {
    Sha256::digest(data)
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}
/// The certificate's short form: the first 8 hex pairs of what iOS shows,
/// so the user can compare it on the profile screen, the admin page and the
/// phone app alike.
pub fn short_hex_fingerprint(data: &[u8]) -> String {
    hex_fingerprint(data)
        .split(' ')
        .take(8)
        .collect::<Vec<_>>()
        .join(" ")
}
fn group(text: &str, size: usize) -> String {
    text.as_bytes()
        .chunks(size)
        .map(|c| String::from_utf8_lossy(c).into_owned())
        .collect::<Vec<_>>()
        .join("-")
}

/// A name the phone gives itself, shown on the laptop.
pub fn validate_name(name: &str) -> Result<String, PhoneError> {
    let name = name.trim();
    let valid =
        (1..=NAME_LIMIT).contains(&name.chars().count()) && !name.chars().any(char::is_control);
    valid
        .then(|| name.to_owned())
        .ok_or(PhoneError::InvalidName)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Device {
    pub id: Uuid,
    pub name: String,
    pub key: DeviceKey,
    pub paired_at: u64,
    #[serde(default)]
    pub last_seen: Option<u64>,
    #[serde(default)]
    pub last_address: Option<String>,
}

/// What `phones.json` keeps: the switch and the paired phones.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhoneBook {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub devices: Vec<Device>,
}
impl PhoneBook {
    pub fn port(&self) -> u16 {
        self.port.unwrap_or(DEFAULT_PORT)
    }
    pub fn device(&self, id: Uuid) -> Option<&Device> {
        self.devices.iter().find(|d| d.id == id)
    }
    pub fn add(&self, device: Device) -> Result<PhoneBook, PhoneError> {
        if self.devices.len() >= MAX_DEVICES {
            return Err(PhoneError::TooManyDevices);
        }
        if self.devices.iter().any(|d| d.key == device.key) {
            return Err(PhoneError::AlreadyPaired);
        }
        let mut next = self.clone();
        next.devices.push(device);
        Ok(next)
    }
    pub fn remove(&self, id: Uuid) -> Result<PhoneBook, PhoneError> {
        let mut next = self.clone();
        next.devices.retain(|d| d.id != id);
        if next.devices.len() == self.devices.len() {
            return Err(PhoneError::NoSuchDevice);
        }
        Ok(next)
    }
    /// A call from a signed-in phone: its last use is recorded when the saved
    /// one is older than `SEEN_EVERY`, so the device list shows when a phone
    /// was last used without writing the book on every call.
    pub fn used(&self, id: Uuid, now: u64) -> Option<PhoneBook> {
        let device = self.device(id)?;
        if device
            .last_seen
            .is_some_and(|seen| now.saturating_sub(seen) < SEEN_EVERY)
        {
            return None;
        }
        let mut next = self.clone();
        if let Some(device) = next.devices.iter_mut().find(|d| d.id == id) {
            device.last_seen = Some(now);
        }
        Some(next)
    }
    /// Records a use; the book is saved only when this changes it.
    pub fn seen(&self, id: Uuid, now: u64, address: &str) -> PhoneBook {
        let mut next = self.clone();
        if let Some(device) = next.devices.iter_mut().find(|d| d.id == id) {
            device.last_seen = Some(now);
            device.last_address = Some(address.to_owned());
        }
        next
    }
}

/// The phone that used an offer's code and waits for the laptop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub name: String,
    pub key: DeviceKey,
    pub address: String,
    pub at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OfferState {
    Waiting,
    Scanned(Candidate),
    Paired {
        device_id: Uuid,
        key: DeviceKey,
    },
    Declined {
        key: DeviceKey,
    },
    Expired,
    /// A wrong code was typed: the offer is used up.
    Failed,
}

/// One pairing offer; a new one replaces it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Offer {
    pub id: Uuid,
    pub code: PairingCode,
    pub created_at: u64,
    pub state: OfferState,
}
impl Offer {
    pub fn new(id: Uuid, code: PairingCode, now: u64) -> Self {
        Self {
            id,
            code,
            created_at: now,
            state: OfferState::Waiting,
        }
    }
    /// When the open state ends: the code 120 s after it was shown, the
    /// laptop confirmation 120 s after the phone used it.
    pub fn deadline(&self) -> Option<u64> {
        match &self.state {
            OfferState::Waiting => Some(self.created_at + OFFER_SECONDS),
            OfferState::Scanned(candidate) => Some(candidate.at + OFFER_SECONDS),
            _ => None,
        }
    }
    pub fn expires_in(&self, now: u64) -> u64 {
        self.deadline().map_or(0, |d| d.saturating_sub(now))
    }
    /// The offer at `now`: an open state past its deadline is expired.
    pub fn at(&self, now: u64) -> Offer {
        match self.deadline() {
            Some(deadline) if now >= deadline => Offer {
                state: OfferState::Expired,
                ..self.clone()
            },
            _ => self.clone(),
        }
    }
    /// A phone typed `code`. A wrong code uses the offer up (returned as
    /// `Failed` with the error), so each code allows one guess.
    pub fn redeem(
        &self,
        code: &PairingCode,
        candidate: Candidate,
        book: &PhoneBook,
    ) -> (Offer, Result<(), PhoneError>) {
        let current = self.at(candidate.at);
        let refuse = |error| (current.clone(), Err(error));
        match current.state {
            OfferState::Waiting => {}
            OfferState::Expired => return refuse(PhoneError::Expired),
            _ => return refuse(PhoneError::AlreadyUsed),
        }
        if !current.code.matches(code) {
            let failed = Offer {
                state: OfferState::Failed,
                ..current
            };
            return (failed, Err(PhoneError::WrongCode));
        }
        if book.devices.len() >= MAX_DEVICES {
            return refuse(PhoneError::TooManyDevices);
        }
        if book.devices.iter().any(|d| d.key == candidate.key) {
            return refuse(PhoneError::AlreadyPaired);
        }
        (
            Offer {
                state: OfferState::Scanned(candidate),
                ..current
            },
            Ok(()),
        )
    }
    /// The user accepted or declined the phone on the laptop. Accepting adds
    /// it to the book under `device_id`.
    pub fn confirm(
        &self,
        accept: bool,
        device_id: Uuid,
        book: &PhoneBook,
        now: u64,
    ) -> Result<(Offer, PhoneBook), PhoneError> {
        let current = self.at(now);
        let OfferState::Scanned(candidate) = &current.state else {
            return Err(match current.state {
                OfferState::Expired => PhoneError::Expired,
                _ => PhoneError::NotScanned,
            });
        };
        if !accept {
            let declined = Offer {
                state: OfferState::Declined {
                    key: candidate.key.clone(),
                },
                ..current
            };
            return Ok((declined, book.clone()));
        }
        let next = book.add(Device {
            id: device_id,
            name: candidate.name.clone(),
            key: candidate.key.clone(),
            paired_at: now,
            last_seen: Some(now),
            last_address: Some(candidate.address.clone()),
        })?;
        let paired = Offer {
            state: OfferState::Paired {
                device_id,
                key: candidate.key.clone(),
            },
            ..current
        };
        Ok((paired, next))
    }
}

/// What a phone sees while it waits for the laptop: only the phone whose key
/// used the offer gets an answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum PairStatus {
    Waiting,
    Paired { device_id: Uuid },
    Declined,
    Expired,
}
pub fn pair_status(offer: Option<&Offer>, key: &DeviceKey, now: u64) -> PairStatus {
    let Some(offer) = offer.map(|o| o.at(now)) else {
        return PairStatus::Expired;
    };
    match &offer.state {
        OfferState::Scanned(candidate) if &candidate.key == key => PairStatus::Waiting,
        OfferState::Paired {
            device_id,
            key: paired,
        } if paired == key => PairStatus::Paired {
            device_id: *device_id,
        },
        OfferState::Declined { key: declined } if declined == key => PairStatus::Declined,
        _ => PairStatus::Expired,
    }
}

/// The bytes a phone signs to open a session: bound to this bridge's host
/// name and a one-time nonce.
pub fn session_message(host: &str, nonce: &str) -> Vec<u8> {
    format!("session-bridge phone v1\n{host}\n{nonce}").into_bytes()
}
/// A session lasts 15 minutes without use and 12 hours at most.
pub const SESSION_IDLE: u64 = 15 * 60;
pub const SESSION_MAX: u64 = 12 * 60 * 60;
pub const NONCE_SECONDS: u64 = 30;
pub fn session_valid(created: u64, last_used: u64, now: u64) -> bool {
    now < created + SESSION_MAX && now < last_used + SESSION_IDLE
}
