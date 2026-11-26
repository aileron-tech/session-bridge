//! Updates from a release (REQ-023), against a local server that plays the
//! GitHub release API: never the real one, never the installed service.
use axum::{Router, extract::State, http::StatusCode, response::IntoResponse, routing::get};
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    process::{Output, Stdio},
    sync::Arc,
};
use tokio::{net::TcpListener, process::Command};

fn asset() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "session-bridge-aarch64-apple-darwin",
        ("macos", "x86_64") => "session-bridge-x86_64-apple-darwin",
        ("windows", "x86_64") => "session-bridge-x86_64-pc-windows-msvc.exe",
        other => panic!("no release asset for {other:?}"),
    }
}
fn executable() -> &'static str {
    if cfg!(windows) {
        "session-bridge.exe"
    } else {
        "session-bridge"
    }
}

struct Release {
    tag: String,
    binary: Vec<u8>,
    sums: String,
}
async fn latest(State(release): State<Arc<Release>>) -> impl IntoResponse {
    serde_json::json!({
        "tag_name": release.tag,
        "assets": [
            {"name": asset(), "browser_download_url": format!("/{}", asset())},
            {"name": "SHA256SUMS", "browser_download_url": "/SHA256SUMS"}
        ]
    })
    .to_string()
}
async fn binary(State(release): State<Arc<Release>>) -> impl IntoResponse {
    release.binary.clone()
}
async fn sums(State(release): State<Arc<Release>>) -> impl IntoResponse {
    release.sums.clone()
}

/// Serves `release`; asset URLs in the listing are made absolute here.
async fn serve(release: Release) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = format!("http://{}", listener.local_addr().unwrap());
    let base = address.clone();
    let release = Arc::new(release);
    let app = Router::new()
        .route(
            "/latest",
            get(move |state: State<Arc<Release>>| {
                let base = base.clone();
                async move {
                    let body = latest(state).await.into_response();
                    let bytes = axum::body::to_bytes(body.into_body(), 1 << 20)
                        .await
                        .unwrap();
                    let text = String::from_utf8(bytes.to_vec())
                        .unwrap()
                        .replace("\"/", &format!("\"{base}/"));
                    (StatusCode::OK, text)
                }
            }),
        )
        .route(&format!("/{}", asset()), get(binary))
        .route("/SHA256SUMS", get(sums))
        .with_state(release);
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("{address}/latest")
}

fn home() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let root = tempfile::Builder::new().prefix("sb-up-").tempdir().unwrap();
    let home = root.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let state = root.path().join("state");
    (root, home, state)
}
async fn cli(home: &Path, _state: &Path, releases: &str, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_session-bridge"))
        .args(args)
        .env("HOME", home)
        .env("USERPROFILE", home)
        // The default state of a temporary home: update refuses a separate
        // bridge, and this home has no service of its own.
        .env_remove("SESSION_BRIDGE_STATE")
        .env("SESSION_BRIDGE_RELEASES_URL", releases)
        .stdin(Stdio::null())
        .output()
        .await
        .unwrap()
}
fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}
fn sums_for(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    format!("{hex}  {}\n", asset())
}

/* REQ-023: A newer release is reported, downloaded, checked against
SHA256SUMS and installed as the executable in ~/.local/bin. */
#[tokio::test]
async fn installs_a_newer_release() {
    let binary = b"new session-bridge build".to_vec();
    let releases = serve(Release {
        tag: "v99.0.0".into(),
        sums: sums_for(&binary),
        binary: binary.clone(),
    })
    .await;
    let (_root, home, state) = home();
    let checked = cli(&home, &state, &releases, &["update", "--check"]).await;
    assert!(checked.status.success(), "{}", text(&checked));
    assert!(
        text(&checked).contains(&format!(
            "Update available: 99.0.0 (running {})",
            env!("CARGO_PKG_VERSION")
        )),
        "{}",
        text(&checked)
    );
    let updated = cli(&home, &state, &releases, &["update"]).await;
    assert!(updated.status.success(), "{}", text(&updated));
    let installed = home.join(".local").join("bin").join(executable());
    assert!(
        text(&updated).contains(&format!("Updated {} to 99.0.0", installed.display())),
        "{}",
        text(&updated)
    );
    assert_eq!(std::fs::read(&installed).unwrap(), binary);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&installed).unwrap().permissions().mode();
        assert_eq!(mode & 0o111, 0o111, "executable");
    }
}

/* REQ-023: A binary whose digest differs from SHA256SUMS is not installed. */
#[tokio::test]
async fn refuses_a_binary_with_a_wrong_checksum() {
    let releases = serve(Release {
        tag: "v99.0.0".into(),
        sums: sums_for(b"what was published"),
        binary: b"something else".to_vec(),
    })
    .await;
    let (_root, home, state) = home();
    let installed = home.join(".local").join("bin").join(executable());
    std::fs::create_dir_all(installed.parent().unwrap()).unwrap();
    std::fs::write(&installed, b"current").unwrap();
    let output = cli(&home, &state, &releases, &["update"]).await;
    assert!(!output.status.success());
    assert!(
        text(&output).contains(&format!("Checksum mismatch for {}", asset())),
        "{}",
        text(&output)
    );
    assert_eq!(std::fs::read(&installed).unwrap(), b"current");
    assert!(
        !home
            .join(".local/state/session-bridge/update")
            .join(asset())
            .exists(),
        "deleted"
    );
}

/* REQ-023: The running version, or a newer one, is up to date. */
#[tokio::test]
async fn reports_up_to_date() {
    let releases = serve(Release {
        tag: format!("v{}", env!("CARGO_PKG_VERSION")),
        sums: String::new(),
        binary: Vec::new(),
    })
    .await;
    let (_root, home, state) = home();
    for args in [&["update", "--check"][..], &["update"][..]] {
        let output = cli(&home, &state, &releases, args).await;
        assert!(output.status.success(), "{}", text(&output));
        assert!(
            text(&output).contains(&format!(
                "session-bridge {} is up to date",
                env!("CARGO_PKG_VERSION")
            )),
            "{}",
            text(&output)
        );
    }
    assert!(!home.join(".local").join("bin").join(executable()).exists());
}
