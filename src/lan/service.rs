//! LAN runtime: the listener for paired bridges, the Zeroconf announcement,
//! both sides of one-time-code pairing, and the client calls the daemon uses
//! to list, forward and report status. Message state stays in the daemon:
//! forwards and status updates reach it as [`Inbound`] events.
use super::{
    channel::{Channel, StaticKey},
    mdns::Announcer,
    model::{
        Attempt, CODE_LIFETIME, Fingerprint, ForwardOutcome, GateError, HostLabel, LanView,
        NetworkView, PairedPeer, PairingCode, PairingGate, Target, label_conflict, merge_hosts,
        order_addresses, resolve_target,
    },
    notify,
    pake::{self, Role},
    state::{DEFAULT_PORT, LanStore, NetworkSettings},
    wire::{ForwardedMessage, PeerReply, PeerRequest},
};
use crate::{
    discovery::Session,
    domain::{self, Delivery},
    usage::{self, HostState, RemoteSample, sampler::Sampler},
};
use anyhow::{Context, Result, anyhow};
use data_encoding::BASE64;
use std::{
    collections::BTreeMap,
    net::SocketAddr,
    path::Path,
    sync::{Arc, Mutex, MutexGuard},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::{Mutex as AsyncMutex, mpsc, oneshot, watch},
    task::{JoinHandle, JoinSet},
    time::timeout,
};
use uuid::Uuid;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const REPLY_TIMEOUT: Duration = Duration::from_secs(15);
/// A paired bridge that answered within this window counts as online, also
/// when neither Zeroconf nor a tunnel shows it (paired by address, a VPN).
const ONLINE_WINDOW: Duration = Duration::from_secs(60);
/// A usage answer is worth less than a page that waits for it.
const USAGE_TIMEOUT: Duration = Duration::from_secs(3);

/// Work for the daemon from a paired bridge.
pub enum Inbound {
    Forward {
        from: PairedPeer,
        message: ForwardedMessage,
        reply: oneshot::Sender<PeerReply>,
    },
    Status {
        from: PairedPeer,
        message_id: Uuid,
        delivery: Delivery,
    },
}

/// A pairing this bridge started, waiting for the code the other one shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutgoingPairing {
    pub id: Uuid,
    pub host: String,
    pub expires_in: u64,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PairStep {
    Paired { host: String, key: String },
    WrongCode { attempts_left: u8 },
}

struct Running {
    label: HostLabel,
    port: u16,
    local_only: bool,
    listener: JoinHandle<()>,
    announcer: Option<Announcer>,
}
impl Running {
    /// Stops listening; returns once the listening socket is closed, so the
    /// port can be bound again at once.
    async fn stop(self) {
        self.listener.abort();
        if let Err(e) = self.listener.await
            && !e.is_cancelled()
        {
            eprintln!("Bridge listener ended with an error: {e}");
        }
        if let Some(announcer) = self.announcer {
            announcer.stop();
        }
    }
    /// Already serving these settings (port 0 accepts any port).
    fn serves(&self, settings: &NetworkSettings) -> bool {
        self.label == settings.label
            && self.local_only == settings.local_only
            && (settings.port == 0 || settings.port == self.port)
    }
}
struct Outgoing {
    channel: Channel<TcpStream>,
    address: SocketAddr,
    expires_at: u64,
}

/// A paired bridge's last usage answer and the query asking it now, if any.
#[derive(Default)]
struct RemoteEntry {
    last: Option<(Instant, RemoteSample)>,
    asking: Option<Instant>,
}

struct Inner {
    store: LanStore,
    key: StaticKey,
    fingerprint: Fingerprint,
    notify: bool,
    inbound: mpsc::Sender<Inbound>,
    live: watch::Receiver<Vec<Session>>,
    usage: Sampler,
    /// Paired bridges' last usage answers, by key (REQ-029).
    remote_usage: Mutex<BTreeMap<Fingerprint, RemoteEntry>>,
    /// When each paired bridge last completed a connection with its key, either way.
    seen: Mutex<BTreeMap<Fingerprint, Instant>>,
    online_window: Duration,
    running: AsyncMutex<Option<Running>>,
    gate: Mutex<PairingGate>,
    outgoing: AsyncMutex<BTreeMap<Uuid, Outgoing>>,
}

