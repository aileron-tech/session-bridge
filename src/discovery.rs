use crate::domain::{Provider, SessionId};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::{
    fs,
    io::{AsyncBufReadExt, BufReader},
    process::Command,
    time::timeout,
};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub session_id: String,
    pub agent: Provider,
    pub project: String,
    pub cwd: PathBuf,
    pub name: String,
    pub status: String,
    pub kind: String,
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
#[serde(rename_all = "camelCase")]
struct ClaudeRecord {
    pid: u32,
    session_id: Uuid,
    cwd: PathBuf,
    messaging_socket_path: Option<PathBuf>,
    name: Option<String>,
    status: Option<String>,
    kind: Option<String>,
    #[serde(default)]
    session_bridge: bool,
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
#[derive(Deserialize)]
struct IndexEntry {
    id: Uuid,
    thread_name: String,
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
pub fn is_alive(pid: u32) -> bool {
    // kill(pid, 0) checks existence without delivering a signal.
    pid > 1 && unsafe { libc::kill(pid as i32, 0) } == 0
}

pub async fn discover(sources: &Sources) -> Result<Vec<Session>> {
    let (codex, claude) = tokio::join!(discover_codex(sources), discover_claude(sources));
    let mut sessions = codex?;
    sessions.extend(claude?);
    sessions.sort_by(|a, b| (&a.project, &a.session_id).cmp(&(&b.project, &b.session_id)));
    sessions.dedup_by(|a, b| a.session_id == b.session_id);
    Ok(sessions)
}
async fn discover_codex(sources: &Sources) -> Result<Vec<Session>> {
    let codex_home = match fs::canonicalize(&sources.codex_home).await {
        Ok(path) => path,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let uid = unsafe { libc::geteuid() }.to_string();
    let output = timeout(
        Duration::from_secs(5),
        Command::new("/usr/sbin/lsof")
            .args(["-nP", "-a", "-u", &uid, "-Fpn"])
            .kill_on_drop(true)
            .output(),
    )
    .await
    .context("Codex discovery timed out")??;
    anyhow::ensure!(
        output.status.success() || output.status.code() == Some(1),
        "lsof failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut pid = 0;
    let mut sessions = Vec::new();
    let mut locked_threads = BTreeMap::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        if let Some(id) = line.strip_prefix('p') {
            pid = id.parse().context("Invalid lsof PID")?;
        }
        if let Some(path) = line.strip_prefix('n') {
            let path = Path::new(path);
            if path.parent() == Some(codex_home.join("thread-writer-locks").as_path())
                && path.extension().is_some_and(|ext| ext == "lock")
                && let Some(stem) = path.file_stem().and_then(|stem| stem.to_str())
                && let Ok(id) = Uuid::parse_str(stem)
            {
                locked_threads.insert(id, pid);
            }
            if path.starts_with(codex_home.join("sessions"))
                && path.extension().is_some_and(|v| v == "jsonl")
            {
                let file = match fs::File::open(path).await {
                    Ok(file) => file,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(e) => return Err(e.into()),
                };
                let mut line = String::new();
                BufReader::new(file).read_line(&mut line).await?;
                match parse_codex_record(&line, pid) {
                    Ok(session) => sessions.push(session),
                    Err(e) => eprintln!(
                        "Ignoring unsupported Codex metadata {}: {e}",
                        path.display()
                    ),
                }
            }
        }
    }
    sessions.extend(discover_locked_codex(sources, &locked_threads).await?);
    let index = sources.codex_home.join("session_index.jsonl");
    match fs::File::open(&index).await {
        Ok(file) => {
            let mut names = BTreeMap::new();
            let mut lines = BufReader::new(file).lines();
            while let Some(line) = lines.next_line().await? {
                match serde_json::from_str::<IndexEntry>(&line) {
                    Ok(entry) => {
                        names.insert(entry.id.to_string(), entry.thread_name);
                    }
                    Err(e) => eprintln!("Ignoring malformed Codex session-index entry: {e}"),
                }
            }
            for session in &mut sessions {
                if let Some(id) = session.session_id.strip_prefix("codex:")
                    && let Some(name) = names.get(id)
                {
                    session.name.clone_from(name);
                }
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    Ok(sessions)
}
#[derive(Deserialize)]
struct CodexThreadMetadata {
    id: Uuid,
    cwd: PathBuf,
    title: String,
    source: String,
    attached_thread_parent_id: Option<String>,
}

async fn discover_locked_codex(
    sources: &Sources,
    owners: &BTreeMap<Uuid, u32>,
) -> Result<Vec<Session>> {
    if owners.is_empty() {
        return Ok(Vec::new());
    }
    let mut entries = fs::read_dir(&sources.codex_home).await?;
    let mut database = None;
    while let Some(entry) = entries.next_entry().await? {
        let name = entry.file_name();
        let Some(version) = name
            .to_str()
            .and_then(|name| name.strip_prefix("state_"))
            .and_then(|name| name.strip_suffix(".sqlite"))
            .and_then(|version| version.parse::<u32>().ok())
        else {
            continue;
        };
        if database
            .as_ref()
            .is_none_or(|(current, _)| version > *current)
        {
            database = Some((version, entry.path()));
        }
    }
    let (_, database) = database.context("Live Codex threads have no metadata database")?;
    // Only parsed UUIDs enter SQL; the database is opened read-only.
    let ids = owners
        .keys()
        .map(|id| format!("'{id}'"))
        .collect::<Vec<_>>()
        .join(",");
    let query = format!(
        "SELECT id,cwd,title,source,attached_thread_parent_id FROM threads WHERE id IN ({ids})"
    );
    let output = timeout(
        Duration::from_secs(5),
        Command::new("/usr/bin/sqlite3")
            .args(["-readonly", "-json"])
            .arg(database)
            .arg(query)
            .kill_on_drop(true)
            .output(),
    )
    .await
    .context("Codex metadata query timed out")??;
    anyhow::ensure!(
        output.status.success(),
        "Codex metadata query failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    if output.stdout.is_empty() {
        return Ok(Vec::new());
    }
    let rows: Vec<CodexThreadMetadata> = serde_json::from_slice(&output.stdout)?;
    rows.into_iter().map(|row| {
        let source = if row.source.starts_with('{') {
            serde_json::from_str(&row.source)?
        } else { serde_json::Value::String(row.source) };
        let record = serde_json::json!({"type":"session_meta","payload":{"id":row.id,"cwd":row.cwd,"source":source}});
        let mut session = parse_codex_record(&record.to_string(), owners[&row.id])?;
            if row.attached_thread_parent_id.is_some() {
                session.kind = "subagent".into();
            }
        session.name = row.title;
        Ok(session)
    }).collect()
}

async fn discover_claude(sources: &Sources) -> Result<Vec<Session>> {
    let mut entries = match fs::read_dir(sources.claude_home.join("sessions")).await {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mut sessions = Vec::new();
    while let Some(entry) = entries.next_entry().await? {
        if entry.path().extension().is_none_or(|v| v != "json") {
            continue;
        }
        let raw = match fs::read_to_string(entry.path()).await {
            Ok(raw) => raw,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.into()),
        };
        let record: ClaudeRecord = match serde_json::from_str(&raw) {
            Ok(record) => record,
            Err(e) => {
                eprintln!(
                    "Ignoring unsupported Claude session record {}: {e}",
                    entry.path().display()
                );
                continue;
            }
        };
        if record.session_bridge || !is_alive(record.pid) {
            continue;
        }
        if let Some(socket) = record.messaging_socket_path {
            if !socket.exists() {
                continue;
            }
            sessions.push(Session {
                session_id: SessionId {
                    provider: Provider::Claude,
                    id: record.session_id,
                }
                .to_string(),
                agent: Provider::Claude,
                project: project_name(&record.cwd),
                cwd: record.cwd,
                name: record.name.unwrap_or_else(|| record.session_id.to_string()),
                status: record.status.unwrap_or_else(|| "open".into()),
                kind: record.kind.unwrap_or_else(|| "session".into()),
                pid: record.pid,
                socket: Some(socket),
            });
        }
    }
    Ok(sessions)
}

pub async fn ancestor_pids() -> Result<Vec<u32>> {
    let mut pid = std::process::id();
    let mut ancestors = Vec::new();
    for _ in 0..12 {
        let output = Command::new("/bin/ps")
            .args(["-p", &pid.to_string(), "-o", "ppid="])
            .output()
            .await?;
        if !output.status.success() {
            break;
        }
        pid = String::from_utf8(output.stdout)?.trim().parse()?;
        if pid <= 1 {
            break;
        }
        ancestors.push(pid);
    }
    Ok(ancestors)
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
