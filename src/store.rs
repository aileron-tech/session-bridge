use crate::domain::{Delivery, SessionId};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
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
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Route {
    pub socket: PathBuf,
    pub recipient: SessionId,
}

pub fn ensure_private_dir(path: &Path) -> Result<()> {
    if !path.exists() {
        fs::DirBuilder::new().recursive(true).create(path)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    let meta = fs::symlink_metadata(path)?;
    anyhow::ensure!(
        meta.is_dir()
            && !meta.file_type().is_symlink()
            && meta.uid() == unsafe { libc::geteuid() }
            && meta.mode() & 0o077 == 0,
        "Not a private user-owned directory: {}",
        path.display()
    );
    Ok(())
}
pub fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let parent = path.parent().context("Missing parent directory")?;
    let temporary = parent.join(format!(".{}.tmp", Uuid::new_v4()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)?;
    file.write_all(&serde_json::to_vec_pretty(value)?)?;
    file.sync_all()?;
    fs::rename(&temporary, path)?;
    File::open(parent)?.sync_all()?;
    Ok(())
}
pub fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let meta = fs::symlink_metadata(path)?;
    anyhow::ensure!(
        meta.is_file()
            && !meta.file_type().is_symlink()
            && meta.uid() == unsafe { libc::geteuid() },
        "Not a user-owned regular file: {}",
        path.display()
    );
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
        };
        store.save(&e).unwrap();
        drop(store);
        let messages = Store::open(root).unwrap().messages().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].message_id, e.message_id);
        assert_eq!(messages[0].delivery, e.delivery);
    }
    /* REQ-006: State directories must not redirect writes through a symlink. */
    #[test]
    fn refuses_symlinked_state_directory() {
        let dir = tempfile::tempdir().unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(dir.path(), &link).unwrap();
        assert!(Store::open(link).is_err());
    }
}
