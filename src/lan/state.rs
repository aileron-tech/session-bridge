//! Durable LAN state in the bridge's private state directory:
//! `network.json` (switch, port, name), `network/identity.json` (static key),
//! and one `peers/<fingerprint>.json` per paired bridge.
use super::{
    channel::StaticKey,
    model::{Fingerprint, HostLabel, PairedPeer},
};
use crate::store::{ensure_private_dir, read_json, write_json};
use anyhow::{Context, Result};
use data_encoding::BASE64;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Port used when the user does not choose one.
pub const DEFAULT_PORT: u16 = 47820;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkSettings {
    pub enabled: bool,
    pub port: u16,
    pub label: HostLabel,
}

#[derive(Serialize, Deserialize)]
struct KeyFile {
    private: String,
    public: String,
}

#[derive(Debug, Clone)]
pub struct LanStore {
    root: PathBuf,
}
impl LanStore {
    pub fn open(state: &Path) -> Result<Self> {
        ensure_private_dir(state)?;
        ensure_private_dir(&state.join("network"))?;
        ensure_private_dir(&state.join("peers"))?;
        Ok(Self {
            root: state.to_owned(),
        })
    }
    /// The bridge key, created on first use.
    pub fn key(&self) -> Result<StaticKey> {
        let path = self.root.join("network").join("identity.json");
        if path.exists() {
            let file: KeyFile = read_json(&path)?;
            return StaticKey::from_parts(
                BASE64.decode(file.private.as_bytes())?,
                BASE64.decode(file.public.as_bytes())?,
            )
            .context("Corrupt bridge key");
        }
        let key = StaticKey::generate()?;
        write_json(
            &path,
            &KeyFile {
                private: BASE64.encode(key.private()),
                public: BASE64.encode(key.public()),
            },
        )?;
        Ok(key)
    }
    pub fn settings(&self) -> Result<Option<NetworkSettings>> {
        let path = self.root.join("network.json");
        if !path.exists() {
            return Ok(None);
        }
        read_json(&path).map(Some)
    }
    pub fn save_settings(&self, settings: &NetworkSettings) -> Result<()> {
        write_json(&self.root.join("network.json"), settings)
    }
    pub fn peers(&self) -> Result<Vec<PairedPeer>> {
        let mut peers = Vec::new();
        for entry in std::fs::read_dir(self.root.join("peers"))? {
            let path = entry?.path();
            if path.extension().is_some_and(|e| e == "json") {
                peers
                    .push(read_json(&path).with_context(|| format!("Reading {}", path.display()))?);
            }
        }
        peers.sort_by(|a: &PairedPeer, b| a.label.cmp(&b.label));
        Ok(peers)
    }
    pub fn peer(&self, fingerprint: &Fingerprint) -> Result<Option<PairedPeer>> {
        Ok(self
            .peers()?
            .into_iter()
            .find(|p| &p.fingerprint == fingerprint))
    }
    pub fn save_peer(&self, peer: &PairedPeer) -> Result<()> {
        write_json(&self.peer_path(&peer.fingerprint), peer)
    }
    /// Forgets a paired bridge; true when it was paired.
    pub fn remove_peer(&self, fingerprint: &Fingerprint) -> Result<bool> {
        match std::fs::remove_file(self.peer_path(fingerprint)) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e.into()),
        }
    }
    fn peer_path(&self, fingerprint: &Fingerprint) -> PathBuf {
        self.root.join("peers").join(format!("{fingerprint}.json"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /* REQ-017: The key, the network switch and paired bridges survive a reopen. */
    #[test]
    fn state_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("state");
        let store = LanStore::open(&root).unwrap();
        assert_eq!(store.settings().unwrap(), None);
        let key = store.key().unwrap();
        let settings = NetworkSettings {
            enabled: true,
            port: 5,
            label: HostLabel::parse("laptop-a").unwrap(),
        };
        store.save_settings(&settings).unwrap();
        let peer = PairedPeer {
            label: HostLabel::parse("laptop-b").unwrap(),
            fingerprint: Fingerprint::of_public_key(b"b"),
            address: Some("192.168.1.2:47820".parse().unwrap()),
            paired_at: 7,
        };
        store.save_peer(&peer).unwrap();
        let reopened = LanStore::open(&root).unwrap();
        assert_eq!(reopened.key().unwrap().fingerprint(), key.fingerprint());
        assert_eq!(reopened.settings().unwrap(), Some(settings));
        assert_eq!(reopened.peers().unwrap(), std::slice::from_ref(&peer));
        assert_eq!(
            reopened.peer(&peer.fingerprint).unwrap(),
            Some(peer.clone())
        );
        assert!(reopened.remove_peer(&peer.fingerprint).unwrap());
        assert!(!reopened.remove_peer(&peer.fingerprint).unwrap());
        assert!(reopened.peers().unwrap().is_empty());
    }

    /* REQ-017: A damaged key file is reported, never replaced silently. */
    #[test]
    fn corrupt_key_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("state");
        let store = LanStore::open(&root).unwrap();
        write_json(
            &root.join("network").join("identity.json"),
            &KeyFile {
                private: BASE64.encode(b"short"),
                public: BASE64.encode(b"short"),
            },
        )
        .unwrap();
        assert!(store.key().is_err());
    }
}
