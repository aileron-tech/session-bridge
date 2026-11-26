//! Messages waiting in Codex's own queue (REQ-032): `codex queue` stores
//! them in `queue_<n>.sqlite` (`queued_items`) until the thread starts a
//! turn. Read only, never changed: parsing is pure, the query is infra.
use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::Path;
use uuid::Uuid;

/// A message waiting in a Codex thread's queue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedItem {
    /// Codex's queued submission id (`queued_items.id`).
    pub id: String,
    pub created_at_ms: u64,
    pub text: String,
    /// The bridge message it carries, when the bridge queued it.
    pub message_id: Option<Uuid>,
}

#[derive(Deserialize)]
struct Row {
    #[serde(default)]
    id: String,
    created_at_ms: u64,
    payload_json: String,
}
#[derive(Deserialize)]
struct Payload {
    #[serde(rename = "UserInput")]
    user_input: Option<UserInput>,
}
#[derive(Deserialize)]
struct UserInput {
    #[serde(default)]
    content: Vec<Content>,
}
#[derive(Deserialize)]
struct Content {
    #[serde(default)]
    text: Option<String>,
}

/// The bridge message id in a delivered text (`Message ID: <uuid>`).
pub fn message_id(text: &str) -> Option<Uuid> {
    text.lines()
        .find_map(|line| line.strip_prefix("Message ID: "))
        .and_then(|id| Uuid::parse_str(id.trim()).ok())
}

/// Parses `sqlite3 -json` rows of `id, created_at_ms, payload_json`; an item that
/// is not user input still counts, with an empty text.
pub fn parse_items(json: &[u8]) -> Result<Vec<QueuedItem>> {
    if json.iter().all(u8::is_ascii_whitespace) {
        return Ok(Vec::new());
    }
    let rows: Vec<Row> = serde_json::from_slice(json).context("Unreadable Codex queue rows")?;
    Ok(rows
        .into_iter()
        .map(|row| {
            let text = serde_json::from_str::<Payload>(&row.payload_json)
                .ok()
                .and_then(|p| p.user_input)
                .map(|input| {
                    input
                        .content
                        .into_iter()
                        .filter_map(|c| c.text)
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default();
            QueuedItem {
                id: row.id,
                created_at_ms: row.created_at_ms,
                message_id: message_id(&text),
                text,
            }
        })
        .collect())
}

/// The items waiting for `thread`, oldest first; none when Codex has no queue.
pub async fn waiting(codex_home: &Path, thread: &Uuid) -> Result<Vec<QueuedItem>> {
    Ok(queue(codex_home, thread).await?.unwrap_or_default())
}

/// The items waiting for `thread`, or `None` when Codex keeps no queue
/// database here (an older Codex, another CODEX_HOME): then nothing can be
/// told about what Codex took.
#[cfg(unix)]
pub async fn queue(codex_home: &Path, thread: &Uuid) -> Result<Option<Vec<QueuedItem>>> {
    let mut newest: Option<(u32, std::path::PathBuf)> = None;
    let mut entries = match tokio::fs::read_dir(codex_home).await {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    while let Some(entry) = entries.next_entry().await? {
        let name = entry.file_name();
        if let Some(version) = name
            .to_str()
            .and_then(|n| n.strip_prefix("queue_"))
            .and_then(|n| n.strip_suffix(".sqlite"))
            .and_then(|v| v.parse::<u32>().ok())
            && newest
                .as_ref()
                .is_none_or(|(current, _)| version > *current)
        {
            newest = Some((version, entry.path()));
        }
    }
    let Some((_, database)) = newest else {
        return Ok(None);
    };
    // Only a parsed UUID enters the SQL; the database is opened read-only.
    let query = format!(
        "SELECT id, created_at_ms, payload_json FROM queued_items WHERE thread_id='{thread}' ORDER BY queue_order"
    );
    parse_items(&crate::discovery::native::query_metadata(&database, &query).await?).map(Some)
}
#[cfg(not(unix))]
pub async fn queue(_codex_home: &Path, _thread: &Uuid) -> Result<Option<Vec<QueuedItem>>> {
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    /* REQ-032: Codex queue rows are read as texts, with the bridge message
    each carries; other payloads still count. */
    #[test]
    fn parses_queued_items() {
        let id = Uuid::new_v4();
        let text = format!("[From the user]\nMessage ID: {id}\nContent: hi");
        let rows = serde_json::json!([
            {"id": "q1", "created_at_ms": 1, "payload_json": serde_json::json!({"UserInput": {"content": [{"type": "text", "text": text}]}}).to_string()},
            {"created_at_ms": 2, "payload_json": "{\"Other\":{}}"}
        ]);
        let items = parse_items(rows.to_string().as_bytes()).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].message_id, Some(id));
        assert_eq!(items[0].id, "q1");
        assert!(items[0].text.contains("Content: hi"));
        assert_eq!(items[1].text, "");
        assert_eq!(items[1].message_id, None);
        assert!(parse_items(b"\n").unwrap().is_empty());
        assert!(parse_items(b"not json").is_err());
    }

    /* REQ-032: The reader finds the newest queue database and reads one
    thread's items in order, read-only. */
    #[cfg(unix)]
    #[tokio::test]
    async fn reads_a_thread_queue() {
        let dir = tempfile::tempdir().unwrap();
        let thread = Uuid::new_v4();
        let other = Uuid::new_v4();
        let payload = |t: &str| {
            serde_json::json!({"UserInput": {"content": [{"type": "text", "text": t}]}})
                .to_string()
                .replace('\'', "''")
        };
        let sql = format!(
            "create table queued_items(id text, thread_id text, payload_json text, queue_order integer, created_at_ms integer); \
             insert into queued_items values('a','{thread}','{}',1,20),('b','{thread}','{}',0,10),('c','{other}','{}',0,5);",
            payload("second"),
            payload("first"),
            payload("elsewhere")
        );
        let created = std::process::Command::new("/usr/bin/sqlite3")
            .arg(dir.path().join("queue_1.sqlite"))
            .arg(sql)
            .output()
            .unwrap();
        assert!(created.status.success(), "{created:?}");
        let items = waiting(dir.path(), &thread).await.unwrap();
        let texts: Vec<&str> = items.iter().map(|i| i.text.as_str()).collect();
        assert_eq!(texts, ["first", "second"]);
        let ids: Vec<&str> = items.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(ids, ["b", "a"]);
        assert!(
            waiting(&dir.path().join("none"), &thread)
                .await
                .unwrap()
                .is_empty()
        );
        // No queue database is told apart from an empty queue (REQ-032).
        assert_eq!(
            queue(&dir.path().join("none"), &thread).await.unwrap(),
            None
        );
        assert_eq!(
            queue(dir.path(), &Uuid::new_v4()).await.unwrap(),
            Some(Vec::new())
        );
    }
}
