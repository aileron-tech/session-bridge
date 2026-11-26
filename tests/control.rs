//! Cross-platform checks of the control endpoint: Unix socket or named pipe.
use session_bridge::{
    daemon::Config,
    discovery::Sources,
    protocol::{Reply, Request},
    transport,
};
use std::{path::Path, process::Stdio, time::Duration};
use tokio::{
    process::{Child, Command},
    time::sleep,
};

fn config(home: &Path) -> Config {
    Config {
        supervised: true,
        tray: false,
        releases: String::new(),
        crontab: String::new(),
        ssh: String::new(),
        state: home.join(".local/state/session-bridge"),
        sources: Sources {
            codex_home: home.join(".codex"),
            claude_home: home.join(".claude"),
        },
        codex: "codex".into(),
        notify: false,
    }
}
fn daemon(home: &Path) -> Child {
    Command::new(env!("CARGO_BIN_EXE_session-bridge"))
        .arg("daemon")
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("CODEX_HOME", home.join(".codex"))
        .env("CLAUDE_CONFIG_DIR", home.join(".claude"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .unwrap()
}
async fn rpc(config: &Config, request: &Request) -> anyhow::Result<Reply> {
    let mut stream = transport::connect(&config.socket()).await?;
    transport::write_frame(&mut stream, request).await?;
    transport::read_frame(&mut stream).await
}
async fn wait_ready(config: &Config, child: &mut Child) -> u32 {
    for _ in 0..200 {
        if let Ok(Reply::Health { pid }) = rpc(config, &Request::Health).await {
            return pid;
        }
        if let Some(status) = child.try_wait().unwrap() {
            panic!("Daemon exited: {status}");
        }
        sleep(Duration::from_millis(50)).await;
    }
    panic!("Daemon did not start")
}

/* REQ-016: The service answers on its private control endpoint, serves
consecutive and concurrent clients, and reports an empty durable store. */
#[tokio::test]
async fn control_endpoint_serves_status_and_listing() {
    let home = tempfile::tempdir().unwrap();
    let config = config(home.path());
    let mut child = daemon(home.path());
    let pid = wait_ready(&config, &mut child).await;
    assert_eq!(Some(pid), child.id());
    let (first, second, third) = tokio::join!(
        rpc(&config, &Request::Status),
        rpc(&config, &Request::Status),
        rpc(&config, &Request::Health)
    );
    for reply in [first.unwrap(), second.unwrap()] {
        match reply {
            Reply::Status {
                pid: owner,
                messages,
                ..
            } => {
                assert_eq!(owner, pid);
                assert!(messages.is_empty());
            }
            other => panic!("Unexpected reply {other:?}"),
        }
    }
    assert!(matches!(third.unwrap(), Reply::Health { .. }));
    let project = home.path().to_str().unwrap().to_owned();
    let reply = rpc(
        &config,
        &Request::List {
            sender: None,
            project: Some(project),
        },
    )
    .await
    .unwrap();
    assert!(matches!(reply, Reply::Sessions { sessions, .. } if sessions.is_empty()));
    child.kill().await.unwrap();
}

/* REQ-016: A send from an unknown session is rejected and nothing is saved. */
#[tokio::test]
async fn rejects_send_from_unregistered_session() {
    let home = tempfile::tempdir().unwrap();
    let config = config(home.path());
    let mut child = daemon(home.path());
    wait_ready(&config, &mut child).await;
    let reply = rpc(
        &config,
        &Request::Send {
            sender: format!("codex:{}", uuid::Uuid::new_v4()),
            session_id: format!("claude:{}", uuid::Uuid::new_v4()),
            message: "hello".into(),
            project: None,
        },
    )
    .await
    .unwrap();
    assert!(matches!(reply, Reply::Error { .. }));
    match rpc(&config, &Request::Status).await.unwrap() {
        Reply::Status { messages, .. } => assert!(messages.is_empty()),
        other => panic!("Unexpected reply {other:?}"),
    }
    child.kill().await.unwrap();
}

/* REQ-016: Only one service owns a state directory; a second one exits and
the first keeps answering. A restarted service serves the same endpoint. */
#[tokio::test]
async fn single_owner_and_restart() {
    let home = tempfile::tempdir().unwrap();
    let config = config(home.path());
    let mut first = daemon(home.path());
    let pid = wait_ready(&config, &mut first).await;
    let mut second = daemon(home.path());
    let status = tokio::time::timeout(Duration::from_secs(10), second.wait())
        .await
        .expect("second daemon must exit")
        .unwrap();
    assert!(!status.success());
    assert!(matches!(
        rpc(&config, &Request::Health).await.unwrap(),
        Reply::Health { pid: owner } if owner == pid
    ));
    first.kill().await.unwrap();
    first.wait().await.unwrap();
    let mut third = daemon(home.path());
    let restarted = wait_ready(&config, &mut third).await;
    assert_ne!(restarted, pid);
    third.kill().await.unwrap();
}
