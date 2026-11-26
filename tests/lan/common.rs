//! Harness for LAN tests: real bridge daemons in private homes on this
//! machine, driven through the control endpoint and the CLI.
#![allow(dead_code)]
use session_bridge::{
    lan::model::LanView,
    protocol::{Reply, Request},
    transport,
};
use std::{
    path::{Path, PathBuf},
    process::{Output, Stdio},
    time::Duration,
};
use tokio::{
    process::{Child, Command},
    time::sleep,
};
use uuid::Uuid;

pub const PROJECT: &str = "mono";

pub struct Node {
    pub home: tempfile::TempDir,
    pub label: String,
    pub session: String,
    pub child: Option<Child>,
    /// Extra environment for the daemon and CLI (for example a stand-in crontab).
    pub env: Vec<(String, String)>,
}
impl Node {
    /// A bridge home with one live (fake) Claude session whose record points to
    /// `inbox`, when given.
    pub fn new(prefix: &str, inbox: Option<&Path>) -> Self {
        let builder = tempfile::Builder::new()
            .prefix("sb-lan-")
            .tempdir_in(temp_root());
        let home = builder.unwrap();
        let session = Uuid::new_v4();
        let records = home.path().join(".claude/sessions");
        std::fs::create_dir_all(&records).unwrap();
        std::fs::create_dir_all(home.path().join(".codex")).unwrap();
        if let Some(inbox) = inbox {
            let record = serde_json::json!({
                "pid": std::process::id(),
                "sessionId": session,
                "cwd": format!("/work/{PROJECT}"),
                "messagingSocketPath": inbox,
                "name": format!("{prefix}-session"),
                "status": "idle",
                "kind": "interactive"
            });
            std::fs::write(
                records.join("me.json"),
                serde_json::to_vec(&record).unwrap(),
            )
            .unwrap();
        }
        let label = format!("{prefix}-{}", &Uuid::new_v4().simple().to_string()[..8]);
        Self {
            home,
            label,
            session: format!("claude:{session}"),
            child: None,
            env: Vec::new(),
        }
    }
    pub fn control(&self) -> PathBuf {
        session_bridge::platform::control_endpoint(&self.state())
    }
    pub fn state(&self) -> PathBuf {
        self.home.path().join(".local/state/session-bridge")
    }
    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_session-bridge"));
        command
            .env("HOME", self.home.path())
            .env("USERPROFILE", self.home.path())
            .env("CODEX_HOME", self.home.path().join(".codex"))
            .env("CLAUDE_CONFIG_DIR", self.home.path().join(".claude"))
            .env("SESSION_BRIDGE_NOTIFY", "0")
            .envs(self.env.iter().map(|(k, v)| (k, v)))
            .kill_on_drop(true);
        command
    }
    pub async fn start(&mut self) {
        let mut child = self
            .command()
            .arg("daemon")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        for _ in 0..200 {
            if let Ok(Reply::Health { .. }) = self.try_rpc(&Request::Health).await {
                self.child = Some(child);
                return;
            }
            if let Some(status) = child.try_wait().unwrap() {
                panic!("Daemon exited: {status}");
            }
            sleep(Duration::from_millis(50)).await;
        }
        panic!("Daemon did not start")
    }
    /// Stops the daemon the way the service manager does (SIGTERM on Unix),
    /// so it shuts down cleanly; kills it only if it does not exit.
    pub async fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
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
    }
    async fn try_rpc(&self, request: &Request) -> anyhow::Result<Reply> {
        let mut stream = transport::connect(&self.control()).await?;
        transport::write_frame(&mut stream, request).await?;
        transport::read_frame(&mut stream).await
    }
    pub async fn rpc(&self, request: Request) -> Reply {
        self.try_rpc(&request).await.unwrap()
    }
    pub async fn lan(&self) -> LanView {
        match self.rpc(Request::Lan).await {
            Reply::Lan { lan } => lan,
            other => panic!("Expected LAN view, got {other:?}"),
        }
    }
    /// Turns networking on and returns the listening port.
    pub async fn network_on(&self, port: u16) -> u16 {
        match self
            .rpc(Request::Network {
                enabled: true,
                port: Some(port),
                name: Some(self.label.clone()),
                local_only: None,
            })
            .await
        {
            Reply::Lan { lan } => lan.network.port.expect("listening port"),
            other => panic!("Network on failed: {other:?}"),
        }
    }
    pub async fn cli(&self, args: &[&str]) -> Output {
        self.command()
            .args(args)
            .stdin(Stdio::null())
            .output()
            .await
            .unwrap()
    }
    pub fn cli_command(&self, args: &[&str]) -> Command {
        let mut command = self.command();
        command.args(args);
        command
    }
    /// Pairs `self` (initiator) with `other` through the control API.
    pub async fn pair_with(&self, other: &Node, host: &str) {
        let id = match self.rpc(Request::Pair { host: host.into() }).await {
            Reply::Pairing { id, .. } => id,
            other => panic!("Pair failed: {other:?}"),
        };
        let code = other.wait_code().await;
        match self.rpc(Request::PairCode { id, code }).await {
            Reply::Paired { host, .. } => assert_eq!(host, other.label),
            other => panic!("Pair code failed: {other:?}"),
        }
    }
    /// The code an incoming request shows on this bridge.
    pub async fn wait_code(&self) -> String {
        for _ in 0..200 {
            if let Some(incoming) = self.lan().await.pairing.incoming {
                return incoming.code;
            }
            sleep(Duration::from_millis(50)).await;
        }
        panic!("No pairing code appeared")
    }
    pub async fn sessions(&self) -> (Vec<session_bridge::discovery::Session>, Vec<String>) {
        match self
            .rpc(Request::List {
                sender: Some(self.session.clone()),
                project: Some(PROJECT.into()),
            })
            .await
        {
            Reply::Sessions {
                sessions,
                unavailable_hosts,
                ..
            } => (sessions, unavailable_hosts),
            other => panic!("List failed: {other:?}"),
        }
    }
}

