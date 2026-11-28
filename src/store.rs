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
/// The message history file in the data folder (REQ-039).
pub const HISTORY: &str = "history.jsonl";

/// Each message's latest state from history lines (the last line of a
/// message wins), in first-seen order, and the 1-based numbers of the lines
/// that are not envelopes (a line cut short by a crash). Pure.
pub fn latest(text: &str) -> (Vec<Envelope>, Vec<usize>) {
    let mut order = Vec::new();
    let mut by_id = std::collections::HashMap::new();
    let mut unreadable = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Envelope>(line) {
            Ok(envelope) => {
                if by_id
                    .insert(envelope.message_id, envelope.clone())
                    .is_none()
                {
                    order.push(envelope.message_id);
                }
            }
            Err(_) => unreadable.push(index + 1),
        }
    }
    let messages = order
        .into_iter()
        .filter_map(|id| by_id.remove(&id))
        .collect();
    (messages, unreadable)
}

/// Whether the file's last byte is not a newline (a torn last line).
fn ends_torn(file: &mut fs::File) -> Result<bool> {
    use std::io::{Read, Seek, SeekFrom};
    if file.metadata()?.len() == 0 {
        return Ok(false);
    }
    file.seek(SeekFrom::End(-1))?;
    let mut last = [0u8; 1];
    file.read_exact(&mut last)?;
    Ok(last[0] != b'\n')
}

#[derive(Debug, Clone)]
pub struct Store {
    pub root: PathBuf,
}
impl Store {
    pub fn open(root: PathBuf) -> Result<Self> {
        ensure_private_dir(&root)?;
        ensure_private_dir(&root.join("routes"))?;
        Ok(Self { root })
    }
    /// Appends the message's current state to the history, durably.
    pub fn save(&self, message: &Envelope) -> Result<()> {
        let mut line = serde_json::to_vec(message)?;
        line.push(b'\n');
        let path = self.root.join(HISTORY);
        let mut file = platform::private_options()
            .create(true)
            .read(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("Opening {}", path.display()))?;
        // A line cut short by a crash has no newline: end it first, so this
        // state starts a line of its own.
        if ends_torn(&mut file)? {
            file.write_all(b"\n")?;
        }
        file.write_all(&line)?;
        file.sync_data()?;
        Ok(())
    }
    pub fn messages(&self) -> Result<Vec<Envelope>> {
        let path = self.root.join(HISTORY);
        if !path.exists() {
            return Ok(Vec::new());
        }
        platform::verify_user_file(&path)?;
        let (messages, unreadable) = latest(&fs::read_to_string(&path)?);
        for line in unreadable {
            eprintln!("Skipped unreadable line {line} of {}", path.display());
        }
        Ok(messages)
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
    fn envelope(delivery: Delivery) -> Envelope {
        Envelope {
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
            delivery,
            created_at: 1,
            scheduled: false,
        }
    }
    /* REQ-039: History is one JSONL file, appended per save; the last line of
    a message is its state. */
    #[test]
    fn history_appends_and_the_last_line_wins() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("data")).unwrap();
        let mut first = envelope(Delivery::Pending {
            reason: "offline".into(),
        });
        let other = envelope(Delivery::Queued);
        store.save(&first).unwrap();
        store.save(&other).unwrap();
        first.delivery = Delivery::Delivered;
        store.save(&first).unwrap();
        let text = fs::read_to_string(dir.path().join("data/history.jsonl")).unwrap();
        assert_eq!(text.lines().count(), 3);
        let messages = store.messages().unwrap();
        assert_eq!(messages.len(), 2);
        let found = messages
            .iter()
            .find(|m| m.message_id == first.message_id)
            .unwrap();
        assert_eq!(found.delivery, Delivery::Delivered);
        assert!(!dir.path().join("data/messages").exists());
    }
    /* REQ-039: A line cut short by a crash is skipped; the lines before it stay. */
    #[test]
    fn history_skips_a_torn_line() {
        let (messages, unreadable) = latest("{\"not\": \"an envelope\"}\n{\"message_id\": \"x");
        assert!(messages.is_empty());
        assert_eq!(unreadable, [1, 2]);
        let kept = envelope(Delivery::Queued);
        let text = format!("{}\n{{\"message_", serde_json::to_string(&kept).unwrap());
        let (messages, unreadable) = latest(&text);
        assert_eq!(messages.len(), 1);
        assert_eq!(unreadable, [2]);
    }
    /* REQ-039: The first save after a torn last line starts a line of its own,
    so the state it records is kept. */
    #[test]
    fn save_after_a_torn_line_starts_a_new_line() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("data")).unwrap();
        let mut message = envelope(Delivery::Pending {
            reason: "offline".into(),
        });
        store.save(&message).unwrap();
        let path = dir.path().join("data").join(HISTORY);
        let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(b"{\"message_id\":\"TORN").unwrap();
        drop(file);
        message.delivery = Delivery::Delivered;
        store.save(&message).unwrap();
        let messages = store.messages().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].delivery, Delivery::Delivered);
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
