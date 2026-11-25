use crate::domain::{Provider, SessionId};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use uuid::Uuid;

#[cfg(unix)]
mod native;

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
    pub fn matches_project(&self, project: &str) -> bool {
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

#[cfg(test)]
mod tests {
    use super::*;
    /* REQ-001: Metadata extraction does not expose conversation contents. */
    #[test]
    fn reads_project_and_provider_from_codex_metadata() {
        let raw = r#"{"type":"session_meta","payload":{"id":"01a1d465-0b2b-76e2-ac61-2c442c669b71","cwd":"/work/mono","source":"cli","base_instructions":"private"}}"#;
        let session = parse_codex_record(raw, 123).unwrap();
        assert_eq!(session.project, "mono");
        assert_eq!(session.kind, "session");
        assert!(!serde_json::to_string(&session).unwrap().contains("private"));
    }
}
