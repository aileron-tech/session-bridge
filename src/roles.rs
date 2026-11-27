//! Roles (REQ-026): named instruction prompts the user assigns to sessions.
//! Assigning sends the prompt to the session once; the session then answers
//! to `@role` in its channels. Kept by session name, like the channel
//! default, so an assignment survives the session restarting. Each role is
//! a Markdown file in the roles folder (REQ-037), `<name>.md` holding the
//! prompt as is; this module decides what a file means. Pure.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::time::SystemTime;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Role {
    pub name: String,
    pub prompt: String,
}

/// Every role and which session (by name) holds which role.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoleBook {
    /// From the roles folder; `roles.json` no longer keeps them, and one
    /// that still does is migrated to files on start.
    #[serde(default, skip_serializing)]
    pub roles: Vec<Role>,
    /// What the roles folder held when last read.
    #[serde(skip)]
    pub files: FolderView,
    /// Session name to role name.
    #[serde(default)]
    pub assignments: BTreeMap<String, String>,
    /// Session ids the user gave the root role (REQ-028): root's authority
    /// follows the session, not its name, which a session can change.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub root_sessions: BTreeSet<String>,
    /// Preset roles already offered: one the user deleted stays deleted.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub presets: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RoleError {
    #[error(
        "A role name uses letters, digits, '.', '_' or '-', does not end in '.' or '-', and is not 'channel'"
    )]
    Name,
    #[error("A role prompt must be nonempty, at most 16000 bytes, and contain no NUL")]
    Prompt,
    #[error("A role named {0} already exists")]
    Exists(String),
    #[error("No role named {0}")]
    Unknown(String),
    #[error("The root role is built in: its instructions can change, not its name")]
    Builtin,
    #[error(
        "The file of the role {0} changed on disk since this page read it: open the role again, then save"
    )]
    Changed(String),
}

/// Why a file in the roles folder is not a role.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum FileProblem {
    #[error("not a regular file")]
    NotRegular,
    #[error("a symbolic link (the bridge does not follow links)")]
    Symlink,
    #[error(
        "the name is not a role name: letters, digits, '.', '_' or '-', not ending in '.' or '-', not 'channel'"
    )]
    Name,
    #[error("empty, over 16000 bytes, or containing NUL")]
    Prompt,
    #[error("not UTF-8 text")]
    NotUtf8,
    #[error("the same role name as {0}")]
    Duplicate(String),
    #[error("could not be read: {0}")]
    Unreadable(String),
}

/// A file of the roles folder that is not loaded, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotLoaded {
    pub file: String,
    pub reason: String,
}

/// The roles folder as last read: each loaded role's content version (by
/// role name) and the files that are not roles.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FolderView {
    pub versions: BTreeMap<String, String>,
    pub not_loaded: Vec<NotLoaded>,
}

/// What a directory listing says about a file, without reading it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Regular,
    Symlink,
    Other,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stamp {
    pub kind: FileKind,
    pub len: u64,
    pub modified: Option<SystemTime>,
}
/// The folder's role files by file name.
pub type Listing = BTreeMap<String, Stamp>;

/// The file a role is kept in.
pub fn file_name(role: &str) -> String {
    format!("{role}.md")
}

/// The role name a folder entry is for; `None` for an entry the folder
/// ignores: hidden or editor files, and anything not `.md`.
pub fn role_of_file(file: &str) -> Option<&str> {
    if file.starts_with('.') {
        return None;
    }
    file.strip_suffix(".md")
}

/// The role a file holds: its name from the file name, its prompt the text as is.
pub fn role_from_file(name: &str, bytes: &[u8]) -> Result<Role, FileProblem> {
    if !valid_name(name) {
        return Err(FileProblem::Name);
    }
    let prompt = std::str::from_utf8(bytes).map_err(|_| FileProblem::NotUtf8)?;
    crate::domain::validate_message(prompt).map_err(|_| FileProblem::Prompt)?;
    Ok(Role {
        name: name.into(),
        prompt: prompt.into(),
    })
}

/// The version of a file's content: changes whenever the bytes change.
pub fn version(bytes: &[u8]) -> String {
    data_encoding::HEXLOWER.encode(&Sha256::digest(bytes)[..16])
}

/// The files to read again: new in `next`, or listed differently than in `last`.
pub fn changed_files(last: &Listing, next: &Listing) -> BTreeSet<String> {
    next.iter()
        .filter(|(file, stamp)| last.get(*file) != Some(*stamp))
        .map(|(file, _)| file.clone())
        .collect()
}

/// Roles by name, root last.
pub fn sort_roles(roles: &mut [Role]) {
    roles.sort_by_key(|r| (crate::spawn::is_root(Some(&r.name)), r.name.to_lowercase()));
}

/// One read file of the folder: its name, and its role with the content
/// version, or why it is not one.
pub type ReadFile = (String, Result<(Role, String), FileProblem>);

