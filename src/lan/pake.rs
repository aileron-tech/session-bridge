//! SPAKE2 over the one-time code, bound to both bridges' key fingerprints,
//! with HMAC-SHA256 key confirmation. An attacker who relays or replaces the
//! connection gets one online guess per attempt and learns nothing offline.
use super::model::{Fingerprint, PairingCode};
use anyhow::Result;
use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;
use spake2::{Ed25519Group, Identity, Password, Spake2};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// The laptop that chose the host and types the code.
    Initiator,
    /// The laptop that shows the code.
    Responder,
}

pub struct PakeState(Spake2<Ed25519Group>);
pub struct PakeKey(Vec<u8>);

/// Starts the exchange; the returned bytes go to the other side.
pub fn start(
    role: Role,
    code: &PairingCode,
    initiator: &Fingerprint,
    responder: &Fingerprint,
) -> (PakeState, Vec<u8>) {
    let password = Password::new(code.password());
    let (a, b) = (
        Identity::new(initiator.as_bytes()),
        Identity::new(responder.as_bytes()),
    );
    let (state, message) = match role {
        Role::Initiator => Spake2::<Ed25519Group>::start_a(&password, &a, &b),
        Role::Responder => Spake2::<Ed25519Group>::start_b(&password, &a, &b),
    };
    (PakeState(state), message)
}
impl PakeState {
    pub fn finish(self, message: &[u8]) -> Result<PakeKey> {
        self.0
            .finish(message)
            .map(PakeKey)
            .map_err(|e| anyhow::anyhow!("Invalid pairing message: {e:?}"))
    }
}

fn mac(
    key: &PakeKey,
    role: Role,
    initiator: &Fingerprint,
    responder: &Fingerprint,
) -> Hmac<Sha256> {
    let mut mac =
        <Hmac<Sha256> as KeyInit>::new_from_slice(&key.0).expect("HMAC accepts keys of any length");
    mac.update(b"session-bridge pair v1 ");
    mac.update(match role {
        Role::Initiator => b"initiator",
        Role::Responder => b"responder",
    });
    mac.update(initiator.as_bytes());
    mac.update(responder.as_bytes());
    mac
}
/// The confirmation `role` sends to prove it derived the same key.
pub fn confirmation(
    key: &PakeKey,
    role: Role,
    initiator: &Fingerprint,
    responder: &Fingerprint,
) -> Vec<u8> {
    mac(key, role, initiator, responder)
        .finalize()
        .into_bytes()
        .to_vec()
}
/// Checks the confirmation from `role` in constant time.
pub fn verify(
    key: &PakeKey,
    role: Role,
    initiator: &Fingerprint,
    responder: &Fingerprint,
    received: &[u8],
) -> bool {
    mac(key, role, initiator, responder)
        .verify_slice(received)
        .is_ok()
}
