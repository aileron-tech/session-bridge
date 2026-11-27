use crate::{
    domain::{Delivery, SessionId},
    platform,
};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Envelope {
    pub message_id: Uuid,
    pub sender: SessionId,
    pub recipient: SessionId,
    pub message: String,
    #[serde(default)]
    pub project: Option<String>,
    pub delivery: Delivery,
    pub created_at: u64,
    /// Posted by one of the user's schedules (REQ-025).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub scheduled: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Route {
    pub socket: PathBuf,
    pub recipient: SessionId,
}

pub use crate::platform::ensure_private_dir;
pub fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    write_bytes(path, &serde_json::to_vec_pretty(value)?)
}
/// Replaces `path` with `bytes` atomically, readable by the user only.
pub fn write_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().context("Missing parent directory")?;
    let temporary = parent.join(format!(".{}.tmp", Uuid::new_v4()));
    let mut file = platform::private_options()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::rename(&temporary, path)?;
    platform::sync_dir(parent)?;
    Ok(())
}
pub fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T> {
    platform::verify_user_file(path)?;
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}
#[derive(Debug, Clone)]
pub struct Store {
    pub root: PathBuf,
}
impl Store {
    pub fn open(root: PathBuf) -> Result<Self> {
        ensure_private_dir(&root)?;
        ensure_private_dir(&root.join("messages"))?;
        ensure_private_dir(&root.join("routes"))?;
        Ok(Self { root })
    }
    pub fn save(&self, message: &Envelope) -> Result<()> {
        write_json(
            &self
                .root
                .join("messages")
                .join(format!("{}.json", message.message_id)),
            message,
        )
    }
    pub fn messages(&self) -> Result<Vec<Envelope>> {
        self.read_directory("messages")
    }
    /// This laptop's user address (REQ-019), created on first use.
    pub fn user(&self) -> Result<SessionId> {
        let path = self.root.join("user.json");
        if path.exists() {
            let user: SessionId = read_json(&path)?;
            anyhow::ensure!(
                user.provider == crate::domain::Provider::User,
                "{} does not hold a user address",
                path.display()
            );
            return Ok(user);
        }
        let user = SessionId {
            provider: crate::domain::Provider::User,
            id: Uuid::new_v4(),
        };
        write_json(&path, &user)?;
        Ok(user)
    }
    pub fn routes(&self) -> Result<Vec<Route>> {
        self.read_directory("routes")
    }
    pub fn save_route(&self, route: &Route) -> Result<()> {
        let id = Uuid::new_v5(
            &Uuid::NAMESPACE_URL,
            route.socket.to_string_lossy().as_bytes(),
        );
        write_json(&self.root.join("routes").join(format!("{id}.json")), route)
    }
    fn read_directory<T: DeserializeOwned>(&self, name: &str) -> Result<Vec<T>> {
        let mut result = Vec::new();
        for entry in fs::read_dir(self.root.join(name))? {
            let path = entry?.path();
            if path.extension().is_some_and(|s| s == "json") {
                result
                    .push(read_json(&path).with_context(|| format!("Reading {}", path.display()))?);
            }
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    /* REQ-003: A queued envelope remains intact when the store is reopened. */
    #[test]
    fn pending_work_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("state");
        let store = Store::open(root.clone()).unwrap();
        let e = Envelope {
            message_id: Uuid::new_v4(),
            sender: SessionId {
                provider: crate::domain::Provider::Codex,
                id: Uuid::new_v4(),
            },
            recipient: SessionId {
                provider: crate::domain::Provider::Claude,
                id: Uuid::new_v4(),
            },
            message: "hello".into(),
            project: None,
            delivery: Delivery::Pending {
                reason: "offline".into(),
            },
            created_at: 1,
            scheduled: false,
        };
        store.save(&e).unwrap();
        drop(store);
        let messages = Store::open(root).unwrap().messages().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].message_id, e.message_id);
        assert_eq!(messages[0].delivery, e.delivery);
    }
    /* REQ-019: The user's address is created once and kept across restarts. */
    #[test]
    fn keeps_the_user_address() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("state");
        let first = Store::open(root.clone()).unwrap().user().unwrap();
        assert_eq!(first.provider, crate::domain::Provider::User);
        assert_eq!(Store::open(root).unwrap().user().unwrap(), first);
    }
    /* REQ-006: State directories must not redirect writes through a symlink. */
    #[cfg(unix)]
    #[test]
    fn refuses_symlinked_state_directory() {
        let dir = tempfile::tempdir().unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(dir.path(), &link).unwrap();
        assert!(Store::open(link).is_err());
    }
}
