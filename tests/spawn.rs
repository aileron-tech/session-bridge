//! Starting and closing sessions (REQ-028), end to end: a real bridge, a
//! stand-in terminal that runs the launch command, and stand-in Claude and
//! Codex processes that register sessions the way the real ones do (a Claude
//! session record; a Codex rollout file held open).
#![cfg(unix)]
use session_bridge::{
    discovery::Session,
    protocol::{Reply, Request},
    spawn::Agent,
};
use std::{fs, os::unix::fs::PermissionsExt, path::Path};

#[path = "lan/common.rs"]
mod common;
use common::{AdminPage, Node, eventually};

const TERMINAL: &str = "#!/bin/sh\n# Stand-in terminal: <title> <command...>, run in the background.\ntitle=$1; shift\n\"$@\" >\"$SB_TEST_LOGS/$title.log\" 2>&1 </dev/null &\n";

const CLAUDE: &str = r#"#!/usr/bin/env python3
# Stand-in Claude: claude -n <name> <prompt>; records itself as a live session.
import json, os, sys, time, uuid
name, prompt = sys.argv[2], sys.argv[3]
home = os.environ["CLAUDE_CONFIG_DIR"]
inbox = os.path.join(home, "inbox-" + name)
open(inbox, "w").close()
with open(os.path.join(home, "prompts", name + ".txt"), "w") as f:
    f.write(prompt)
record = {"pid": os.getpid(), "sessionId": str(uuid.uuid4()), "cwd": os.getcwd(),
          "messagingSocketPath": inbox, "name": name, "status": "idle", "kind": "interactive"}
with open(os.path.join(home, "sessions", f"{os.getpid()}.json"), "w") as f:
    json.dump(record, f)
while True:
    time.sleep(1)
"#;

const CODEX: &str = r#"#!/usr/bin/env python3
# Stand-in Codex: codex <prompt>; holds its rollout file open like Codex.
import json, os, sys, time, uuid
home = os.environ["CODEX_HOME"]
with open(os.path.join(home, "prompts.txt"), "a") as f:
    f.write(sys.argv[1] + "\n")
path = os.path.join(home, "sessions", f"rollout-{os.getpid()}.jsonl")
rollout = open(path, "w")
rollout.write(json.dumps({"type": "session_meta", "payload": {"id": str(uuid.uuid4()), "cwd": os.getcwd(), "source": "cli"}}) + "\n")
rollout.flush()
while True:
    time.sleep(1)
"#;

/// Ends every stand-in this test started, also when it fails.
struct Cleanup(std::path::PathBuf);
impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = std::process::Command::new("pkill")
            .args(["-f", &self.0.display().to_string()])
            .status();
    }
}

fn script(path: &Path, text: &str) -> String {
    fs::write(path, text).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    path.display().to_string()
}

async fn local(node: &Node) -> Vec<Session> {
    match node
        .rpc(Request::List {
            sender: None,
            project: None,
        })
        .await
    {
        Reply::Sessions { sessions, .. } => sessions,
        other => panic!("{other:?}"),
    }
}
async fn live(node: &Node, name: &str) -> Session {
    eventually(&format!("{name} is live"), async || {
        local(node).await.into_iter().find(|s| s.name == name)
    })
    .await
}
fn error(reply: Reply) -> String {
    match reply {
        Reply::Error { message } => message,
        other => panic!("expected a refusal: {other:?}"),
    }
}