#[derive(Clone)]
pub struct Lan(Arc<Inner>);

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}
fn random_u32() -> Result<u32> {
    let mut bytes = [0u8; 4];
    getrandom::fill(&mut bytes).map_err(|e| anyhow!("No system randomness: {e}"))?;
    Ok(u32::from_be_bytes(bytes))
}
fn refuse(message: impl Into<String>) -> PeerReply {
    PeerReply::Error {
        message: message.into(),
    }
}
fn unexpected(reply: PeerReply) -> anyhow::Error {
    match reply {
        PeerReply::Error { message } => anyhow!(message),
        other => anyhow!("Unexpected answer from the other bridge: {other:?}"),
    }
}

impl Lan {
    /// `live` carries this laptop's parent sessions; `inbound` reaches the
    /// daemon; `usage` answers paired bridges' usage requests (REQ-029).
    pub fn open(
        state: &Path,
        notify: bool,
        inbound: mpsc::Sender<Inbound>,
        live: watch::Receiver<Vec<Session>>,
        usage: Sampler,
    ) -> Result<Self> {
        let store = LanStore::open(state)?;
        let key = store.key()?;
        Ok(Self(Arc::new(Inner {
            fingerprint: key.fingerprint(),
            store,
            key,
            notify,
            inbound,
            live,
            usage,
            remote_usage: Mutex::new(BTreeMap::new()),
            seen: Mutex::new(BTreeMap::new()),
            online_window: ONLINE_WINDOW,
            running: AsyncMutex::new(None),
            gate: Mutex::new(PairingGate::default()),
            outgoing: AsyncMutex::new(BTreeMap::new()),
        })))
    }
    fn gate(&self) -> MutexGuard<'_, PairingGate> {
        match self.0.gate.lock() {
            Ok(gate) => gate,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
    fn seen(&self) -> MutexGuard<'_, BTreeMap<Fingerprint, Instant>> {
        match self.0.seen.lock() {
            Ok(seen) => seen,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
    /// A paired bridge proved it is there: it completed a connection with its key.
    fn saw(&self, peer: &Fingerprint) {
        self.seen().insert(*peer, Instant::now());
    }
    fn settings(&self) -> Result<NetworkSettings> {
        Ok(self.0.store.settings()?.unwrap_or_else(|| NetworkSettings {
            enabled: false,
            port: DEFAULT_PORT,
            label: HostLabel::from_machine_name(&gethostname::gethostname().to_string_lossy()),
            local_only: false,
        }))
    }
    /// Starts networking when the saved switch is on.
    pub async fn start_saved(&self) -> Result<()> {
        let settings = self.settings()?;
        if settings.enabled {
            self.start(&settings).await?;
        }
        Ok(())
    }
    async fn start(&self, settings: &NetworkSettings) -> Result<u16> {
        let mut running = self.0.running.lock().await;
        if let Some(previous) = running.take() {
            if previous.serves(settings) {
                let port = previous.port;
                *running = Some(previous);
                return Ok(port);
            }
            previous.stop().await;
        }
        let address = if settings.local_only {
            "127.0.0.1"
        } else {
            "0.0.0.0"
        };
        let listener = TcpListener::bind((address, settings.port))
            .await
            .with_context(|| format!("Cannot listen on port {}", settings.port))?;
        let port = listener.local_addr()?.port();
        let announcer = if settings.local_only {
            None
        } else {
            match Announcer::start(&settings.label, &self.0.fingerprint, port) {
                Ok(announcer) => Some(announcer),
                Err(e) => {
                    eprintln!(
                        "Network discovery unavailable; pairing by address still works: {e:#}"
                    );
                    None
                }
            }
        };
        let lan = self.clone();
        *running = Some(Running {
            label: settings.label.clone(),
            port,
            local_only: settings.local_only,
            listener: tokio::spawn(lan.serve(listener)),
            announcer,
        });
        eprintln!("Networking on: {} listening on port {port}", settings.label);
        Ok(port)
    }
    pub async fn stop(&self) {
        if let Some(running) = self.0.running.lock().await.take() {
            running.stop().await;
        }
    }
    async fn identity(&self) -> Result<(HostLabel, u16)> {
        self.0
            .running
            .lock()
            .await
            .as_ref()
            .map(|r| (r.label.clone(), r.port))
            .context("Networking is off; run session-bridge network on")
    }
    /// Switches networking and saves the choice. A name or port left out keeps the saved one.
    pub async fn set_network(
        &self,
        enabled: bool,
        port: Option<u16>,
        name: Option<String>,
        local_only: Option<bool>,
    ) -> Result<LanView> {
        let current = self.settings()?;
        let label = match name {
            Some(name) => HostLabel::parse(&name)?,
            None => current.label,
        };
        let settings = NetworkSettings {
            enabled,
            port: port.unwrap_or(current.port),
            label,
            local_only: local_only.unwrap_or(current.local_only),
        };
        if enabled {
            self.start(&settings).await?;
        } else {
            self.stop().await;
        }
        self.0.store.save_settings(&settings)?;
        self.view().await
    }
    pub async fn view(&self) -> Result<LanView> {
        let settings = self.settings()?;
        let (port, discovered) = {
            let running = self.0.running.lock().await;
            (
                running.as_ref().map(|r| r.port),
                running
                    .as_ref()
                    .and_then(|r| r.announcer.as_ref())
                    .map(Announcer::hosts)
                    .unwrap_or_default(),
            )
        };
        let own = self.0.fingerprint;
        Ok(LanView {
            network: NetworkView {
                enabled: settings.enabled,
                host: settings.label.to_string(),
                key: own.short(),
                fingerprint: own.to_string(),
                port,
                local_only: settings.local_only,
            },
            hosts: {
                let mut hosts = merge_hosts(&own, &discovered, &self.0.store.peers()?);
                let answered: std::collections::BTreeSet<String> = self
                    .seen()
                    .iter()
                    .filter(|(_, at)| at.elapsed() < self.0.online_window)
                    .map(|(key, _)| key.to_string())
                    .collect();
                for host in hosts.iter_mut().filter(|h| h.paired) {
                    host.online |= answered.contains(&host.fingerprint);
                }
                hosts
            },
            pairing: self.gate().view(now()),
        })
    }

    async fn addresses(&self, peer: &PairedPeer) -> Vec<SocketAddr> {
        let mut addresses: Vec<SocketAddr> = {
            let running = self.0.running.lock().await;
            running
                .as_ref()
                .and_then(|r| r.announcer.as_ref())
                .map(Announcer::hosts)
                .unwrap_or_default()
                .into_iter()
                .filter(|h| h.fingerprint == peer.fingerprint)
                .flat_map(|h| h.addresses)
                .collect()
        };
        addresses.extend(peer.address);
        order_addresses(addresses)
    }
    /// Connects to the first address answered by `expected` (any bridge when
    /// `None`). An address answered by another bridge, such as this laptop's
    /// own on a shared port, is skipped rather than fatal.
    async fn connect(
        &self,
        addresses: &[SocketAddr],
        expected: Option<&Fingerprint>,
    ) -> Result<(Channel<TcpStream>, SocketAddr)> {
        let mut last = anyhow!("No address is known");
        for address in addresses {
            match timeout(CONNECT_TIMEOUT, TcpStream::connect(address)).await {
                Ok(Ok(stream)) => match Channel::initiate(stream, &self.0.key).await {
                    Ok(channel) if expected.is_none_or(|e| channel.remote() == e) => {
                        return Ok((channel, *address));
                    }
                    Ok(_) => last = anyhow!("{address}: a bridge with a different key answers"),
                    Err(e) => last = anyhow!("{address}: {e:#}"),
                },
                Ok(Err(e)) => last = anyhow!("{address}: {e}"),
                Err(_) => last = anyhow!("{address}: connection timed out"),
            }
        }
        Err(last)
    }
    async fn connect_peer(&self, peer: &PairedPeer) -> Result<Channel<TcpStream>> {
        let addresses = self.addresses(peer).await;
        let (channel, _) = self
            .connect(&addresses, Some(&peer.fingerprint))
            .await
            .with_context(|| format!("No address of {} answers with its key", peer.label))?;
        self.saw(&peer.fingerprint);
        Ok(channel)
    }

    /// Asks `host` to show a one-time code.
    pub async fn pair(&self, host: &str) -> Result<OutgoingPairing> {
        let target = resolve_target(host, &self.view().await?.hosts)?;
        let (addresses, expected) = match target {
            Target::Peer {
                fingerprint,
                addresses,
            } => (addresses, Some(fingerprint)),
            Target::Address(address) => (vec![address], None),
        };
        self.pair_at(host, addresses, expected, None).await
    }
    /// Asks the bridge at the end of an SSH tunnel (REQ-027) to show a code:
    /// it is reached at `address` here and reaches this bridge back at
    /// `back_port` on its own loopback.
    pub async fn pair_through(
        &self,
        host: &str,
        address: SocketAddr,
        back_port: u16,
    ) -> Result<OutgoingPairing> {
        self.pair_at(host, vec![address], None, Some(back_port))
            .await
    }
    async fn pair_at(
        &self,
        host: &str,
        addresses: Vec<SocketAddr>,
        expected: Option<Fingerprint>,
        announce: Option<u16>,
    ) -> Result<OutgoingPairing> {
        let (label, listening) = self.identity().await?;
        let port = announce.unwrap_or(listening);
        anyhow::ensure!(!addresses.is_empty(), "{host} is not online");
        let (mut channel, address) = self
            .connect(&addresses, expected.as_ref())
            .await
            .with_context(|| format!("Cannot reach {host}"))?;
        anyhow::ensure!(
            channel.remote() != &self.0.fingerprint,
            "That address is this bridge itself"
        );
        channel
            .send(&PeerRequest::PairRequest {
                name: label.to_string(),
                port,
            })
            .await?;
        match channel.recv(REPLY_TIMEOUT).await? {
            PeerReply::CodeShown { expires_in } => {
                let id = Uuid::new_v4();
                let mut outgoing = self.0.outgoing.lock().await;
                outgoing.retain(|_, o| o.expires_at > now());
                outgoing.insert(
                    id,
                    Outgoing {
                        channel,
                        address,
                        expires_at: now() + expires_in,
                    },
                );
                Ok(OutgoingPairing {
                    id,
                    host: host.into(),
                    expires_in,
                })
            }
            other => Err(unexpected(other)),
        }
    }
    /// Tries the code typed by the user for pairing `id`.
    pub async fn submit_code(&self, id: Uuid, typed: &str) -> Result<PairStep> {
        let code = PairingCode::parse(typed)?;
        let mut out =
            self.0.outgoing.lock().await.remove(&id).context(
                "No pairing is in progress; start again with session-bridge pair <host>",
            )?;
        anyhow::ensure!(out.expires_at > now(), GateError::Expired);
        let (own, other) = (self.0.fingerprint, *out.channel.remote());
        let (state, message) = pake::start(Role::Initiator, &code, &own, &other);
        out.channel
            .send(&PeerRequest::Pair {
                spake: BASE64.encode(&message),
            })
            .await?;
        let spake = match out.channel.recv(REPLY_TIMEOUT).await? {
            PeerReply::Pair { spake } => spake,
            other => return Err(unexpected(other)),
        };
        let key = state.finish(&BASE64.decode(spake.as_bytes())?)?;
        out.channel
            .send(&PeerRequest::PairConfirm {
                mac: BASE64.encode(&pake::confirmation(&key, Role::Initiator, &own, &other)),
            })
            .await?;
        match out.channel.recv(REPLY_TIMEOUT).await? {
            PeerReply::Paired { name, mac } => {
                anyhow::ensure!(
                    pake::verify(
                        &key,
                        Role::Responder,
                        &own,
                        &other,
                        &BASE64.decode(mac.as_bytes())?
                    ),
                    "The other bridge failed key confirmation; it is not trusted"
                );
                let label = HostLabel::parse(&name)?;
                anyhow::ensure!(
                    !label_conflict(&self.0.store.peers()?, &label, &other),
                    "A paired bridge already uses the name {label}; unpair it first"
                );
                self.0.store.save_peer(&PairedPeer {
                    label: label.clone(),
                    fingerprint: other,
                    address: Some(out.address),
                    paired_at: now(),
                })?;
                Ok(PairStep::Paired {
                    host: label.to_string(),
                    key: other.short(),
                })
            }
            PeerReply::WrongCode { attempts_left } => {
                self.0.outgoing.lock().await.insert(id, out);
                Ok(PairStep::WrongCode { attempts_left })
            }
            other => Err(unexpected(other)),
        }
    }
    /// Forgets a paired bridge by name or key prefix; returns its name.
    pub fn unpair(&self, host: &str) -> Result<String> {
        let peers = self.0.store.peers()?;
        let matches: Vec<&PairedPeer> = peers
            .iter()
            .filter(|p| p.label.as_str() == host || p.fingerprint.matches_prefix(host))
            .collect();
        match matches.as_slice() {
            [peer] => {
                self.0.store.remove_peer(&peer.fingerprint)?;
                Ok(peer.label.to_string())
            }
            [] => anyhow::bail!("No paired bridge named {host}"),
            _ => anyhow::bail!("Several paired bridges match {host}; use the full key"),
        }
    }

    /// Sessions of every paired bridge in `project`, and the names of bridges
    /// that did not answer.
    pub async fn list_remote(
        &self,
        project: Option<String>,
    ) -> Result<(Vec<Session>, Vec<String>)> {
        if self.identity().await.is_err() {
            return Ok((Vec::new(), Vec::new()));
        }
        let mut queries = JoinSet::new();
        for peer in self.0.store.peers()? {
            let lan = self.clone();
            let project = project.clone();
            queries.spawn(async move {
                let result = lan.query_sessions(&peer, project).await;
                (peer, result)
            });
        }
        let (mut sessions, mut unavailable) = (Vec::new(), Vec::new());
        while let Some(joined) = queries.join_next().await {
            let (peer, result) = joined.context("Session query panicked")?;
            match result {
                Ok(found) => sessions.extend(found.into_iter().map(|mut s| {
                    s.host = Some(peer.label.to_string());
                    s
                })),
                Err(_) => unavailable.push(peer.label.to_string()),
            }
        }
        unavailable.sort();
        sessions.sort_by(|a, b| (&a.host, &a.session_id).cmp(&(&b.host, &b.session_id)));
        Ok((sessions, unavailable))
    }
    async fn query_sessions(
        &self,
        peer: &PairedPeer,
        project: Option<String>,
    ) -> Result<Vec<Session>> {
        let mut channel = timeout(REQUEST_TIMEOUT, self.connect_peer(peer))
            .await
            .context("Connection timed out")??;
        channel.send(&PeerRequest::List { project }).await?;
        match channel.recv(REQUEST_TIMEOUT).await? {
            PeerReply::Sessions { sessions } => {
                Ok(sessions.into_iter().filter(|s| !s.is_subagent()).collect())
            }
            other => Err(unexpected(other)),
        }
    }
    fn remote_usage(&self) -> MutexGuard<'_, BTreeMap<Fingerprint, RemoteEntry>> {
        match self.0.remote_usage.lock() {
            Ok(cache) => cache,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
    /// The usage of every paired bridge (REQ-029), by name. A bridge that is
    /// offline, slow, or too old to know the request is unavailable. Answers
    /// are reused, failing bridges asked less often ([`usage::remote_due`]),
    /// and a bridge already being asked is not asked again by another poll.
    pub async fn usage_remote(&self) -> Result<Vec<(String, HostState)>> {
        if self.identity().await.is_err() {
            return Ok(Vec::new());
        }
        let mut hosts = Vec::new();
        let mut queries = JoinSet::new();
        for peer in self.0.store.peers()? {
            // Decide and mark under one lock, so two polls cannot both ask.
            let shown = {
                let mut cache = self.remote_usage();
                let entry = cache.entry(peer.fingerprint).or_default();
                let due = entry
                    .last
                    .as_ref()
                    .is_none_or(|(at, sample)| usage::remote_due(Some(sample), at.elapsed()));
                if !due || usage::query_in_flight(entry.asking.map(|a| a.elapsed())) {
                    Some(entry.last.as_ref().map_or_else(
                        || HostState::Unavailable {
                            reason: "Waiting for its first answer".into(),
                        },
                        |(_, sample)| sample.state.clone(),
                    ))
                } else {
                    entry.asking = Some(Instant::now());
                    None
                }
            };
            match shown {
                Some(state) => hosts.push((peer.label.to_string(), state)),
                None => {
                    let lan = self.clone();
                    queries.spawn(async move {
                        let state = match timeout(USAGE_TIMEOUT, lan.query_usage(&peer)).await {
                            Ok(Ok(usage)) => HostState::Available { usage },
                            Ok(Err(e)) => HostState::Unavailable {
                                reason: format!("{e:#}"),
                            },
                            Err(_) => HostState::Unavailable {
                                reason: "No answer in time".into(),
                            },
                        };
                        (peer, state)
                    });
                }
            }
        }
        while let Some(joined) = queries.join_next().await {
            let (peer, state) = joined.context("Usage query panicked")?;
            let mut cache = self.remote_usage();
            let entry = cache.entry(peer.fingerprint).or_default();
            let sample = usage::next_remote(entry.last.as_ref().map(|(_, s)| s), state.clone());
            *entry = RemoteEntry {
                last: Some((Instant::now(), sample)),
                asking: None,
            };
            hosts.push((peer.label.to_string(), state));
        }
        hosts.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(hosts)
    }
    async fn query_usage(&self, peer: &PairedPeer) -> Result<crate::usage::HostUsage> {
        let mut channel = self.connect_peer(peer).await?;
        channel.send(&PeerRequest::Usage).await?;
        match channel.recv(REQUEST_TIMEOUT).await? {
            PeerReply::Usage { usage } => Ok(usage),
            other => Err(unexpected(other)),
        }
    }
    /// Hands a message to the paired bridge named `host`.
    pub async fn forward(&self, host: &str, message: ForwardedMessage) -> ForwardOutcome {
        if self.identity().await.is_err() {
            return ForwardOutcome::Unreachable {
                reason: "Networking is off on this laptop; run session-bridge network on".into(),
            };
        }
        let peer = match self.0.store.peers() {
            Ok(peers) => peers.into_iter().find(|p| p.label.as_str() == host),
            Err(e) => {
                return ForwardOutcome::Unreachable {
                    reason: format!("Cannot read paired bridges: {e:#}"),
                };
            }
        };
        let Some(peer) = peer else {
            return ForwardOutcome::Rejected {
                reason: format!("{host} is not paired with this laptop"),
            };
        };
        let mut channel = match self.connect_peer(&peer).await {
            Ok(channel) => channel,
            Err(e) => {
                return ForwardOutcome::Unreachable {
                    reason: format!("{host} is unreachable: {e:#}"),
                };
            }
        };
        if let Err(e) = channel.send(&PeerRequest::Forward { message }).await {
            return ForwardOutcome::Interrupted {
                reason: format!("Sending to {host} failed: {e:#}"),
            };
        }
        match channel.recv(REPLY_TIMEOUT).await {
            Ok(PeerReply::Accepted { delivery }) => ForwardOutcome::Accepted {
                host: host.into(),
                remote: delivery,
            },
            Ok(PeerReply::Error { message }) => ForwardOutcome::Rejected {
                reason: format!("{host} refused the message: {message}"),
            },
            Ok(other) => ForwardOutcome::Interrupted {
                reason: format!("Unexpected answer from {host}: {other:?}"),
            },
            Err(e) => ForwardOutcome::Interrupted {
                reason: format!("No answer from {host}: {e:#}"),
            },
        }
    }
    /// Tells the bridge named `host`, which sent `message_id`, its new delivery state.
    pub async fn push_status(
        &self,
        host: &str,
        message_id: Uuid,
        delivery: Delivery,
    ) -> Result<()> {
        let peer = self
            .0
            .store
            .peers()?
            .into_iter()
            .find(|p| p.label.as_str() == host)
            .with_context(|| format!("{host} is not paired"))?;
        let mut channel = self.connect_peer(&peer).await?;
        channel
            .send(&PeerRequest::Status {
                message_id,
                delivery,
            })
            .await?;
        match channel.recv(REQUEST_TIMEOUT).await? {
            PeerReply::Ok => Ok(()),
            other => Err(unexpected(other)),
        }
    }

    async fn serve(self, listener: TcpListener) {
        let mut handlers = JoinSet::new();
        loop {
            tokio::select! {
                accepted = listener.accept() => match accepted {
                    Ok((stream, address)) => {
                        let lan = self.clone();
                        handlers.spawn(async move {
                            if let Err(e) = lan.handle(stream, address).await {
                                eprintln!("Bridge connection from {address} failed: {e:#}");
                            }
                        });
                    }
                    Err(e) => {
                        eprintln!("Bridge listener error: {e}");
                        tokio::time::sleep(Duration::from_millis(200)).await;
                    }
                },
                Some(joined) = handlers.join_next() => if let Err(e) = joined {
                    eprintln!("Bridge connection handler stopped: {e}");
                },
            }
        }
    }
    async fn handle(&self, stream: TcpStream, address: SocketAddr) -> Result<()> {
        let mut channel = Channel::respond(stream, &self.0.key).await?;
        let request: PeerRequest = channel.recv(REQUEST_TIMEOUT).await?;
        let peer = self.0.store.peer(channel.remote())?;
        if let Some(from) = &peer {
            self.saw(&from.fingerprint);
        }
        let reply = match (request, peer) {
            (PeerRequest::PairRequest { name, port }, _) => {
                let initiator = *channel.remote();
                let result = self.serve_pairing(&mut channel, address, name, port).await;
                let mut gate = self.gate();
                *gate = gate.abandon(&initiator);
                return result;
            }
            (_, None) => refuse("This bridge is not paired with yours; pair them first"),
            (PeerRequest::List { project }, Some(_)) => self.local_sessions(project.as_deref()),
            (PeerRequest::Forward { message }, Some(from)) => {
                let (reply, answer) = oneshot::channel();
                self.0
                    .inbound
                    .send(Inbound::Forward {
                        from,
                        message,
                        reply,
                    })
                    .await?;
                timeout(REPLY_TIMEOUT, answer)
                    .await
                    .context("The bridge did not answer the forward")??
            }
            (
                PeerRequest::Status {
                    message_id,
                    delivery,
                },
                Some(from),
            ) => {
                self.0
                    .inbound
                    .send(Inbound::Status {
                        from,
                        message_id,
                        delivery,
                    })
                    .await?;
                PeerReply::Ok
            }
            (PeerRequest::Usage, Some(from)) => match self.0.usage.host().await {
                Ok(usage) => PeerReply::Usage { usage },
                Err(e) => {
                    // The detail can name local paths; it stays in this log.
                    eprintln!("Usage for {} unavailable: {e:#}", from.label);
                    refuse("Usage unavailable on this bridge")
                }
            },
            (PeerRequest::Pair { .. } | PeerRequest::PairConfirm { .. }, Some(_)) => {
                refuse("Send a pairing request first")
            }
        };
        channel.send(&reply).await
    }
    fn local_sessions(&self, project: Option<&str>) -> PeerReply {
        if let Some(Err(e)) = project.map(domain::validate_project) {
            return refuse(e.to_string());
        }
        let live = self.0.live.borrow().clone();
        PeerReply::Sessions {
            sessions: live
                .into_iter()
                .filter(|s| !s.is_subagent() && s.host.is_none())
                .filter(|s| project.is_none_or(|p| s.matches_project(p)))
                .collect(),
        }
    }
    fn announce(&self, title: &'static str, body: String) {
        let enabled = self.0.notify;
        tokio::spawn(async move { notify::show(enabled, title, &body).await });
    }
    /// The chosen laptop's side: show a code, then verify up to three tries.
    async fn serve_pairing(
        &self,
        channel: &mut Channel<TcpStream>,
        address: SocketAddr,
        name: String,
        port: u16,
    ) -> Result<()> {
        let (initiator, own) = (*channel.remote(), self.0.fingerprint);
        let label = match HostLabel::parse(&name) {
            Ok(label) => label,
            Err(e) => return channel.send(&refuse(e.to_string())).await,
        };
        if initiator == own {
            return channel
                .send(&refuse("A bridge cannot pair with itself"))
                .await;
        }
        if label_conflict(&self.0.store.peers()?, &label, &initiator) {
            return channel
                .send(&refuse(format!(
                    "A paired bridge already uses the name {label}; unpair it or rename this laptop"
                )))
                .await;
        }
        let (own_label, _) = self.identity().await?;
        let code = PairingCode::from_random(random_u32()?);
        let opened = {
            let mut gate = self.gate();
            gate.request(now(), label.clone(), initiator, address.ip(), code)
                .map(|next| *gate = next)
        };
        if let Err(e) = opened {
            return channel.send(&refuse(e.to_string())).await;
        }
        channel
            .send(&PeerReply::CodeShown {
                expires_in: CODE_LIFETIME,
            })
            .await?;
        eprintln!("Pairing request from {label} ({})", address.ip());
        self.announce(
            "session-bridge pairing",
            format!("{label} ({}) wants to pair. Code {code}", address.ip()),
        );
        let deadline = Instant::now() + Duration::from_secs(CODE_LIFETIME);
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let spake = match channel.recv(remaining).await? {
                PeerRequest::Pair { spake } => spake,
                _ => return channel.send(&refuse("Expected the pairing exchange")).await,
            };
            let (state, message) = pake::start(Role::Responder, &code, &initiator, &own);
            channel
                .send(&PeerReply::Pair {
                    spake: BASE64.encode(&message),
                })
                .await?;
            let key = BASE64
                .decode(spake.as_bytes())
                .ok()
                .and_then(|m| state.finish(&m).ok());
            let mac = match channel.recv(REQUEST_TIMEOUT).await? {
                PeerRequest::PairConfirm { mac } => mac,
                _ => return channel.send(&refuse("Expected the key confirmation")).await,
            };
            let matched = match (&key, BASE64.decode(mac.as_bytes())) {
                (Some(key), Ok(mac)) => pake::verify(key, Role::Initiator, &initiator, &own, &mac),
                _ => false,
            };
            let attempt = {
                let mut gate = self.gate();
                gate.attempt(now(), &initiator, matched)
                    .map(|(next, attempt)| {
                        *gate = next;
                        attempt
                    })
            };
            match (attempt, key) {
                (Ok(Attempt::Paired), Some(key)) => {
                    self.0.store.save_peer(&PairedPeer {
                        label: label.clone(),
                        fingerprint: initiator,
                        address: Some(SocketAddr::new(address.ip(), port)),
                        paired_at: now(),
                    })?;
                    channel
                        .send(&PeerReply::Paired {
                            name: own_label.to_string(),
                            mac: BASE64.encode(&pake::confirmation(
                                &key,
                                Role::Responder,
                                &initiator,
                                &own,
                            )),
                        })
                        .await?;
                    eprintln!("Paired with {label}");
                    self.announce(
                        "session-bridge paired",
                        format!("Paired with {label} (key {})", initiator.short()),
                    );
                    return Ok(());
                }
                (Ok(Attempt::Wrong { attempts_left }), _) => {
                    channel
                        .send(&PeerReply::WrongCode { attempts_left })
                        .await?
                }
                (Ok(Attempt::Exhausted), _) => {
                    return channel
                        .send(&refuse(GateError::Exhausted.to_string()))
                        .await;
                }
                (Ok(Attempt::Paired), None) => {
                    return channel.send(&refuse("Pairing failed")).await;
                }
                (Err(e), _) => return channel.send(&refuse(e.to_string())).await,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn node(root: &Path, name: &str) -> Lan {
        let (inbound, _) = mpsc::channel(1);
        let (_, live) = watch::channel(Vec::new());
        let usage = Sampler::new(root.to_path_buf()).unwrap();
        let lan = Lan::open(&root.join(name), false, inbound, live, usage).unwrap();
        let label = format!("{name}-{}", &Uuid::new_v4().simple().to_string()[..8]);
        lan.set_network(true, Some(0), Some(label), None)
            .await
            .unwrap();
        lan
    }
    async fn address(lan: &Lan) -> SocketAddr {
        SocketAddr::from((
            [127, 0, 0, 1],
            lan.view().await.unwrap().network.port.unwrap(),
        ))
    }

    /* REQ-017: Two bridges paired by address, with neither Zeroconf nor a
    tunnel (local-only), show each other online once one answered the
    other, and offline again when the window passes after one stops. */
    #[tokio::test]
    async fn answers_keep_address_paired_bridges_online() {
        let dir = tempfile::tempdir().unwrap();
        let open = async |name: &str| {
            let (inbound, _) = mpsc::channel(1);
            let (_, live) = watch::channel(Vec::new());
            let usage = Sampler::new(dir.path().to_path_buf()).unwrap();
            let mut lan = Lan::open(&dir.path().join(name), false, inbound, live, usage).unwrap();
            Arc::get_mut(&mut lan.0).unwrap().online_window = Duration::from_millis(500);
            let label = format!("{name}-{}", &Uuid::new_v4().simple().to_string()[..8]);
            lan.set_network(true, Some(0), Some(label), Some(true))
                .await
                .unwrap();
            lan
        };
        let (a, b) = (open("a").await, open("b").await);
        let pairing = a.pair(&address(&b).await.to_string()).await.unwrap();
        let code = b.view().await.unwrap().pairing.incoming.unwrap().code;
        assert!(matches!(
            a.submit_code(pairing.id, &code).await.unwrap(),
            PairStep::Paired { .. }
        ));
        let online = async |lan: &Lan| lan.view().await.unwrap().hosts[0].online;
        // Pairing is not an answer to a request; let its traces pass.
        tokio::time::sleep(Duration::from_millis(600)).await;
        assert!(!online(&a).await && !online(&b).await);

        let (_, unavailable) = a.list_remote(None).await.unwrap();
        assert!(unavailable.is_empty(), "{unavailable:?}");
        assert!(online(&a).await, "the bridge that answered is online");
        assert!(online(&b).await, "the bridge that asked is online");

        b.stop().await;
        tokio::time::sleep(Duration::from_millis(600)).await;
        let (_, unavailable) = a.list_remote(None).await.unwrap();
        assert_eq!(unavailable.len(), 1);
        assert!(
            !online(&a).await,
            "a stopped bridge goes offline after the window"
        );
        a.stop().await;
    }

    /* REQ-017: An address answered by a bridge with another key (for example
    this laptop's own bridge on a shared port) is skipped, not fatal. */
    #[tokio::test]
    async fn connect_skips_addresses_answered_by_another_key() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b, c) = (
            node(dir.path(), "a").await,
            node(dir.path(), "b").await,
            node(dir.path(), "c").await,
        );
        let (b_address, c_address) = (address(&b).await, address(&c).await);
        let expected = b.0.fingerprint;
        let (channel, chosen) = a
            .connect(&[c_address, b_address], Some(&expected))
            .await
            .unwrap();
        assert_eq!(chosen, b_address);
        assert_eq!(channel.remote(), &expected);
        let Err(error) = a.connect(&[c_address], Some(&expected)).await else {
            panic!("another key must not be accepted");
        };
        assert!(error.to_string().contains("different key"), "{error}");
        let (_, any) = a.connect(&[c_address], None).await.unwrap();
        assert_eq!(any, c_address);
        for lan in [a, b, c] {
            lan.stop().await;
        }
    }
}
