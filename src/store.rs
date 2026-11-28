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

/// The history as read from its lines (REQ-039).
#[derive(Debug, Clone)]
pub struct History {
    /// Each message's latest state (its last line), in first-seen order.
    pub messages: Vec<Envelope>,
    /// The envelope lines read.
    pub lines: usize,
    /// How the start makes the file end with a whole line, when it does not.
    pub repair: Option<Repair>,
}
/// The end of a history a crash or power loss left: the file is cut at
/// `offset` and, with `newline`, its whole last line ended with a newline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Repair {
    pub offset: usize,
    pub newline: bool,
    /// The 1-based number of a torn last line that is cut off.
    pub torn: Option<usize>,
}
/// A history line that is not an envelope and not a torn last line: the
/// start fails rather than fall back to an older state of a message.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{HISTORY} line {line}: {reason}")]
pub struct Unreadable {
    pub line: usize,
    pub reason: String,
}

/// Reads history lines: the last line of a message wins. Only the last
/// segment (the last line, followed by nothing but blank or NUL bytes) may be
/// damaged: NUL
/// bytes a power loss left at its end are dropped, and when what remains is
/// empty or JSON that ends early, it is a torn line, cut off. Any other line
/// that is not an envelope is an error. Pure.
pub fn latest(bytes: &[u8]) -> Result<History, Unreadable> {
    let mut order = Vec::new();
    let mut by_id = std::collections::HashMap::new();
    let mut lines = 0;
    let mut repair = None;
    let mut offset = 0;
    let mut segments = bytes.split_inclusive(|b| *b == b'\n').enumerate();
    while let Some((index, segment)) = segments.next() {
        let start = offset;
        offset += segment.len();
        // The last segment: only blank or NUL bytes follow it.
        let last = segments
            .clone()
            .all(|(_, rest)| rest.iter().all(|b| b.is_ascii_whitespace() || *b == 0));
        let line = if last {
            let end = segment.iter().rposition(|b| *b != 0).map_or(0, |i| i + 1);
            &segment[..end]
        } else {
            segment
        };
        let padded = line.len() < segment.len();
        let torn = Repair {
            offset: start,
            newline: false,
            torn: Some(index + 1),
        };
        if line.trim_ascii().is_empty() {
            if padded {
                repair = Some(torn);
            }
            continue;
        }
        match serde_json::from_slice::<Envelope>(line) {
            Ok(envelope) => {
                lines += 1;
                if by_id
                    .insert(envelope.message_id, envelope.clone())
                    .is_none()
                {
                    order.push(envelope.message_id);
                }
                if last && (padded || !line.ends_with(b"\n")) {
                    repair = Some(Repair {
                        offset: start + line.len(),
                        newline: !line.ends_with(b"\n"),
                        torn: None,
                    });
                }
            }
            Err(e) if last && e.is_eof() => repair = Some(torn),
            Err(e) => {
                return Err(Unreadable {
                    line: index + 1,
                    reason: e.to_string(),
                });
            }
        }
    }
    let messages = order
        .into_iter()
        .filter_map(|id| by_id.remove(&id))
        .collect();
    Ok(History {
        messages,
        lines,
        repair,
    })
}

/// Whether the history is worth rewriting with only the latest states: it
/// holds more than twice as many lines (REQ-039). Pure.
pub fn should_compact(lines: usize, latest: usize) -> bool {
    lines > 2 * latest
}

/// What the start did to the history before any save (REQ-039).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tidied {
    Nothing,
    /// The damaged end was cut off, or the whole last line ended.
    Repaired(Repair),
    /// Rewritten with the latest state of each message, from `lines` lines;
    /// a torn last line (its number) is dropped with them.
    Compacted {
        lines: usize,
        messages: usize,
        torn: Option<usize>,
    },
}

/// Replaces the history in `root` with `messages`, one line each, through
/// `history.jsonl.tmp` (readable by the user only), synced and renamed.
pub fn replace_history(root: &Path, messages: &[Envelope]) -> Result<()> {
    let mut bytes = Vec::new();
    for message in messages {
        serde_json::to_writer(&mut bytes, message)?;
        bytes.push(b'\n');
    }
    let temporary = root.join(format!("{HISTORY}.tmp"));
    match fs::remove_file(&temporary) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            return Err(e).with_context(|| format!("Removing {}", temporary.display()));
        }
        _ => {}
    }
    let mut file = platform::private_options()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .with_context(|| format!("Creating {}", temporary.display()))?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&temporary, root.join(HISTORY))
        .with_context(|| format!("Moving {} into place", temporary.display()))?;
    platform::sync_dir(root)?;
    Ok(())
}

