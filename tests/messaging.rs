// Unix only: these tests drive Unix sockets and the native Unix adapters.
#![cfg(unix)]
use session_bridge::{
    discovery,
    protocol::{Reply, Request},
    transport,
};
use std::{fs, path::Path, process::Stdio, time::Duration};
use tokio::{
    process::Command,
    time::{sleep, timeout},
};
use uuid::Uuid;

/* REQ-015: Messaging startup neither requires Codex nor owns a model runtime. */
#[tokio::test]
async fn daemon_runs_without_codex_or_model_state() {
    let home = tempfile::tempdir().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_session-bridge"))
        .arg("daemon")
        .env("HOME", home.path())
        .env("CODEX_HOME", home.path().join(".codex"))
        .env("CLAUDE_CONFIG_DIR", home.path().join(".claude"))
        .env("SESSION_BRIDGE_CODEX_BIN", "/no-codex-runtime-allowed")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let socket = home.path().join(".local/state/session-bridge/control.sock");
    timeout(Duration::from_secs(10), async {
        loop {
            assert!(child.try_wait().unwrap().is_none());
            if let Ok(mut stream) = transport::connect(&socket).await {
                transport::write_frame(&mut stream, &Request::Status)
                    .await
                    .unwrap();
                let reply: Reply = transport::read_frame(&mut stream).await.unwrap();
                assert!(matches!(reply, Reply::Status { .. }));
                assert!(
                    !serde_json::to_string(&reply)
                        .unwrap()
                        .contains("app_server")
                );
                break;
            }
            sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    sleep(Duration::from_millis(250)).await;
    assert!(!home.path().join(".codex").exists());
    unsafe {
        libc::kill(child.id().unwrap() as i32, libc::SIGTERM);
    }
    assert!(
        timeout(Duration::from_secs(5), child.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
}

fn metadata_database(home: &Path, id: Uuid, source: &str) {
    let query = format!(
        "CREATE TABLE threads(id TEXT,cwd TEXT,title TEXT,source TEXT,attached_thread_parent_id TEXT); INSERT INTO threads(id,cwd,title,source) VALUES('{id}','/work/mono','Messaging test','{source}');"
    );
    let output = std::process::Command::new("/usr/bin/sqlite3")
        .arg(home.join("state_5.sqlite"))
        .arg(query)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/* REQ-015: Discover current Codex sessions from live writer handles and read-only metadata, without RPC. */
#[tokio::test]
async fn discovers_live_codex_writer_and_drops_closed_handle() {
    let home = tempfile::tempdir().unwrap();
    let codex = home.path().join(".codex");
    let locks = codex.join("thread-writer-locks");
    fs::create_dir_all(&locks).unwrap();
    let id = Uuid::new_v4();
    metadata_database(&codex, id, "cli");
    let file = fs::File::create(locks.join(format!("{id}.lock"))).unwrap();
    let sources = discovery::Sources {
        codex_home: codex.clone(),
        claude_home: home.path().join(".claude"),
    };
    let sessions = discovery::discover(&sources).await.unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].session_id, format!("codex:{id}"));
    assert_eq!(sessions[0].pid, std::process::id());
    assert_eq!(sessions[0].cwd, Path::new("/work/mono"));
    assert!(!sessions[0].is_subagent());
    drop(file);
    assert!(discovery::discover(&sources).await.unwrap().is_empty());
    assert!(!codex.join("app-server-control").exists());
}

/* REQ-015: Native delivery passes only queue/thread/message, preserving arguments and native policy. */
#[tokio::test]
async fn native_queue_preserves_text_and_reports_acceptance() {
    use std::os::unix::fs::PermissionsExt;
    let home = tempfile::tempdir().unwrap();
    let executable = home.path().join("codex");
    fs::write(
        &executable,
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$CODEX_HOME/args\"\nexit 0\n",
    )
    .unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    let id = Uuid::new_v4().to_string();
    let text = "Peer input: $(touch /not-executed)\n--resume";
    let result = session_bridge::codex_sessions::queue(&executable, home.path(), &id, text).await;
    assert_eq!(result, session_bridge::domain::Delivery::Queued);
    assert_eq!(
        fs::read_to_string(home.path().join("args")).unwrap(),
        format!("queue\n--thread\n{id}\n--message\n{text}\n")
    );
}

/* REQ-003: A missing executable remains pending; a failed launched command is never retried automatically. */
#[tokio::test]
async fn native_queue_distinguishes_unsent_and_uncertain_failures() {
    use session_bridge::{codex_sessions::queue, domain::Delivery};
    use std::os::unix::fs::PermissionsExt;
    let home = tempfile::tempdir().unwrap();
    let executable = home.path().join("codex");
    let id = Uuid::new_v4().to_string();
    assert!(matches!(
        queue(&executable, home.path(), &id, "hello").await,
        Delivery::Pending { .. }
    ));
    fs::write(
        &executable,
        "#!/bin/sh\necho 'native command failed' >&2\nexit 7\n",
    )
    .unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(
        matches!(queue(&executable, home.path(), &id, "hello").await, Delivery::Uncertain { reason } if reason.contains("native command failed"))
    );
}

/* REQ-007: Writer-handle discovery retains attached-thread identity so the bridge excludes it. */
#[tokio::test]
async fn attached_codex_threads_remain_subagents() {
    let home = tempfile::tempdir().unwrap();
    let codex = home.path().join(".codex");
    fs::create_dir_all(codex.join("thread-writer-locks")).unwrap();
    let id = Uuid::new_v4();
    metadata_database(&codex, id, "cli");
    let output = std::process::Command::new("/usr/bin/sqlite3")
        .arg(codex.join("state_5.sqlite"))
        .arg(format!(
            "UPDATE threads SET attached_thread_parent_id='{}'",
            Uuid::new_v4()
        ))
        .output()
        .unwrap();
    assert!(output.status.success());
    let _file =
        fs::File::create(codex.join("thread-writer-locks").join(format!("{id}.lock"))).unwrap();
    let sessions = discovery::discover(&discovery::Sources {
        codex_home: codex,
        claude_home: home.path().join(".claude"),
    })
    .await
    .unwrap();
    assert_eq!(sessions.len(), 1);
    assert!(sessions[0].is_subagent());
}
