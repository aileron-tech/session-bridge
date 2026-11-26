use crate::domain::{Provider, SessionId};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use uuid::Uuid;

#[cfg(unix)]
pub(crate) mod native;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub session_id: String,
    pub agent: Provider,
    pub project: String,
    pub cwd: PathBuf,
    pub name: String,
    pub status: String,
    pub kind: String,
    /// The paired bridge that owns a remote session; `None` for local sessions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    #[serde(skip)]
    pub pid: u32,
    #[serde(skip)]
    pub socket: Option<PathBuf>,
}
impl Session {
    pub fn is_subagent(&self) -> bool {
        self.kind == "subagent"
    }
    /// The user on this laptop: no project or directory of its own (REQ-019).
    pub fn user(id: &SessionId) -> Self {
        Self {
            session_id: id.to_string(),
            agent: Provider::User,
            project: "user".into(),
            cwd: PathBuf::new(),
            name: "user".into(),
            status: "user".into(),
            kind: "user".into(),
            host: None,
            pid: 0,
            socket: None,
        }
    }
    /// The user posting in a project's channel (REQ-020): an address of its
    /// own per project, so answers to the channel come back to the channel.
    pub fn in_channel(user: &SessionId, project: &str) -> Self {
        let id = SessionId {
            provider: Provider::User,
            id: Uuid::new_v5(
                &Uuid::NAMESPACE_URL,
                format!("session-bridge:{user}/channel/{project}").as_bytes(),
            ),
        };
        Self {
            project: project.into(),
            name: format!("#{project}"),
            kind: "channel".into(),
            ..Self::user(&id)
        }
    }
    /// The project channel of a user address, if it is one.
    pub fn channel(&self) -> Option<&str> {
        match self.agent {
            Provider::User => self.name.strip_prefix('#'),
            _ => None,
        }
    }
    /// How people read this party: "name (project)", "user" or "user in
    /// #project", plus its host.
    pub fn label(&self) -> String {
        let base = match (&self.agent, self.channel()) {
            (Provider::User, Some(channel)) => format!("user in #{channel}"),
            (Provider::User, None) => "user".to_owned(),
            _ => format!("{} ({})", self.name, self.project),
        };
        match &self.host {
            Some(host) => format!("{base} on {host}"),
            None => base,
        }
    }
    /// The user takes part in every project; a session matches by name or path.
    pub fn matches_project(&self, project: &str) -> bool {
        if self.agent == Provider::User {
            return true;
        }
        if Path::new(project).is_absolute() {
            self.cwd == Path::new(project)
        } else {
            self.project == project
        }
    }
}

#[derive(Debug, Clone)]
pub struct Sources {
    pub codex_home: PathBuf,
    pub claude_home: PathBuf,
}

#[derive(Deserialize)]
struct CodexRecord {
    #[serde(rename = "type")]
    kind: String,
    payload: CodexMeta,
}
#[derive(Deserialize)]
struct CodexMeta {
    id: Uuid,
    cwd: PathBuf,
    source: serde_json::Value,
}

pub fn parse_codex_record(raw: &str, pid: u32) -> Result<Session> {
    let record: CodexRecord = serde_json::from_str(raw)?;
    anyhow::ensure!(record.kind == "session_meta", "Missing session metadata");
    let meta = record.payload;
    let kind = if meta.source.is_object() {
        "subagent"
    } else {
        "session"
    };
    Ok(Session {
        session_id: SessionId {
            provider: Provider::Codex,
            id: meta.id,
        }
        .to_string(),
        agent: Provider::Codex,
        project: project_name(&meta.cwd),
        cwd: meta.cwd,
        name: meta.id.to_string(),
        status: "open".into(),
        kind: kind.into(),
        host: None,
        pid,
        socket: None,
    })
}
pub fn project_name(path: &Path) -> String {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}
pub use crate::platform::{ancestor_pids, is_alive};

/// Windows has no native session discovery yet: the Claude named-pipe inbox
/// and the Codex handle scan are not implemented there.
#[cfg(not(unix))]
pub async fn discover(_sources: &Sources) -> Result<Vec<Session>> {
    Ok(Vec::new())
}
#[cfg(unix)]
pub async fn discover(sources: &Sources) -> Result<Vec<Session>> {
    let (codex, claude) = tokio::join!(
        native::discover_codex(sources),
        native::discover_claude(sources)
    );
    let mut sessions = codex?;
    sessions.extend(claude?);
    sessions.sort_by(|a, b| (&a.project, &a.session_id).cmp(&(&b.project, &b.session_id)));
    sessions.dedup_by(|a, b| a.session_id == b.session_id);
    Ok(sessions)
}

/// What the expensive Codex scan depends on: the Codex processes and the
/// thread lock files (name, size, modification time).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CodexState {
    pub pids: Vec<u32>,
    pub locks: Vec<(String, u64, std::time::SystemTime)>,
}

/// Whether the Codex scan must run again: Codex's state changed, nothing was
/// scanned yet, or the last scan is older than the safety interval.
pub fn codex_scan_due(
    previous: Option<&CodexState>,
    now: &CodexState,
    since_scan: Option<std::time::Duration>,
    safety: std::time::Duration,
) -> bool {
    previous != Some(now) || since_scan.is_none_or(|d| d >= safety)
}

/// Discovery for the bridge's background task (REQ-015): Claude's session
/// records every time (cheap file reads); Codex's process scan (`lsof`, the
/// costly part) only when Codex's processes or lock files changed, and at
/// least every [`Watcher::SAFETY`]. Between scans the last Codex sessions are
/// kept while their process lives.
#[derive(Debug, Default)]
pub struct Watcher {
    #[cfg(unix)]
    state: Option<CodexState>,
    #[cfg(unix)]
    scanned_at: Option<std::time::Instant>,
    #[cfg(unix)]
    codex: Vec<Session>,
}
impl Watcher {
    pub const SAFETY: std::time::Duration = std::time::Duration::from_secs(30);