/// Cuts `path` to `len` bytes and syncs it, through a handle opened for
/// writing (an append-only handle cannot cut on Windows).
fn cut(path: &Path, len: u64) -> std::io::Result<()> {
    let file = fs::OpenOptions::new().write(true).open(path)?;
    file.set_len(len)?;
    file.sync_all()
}

#[derive(Debug, Clone)]
pub struct Store {
    pub root: PathBuf,
    /// Why saves are refused until a restart: a failed write could not be
    /// cut off, so the file may end with a torn line (REQ-039).
    poisoned: std::sync::Arc<std::sync::OnceLock<String>>,
}
impl Store {
    pub fn open(root: PathBuf) -> Result<Self> {
        ensure_private_dir(&root)?;
        ensure_private_dir(&root.join("routes"))?;
        Ok(Self {
            root,
            poisoned: Default::default(),
        })
    }
    /// Appends the message's current state to the history, durably. A write
    /// that fails is cut off again; when that fails too, every later save is
    /// refused until a restart, whose start repairs the end of the file.
    pub fn save(&self, message: &Envelope) -> Result<()> {
        if let Some(reason) = self.poisoned.get() {
            anyhow::bail!("Saving is stopped until the bridge restarts: {reason}");
        }
        let mut line = serde_json::to_vec(message)?;
        line.push(b'\n');
        let path = self.root.join(HISTORY);
        let created = fs::symlink_metadata(&path).is_err();
        let mut file = platform::private_options()
            .create(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("Opening {}", path.display()))?;
        let before = file.metadata()?.len();
        if let Err(e) = file.write_all(&line).and_then(|()| file.sync_data()) {
            drop(file);
            self.undo_write(&path, before);
            return Err(e).with_context(|| format!("Saving to {}", path.display()));
        }
        if created {
            platform::sync_dir(&self.root)?;
        }
        Ok(())
    }
    /// Cuts a failed write off `path` back to `before` bytes, or stops saving.
    fn undo_write(&self, path: &Path, before: u64) {
        if let Err(e) = cut(path, before) {
            let reason = format!(
                "a failed write could not be cut from {}: {e}",
                path.display()
            );
            eprintln!("{reason}");
            let _ = self.poisoned.set(reason);
        }
    }
    /// The history, read; an unreadable line other than a torn last one is
    /// an error (REQ-039).
    fn history(&self) -> Result<Option<History>> {
        let path = self.root.join(HISTORY);
        if !path.exists() {
            return Ok(None);
        }
        platform::verify_user_file(&path)?;
        Ok(Some(latest(&fs::read(&path)?)?))
    }
    pub fn messages(&self) -> Result<Vec<Envelope>> {
        Ok(self.history()?.map(|h| h.messages).unwrap_or_default())
    }
    /// On start, under the daemon lock and before any save (REQ-039): makes
    /// the file end with a whole line (cuts a torn or NUL-padded end off, ends
    /// an unterminated last line), and rewrites a history that holds more
    /// than twice the lines of its latest states with only those.
    pub fn tidy_history(&self) -> Result<Tidied> {
        let Some(history) = self.history()? else {
            return Ok(Tidied::Nothing);
        };
        if should_compact(history.lines, history.messages.len()) {
            replace_history(&self.root, &history.messages)?;
            return Ok(Tidied::Compacted {
                lines: history.lines,
                messages: history.messages.len(),
                torn: history.repair.and_then(|r| r.torn),
            });
        }
        let Some(repair) = history.repair else {
            return Ok(Tidied::Nothing);
        };
        let path = self.root.join(HISTORY);
        cut(&path, repair.offset as u64).with_context(|| format!("Cutting {}", path.display()))?;
        if repair.newline {
            let mut file = fs::OpenOptions::new().append(true).open(&path)?;
            file.write_all(b"\n")?;
            file.sync_all()?;
        }
        Ok(Tidied::Repaired(repair))
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
    fn line(e: &Envelope) -> String {
        serde_json::to_string(e).unwrap()
    }
    /* REQ-039: Only the last segment may be damaged: JSON ending early, or NUL
    bytes a power loss left (alone or after a partial line), is a torn line,
    cut from where it starts; the lines before it stay. */
    #[test]
    fn history_skips_only_a_torn_last_line() {
        let kept = envelope(Delivery::Queued);
        let first = format!("{}\n", line(&kept));
        let torn = Some(Repair {
            offset: first.len(),
            newline: false,
            torn: Some(2),
        });
        let cut_char = {
            let mut bytes = format!("{first}{{\"message\":\"caf\u{e9}").into_bytes();
            bytes.pop();
            bytes
        };
        for bytes in [
            format!("{first}{{\"message_id\": \"x").into_bytes(),
            cut_char,
            format!("{first}{{\n\n").into_bytes(),
            format!("{first}{{\"message_id\": \"x\0\0\0").into_bytes(),
            format!("{first}\0\0\0").into_bytes(),
        ] {
            let history = latest(&bytes).unwrap();
            let text = String::from_utf8_lossy(&bytes);
            assert_eq!(history.messages.len(), 1, "{text}");
            assert_eq!(history.lines, 1);
            assert_eq!(history.repair, torn, "{text}");
        }
        let whole = latest(first.as_bytes()).unwrap();
        assert_eq!(whole.repair, None);
        assert_eq!(latest(b"").unwrap().lines, 0);
    }
    /* REQ-039: A whole last line without its newline, or followed by NUL
    bytes, is kept; the start cuts the NULs and ends the line. */
    #[test]
    fn history_keeps_a_whole_unterminated_last_line() {
        let a = line(&envelope(Delivery::Queued));
        let b = line(&envelope(Delivery::Delivered));
        let ended = Some(Repair {
            offset: a.len() + 1 + b.len(),
            newline: true,
            torn: None,
        });
        for text in [format!("{a}\n{b}"), format!("{a}\n{b}\0\0")] {
            let history = latest(text.as_bytes()).unwrap();
            assert_eq!(history.lines, 2);
            assert_eq!(history.repair, ended, "{text:?}");
        }
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("data")).unwrap();
        let path = dir.path().join("data").join(HISTORY);
        fs::write(&path, format!("{a}\n{b}\0\0\0")).unwrap();
        assert!(matches!(store.tidy_history().unwrap(), Tidied::Repaired(r) if r.newline));
        assert_eq!(fs::read_to_string(&path).unwrap(), format!("{a}\n{b}\n"));
        assert_eq!(store.tidy_history().unwrap(), Tidied::Nothing);
    }
    /* REQ-039: Any other unreadable line (not an envelope, NUL bytes before
    the last segment, or cut short but followed by more lines) fails the
    start, naming the line, so no message falls back to an older state and is
    resent. */
    #[test]
    fn history_fails_closed_on_other_unreadable_lines() {
        let a = line(&envelope(Delivery::Queued));
        let b = line(&envelope(Delivery::Delivered));
        for (text, at) in [
            (format!("{a}\n{{\"not\": \"an envelope\"}}\n{b}\n"), 2),
            (format!("{a}\n{{\"not\": \"an envelope\"}}"), 2),
            (format!("{a}\n{{\"message_id\": \"x\n{b}\n"), 2),
            (format!("{a}\n\0\0\0\n{b}\n"), 2),
            (format!("{a}\0\0\n{b}\n"), 1),
            (format!("garbage\n{a}\n"), 1),
        ] {
            let error = latest(text.as_bytes()).unwrap_err();
            assert_eq!(error.line, at, "{text:?}");
            assert!(
                error
                    .to_string()
                    .starts_with(&format!("history.jsonl line {at}: ")),
                "{error}"
            );
        }
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("data")).unwrap();
        fs::write(
            dir.path().join("data").join(HISTORY),
            format!("{a}\n[1]\n{b}\n"),
        )
        .unwrap();
        let error = store.messages().unwrap_err();
        assert!(
            format!("{error:#}").contains("history.jsonl line 2"),
            "{error:#}"
        );
        assert!(store.tidy_history().is_err());
    }
    /* REQ-039: The start cuts a torn last line off before any save, so the
    next state is a whole line and the history reads after every restart. */
    #[test]
    fn start_cuts_a_torn_last_line_before_saving() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("data")).unwrap();
        let mut message = envelope(Delivery::Pending {
            reason: "offline".into(),
        });
        store.save(&message).unwrap();
        let path = dir.path().join("data").join(HISTORY);
        let whole = fs::read(&path).unwrap();
        let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(b"{\"message_id\":\"TORN\0\0").unwrap();
        drop(file);
        assert_eq!(
            store.tidy_history().unwrap(),
            Tidied::Repaired(Repair {
                offset: whole.len(),
                newline: false,
                torn: Some(2)
            })
        );
        assert_eq!(fs::read(&path).unwrap(), whole);
        message.delivery = Delivery::Delivered;
        store.save(&message).unwrap();
        let messages = store.messages().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].delivery, Delivery::Delivered);
        assert_eq!(store.tidy_history().unwrap(), Tidied::Nothing);
    }
    /* REQ-039: A failed save is cut back through a handle opened for writing;
    when the cut fails too, every later save is refused until a restart, so
    no torn line is buried under a new one. */
    #[test]
    fn a_failed_save_is_cut_or_stops_saving() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("data");
        let store = Store::open(root.clone()).unwrap();
        let message = envelope(Delivery::Queued);
        store.save(&message).unwrap();
        let path = root.join(HISTORY);
        let whole = fs::read(&path).unwrap();
        let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(b"{\"message_id\":\"half").unwrap();
        drop(file);
        store.undo_write(&path, whole.len() as u64);
        assert_eq!(fs::read(&path).unwrap(), whole);
        store.save(&message).unwrap();

        store.undo_write(&root.join("missing.jsonl"), 0);
        let refused = store.save(&message).unwrap_err().to_string();
        assert!(refused.contains("until the bridge restarts"), "{refused}");
        assert!(store.clone().save(&message).is_err(), "a clone saved");
        assert_eq!(fs::read(&path).unwrap().len(), 2 * whole.len());
        let restarted = Store::open(root).unwrap();
        restarted.save(&message).unwrap();
        assert_eq!(restarted.messages().unwrap().len(), 1);
    }
    /* REQ-039: The start compacts a history with more than twice the lines of
    its latest states: each message keeps its latest state, in first-seen
    order, in a file readable by the user only. */
    #[test]
    fn compaction_keeps_the_latest_states_and_their_order() {
        assert!(!should_compact(0, 0));
        assert!(!should_compact(4, 2));
        assert!(should_compact(5, 2));
        assert!(should_compact(1, 0));
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("data");
        let store = Store::open(root.clone()).unwrap();
        let mut a = envelope(Delivery::Pending {
            reason: "offline".into(),
        });
        let mut b = envelope(Delivery::Queued);
        store.save(&a).unwrap();
        store.save(&b).unwrap();
        a.delivery = Delivery::Queued;
        store.save(&a).unwrap();
        b.delivery = Delivery::Delivered;
        store.save(&b).unwrap();
        assert_eq!(store.tidy_history().unwrap(), Tidied::Nothing);
        a.delivery = Delivery::Uncertain { reason: "x".into() };
        store.save(&a).unwrap();
        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(root.join(HISTORY))
            .unwrap();
        file.write_all(b"{\"message_id\":").unwrap();
        drop(file);
        assert_eq!(
            store.tidy_history().unwrap(),
            Tidied::Compacted {
                lines: 5,
                messages: 2,
                torn: Some(6)
            }
        );
        let text = fs::read_to_string(root.join(HISTORY)).unwrap();
        assert_eq!(text, format!("{}\n{}\n", line(&a), line(&b)));
        assert!(!root.join(format!("{HISTORY}.tmp")).exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(root.join(HISTORY))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let messages = store.messages().unwrap();
        assert_eq!(messages[0].message_id, a.message_id);
        assert_eq!(messages[0].delivery, a.delivery);
        assert_eq!(messages[1].delivery, Delivery::Delivered);
        assert_eq!(store.tidy_history().unwrap(), Tidied::Nothing);
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
