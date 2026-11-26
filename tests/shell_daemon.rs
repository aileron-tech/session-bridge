//! Running the bridge from a shell, without the installed service (REQ-021):
//! a separate state directory, clients that do not wake the service, and an
//! uninstall that never touches a service registered for another home.
use std::{
    path::{Path, PathBuf},
    process::{Output, Stdio},
    time::Duration,
};
use tokio::{process::Command, time::sleep};

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

    // This message comes only from the immediate refusal; waking the service
    // would end with "did not recover within 5 seconds" instead.
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
    // REQ-022: `stop` ends a bridge started from a shell.
    let stopped = cli(&home, Some(&state), &["stop"]).await;
    assert!(stopped.status.success(), "{}", text(&stopped));
    assert!(
        text(&stopped).contains(&format!("Stopped the bridge for {}", state.display())),
        "{}",
        text(&stopped)
    );
    let mut daemon = daemon;
    let exited = tokio::time::timeout(Duration::from_secs(10), daemon.wait()).await;
    assert!(
        exited.is_ok_and(|status| status.is_ok_and(|s| s.success())),
        "the bridge exits cleanly"
    );
    let after = cli(&home, Some(&state), &["status"]).await;
    assert!(!after.status.success(), "{}", text(&after));
    let again = cli(&home, Some(&state), &["stop"]).await;
    assert!(
        text(&again).contains("No bridge is running"),
        "{}",
        text(&again)
    );
}

/* REQ-021: A separate bridge (SESSION_BRIDGE_STATE) never installs,
uninstalls or updates the service, which belongs to the default state. */
#[tokio::test]
async fn separate_bridge_leaves_the_service_alone() {
    let root = tempfile::Builder::new()
        .prefix("sb-sp-")
        .tempdir_in(temp_root())
        .unwrap();
    let home = root.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let state = root.path().join("state");
    for command in [&["install"][..], &["uninstall"][..], &["update"][..]] {
        let output = cli(&home, Some(&state), command).await;
        assert!(!output.status.success(), "{command:?}: {}", text(&output));
        assert!(
            text(&output)
                .contains("manage the service of the default state; unset SESSION_BRIDGE_STATE"),
            "{command:?}: {}",
            text(&output)
        );
    }
    assert!(!home.join(".local").exists(), "nothing was installed");
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

/* REQ-022: With SESSION_BRIDGE_TRAY=1 the bridge shows its menu-bar icon,
whose menu reflects the bridge; stopping the bridge removes the icon. */
#[cfg(target_os = "macos")]
#[tokio::test]
async fn bridge_shows_its_menu_bar_icon() {
    use tokio::io::{AsyncBufReadExt, BufReader};
    let root = tempfile::Builder::new()
        .prefix("sb-tr-")
        .tempdir_in(temp_root())
        .unwrap();
    let home = root.path().join("home");
    std::fs::create_dir_all(home.join(".claude/sessions")).unwrap();
    std::fs::create_dir_all(home.join(".codex")).unwrap();
    let state = root.path().join("state");
    let mut daemon = command(&home, Some(&state))
        .env("SESSION_BRIDGE_TRAY", "1")
        .arg("daemon")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(daemon.stderr.take().unwrap()).lines();
    let menu = tokio::time::timeout(Duration::from_secs(20), async {
        while let Some(line) = lines.next_line().await.unwrap() {
            if line.starts_with("Menu bar: ") {
                return line;
            }
        }
        panic!("the bridge ended before showing its menu")
    })
    .await
    .expect("menu shown");
    assert_eq!(
        menu,
        format!(
            "Menu bar: session-bridge {}: running, 0 sessions | Networking off; separate bridge from a shell | - | [Open admin] | - | [Check for updates] | - | [Shut down...]",
            env!("CARGO_PKG_VERSION")
        )
    );
    let tray = std::process::Command::new("/usr/bin/pgrep")
        .args(["-P", &daemon.id().unwrap().to_string()])
        .output()
        .unwrap();
    let tray: u32 = String::from_utf8_lossy(&tray.stdout)
        .trim()
        .parse()
        .expect("one icon process under the bridge");
    let stopped = cli(&home, Some(&state), &["stop"]).await;
    assert!(stopped.status.success(), "{}", text(&stopped));
    tokio::time::timeout(Duration::from_secs(10), daemon.wait())
        .await
        .expect("bridge exits")
        .unwrap();
    let mut gone = false;
    for _ in 0..50 {
        if !session_bridge::platform::is_alive(tray) {
            gone = true;
            break;
        }
        sleep(Duration::from_millis(100)).await;
    }
    assert!(gone, "the icon process ends with its bridge");
}