/* REQ-028: The user starts a root session in a project; the root session
starts a non-root session there (named {project}-{role}-{n}, with the role's
instructions as its first prompt and its role assigned), and closes it; root
cannot start or close root sessions, other sessions can do neither, and the
admin page acts only as the user. */
#[tokio::test]
async fn root_sessions_start_and_close_sessions() {
    let mut node = Node::new("sp", None);
    let home = node.home.path().to_path_buf();
    let tools = home.join("tools");
    let logs = home.join("logs");
    let work = home.join("work").join("mono");
    for dir in [
        &tools,
        &logs,
        &work,
        &home.join(".claude/prompts"),
        &home.join(".codex/sessions"),
    ] {
        fs::create_dir_all(dir).unwrap();
    }
    node.env.extend([
        (
            "SESSION_BRIDGE_TERMINAL".into(),
            script(&tools.join("terminal"), TERMINAL),
        ),
        (
            "SESSION_BRIDGE_CLAUDE_BIN".into(),
            script(&tools.join("claude"), CLAUDE),
        ),
        (
            "SESSION_BRIDGE_CODEX_BIN".into(),
            script(&tools.join("codex"), CODEX),
        ),
        ("SB_TEST_LOGS".into(), logs.display().to_string()),
    ]);
    let _cleanup = Cleanup(tools.clone());
    node.start().await;
    let page = AdminPage::open(&node).await;

    let roles = page.ok(serde_json::json!({"method": "roles"})).await;
    assert!(
        roles["roles"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["name"] == "root"),
        "{roles}"
    );
    page.ok(serde_json::json!({"method": "save_role", "name": "reviewer", "prompt": "Review the diff."}))
        .await;

    // The user starts the root session.
    let spawned = page
        .ok(serde_json::json!({"method": "spawn", "project": work, "agent": "claude", "role": "root"}))
        .await;
    assert_eq!(spawned["name"], "mono-root-1", "{spawned}");
    let root = live(&node, "mono-root-1").await;
    let first = fs::read_to_string(home.join(".claude/prompts/mono-root-1.txt")).unwrap();
    assert!(
        first.starts_with("Your role in this workspace: root."),
        "{first}"
    );
    assert_eq!(root.cwd, fs::canonicalize(&work).unwrap());

    // The admin page never acts as a session.
    let (_, refused) = page
        .call(serde_json::json!({"method": "spawn", "sender": root.session_id, "agent": "codex", "role": "reviewer"}))
        .await;
    assert!(
        refused["message"].as_str().unwrap().contains("as the user"),
        "{refused}"
    );

    // Root starts a reviewer in Codex: named through the launched process.
    let reply = node
        .rpc(Request::Spawn {
            sender: Some(root.session_id.clone()),
            project: None,
            agent: Agent::Codex,
            role: "reviewer".into(),
        })
        .await;
    assert!(
        matches!(&reply, Reply::Spawned { name, .. } if name == "mono-reviewer-1"),
        "{reply:?}"
    );
    let reviewer = live(&node, "mono-reviewer-1").await;
    assert_eq!(reviewer.agent, session_bridge::domain::Provider::Codex);
    let prompts = fs::read_to_string(home.join(".codex/prompts.txt")).unwrap();
    assert!(
        prompts.contains("Your role in this workspace: reviewer."),
        "{prompts}"
    );
    match node.rpc(Request::Roles).await {
        Reply::Roles { assignments, .. } => {
            assert_eq!(assignments["mono-root-1"], "root");
            assert_eq!(assignments["mono-reviewer-1"], "reviewer");
        }
        other => panic!("{other:?}"),
    }

    // A session that takes root's name is not root; a directory elsewhere
    // with the same name is another project.
    let elsewhere = home.join("elsewhere").join("mono");
    fs::create_dir_all(&elsewhere).unwrap();
    // Started here, so this test reaps them once they end.
    let mut strays: Vec<std::process::Child> = [(&work, "mono-root-1"), (&elsewhere, "far")]
        .into_iter()
        .map(|(dir, name)| {
            std::process::Command::new(tools.join("claude"))
                .args(["-n", name, "stand-in"])
                .current_dir(dir)
                .env("CLAUDE_CONFIG_DIR", home.join(".claude"))
                .stdout(std::process::Stdio::null())
                .spawn()
                .unwrap()
        })
        .collect();
    let impostor = eventually("the impostor is live", async || {
        local(&node)
            .await
            .into_iter()
            .find(|s| s.name == "mono-root-1" && s.session_id != root.session_id)
    })
    .await;
    let far = live(&node, "far").await;
    let message = error(
        node.rpc(Request::Close {
            sender: Some(impostor.session_id.clone()),
            session_id: reviewer.session_id.clone(),
        })
        .await,
    );
    assert!(
        message.contains("Only a session with the root role"),
        "{message}"
    );
    let message = error(
        node.rpc(Request::Close {
            sender: Some(root.session_id.clone()),
            session_id: far.session_id.clone(),
        })
        .await,
    );
    assert!(message.contains("its own project only"), "{message}");
    for session in [&impostor, &far] {
        assert!(matches!(
            node.rpc(Request::Close {
                sender: None,
                session_id: session.session_id.clone(),
            })
            .await,
            Reply::Closed { .. }
        ));
    }
    for child in &mut strays {
        child.wait().unwrap();
    }
    eventually("the impostor and far ended", async || {
        let ids: Vec<String> = local(&node)
            .await
            .into_iter()
            .map(|s| s.session_id)
            .collect();
        (!ids.contains(&impostor.session_id) && !ids.contains(&far.session_id)).then_some(())
    })
    .await;

    // Root starts no root session, a reviewer starts nothing.
    let message = error(
        node.rpc(Request::Spawn {
            sender: Some(root.session_id.clone()),
            project: None,
            agent: Agent::Claude,
            role: "root".into(),
        })
        .await,
    );
    assert!(
        message.contains("Only the user starts a root session"),
        "{message}"
    );
    let message = error(
        node.rpc(Request::Spawn {
            sender: Some(reviewer.session_id.clone()),
            project: None,
            agent: Agent::Claude,
            role: "reviewer".into(),
        })
        .await,
    );
    assert!(
        message.contains("Only a session with the root role"),
        "{message}"
    );
    let message = error(
        node.rpc(Request::Close {
            sender: Some(reviewer.session_id.clone()),
            session_id: root.session_id.clone(),
        })
        .await,
    );
    assert!(
        message.contains("Only a session with the root role"),
        "{message}"
    );

    // Root closes the reviewer, not itself.
    let message = error(
        node.rpc(Request::Close {
            sender: Some(root.session_id.clone()),
            session_id: root.session_id.clone(),
        })
        .await,
    );
    assert!(
        message.contains("Only the user closes a root session"),
        "{message}"
    );
    let closed = node
        .rpc(Request::Close {
            sender: Some(root.session_id.clone()),
            session_id: reviewer.session_id.clone(),
        })
        .await;
    assert!(
        matches!(&closed, Reply::Closed { name } if name == "mono-reviewer-1"),
        "{closed:?}"
    );
    eventually("the reviewer ended", async || {
        (!local(&node)
            .await
            .iter()
            .any(|s| s.session_id == reviewer.session_id))
        .then_some(())
    })
    .await;

    // The next reviewer takes the free number again; the user closes root.
    let again = page
        .ok(serde_json::json!({"method": "spawn", "project": "mono", "agent": "claude", "role": "reviewer"}))
        .await;
    assert_eq!(again["name"], "mono-reviewer-1", "{again}");
    live(&node, "mono-reviewer-1").await;
    let closed = page
        .ok(serde_json::json!({"method": "close", "session_id": root.session_id}))
        .await;
    assert_eq!(closed["name"], "mono-root-1");

    // Short names and the CLI.
    let settings = page
        .ok(serde_json::json!({"method": "save_spawn_settings", "project": "mono", "short_name": "mo"}))
        .await;
    assert_eq!(settings["short_names"]["mono"], "mo");
    let (_, bad) = page
        .call(serde_json::json!({"method": "save_spawn_settings", "project": "mono", "short_name": "m o"}))
        .await;
    assert_eq!(bad["result"], "error", "{bad}");
    let cli = node
        .cli(&[
            "spawn",
            "claude",
            "reviewer",
            "--project",
            work.to_str().unwrap(),
        ])
        .await;
    assert!(
        cli.status.success(),
        "{}",
        String::from_utf8_lossy(&cli.stderr)
    );
    assert!(String::from_utf8_lossy(&cli.stdout).contains("mo-reviewer-1"));
    let missing = node
        .cli(&["spawn", "claude", "nobody", "--project", "mono"])
        .await;
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("No role named nobody"));
    for session in local(&node).await {
        let _ = node
            .rpc(Request::Close {
                sender: None,
                session_id: session.session_id,
            })
            .await;
    }
    node.stop().await;
}