    #[cfg(not(unix))]
    pub async fn discover(&mut self, sources: &Sources) -> Result<Vec<Session>> {
        discover(sources).await
    }
    #[cfg(unix)]
    pub async fn discover(&mut self, sources: &Sources) -> Result<Vec<Session>> {
        let now = native::codex_state(sources).await?;
        let due = codex_scan_due(
            self.state.as_ref(),
            &now,
            self.scanned_at.map(|t| t.elapsed()),
            Self::SAFETY,
        );
        let (codex, claude) = if due {
            let (codex, claude) = tokio::join!(
                native::discover_codex(sources),
                native::discover_claude(sources)
            );
            let codex = codex?;
            self.codex = codex.clone();
            self.state = Some(now);
            self.scanned_at = Some(std::time::Instant::now());
            (codex, claude?)
        } else {
            self.codex.retain(|s| is_alive(s.pid));
            (self.codex.clone(), native::discover_claude(sources).await?)
        };
        let mut sessions = codex;
        sessions.extend(claude);
        sessions.sort_by(|a, b| (&a.project, &a.session_id).cmp(&(&b.project, &b.session_id)));
        sessions.dedup_by(|a, b| a.session_id == b.session_id);
        Ok(sessions)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /* REQ-015: The costly Codex scan runs when Codex's processes or lock
    files change, when nothing was scanned yet, or after the safety
    interval; otherwise it is skipped. */
    #[test]
    fn scans_codex_only_when_needed() {
        use std::time::{Duration, SystemTime};
        let a = CodexState {
            pids: vec![10],
            locks: vec![("t.lock".into(), 0, SystemTime::UNIX_EPOCH)],
        };
        let safety = Duration::from_secs(30);
        assert!(codex_scan_due(None, &a, None, safety));
        assert!(!codex_scan_due(
            Some(&a),
            &a,
            Some(Duration::from_secs(3)),
            safety
        ));
        assert!(codex_scan_due(Some(&a), &a, Some(safety), safety));
        let more = CodexState {
            pids: vec![10, 11],
            ..a.clone()
        };
        assert!(codex_scan_due(
            Some(&a),
            &more,
            Some(Duration::from_secs(3)),
            safety
        ));
        let lock = CodexState {
            locks: vec![("u.lock".into(), 0, SystemTime::UNIX_EPOCH)],
            ..a.clone()
        };
        assert!(codex_scan_due(
            Some(&a),
            &lock,
            Some(Duration::from_secs(3)),
            safety
        ));
    }
    /* REQ-001: Metadata extraction does not expose conversation contents. */
    #[test]
    fn reads_project_and_provider_from_codex_metadata() {
        let raw = r#"{"type":"session_meta","payload":{"id":"01a1d465-0b2b-76e2-ac61-2c442c669b71","cwd":"/work/mono","source":"cli","base_instructions":"private"}}"#;
        let session = parse_codex_record(raw, 123).unwrap();
        assert_eq!(session.project, "mono");
        assert_eq!(session.kind, "session");
        assert!(!serde_json::to_string(&session).unwrap().contains("private"));
    }
    /* REQ-019: The user is reachable from every project and is named plainly;
    sessions are named by name, project and host. */
    #[test]
    fn describes_the_user_and_sessions() {
        let id = SessionId {
            provider: Provider::User,
            id: Uuid::nil(),
        };
        let user = Session::user(&id);
        assert_eq!(user.session_id, id.to_string());
        assert_eq!(user.agent, Provider::User);
        assert!(user.matches_project("mono") && user.matches_project("/work/other"));
        assert_eq!(user.label(), "user");
        let remote = Session {
            host: Some("laptop-b".into()),
            ..user
        };
        assert_eq!(remote.label(), "user on laptop-b");
        let mut session = parse_codex_record(
            r#"{"type":"session_meta","payload":{"id":"01a1d465-0b2b-76e2-ac61-2c442c669b71","cwd":"/work/mono","source":"cli"}}"#,
            1,
        )
        .unwrap();
        session.name = "fixer".into();
        assert!(!session.matches_project("other"));
        assert_eq!(session.label(), "fixer (mono)");
        session.host = Some("laptop-b".into());
        assert_eq!(session.label(), "fixer (mono) on laptop-b");
    }
    /* REQ-020: Each project channel has a stable user address of its own,
    named after the channel and reachable from that project. */
    #[test]
    fn names_channel_addresses() {
        let user = SessionId {
            provider: Provider::User,
            id: Uuid::new_v4(),
        };
        let mono = Session::in_channel(&user, "mono");
        assert_eq!(
            Session::in_channel(&user, "mono").session_id,
            mono.session_id
        );
        assert_ne!(
            Session::in_channel(&user, "other").session_id,
            mono.session_id
        );
        assert_ne!(mono.session_id, user.to_string());
        let id = SessionId::parse(&mono.session_id).unwrap();
        assert_eq!(id.provider, Provider::User);
        assert_eq!(mono.channel(), Some("mono"));
        assert_eq!(Session::user(&user).channel(), None);
        assert_eq!(mono.label(), "user in #mono");
        let remote = Session {
            host: Some("laptop-a".into()),
            ..mono
        };
        assert_eq!(remote.label(), "user in #mono on laptop-a");
        assert_eq!(remote.channel(), Some("mono"));
    }
}
