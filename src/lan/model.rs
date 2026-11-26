//! Pure LAN model: host identity values, the one-time-code pairing gate, the
//! discovered-host view and forward delivery transitions. No I/O, no clock:
//! callers pass `now` in seconds.
use crate::domain::Delivery;
use data_encoding::BASE32_NOPAD;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fmt,
    net::{IpAddr, SocketAddr},
};
use thiserror::Error;

/// Seconds a one-time code stays valid.
pub const CODE_LIFETIME: u64 = 120;
/// Wrong codes allowed per request.
pub const ATTEMPTS: u8 = 3;
/// Seconds an address waits after using up its attempts.
pub const COOLDOWN: u64 = 30;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum LanError {
    #[error("Expected a key fingerprint of 52 base32 characters")]
    InvalidFingerprint,
    #[error("Host name must be 1-63 letters, digits or '-', not starting or ending with '-'")]
    InvalidLabel,
    #[error("Expected a 6-digit pairing code such as 482-913")]
    InvalidCode,
    #[error("No bridge named {0} was found; run session-bridge discover")]
    UnknownHost(String),
    #[error("Several bridges are named {0}; use the key shown by session-bridge discover")]
    AmbiguousHost(String),
}

/// SHA-256 of a bridge's static Noise public key: the bridge's identity.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Fingerprint([u8; 32]);
impl Fingerprint {
    pub fn of_public_key(key: &[u8]) -> Self {
        Self(Sha256::digest(key).into())
    }
    pub fn parse(value: &str) -> Result<Self, LanError> {
        let bytes = BASE32_NOPAD
            .decode(value.trim().to_ascii_uppercase().as_bytes())
            .map_err(|_| LanError::InvalidFingerprint)?;
        Ok(Self(
            bytes.try_into().map_err(|_| LanError::InvalidFingerprint)?,
        ))
    }
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
    /// First eight characters, grouped for reading aloud: `6Q2M-4KXA`.
    pub fn short(&self) -> String {
        let text = self.to_string();
        format!("{}-{}", &text[..4], &text[4..8])
    }
    /// True when `prefix` (at least four characters, dashes ignored) starts the text form.
    pub fn matches_prefix(&self, prefix: &str) -> bool {
        let prefix: String = prefix
            .chars()
            .filter(|c| *c != '-')
            .collect::<String>()
            .to_ascii_uppercase();
        prefix.len() >= 4 && self.to_string().starts_with(&prefix)
    }
}
impl fmt::Display for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&BASE32_NOPAD.encode(&self.0))
    }
}
impl fmt::Debug for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Fingerprint({})", self.short())
    }
}
impl Serialize for Fingerprint {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}
impl<'de> Deserialize<'de> for Fingerprint {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::parse(&String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

/// The name a bridge announces: a DNS label.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct HostLabel(String);
impl HostLabel {
    pub fn parse(value: &str) -> Result<Self, LanError> {
        let valid = (1..=63).contains(&value.len())
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            && !value.starts_with('-')
            && !value.ends_with('-');
        if valid {
            Ok(Self(value.to_owned()))
        } else {
            Err(LanError::InvalidLabel)
        }
    }
    /// A valid label from any machine name: `.local` dropped, other characters
    /// replaced by single dashes, cut to 63 characters.
    pub fn from_machine_name(raw: &str) -> Self {
        let base = raw.strip_suffix(".local").unwrap_or(raw);
        let mut label = String::new();
        for c in base.chars() {
            if c.is_ascii_alphanumeric() {
                label.push(c);
            } else if !label.is_empty() && !label.ends_with('-') {
                label.push('-');
            }
        }
        label.truncate(63);
        let label = label.trim_end_matches('-');
        Self::parse(label).unwrap_or_else(|_| Self("session-bridge".into()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl TryFrom<String> for HostLabel {
    type Error = LanError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}
impl From<HostLabel> for String {
    fn from(value: HostLabel) -> Self {
        value.0
    }
}
impl fmt::Display for HostLabel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A six-digit one-time code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PairingCode(u32);
impl PairingCode {
    pub fn from_random(value: u32) -> Self {
        Self(value % 1_000_000)
    }
    pub fn parse(typed: &str) -> Result<Self, LanError> {
        let digits: String = typed
            .chars()
            .filter(|c| !c.is_whitespace() && *c != '-')
            .collect();
        if digits.len() != 6 || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return Err(LanError::InvalidCode);
        }
        digits.parse().map(Self).map_err(|_| LanError::InvalidCode)
    }
    /// The SPAKE2 password: the six digits without separator.
    pub fn password(&self) -> String {
        format!("{:06}", self.0)
    }
}
impl fmt::Display for PairingCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:03}-{:03}", self.0 / 1000, self.0 % 1000)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum GateError {
    #[error("Another laptop is pairing with this host; try again in two minutes")]
    Busy,
    #[error("Too many wrong codes from this address; try again in {seconds} s")]
    CoolingDown { seconds: u64 },
    #[error("No pairing request is open; start again with session-bridge pair <host>")]
    NoRequest,
    #[error("The pairing code expired; start again")]
    Expired,
    #[error("Too many wrong codes; start again")]
    Exhausted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Attempt {
    Paired,
    Wrong { attempts_left: u8 },
    Exhausted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum PairingOutcome {
    Paired { host: String, key: String },
    Failed { host: String, reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct IncomingRequest {
    label: HostLabel,
    fingerprint: Fingerprint,
    address: IpAddr,
    code: PairingCode,
    expires_at: u64,
    attempts_left: u8,
}

/// An incoming request as the user sees it on the chosen laptop.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IncomingView {
    pub host: String,
    pub key: String,
    pub address: IpAddr,
    pub code: String,
    pub expires_in: u64,
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairingView {
    pub incoming: Option<IncomingView>,
    pub last: Option<PairingOutcome>,
}

/// Incoming pairing state of one bridge: at most one open request, per-address
/// cooldown after too many wrong codes, and the last outcome for display.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PairingGate {
    pending: Option<IncomingRequest>,
    cooldowns: BTreeMap<IpAddr, u64>,
    last: Option<PairingOutcome>,
}
impl PairingGate {
    fn open(&self, now: u64) -> Option<&IncomingRequest> {
        self.pending.as_ref().filter(|p| p.expires_at > now)
    }
    pub fn request(
        &self,
        now: u64,
        label: HostLabel,
        fingerprint: Fingerprint,
        address: IpAddr,
        code: PairingCode,
    ) -> Result<Self, GateError> {
        if let Some(until) = self.cooldowns.get(&address).filter(|until| **until > now) {
            return Err(GateError::CoolingDown {
                seconds: until - now,
            });
        }
        if self.open(now).is_some_and(|p| p.fingerprint != fingerprint) {
            return Err(GateError::Busy);
        }
        let mut next = self.clone();
        next.cooldowns.retain(|_, until| *until > now);
        next.pending = Some(IncomingRequest {
            label,
            fingerprint,
            address,
            code,
            expires_at: now + CODE_LIFETIME,
            attempts_left: ATTEMPTS,
        });
        Ok(next)
    }
    /// The code of the open request from `fingerprint`.
    pub fn code(&self, fingerprint: &Fingerprint) -> Option<PairingCode> {
        self.pending
            .as_ref()
            .filter(|p| &p.fingerprint == fingerprint)
            .map(|p| p.code)
    }
    /// Records one SPAKE2 confirmation: `matched` is true when the key confirmation verified.
    pub fn attempt(
        &self,
        now: u64,
        fingerprint: &Fingerprint,
        matched: bool,
    ) -> Result<(Self, Attempt), GateError> {
        let pending = self
            .pending
            .as_ref()
            .filter(|p| &p.fingerprint == fingerprint)
            .ok_or(GateError::NoRequest)?;
        let mut next = self.clone();
        let host = pending.label.to_string();
        if pending.expires_at <= now {
            next.pending = None;
            next.last = Some(PairingOutcome::Failed {
                host,
                reason: GateError::Expired.to_string(),
            });
            return Err(GateError::Expired);
        }
        if matched {
            next.pending = None;
            next.last = Some(PairingOutcome::Paired {
                host,
                key: fingerprint.short(),
            });
            return Ok((next, Attempt::Paired));
        }
        let attempts_left = pending.attempts_left - 1;
        if attempts_left == 0 {
            next.pending = None;
            next.cooldowns.insert(pending.address, now + COOLDOWN);
            next.last = Some(PairingOutcome::Failed {
                host,
                reason: GateError::Exhausted.to_string(),
            });
            return Ok((next, Attempt::Exhausted));
        }
        if let Some(open) = next.pending.as_mut() {
            open.attempts_left = attempts_left;
        }
        Ok((next, Attempt::Wrong { attempts_left }))
    }
    /// Drops the request of `fingerprint` when its connection ends.
    pub fn abandon(&self, fingerprint: &Fingerprint) -> Self {
        let mut next = self.clone();
        if next
            .pending
            .as_ref()
            .is_some_and(|p| &p.fingerprint == fingerprint)
        {
            next.pending = None;
        }
        next
    }
    pub fn view(&self, now: u64) -> PairingView {
        PairingView {
            incoming: self.open(now).map(|p| IncomingView {
                host: p.label.to_string(),
                key: p.fingerprint.short(),
                address: p.address,
                code: p.code.to_string(),
                expires_in: p.expires_at - now,
            }),
            last: self.last.clone(),
        }
    }
}

/// A bridge announced on the network.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredHost {
    pub label: HostLabel,
    pub fingerprint: Fingerprint,
    pub addresses: Vec<SocketAddr>,
}
/// A bridge this one trusts, saved after pairing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairedPeer {
    pub label: HostLabel,
    pub fingerprint: Fingerprint,
    pub address: Option<SocketAddr>,
    pub paired_at: u64,
}
/// One row of `session-bridge discover`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostView {
    pub host: String,
    pub key: String,
    pub fingerprint: String,
    pub addresses: Vec<SocketAddr>,
    pub paired: bool,
    pub online: bool,
}

/// This bridge's network state as shown to the user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkView {
    pub enabled: bool,
    pub host: String,
    pub key: String,
    pub fingerprint: String,
    /// The listening port while networking is on.
    pub port: Option<u16>,
}
/// Everything the setup screens show: own state, other bridges, pairing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LanView {
    pub network: NetworkView,
    pub hosts: Vec<HostView>,
    pub pairing: PairingView,
}

/// IPv4 before IPv6, routable before link-local, without duplicates.
pub fn order_addresses(addresses: Vec<SocketAddr>) -> Vec<SocketAddr> {
    let rank = |a: &SocketAddr| match a.ip() {
        IpAddr::V4(v4) if v4.is_link_local() => 1,
        IpAddr::V4(v4) if v4.is_loopback() => 2,
        IpAddr::V4(_) => 0,
        IpAddr::V6(_) => 3,
    };
    let mut ordered: Vec<SocketAddr> = Vec::new();
    for address in addresses {
        if !ordered.contains(&address) {
            ordered.push(address);
        }
    }
    ordered.sort_by_key(rank);
    ordered
}

pub fn merge_hosts(
    own: &Fingerprint,
    discovered: &[DiscoveredHost],
    paired: &[PairedPeer],
) -> Vec<HostView> {
    let mut hosts: BTreeMap<Fingerprint, HostView> = BTreeMap::new();
    for host in discovered.iter().filter(|h| &h.fingerprint != own) {
        hosts.insert(
            host.fingerprint,
            HostView {
                host: host.label.to_string(),
                key: host.fingerprint.short(),
                fingerprint: host.fingerprint.to_string(),
                addresses: host.addresses.clone(),
                paired: false,
                online: true,
            },
        );
    }
    for peer in paired {
        let view = hosts.entry(peer.fingerprint).or_insert_with(|| HostView {
            host: peer.label.to_string(),
            key: peer.fingerprint.short(),
            fingerprint: peer.fingerprint.to_string(),
            addresses: Vec::new(),
            paired: true,
            online: false,
        });
        view.paired = true;
        view.addresses.extend(peer.address);
        view.addresses = order_addresses(std::mem::take(&mut view.addresses));
    }
    let mut views: Vec<HostView> = hosts.into_values().collect();
    views.sort_by(|a, b| (&a.host, &a.fingerprint).cmp(&(&b.host, &b.fingerprint)));
    views
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Peer {
        fingerprint: Fingerprint,
        addresses: Vec<SocketAddr>,
    },
    Address(SocketAddr),
}
/// Chooses a host by exact name, key prefix, or `ip:port`.
pub fn resolve_target(query: &str, hosts: &[HostView]) -> Result<Target, LanError> {
    if let Ok(address) = query.parse::<SocketAddr>() {
        return Ok(Target::Address(address));
    }
    let named: Vec<&HostView> = hosts.iter().filter(|h| h.host == query).collect();
    let matches = if named.is_empty() {
        hosts
            .iter()
            .filter(|h| Fingerprint::parse(&h.fingerprint).is_ok_and(|fp| fp.matches_prefix(query)))
            .collect()
    } else {
        named
    };
    match matches.as_slice() {
        [host] => Ok(Target::Peer {
            fingerprint: Fingerprint::parse(&host.fingerprint)?,
            addresses: host.addresses.clone(),
        }),
        [] => Err(LanError::UnknownHost(query.into())),
        _ => Err(LanError::AmbiguousHost(query.into())),
    }
}

/// True when another paired key already uses `label`.
pub fn label_conflict(paired: &[PairedPeer], label: &HostLabel, fingerprint: &Fingerprint) -> bool {
    paired
        .iter()
        .any(|p| &p.label == label && &p.fingerprint != fingerprint)
}

/// Result of handing a message to a paired bridge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForwardOutcome {
    /// Nothing was written: retry later.
    Unreachable { reason: String },
    /// The other bridge saved the message; `remote` is its delivery state.
    Accepted { host: String, remote: Delivery },
    /// The other bridge refused the message.
    Rejected { reason: String },
    /// Written, but no answer: the other bridge may have it.
    Interrupted { reason: String },
}
/// New state of a dispatching message after a forward attempt. `None` keeps
/// the current state, for example when a status update arrived first.
pub fn complete_forward(current: &Delivery, outcome: &ForwardOutcome) -> Option<Delivery> {
    if current != &Delivery::Dispatching {
        return None;
    }
    Some(match outcome {
        ForwardOutcome::Unreachable { reason } => Delivery::Pending {
            reason: reason.clone(),
        },
        ForwardOutcome::Accepted { host, remote } => apply_remote_status(current, remote)
            .unwrap_or(Delivery::Forwarded { host: host.clone() }),
        ForwardOutcome::Rejected { reason } => Delivery::Refused {
            reason: reason.clone(),
        },
        ForwardOutcome::Interrupted { reason } => Delivery::Uncertain {
            reason: format!("{reason}; inspect the other host before resending"),
        },
    })
}
/// New state after a status update from the bridge that owns delivery.
/// Pending and in-flight states there never reopen the message here.
pub fn apply_remote_status(current: &Delivery, remote: &Delivery) -> Option<Delivery> {
    let open = matches!(
        current,
        Delivery::Dispatching
            | Delivery::Forwarded { .. }
            | Delivery::Queued
            | Delivery::Held { .. }
    );
    let reportable = matches!(
        remote,
        Delivery::Queued
            | Delivery::Delivered
            | Delivery::Held { .. }
            | Delivery::Refused { .. }
            | Delivery::Uncertain { .. }
    );
    (open && reportable && current != remote).then(|| remote.clone())
}

#[cfg(test)]
#[path = "model_tests.rs"]
mod tests;
