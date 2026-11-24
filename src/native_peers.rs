use crate::{
    client,
    daemon::Config,
    discovery::Session,
    domain::{Provider, SessionId},
    protocol::Request,
    store,
    transport::{self, PeerFrame},
};
use anyhow::{Context, Result};
use serde::Serialize;
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::Stdio,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixListener,
    process::{Child, Command},
    task::JoinSet,
};

#[derive(Default)]
pub struct NativePeers {
    children: BTreeMap<String, (Session, Child)>,
}
impl NativePeers {
    pub fn socket(&self, session_id: &str) -> Option<PathBuf> {
        self.children
            .get(session_id)
            .and_then(|(_, child)| child.id())
            .map(socket_path)
    }
    pub async fn sync(&mut self, config: &Config, sessions: &[Session]) -> Result<()> {
        let desired: BTreeMap<_, _> = sessions
            .iter()
            .filter(|s| s.agent == Provider::Codex && !s.is_subagent())
            .map(|s| (s.session_id.clone(), s))
            .collect();
        let mut remove = Vec::new();
        for (id, (registered, child)) in &mut self.children {
            if child.try_wait()?.is_some()
                || desired.get(id).is_none_or(|s| {
                    s.pid != registered.pid
                        || s.cwd != registered.cwd
                        || s.project != registered.project
                })
            {
                remove.push(id.clone());
            }
        }
        for id in remove {
            if let Some((_, mut child)) = self.children.remove(&id) {
                stop(&mut child).await?;
            }
        }
        for (id, session) in desired {
            if self.children.contains_key(&id) {
                continue;
            }
            let mut child = Command::new(std::env::current_exe()?)
                .arg("native-peer")
                .env("CODEX_HOME", &config.sources.codex_home)
                .env("CLAUDE_CONFIG_DIR", &config.sources.claude_home)
                .env("SESSION_BRIDGE_CODEX_BIN", &config.codex)
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::inherit())
                .kill_on_drop(true)
                .spawn()?;
            let input = PeerInput {
                session: session.clone(),
                owner_pid: session.pid,
            };
            child
                .stdin
                .take()
                .context("Missing native peer input")?
                .write_all(&serde_json::to_vec(&input)?)
                .await?;
            let pid = child.id().context("Native peer exited during startup")?;
            let record = config
                .sources
                .claude_home
                .join(format!("sessions/{pid}.json"));
            tokio::time::timeout(Duration::from_secs(5), async {
                while !record.exists() {
                    anyhow::ensure!(
                        child.try_wait()?.is_none(),
                        "Native peer exited before registration"
                    );
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                Ok::<_, anyhow::Error>(())
            })
            .await
            .context("Native peer registration timed out")??;
            self.children.insert(id, (session.clone(), child));
        }
        Ok(())
    }
    pub async fn shutdown(&mut self) -> Result<()> {
        for (_, child) in self.children.values_mut() {
            stop(child).await?;
        }
        self.children.clear();
        Ok(())
    }
}
fn socket_path(pid: u32) -> PathBuf {
    PathBuf::from(format!("/tmp/session-bridge-{}/{pid}.sock", unsafe {
        libc::geteuid()
    }))
}
async fn stop(child: &mut Child) -> Result<()> {
    if child.try_wait()?.is_some() {
        return Ok(());
    }
    if let Some(pid) = child.id() {
        unsafe {
            libc::kill(pid as i32, libc::SIGTERM);
        }
    }
    match tokio::time::timeout(Duration::from_secs(5), child.wait()).await {
        Ok(result) => {
            result?;
        }
        Err(_) => {
            child.kill().await?;
        }
    }
    Ok(())
}
#[derive(serde::Serialize, serde::Deserialize)]
pub struct PeerInput {
    pub session: Session,
    pub owner_pid: u32,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct NativeRecord {
    pid: u32,
    session_id: String,
    cwd: PathBuf,
    started_at: u128,
    proc_start: String,
    pid_domain: &'static str,
    peer_protocol: u8,
    peer_features: Vec<&'static str>,
    kind: &'static str,
    entrypoint: &'static str,
    session_bridge: bool,
    name: String,
    name_source: &'static str,
    messaging_socket_path: PathBuf,
}
pub async fn serve(config: Config) -> Result<()> {
    let mut raw = Vec::new();
    tokio::io::stdin()
        .take(transport::FRAME_LIMIT)
        .read_to_end(&mut raw)
        .await?;
    let input: PeerInput = serde_json::from_slice(&raw)?;
    let id = SessionId::parse(&input.session.session_id)?;
    anyhow::ensure!(
        id.provider == Provider::Codex && !input.session.is_subagent(),
        "Only parent Codex sessions can be native peers"
    );
    let parent = unsafe { libc::getppid() };
    let pid = std::process::id();
    // A real helper owns each inbox: Claude checks the connected process PID.
    let socket_dir = PathBuf::from(format!("/tmp/session-bridge-{}", unsafe {
        libc::geteuid()
    }));
    store::ensure_private_dir(&socket_dir)?;
    let socket = socket_path(pid);
    let listener = UnixListener::bind(&socket)?;
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600))?;
    let records = config.sources.claude_home.join("sessions");
    fs::create_dir_all(&records)?;
    let record_path = records.join(format!("{pid}.json"));
    anyhow::ensure!(!record_path.exists(), "Native peer record already exists");
    let process = Command::new("/bin/ps")
        .args(["-o", "lstart=", "-p", &pid.to_string()])
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .output()
        .await?;
    anyhow::ensure!(
        process.status.success(),
        "Cannot read native peer process start"
    );
    let record = NativeRecord {
        pid,
        session_id: id.id.to_string(),
        cwd: input.session.cwd,
        started_at: SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis(),
        proc_start: String::from_utf8(process.stdout)?.trim().to_owned(),
        pid_domain: "darwin",
        peer_protocol: 1,
        peer_features: vec!["reply_across_default_dirs"],
        kind: "interactive",
        entrypoint: "session-bridge",
        session_bridge: true,
        name: format!(
            "codex-{}-{}",
            input.session.project.chars().take(32).collect::<String>(),
            &id.id.to_string()[..8]
        ),
        name_source: "derived",
        messaging_socket_path: socket.clone(),
    };
    store::write_json(&record_path, &record)?;
    let result = run_inbox(config, listener, id.to_string(), parent, input.owner_pid).await;
    for path in [record_path, socket] {
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => eprintln!("Native peer cleanup {}: {e}", path.display()),
        }
    }
    result
}
async fn run_inbox(
    config: Config,
    listener: UnixListener,
    recipient: String,
    parent: i32,
    owner: u32,
) -> Result<()> {
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut interval = tokio::time::interval(Duration::from_secs(1));
    let mut handlers = JoinSet::new();
    loop {
        tokio::select! {
            _ = terminate.recv() => break,
            _ = interval.tick() => {
                if unsafe { libc::getppid() } != parent || !crate::discovery::is_alive(owner) { break; }
            },
            accepted = listener.accept() => {
                let (mut stream, _) = accepted?;
                let config = config.clone();
                let recipient = recipient.clone();
                handlers.spawn(async move {
                    transport::verify_peer(&stream)?;
                    // Native discovery probes connect and close without sending a frame.
                    let Some(frame) = transport::read_optional_frame::<PeerFrame>(&mut stream).await? else {
                        return Ok::<_, anyhow::Error>(());
                    };
                    client::call(&config, &Request::NativePeer { recipient, frame: Box::new(frame) }).await?;
                    Ok(())
                });
            },
            Some(result) = handlers.join_next() => {
                if let Err(error) = result.context("Native peer handler panicked").and_then(|r|r) {
                    eprintln!("Native peer rejected message: {error:#}");
                }
            }
        }
    }
    handlers.abort_all();
    Ok(())
}