/// The admin page of a running bridge, called over plain HTTP.
pub struct AdminPage {
    pub port: u16,
    pub token: String,
}
impl AdminPage {
    pub async fn open(node: &Node) -> Self {
        let url = match node.rpc(Request::Admin).await {
            Reply::Admin { url } => url,
            other => panic!("Admin failed: {other:?}"),
        };
        let rest = url.strip_prefix("http://127.0.0.1:").unwrap();
        let (port, token) = rest.split_once("/#token=").unwrap();
        Self {
            port: port.parse().unwrap(),
            token: token.into(),
        }
    }
    /// Posts a control request; returns the HTTP status and the JSON body (or null).
    pub async fn call(&self, body: serde_json::Value) -> (u16, serde_json::Value) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let body = body.to_string();
        let request = format!(
            "POST /api/call HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nAuthorization: Bearer {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            self.port,
            self.token,
            body.len()
        );
        let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", self.port))
            .await
            .unwrap();
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).await.unwrap();
        let status = response[9..12].parse().unwrap();
        let json = response
            .split_once("\r\n\r\n")
            .and_then(|(_, b)| serde_json::from_str(b).ok())
            .unwrap_or(serde_json::Value::Null);
        (status, json)
    }
    /// Calls and expects HTTP 200 with a non-error reply.
    pub async fn ok(&self, body: serde_json::Value) -> serde_json::Value {
        let (status, reply) = self.call(body.clone()).await;
        assert_eq!(status, 200, "{body} -> {reply}");
        assert_ne!(reply["result"], "error", "{body} -> {reply}");
        reply
    }
}

fn temp_root() -> PathBuf {
    // Unix socket paths must stay short.
    if cfg!(unix) {
        PathBuf::from("/tmp")
    } else {
        std::env::temp_dir()
    }
}

/// A code that differs from `code` in its last digit.
pub fn wrong(code: &str) -> String {
    let mut digits: Vec<char> = code.chars().collect();
    let last = digits.last_mut().unwrap();
    *last = if *last == '0' { '1' } else { '0' };
    digits.into_iter().collect()
}

pub async fn eventually<T>(what: &str, mut probe: impl AsyncFnMut() -> Option<T>) -> T {
    for _ in 0..400 {
        if let Some(value) = probe().await {
            return value;
        }
        sleep(Duration::from_millis(50)).await;
    }
    panic!("Timed out waiting for {what}")
}