/// The roles of a folder and what it shows about them: two files for one
/// role name (which differ only in case) load the first by file name.
pub fn assemble(files: Vec<ReadFile>) -> (Vec<Role>, FolderView) {
    let mut roles: Vec<Role> = Vec::new();
    let mut view = FolderView::default();
    for (file, read) in files {
        let read = read.and_then(|(role, version)| {
            match roles
                .iter()
                .find(|r| r.name.eq_ignore_ascii_case(&role.name))
            {
                Some(first) => Err(FileProblem::Duplicate(file_name(&first.name))),
                None => Ok((role, version)),
            }
        });
        match read {
            Ok((role, version)) => {
                view.versions.insert(role.name.clone(), version);
                roles.push(role);
            }
            Err(problem) => view.not_loaded.push(NotLoaded {
                file,
                reason: problem.to_string(),
            }),
        }
    }
    sort_roles(&mut roles);
    (roles, view)
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && !name.eq_ignore_ascii_case("channel")
        && !name.ends_with(['.', '-'])
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

impl RoleBook {
    /// The book with the built-in root role (REQ-028), added when missing.
    pub fn with_root(mut self) -> RoleBook {
        if self.role(crate::spawn::ROOT).is_none() {
            self.roles.push(Role {
                name: crate::spawn::ROOT.into(),
                prompt: crate::spawn::ROOT_PROMPT.into(),
            });
        }
        self
    }
    /// The book with the preset roles (master, researcher) offered once:
    /// each is added (before root) unless it was offered before or the user
    /// has a role of that name, whose text is then kept.
    pub fn with_presets(mut self) -> RoleBook {
        for (name, prompt) in crate::role_presets::PRESETS {
            if !self.presets.insert(name.to_owned()) || self.role(name).is_some() {
                continue;
            }
            self.roles.push(Role {
                name: name.into(),
                prompt: prompt.into(),
            });
        }
        sort_roles(&mut self.roles);
        self
    }
    pub fn role(&self, name: &str) -> Option<&Role> {
        self.roles
            .iter()
            .find(|r| r.name.eq_ignore_ascii_case(name))
    }
    /// Adds a role, or changes `previous` (renaming it and its assignments).
    pub fn save(&self, role: Role, previous: Option<&str>) -> Result<RoleBook, RoleError> {
        if !valid_name(&role.name) {
            return Err(RoleError::Name);
        }
        crate::domain::validate_message(&role.prompt).map_err(|_| RoleError::Prompt)?;
        let mut next = self.clone();
        let old = match previous {
            Some(previous) => Some(
                next.role(previous)
                    .ok_or_else(|| RoleError::Unknown(previous.into()))?
                    .name
                    .clone(),
            ),
            None => None,
        };
        if old
            .as_deref()
            .is_some_and(|o| crate::spawn::is_root(Some(o)))
            && !crate::spawn::is_root(Some(&role.name))
        {
            return Err(RoleError::Builtin);
        }
        if let Some(existing) = next.role(&role.name)
            && old.as_deref() != Some(existing.name.as_str())
        {
            return Err(RoleError::Exists(existing.name.clone()));
        }
        match &old {
            Some(old) => {
                for held in next.assignments.values_mut().filter(|r| *r == old) {
                    held.clone_from(&role.name);
                }
                if let Some(slot) = next.roles.iter_mut().find(|r| &r.name == old) {
                    *slot = role;
                }
            }
            None => next.roles.push(role),
        }
        // By name, the built-in root role last.
        sort_roles(&mut next.roles);
        Ok(next)
    }
    /// Removes a role and every assignment of it.
    pub fn delete(&self, name: &str) -> Result<RoleBook, RoleError> {
        if crate::spawn::is_root(Some(name)) {
            return Err(RoleError::Builtin);
        }
        let found = self
            .role(name)
            .ok_or_else(|| RoleError::Unknown(name.into()))?
            .name
            .clone();
        let mut next = self.clone();
        next.roles.retain(|r| r.name != found);
        next.assignments.retain(|_, r| *r != found);
        Ok(next)
    }
    /// Gives `session` the role `role`, or none.
    pub fn assign(&self, session: &str, role: Option<&str>) -> Result<RoleBook, RoleError> {
        let mut next = self.clone();
        match role {
            Some(role) => {
                let name = self
                    .role(role)
                    .ok_or_else(|| RoleError::Unknown(role.into()))?
                    .name
                    .clone();
                next.assignments.insert(session.to_owned(), name);
            }
            None => {
                next.assignments.remove(session);
            }
        }
        Ok(next)
    }
    /// Gives the session `session_id` named `name` the role `role`, or none;
    /// root is also recorded by session id.
    pub fn assign_session(
        &self,
        session_id: &str,
        name: &str,
        role: Option<&str>,
    ) -> Result<RoleBook, RoleError> {
        let mut next = self.assign(name, role)?;
        if crate::spawn::is_root(role) {
            next.root_sessions.insert(session_id.to_owned());
        } else {
            next.root_sessions.remove(session_id);
        }
        Ok(next)
    }
    pub fn role_of(&self, session: &str) -> Option<&str> {
        self.assignments.get(session).map(String::as_str)
    }
}

/// The message that gives a session its role.
pub fn assignment_message(role: &Role) -> String {
    format!(
        "Your role in this workspace: {name}.\n\n{prompt}\n\nIn channels, posts that mention @{name} reach you.",
        name = role.name,
        prompt = role.prompt.trim_end()
    )
}

#[cfg(test)]
#[path = "roles_tests.rs"]
mod tests;
