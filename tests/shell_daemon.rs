//! Running the bridge from a shell, without the installed service (REQ-021):
//! a separate state directory, clients that do not wake the service, and an
//! uninstall that never touches a service registered for another home.
use std::{
    path::{Path, PathBuf},
    process::{Output, Stdio},
    time::{Duration, Instant},
};
use tokio::{
    process::{Child, Command},
    time::sleep,
};

fn temp_root() -> PathBuf {
    // Unix socket paths must stay short.
    if cfg!(unix) {
        PathBuf::from("/tmp")
    } else {
        std::env::temp_dir()
    }
}
fn command(home: &Path, state: Option<&Path>) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_session-bridge"));
    command
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("CODEX_HOME", home.join(".codex"))
        .env("CLAUDE_CONFIG_DIR", home.join(".claude"))
        .env("SESSION_BRIDGE_NOTIFY", "0")
        .env_remove("SESSION_BRIDGE_STATE")
        .stdin(Stdio::null())
        .kill_on_drop(true);
    if let Some(state) = state {
        command.env("SESSION_BRIDGE_STATE", state);
    }
    command
}
async fn cli(home: &Path, state: Option<&Path>, args: &[&str]) -> Output {
    command(home, state).args(args).output().await.unwrap()
}
fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}
async fn stop(mut child: Child) {
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        unsafe {
            libc::kill(pid as i32, libc::SIGTERM);
        }
        if tokio::time::timeout(Duration::from_secs(10), child.wait())
            .await
            .is_ok()
        {
            return;
        }
    }
    child.kill().await.unwrap();
    child.wait().await.unwrap();
}

/* REQ-021: A bridge started from a shell with SESSION_BRIDGE_STATE serves
clients that name the same state; without it, clients fail at once with a
hint instead of waking the installed service. */
#[tokio::test]
async fn shell_daemon_serves_its_own_state() {
    let root = tempfile::Builder::new()
        .prefix("sb-sh-")
        .tempdir_in(temp_root())
        .unwrap();
    let home = root.path().join("home");
    std::fs::create_dir_all(home.join(".claude/sessions")).unwrap();
    std::fs::create_dir_all(home.join(".codex")).unwrap();
    let state = root.path().join("state");

    let started = Instant::now();
    let refused = cli(&home, Some(&state), &["status"]).await;
    assert!(!refused.status.success());
    assert!(
        text(&refused).contains(&format!(
            "No bridge is running for {}. Start one in a shell: session-bridge daemon",
            state.display()
        )),
        "{}",
        text(&refused)
    );
    assert!(started.elapsed() < Duration::from_secs(3), "fails at once");

    let relative = cli(&home, Some(Path::new("relative/state")), &["status"]).await;
    assert!(!relative.status.success());
    assert!(
        text(&relative).contains("SESSION_BRIDGE_STATE must be an absolute path"),
        "{}",
        text(&relative)
    );

    let daemon = command(&home, Some(&state))
        .arg("daemon")
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let mut status = None;
    for _ in 0..100 {
        let output = cli(&home, Some(&state), &["status"]).await;
        if output.status.success() {
            status = Some(text(&output));
            break;
        }
        sleep(Duration::from_millis(100)).await;
    }
    let status = status.expect("shell daemon answers");
    assert!(status.contains(r#""result": "status""#), "{status}");
    assert!(
        session_bridge::platform::control_endpoint(&state)
            .to_string_lossy()
            .contains(if cfg!(unix) {
                state.to_str().unwrap()
            } else {
                "session-bridge"
            })
    );
    assert!(
        state.join("daemon.lock").exists(),
        "state lives where asked"
    );
    assert!(
        !home
            .join(".local/state/session-bridge/daemon.lock")
            .exists(),
        "the default state is untouched"
    );
    stop(daemon).await;
    let after = cli(&home, Some(&state), &["status"]).await;
    assert!(!after.status.success(), "{}", text(&after));
}

/* REQ-021: Uninstall removes only a service registered for this home; with
none, it says so and leaves any other registration alone. */
#[tokio::test]
async fn uninstall_without_a_service_for_this_home() {
    let root = tempfile::Builder::new()
        .prefix("sb-un-")
        .tempdir_in(temp_root())
        .unwrap();
    let home = root.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let output = cli(&home, None, &["uninstall"]).await;
    assert!(output.status.success(), "{}", text(&output));
    assert!(
        text(&output).contains("No background service is installed for this home"),
        "{}",
        text(&output)
    );
}
