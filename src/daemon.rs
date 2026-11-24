use crate::{
    discovery::{self, Session, Sources},
    domain::{self, Delivery, Provider, SessionId},
    protocol::{Reply, Request},
    store::{self, Envelope, Route, Store},
    transport::{self, PeerFrame},
};
use anyhow::{Context, Result};
use fs2::FileExt;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    net::UnixListener,
    sync::{mpsc, oneshot},
    task::JoinSet,
};
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct Config {
    pub state: PathBuf,
    pub sources: Sources,
    pub codex: PathBuf,
}
impl Config {
    pub fn socket(&self) -> PathBuf {
        self.state.join("control.sock")
    }
}
enum Event {
    Rpc(Request, oneshot::Sender<Reply>),
    Peer(Route, Box<PeerFrame>),
}
struct Bridge {
    config: Config,
    store: Store,
    messages: BTreeMap<Uuid, Envelope>,
    live: Vec<Session>,
    known: BTreeMap<String, Session>,
    bound: BTreeSet<PathBuf>,
    events: mpsc::Sender<Event>,
    listeners: JoinSet<Result<()>>,
    native_peers: crate::native_peers::NativePeers,
}
pub async fn run(config: Config) -> Result<()> {
    let store = Store::open(config.state.clone())?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(config.state.join("daemon.lock"))?;
    lock.try_lock_exclusive()
        .context("Another session-bridge daemon is running")?;
    let socket = config.socket();
    remove_stale_socket(&socket)?;
    let listener = UnixListener::bind(&socket)?;
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600))?;
    let (events, mut receiver) = mpsc::channel(128);
    let mut messages = BTreeMap::new();
    for mut envelope in store.messages()? {
        let recovered = domain::recover_delivery(&envelope.delivery);
        if recovered != envelope.delivery {
            envelope.delivery = recovered;
            store.save(&envelope)?;
        }
        messages.insert(envelope.message_id, envelope);
    }
    let known_file = config.state.join("sessions.json");
    let known = if known_file.exists() {
        store::read_json(&known_file)?
    } else {
        BTreeMap::new()
    };
    let mut bridge = Bridge {
        config,
        store,
        messages,
        live: Vec::new(),
        known,
        bound: BTreeSet::new(),
        events: events.clone(),
        listeners: JoinSet::new(),
        native_peers: crate::native_peers::NativePeers::default(),
    };
    bridge.refresh().await?;
    for route in bridge.store.routes()? {
        if let Err(e) = bridge.bind_route(route).await {
            eprintln!("Reply route awaits recovery: {e:#}");
        }
    }
    let mut interval = tokio::time::interval(Duration::from_secs(3));
    let mut handlers = JoinSet::new();
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    eprintln!(
        "session-bridge ready: {} ({} live sessions)",
        socket.display(),
        bridge.live.len()
    );
    loop {
        tokio::select! {
            _=tokio::signal::ctrl_c() => break,
            _=terminate.recv() => break,
            accepted=listener.accept()=>{
                let (mut stream,_)=accepted?;let events=events.clone();
                handlers.spawn(async move {
                    transport::verify_peer(&stream)?;
                    let request=match transport::read_frame(&mut stream).await {
                        Ok(request)=>request,
                        Err(e)=>{transport::write_frame(&mut stream,&Reply::Error{message:e.to_string()}).await?;return Ok::<_,anyhow::Error>(());}
                    };
                    let (reply,result)=oneshot::channel();
                    events.send(Event::Rpc(request,reply)).await?;
                    transport::write_frame(&mut stream,&result.await?).await
                });
            },
            Some(event)=receiver.recv()=> match event {
                Event::Rpc(request,response)=>{
                    let reply=bridge.handle_request(request).await.unwrap_or_else(|e|Reply::Error{message:format!("{e:#}")});
                    if response.send(reply).is_err(){eprintln!("Client disconnected before bridge response");}
                },
                Event::Peer(route,frame)=>if let Err(e)=bridge.handle_peer(route,*frame).await {eprintln!("Rejected peer message: {e:#}");},
            },
            _=interval.tick()=>{
                if let Err(e)=bridge.refresh().await {eprintln!("Session discovery failed; retained pending work: {e:#}");}
                for route in bridge.store.routes()? {
                    if !bridge.bound.contains(&route.socket)
                        && let Err(e)=bridge.bind_route(route).await {
                        eprintln!("Reply route recovery pending: {e:#}");
                    }
                }
                let pending:Vec<_>=bridge.messages.values().filter(|m|matches!(m.delivery,Delivery::Pending{..})).map(|m|m.message_id).collect();
                for id in pending {if let Err(e)=bridge.dispatch(id).await{eprintln!("Pending message {id}: {e:#}");}}
            },
            Some(result)=handlers.join_next()=>if let Err(e)=result.context("Client handler panicked").and_then(|r|r){eprintln!("Client request failed: {e:#}");},
            Some(result)=bridge.listeners.join_next()=>{result.context("Reply listener panicked")??;},
        }
    }
    handlers.abort_all();
    bridge.native_peers.shutdown().await?;
    bridge.listeners.abort_all();
    fs::remove_file(&socket)?;
    for path in bridge.bound {
        if path.exists() {
            fs::remove_file(path)?;
        }
    }
    drop(lock);
    Ok(())
}
fn remove_stale_socket(path: &Path) -> Result<()> {
    if fs::symlink_metadata(path).is_ok() {
        transport::verify_socket(path)?;
        fs::remove_file(path)?;
    }
    Ok(())
}
fn now() -> Result<u64> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
}
impl Bridge {
    async fn refresh(&mut self) -> Result<()> {
        let live = discovery::discover(&self.config.sources).await?;
        for session in &live {
            self.known
                .insert(session.session_id.clone(), session.clone());
        }
        store::write_json(&self.config.state.join("sessions.json"), &self.known)?;
        self.live = live.into_iter().filter(|s| !s.is_subagent()).collect();
        self.native_peers.sync(&self.config, &self.live).await?;
        Ok(())
    }
    fn project_scope(
        &self,
        sender: Option<&str>,
        project: Option<String>,
    ) -> Result<Option<String>> {
        let project = match (project, sender) {
            (Some(project), _) => Some(project),
            (None, Some(sender)) => Some(
                self.known
                    .get(sender)
                    .context("Cannot resolve caller project: sender session is not registered")?
                    .cwd
                    .to_str()
                    .context("Caller directory is not valid UTF-8")?
                    .to_owned(),
            ),
            (None, None) => None,
        };
        if let Some(project) = &project {
            domain::validate_project(project)?;
        }
        Ok(project)
    }
    async fn handle_request(&mut self, request: Request) -> Result<Reply> {
        match request {
            Request::NativePeer { recipient, frame } => {
                let recipient = SessionId::parse(&recipient)?;
                anyhow::ensure!(
                    recipient.provider == Provider::Codex,
                    "Native bridge peers target Codex only"
                );
                anyhow::ensure!(
                    self.known
                        .get(&recipient.to_string())
                        .is_some_and(|s| !s.is_subagent()),
                    "Unknown parent Codex recipient"
                );
                self.handle_peer(
                    Route {
                        socket: self.config.socket(),
                        recipient,
                    },
                    *frame,
                )
                .await?;
                Ok(Reply::Health {
                    pid: std::process::id(),
                })
            }
            Request::Health => Ok(Reply::Health {
                pid: std::process::id(),
            }),
            Request::List { sender, project } => {
                self.refresh().await?;
                let project = self.project_scope(sender.as_deref(), project)?;
                let mut recent_messages: Vec<crate::protocol::Receipt> = self
                    .messages
                    .values()
                    .filter(|m| sender.as_ref().is_some_and(|s| s == &m.sender.to_string()))
                    .filter(|m| {
                        project.as_ref().is_none_or(|p| {
                            self.known
                                .get(&m.recipient.to_string())
                                .is_some_and(|s| s.matches_project(p))
                        })
                    })
                    .map(Into::into)
                    .collect();
                recent_messages.sort_by_key(|m| std::cmp::Reverse(m.created_at));
                recent_messages.truncate(10);
                Ok(Reply::Sessions {
                    sessions: self
                        .live
                        .iter()
                        .filter(|s| project.as_ref().is_none_or(|p| s.matches_project(p)))
                        .cloned()
                        .collect(),
                    recent_messages,
                })
            }
            Request::Status => {
                let mut messages: Vec<crate::protocol::Receipt> =
                    self.messages.values().map(Into::into).collect();
                messages.sort_by_key(|m| std::cmp::Reverse(m.created_at));
                messages.truncate(100);
                Ok(Reply::Status {
                    pid: std::process::id(),
                    sessions: self.live.len(),
                    messages,
                })
            }
            Request::Send {
                sender,
                session_id,
                message,
                project,
            } => {
                domain::validate_message(&message)?;
                if let Some(project) = &project {
                    domain::validate_project(project)?;
                }
                let sender = SessionId::parse(&sender)?;
                let recipient = SessionId::parse(&session_id)?;
                anyhow::ensure!(sender != recipient, "Send to another session, not yourself");
                self.refresh().await?;
                anyhow::ensure!(
                    !self.involves_subagent(&sender, &recipient),
                    "Subagents are managed by their parent sessions and cannot use cross-session messaging"
                );
                anyhow::ensure!(
                    self.known.contains_key(&sender.to_string()),
                    "Sender session is not registered"
                );
                anyhow::ensure!(
                    self.known.contains_key(&recipient.to_string()),
                    "Recipient session is unknown; use list_sessions first"
                );
                let project = self.project_scope(Some(&sender.to_string()), project)?;
                if let Some(project) = &project {
                    anyhow::ensure!(
                        self.known
                            .get(&recipient.to_string())
                            .is_some_and(|s| s.matches_project(project)),
                        "Recipient does not match project filter: {project}"
                    );
                }
                let id = Uuid::new_v4();
                let envelope = Envelope {
                    message_id: id,
                    sender,
                    recipient,
                    message,
                    project,
                    delivery: Delivery::Pending {
                        reason: "Waiting for destination".into(),
                    },
                    created_at: now()?,
                };
                self.store.save(&envelope)?;
                self.messages.insert(id, envelope);
                self.dispatch(id).await?;
                Ok(Reply::Receipt {
                    receipt: self.messages.get(&id).context("Missing receipt")?.into(),
                })
            }
        }
    }
    async fn dispatch(&mut self, id: Uuid) -> Result<()> {
        let message = self.messages.get(&id).context("Unknown message")?.clone();
        if self.involves_subagent(&message.sender, &message.recipient) {
            return self.set_delivery(
                id,
                Delivery::Refused {
                    reason: "Subagents are managed by their parent sessions".into(),
                },
            );
        }
        let Some(target) = self
            .live
            .iter()
            .find(|s| s.session_id == message.recipient.to_string())
            .cloned()
        else {
            return Ok(());
        };
        if let Some(project) = &message.project
            && (domain::validate_project(project).is_err() || !target.matches_project(project))
        {
            return self.set_delivery(
                id,
                Delivery::Refused {
                    reason: format!("Recipient no longer matches project filter: {project}"),
                },
            );
        }
        let sender = self
            .known
            .get(&message.sender.to_string())
            .context("Missing sender metadata")?
            .clone();
        let text = transport::render_message(&message, &sender)?;
        let route = if let Some(socket) = &target.socket {
            let directory = socket.parent().context("Socket has no parent")?;
            let hash = Uuid::new_v5(&Uuid::NAMESPACE_URL, message.sender.to_string().as_bytes())
                .simple()
                .to_string();
            let native_return = self.native_peers.socket(&sender.session_id);
            let route = Route {
                socket: native_return
                    .clone()
                    .unwrap_or_else(|| directory.join(format!("{}.sock", &hash[..16]))),
                recipient: message.sender.clone(),
            };
            if native_return.is_none() {
                self.store.save_route(&route)?;
                self.bind_route(route.clone()).await?;
            }
            Some(route)
        } else {
            None
        };
        self.set_delivery(id, Delivery::Dispatching)?;
        let delivery = match message.recipient.provider {
            Provider::Codex => {
                crate::codex_sessions::queue(
                    &self.config.codex,
                    &self.config.sources.codex_home,
                    &message.recipient.id.to_string(),
                    &text,
                )
                .await
            }
            Provider::Claude => {
                transport::send_claude(
                    target.socket.as_deref().context("Claude has no socket")?,
                    &route.context("Claude has no return route")?.socket,
                    &message,
                    text,
                )
                .await
            }
        };
        self.set_delivery(id, delivery)
    }
    fn set_delivery(&mut self, id: Uuid, delivery: Delivery) -> Result<()> {
        let current = self.messages.get(&id).context("Unknown message receipt")?;
        if current.delivery == delivery {
            return Ok(());
        }
        let mut updated = current.clone();
        updated.delivery = delivery;
        self.store.save(&updated)?;
        self.messages.insert(id, updated);
        Ok(())
    }
    fn involves_subagent(&self, sender: &SessionId, recipient: &SessionId) -> bool {
        [sender, recipient].iter().any(|id| {
            self.known
                .get(&id.to_string())
                .is_some_and(Session::is_subagent)
        })
    }
    async fn bind_route(&mut self, route: Route) -> Result<()> {
        if self.bound.contains(&route.socket) {
            return Ok(());
        }
        store::ensure_private_dir(route.socket.parent().context("No reply socket directory")?)?;
        remove_stale_socket(&route.socket)?;
        let listener = UnixListener::bind(&route.socket)?;
        fs::set_permissions(&route.socket, fs::Permissions::from_mode(0o600))?;
        self.bound.insert(route.socket.clone());
        let events = self.events.clone();
        self.listeners.spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await?;
                let frame = async {
                    transport::verify_peer(&stream)?;
                    transport::read_frame::<PeerFrame>(&mut stream).await
                }
                .await;
                match frame {
                    Ok(frame) => {
                        events
                            .send(Event::Peer(route.clone(), Box::new(frame)))
                            .await?
                    }
                    Err(e) => eprintln!("Reply frame rejected: {e:#}"),
                }
            }
        });
        Ok(())
    }
    async fn handle_peer(&mut self, route: Route, frame: PeerFrame) -> Result<()> {
        self.refresh().await?;
        let from = frame
            .from
            .as_deref()
            .and_then(|v| v.strip_prefix("uds:"))
            .context("Missing peer return address")?;
        let sender = self
            .live
            .iter()
            .find(|s| s.agent == Provider::Claude && s.socket.as_deref() == Some(Path::new(from)))
            .context("Reply is not from a known live Claude session")?
            .clone();
        if frame.kind == "control" {
            anyhow::ensure!(
                frame.action.as_deref() == Some("peer_message_status"),
                "Unsupported peer control action"
            );
            let id = Uuid::parse_str(
                frame
                    .orig_msg_id
                    .as_deref()
                    .context("Uncorrelated peer status")?,
            )?;
            let original = self
                .messages
                .get(&id)
                .context("Unknown status correlation")?;
            anyhow::ensure!(
                original.sender == route.recipient
                    && original.recipient.to_string() == sender.session_id,
                "Status route mismatch"
            );
            let reason = frame
                .reason
                .unwrap_or_else(|| "Claude inbound policy".into());
            let delivery = match frame.status.as_deref() {
                Some("held") => Delivery::Held { reason },
                Some("delivered") => Delivery::Delivered,
                Some("refused" | "denied" | "expired" | "dropped") => Delivery::Refused { reason },
                _ => anyhow::bail!("Unsupported peer delivery status"),
            };
            self.set_delivery(id, delivery)?;
            return Ok(());
        }
        anyhow::ensure!(frame.kind == "user", "Unsupported peer message type");
        let message = frame
            .message
            .context("Peer message has no content")?
            .content;
        domain::validate_message(&message)?;
        let id = match frame.msg_id.or(frame.uuid) {
            Some(id) => Uuid::parse_str(&id)?,
            None => Uuid::new_v4(),
        };
        let sender = SessionId::parse(&sender.session_id)?;
        if let Some(existing) = self.messages.get(&id) {
            anyhow::ensure!(
                existing.sender == sender
                    && existing.recipient == route.recipient
                    && existing.message == message,
                "Duplicate message ID with different content"
            );
            return Ok(());
        }
        let envelope = Envelope {
            message_id: id,
            sender,
            recipient: route.recipient,
            message,
            project: None,
            delivery: Delivery::Pending {
                reason: "Reply awaits destination".into(),
            },
            created_at: now()?,
        };
        self.store.save(&envelope)?;
        self.messages.insert(id, envelope);
        self.dispatch(id).await
    }
}
