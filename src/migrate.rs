//! The data folder's first start (REQ-039). The bridge keeps its config and
//! message history in `~/.session-bridge`; older versions kept everything in
//! the runtime folder. When the data folder does not exist yet, the config
//! files and folders are copied from the old folder and the history starts
//! with the messages still in flight, so none of them is lost. The old folder
//! is never changed. Infrastructure: files only.
use crate::{
    domain::Delivery,
    platform,
    store::{self, Envelope, Store},
};
use anyhow::{Context, Result};
use std::{fs, path::Path};

/// Config files carried over: an allowlist, so runtime files (socket, lock,
/// logs) and the old per-message files never move.
pub const CONFIG_FILES: &[&str] = &[
    "user.json",
    "channels.json",
    "schedules.json",
    "schedule_runs.json",
    "steered.json",
    "spawn.json",
    "roles.json",
    "remotes.json",
    "names.json",
    "sessions.json",
    "network.json",
    "phones.json",
    "phone-ca.json",
    "phone-identity.json",
];
/// Config folders carried over with everything in them.
pub const CONFIG_DIRS: &[&str] = &["roles", "network", "peers", "routes"];

/// A message the bridge still works on: retried, tracked or reported back.
/// Pure.
pub fn unfinished(delivery: &Delivery) -> bool {
    matches!(
        delivery,
        Delivery::Pending { .. }
            | Delivery::Dispatching
            | Delivery::Queued
            | Delivery::Forwarded { .. }
            | Delivery::Held { .. }
    )
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The data folder exists: nothing was done.
    Present,
    /// No old folder: the data folder starts empty.
    Fresh,
    /// Config files copied and unfinished messages carried into the history.
    Copied { files: usize, unfinished: usize },
}

/// Prepares `data` from the `old` folder on the first start. The copy is
/// built beside `data` and renamed into place, so a crash leaves no half
/// data folder; the next start builds it again.
pub fn prepare(old: &Path, data: &Path) -> Result<Outcome> {
    if fs::symlink_metadata(data).is_ok() {
        return Ok(Outcome::Present);
    }
    if !old.is_dir() || !has_data(old) {
        return Ok(Outcome::Fresh);
    }
    let name = data
        .file_name()
        .context("The data folder has no name")?
        .to_string_lossy();
    let staging = data.with_file_name(format!("{name}.migrating"));
    if fs::symlink_metadata(&staging).is_ok() {
        // A copy an earlier start did not finish.
        fs::remove_dir_all(&staging).with_context(|| format!("Removing {}", staging.display()))?;
    }
    platform::ensure_private_dir(&staging)?;
    let mut files = 0;
    for name in CONFIG_FILES {
        let from = old.join(name);
        match fs::symlink_metadata(&from) {
            Ok(m) if m.is_file() => {
                copy_file(&from, &staging.join(name))?;
                files += 1;
            }
            Ok(_) => eprintln!("Not copied (not a file): {}", from.display()),
            Err(_) => {}
        }
    }
    for name in CONFIG_DIRS {
        let from = old.join(name);
        if fs::symlink_metadata(&from).is_ok_and(|m| m.is_dir()) {
            files += copy_dir(&from, &staging.join(name))?;
        }
    }
    let mut carried = old_messages(&old.join("messages"))?
        .into_iter()
        .filter(|m| unfinished(&m.delivery))
        .collect::<Vec<_>>();
    carried.sort_by_key(|m| m.created_at);
    let history = Store::open(staging.clone())?;
    for message in &carried {
        history.save(message)?;
    }
    fs::rename(&staging, data)
        .with_context(|| format!("Moving {} to {}", staging.display(), data.display()))?;
    if let Some(parent) = data.parent() {
        platform::sync_dir(parent)?;
    }
    Ok(Outcome::Copied {
        files,
        unfinished: carried.len(),
    })
}

/// Whether the old folder holds anything to carry: a config entry or old
/// messages (a new install's runtime folder holds neither).
fn has_data(old: &Path) -> bool {
    CONFIG_FILES
        .iter()
        .chain(CONFIG_DIRS)
        .chain(&["messages"])
        .any(|name| fs::symlink_metadata(old.join(name)).is_ok())
}

fn copy_file(from: &Path, to: &Path) -> Result<()> {
    fs::copy(from, to).with_context(|| format!("Copying {}", from.display()))?;
    platform::set_mode(to, 0o600)?;
    Ok(())
}

/// Copies regular files and folders; links are never followed (logged).
fn copy_dir(from: &Path, to: &Path) -> Result<usize> {
    platform::ensure_private_dir(to)?;
    let mut files = 0;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let target = to.join(entry.file_name());
        if kind.is_file() {
            copy_file(&entry.path(), &target)?;
            files += 1;
        } else if kind.is_dir() {
            files += copy_dir(&entry.path(), &target)?;
        } else {
            eprintln!(
                "Not copied (not a file or folder): {}",
                entry.path().display()
            );
        }
    }
    Ok(files)
}

/// The old per-message files; one that cannot be read is logged and left.
fn old_messages(dir: &Path) -> Result<Vec<Envelope>> {
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut messages = Vec::new();
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        match store::read_json::<Envelope>(&path) {
            Ok(message) => messages.push(message),
            Err(e) => eprintln!("Old message not carried over, {}: {e:#}", path.display()),
        }
    }
    Ok(messages)
}

#[cfg(test)]
#[path = "migrate_tests.rs"]
mod tests;
