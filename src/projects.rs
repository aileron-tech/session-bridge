//! Projects on this laptop and their masters (REQ-030). Pure.
//!
//! A project is a directory with live sessions, or one the bridge knew a
//! session in before (REQ-038): it stays after its sessions end. Its master
//! is the session the user chose, else the session named or holding the role
//! `master`, else `lead`, else the first session by name.
use crate::{discovery::Session, roles::RoleBook};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

/// A project as the Projects tab shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectView {
    /// The project (channel) name: the directory's last component.
    pub project: String,
    pub dir: PathBuf,
    /// The master's session name, if the project has live sessions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub master: Option<String>,
    /// The user chose the master (else it follows the rule).
    pub master_chosen: bool,
    /// Session ids, by name.
    pub sessions: Vec<String>,
}

/// A session's name and role, for choosing a master.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Member<'a> {
    pub name: &'a str,
    pub role: Option<&'a str>,
}

/// The master among `members`, and whether it is the user's choice: the
/// chosen one when live, else `master`, `lead` (in a name or a role), else the
/// first by name.
pub fn master(members: &[Member], chosen: Option<&str>) -> Option<(String, bool)> {
    if let Some(chosen) = chosen
        && let Some(found) = members.iter().find(|m| m.name.eq_ignore_ascii_case(chosen))
    {
        return Some((found.name.to_owned(), true));
    }
    let mut sorted: Vec<&Member> = members.iter().collect();
    sorted.sort_by(|a, b| a.name.cmp(b.name));
    let holds = |m: &Member, word: &str| {
        m.name.to_ascii_lowercase().contains(word)
            || m.role
                .is_some_and(|r| r.to_ascii_lowercase().contains(word))
    };
    ["master", "lead"]
        .iter()
        .find_map(|word| sorted.iter().find(|m| holds(m, word)))
        .or_else(|| sorted.first())
        .map(|m| (m.name.to_owned(), false))
}

/// This laptop's projects (local, non-user sessions grouped by directory, and
/// the `remembered` directories without live sessions), with masters from
/// `chosen` (project name to session name).
pub fn projects(
    sessions: &[Session],
    remembered: &BTreeSet<PathBuf>,
    book: &RoleBook,
    chosen: &BTreeMap<String, String>,
) -> Vec<ProjectView> {
    let mut by_dir: BTreeMap<PathBuf, Vec<&Session>> = BTreeMap::new();
    for session in sessions
        .iter()
        .filter(|s| s.host.is_none() && s.agent != crate::domain::Provider::User)
    {
        by_dir.entry(session.cwd.clone()).or_default().push(session);
    }
    for dir in remembered {
        by_dir.entry(dir.clone()).or_default();
    }
    by_dir
        .into_iter()
        .map(|(dir, mut members)| {
            members.sort_by(|a, b| a.name.cmp(&b.name));
            let project = members.first().map_or_else(
                || crate::discovery::project_name(&dir),
                |s| s.project.clone(),
            );
            let named: Vec<Member> = members
                .iter()
                .map(|s| Member {
                    name: &s.name,
                    role: book.role_of(&s.name),
                })
                .collect();
            let found = master(&named, chosen.get(&project).map(String::as_str));
            ProjectView {
                project,
                dir,
                master_chosen: found.as_ref().is_some_and(|(_, c)| *c),
                master: found.map(|(name, _)| name),
                sessions: members.iter().map(|s| s.session_id.clone()).collect(),
            }
        })
        .collect()
}

/// The directories of this laptop's sessions among `known` (every session the
/// bridge has recorded): the projects the Projects tab remembers (REQ-038).
pub fn remembered<'a>(known: impl IntoIterator<Item = &'a Session>) -> BTreeSet<PathBuf> {
    known
        .into_iter()
        .filter(|s| {
            s.host.is_none() && s.agent != crate::domain::Provider::User && !s.is_subagent()
        })
        .map(|s| s.cwd.clone())
        .collect()
}

/// Whether `dir` is a scratch folder rather than a project to remember
/// (REQ-038): under one of the system temp `roots` and not inside the bridge's
/// own home, given in each of its `homes` spellings (test bridges keep their
/// home in a temp folder, which sessions may report resolved).
pub fn is_scratch(dir: &Path, roots: &[PathBuf], homes: &[PathBuf]) -> bool {
    roots.iter().any(|root| dir.starts_with(root))
        && !homes.iter().any(|home| dir.starts_with(home))
}

#[cfg(test)]
#[path = "projects_tests.rs"]
mod tests;
