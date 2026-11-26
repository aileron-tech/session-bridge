//! Roles (REQ-026): named instruction prompts the user assigns to sessions.
//! Assigning sends the prompt to the session once; the session then answers
//! to `@role` in its channels. Kept by session name, like the channel
//! default, so an assignment survives the session restarting. Pure.
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Role {
    pub name: String,
    pub prompt: String,
}

/// Every role and which session (by name) holds which role.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoleBook {
    #[serde(default)]
    pub roles: Vec<Role>,
    /// Session name to role name.
    #[serde(default)]
    pub assignments: BTreeMap<String, String>,
    /// Session ids the user gave the root role (REQ-028): root's authority
    /// follows the session, not its name, which a session can change.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub root_sessions: BTreeSet<String>,
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
            // The built-in root role stays last.
            None => {
                let at = next
                    .roles
                    .iter()
                    .position(|r| crate::spawn::is_root(Some(&r.name)))
                    .unwrap_or(next.roles.len());
                next.roles.insert(at, role);
            }
        }
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
