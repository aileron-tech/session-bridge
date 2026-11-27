//! Phone access state (REQ-036): the switch and paired phones in
//! `phones.json`, the open pairing offer, phone sessions and limits, and the
//! listener while it runs. Answers the admin page's phone requests.
use super::{
    limits::{Bucket, Failures, Kind},
    model::{CODE_BYTES, Offer, PairingCode, PhoneBook, PhoneError},
    net::{Interface, host_name},
    replay::Replays,
    view::{PhonesView, Runtime, ViewInput, phones_view},
};
use crate::{daemon::Control, protocol::Reply, protocol::Request, store};
use anyhow::{Result, anyhow};
use std::{
    collections::BTreeMap,
    net::IpAddr,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
    time::{SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}
/// This machine's IPv4 interfaces; empty (and logged) when they cannot be read.
pub fn local_interfaces() -> Vec<Interface> {
    match if_addrs::get_if_addrs() {
        Ok(interfaces) => interfaces
            .into_iter()
            .filter_map(|i| match i.addr {
                if_addrs::IfAddr::V4(v4) => Some(Interface {
                    ip: v4.ip,
                    netmask: v4.netmask,
                }),
                if_addrs::IfAddr::V6(_) => None,
            })
            .collect(),
        Err(e) => {
            eprintln!("Network interfaces not readable: {e}");
            Vec::new()
        }
    }
}
/// The IPv4 address of the interface with the default route: a UDP socket
/// connected to a documentation address (RFC 5737) shows which local address
/// the system would use. Connecting UDP sends nothing.
pub fn default_route_ip() -> Option<std::net::Ipv4Addr> {
    let socket = std::net::UdpSocket::bind(("0.0.0.0", 0)).ok()?;
    socket.connect(("192.0.2.1", 9)).ok()?;
    match socket.local_addr().ok()?.ip() {
        IpAddr::V4(ip) if !ip.is_unspecified() => Some(ip),
        _ => None,
    }
}
/// The addresses a phone can use to reach this bridge, the default route's
/// first.
pub fn current_addresses() -> Vec<std::net::Ipv4Addr> {
    super::net::lan_addresses(&local_interfaces(), default_route_ip())
}
pub fn random_bytes<const N: usize>() -> Result<[u8; N]> {
    let mut bytes = [0u8; N];
    getrandom::fill(&mut bytes).map_err(|e| anyhow!("No system randomness: {e}"))?;
    Ok(bytes)
}
/// Locks a mutex; a poisoned one is still used (its data stays consistent:
/// every change replaces whole values).
pub fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[derive(Debug, Clone, Copy)]
pub struct PhoneSession {
    pub device: Uuid,
    pub created: u64,
    pub last: u64,
}

#[derive(Default)]
pub struct Limits {
    pub unauthenticated: BTreeMap<IpAddr, Bucket>,
    pub failures: BTreeMap<IpAddr, Failures>,
    pub devices: BTreeMap<(Uuid, Kind), Bucket>,
}

/// State the admin page and the listener share.
pub struct Shared {
    file: PathBuf,
    pub identity_file: PathBuf,
    pub book: Mutex<PhoneBook>,
    pub offer: Mutex<Option<Offer>>,
    pub runtime: Mutex<Runtime>,
    pub host: Mutex<String>,
    /// Session token to session.
    pub sessions: Mutex<BTreeMap<String, PhoneSession>>,
    /// Challenge nonce to the time it was issued.
    pub challenges: Mutex<BTreeMap<String, u64>>,
    pub limits: Mutex<Limits>,
    pub replays: Mutex<Replays>,
}
impl Shared {
    /// Saves and installs a new book.
    pub fn save_book(&self, next: PhoneBook) -> Result<()> {
        store::write_json(&self.file, &next)?;
        *lock(&self.book) = next;
        Ok(())
    }
    /// Ends every session of `device`, or all with `None`.
    pub fn end_sessions(&self, device: Option<Uuid>) {
        lock(&self.sessions).retain(|_, s| device.is_some_and(|d| s.device != d));
    }
    pub fn host(&self) -> String {
        lock(&self.host).clone()
    }
    pub fn view(&self) -> PhonesView {
        let now = now();
        let book = lock(&self.book).clone();
        let offer = lock(&self.offer).clone();
        let mut runtime = lock(&self.runtime).clone();
        if runtime.listening {
            runtime.addresses = current_addresses()
                .iter()
                .map(|ip| ip.to_string())
                .collect();
        }
        let sessions = lock(&self.sessions).clone();
        let host = self.host();
        phones_view(ViewInput {
            book: &book,
            offer: offer.as_ref(),
            runtime: &runtime,
            host: &host,
            sessions: &|id| {
                sessions
                    .values()
                    .filter(|s| {
                        s.device == id && super::model::session_valid(s.created, s.last, now)
                    })
                    .count()
            },
            now,
        })
    }
}

#[derive(Clone)]
pub struct PhoneService {
    shared: Arc<Shared>,
    /// Desktop notifications on (`SESSION_BRIDGE_NOTIFY` not `0`).
    notify: bool,
    #[cfg(not(windows))]
    running: Arc<tokio::sync::Mutex<Option<super::server::Running>>>,
}
impl PhoneService {
    pub fn open(state: &Path, notify: bool) -> Result<Self> {
        let file = state.join("phones.json");
        // Before 0.4.5 the phone certificate was a self-signed leaf; the root
        // in `phone-ca.json` replaces it.
        let old = state.join("phone-identity.json");
        if old.exists()
            && let Err(e) = std::fs::remove_file(&old)
        {
            eprintln!("Old phone certificate not removed: {e}");
        }
        let book: PhoneBook = if file.exists() {
            store::read_json(&file)?
        } else {
            PhoneBook::default()
        };
        Ok(Self {
            shared: Arc::new(Shared {
                file,
                identity_file: state.join("phone-ca.json"),
                book: Mutex::new(book),
                offer: Mutex::new(None),
                runtime: Mutex::new(Runtime::default()),
                host: Mutex::new(String::new()),
                sessions: Mutex::default(),
                challenges: Mutex::default(),
                limits: Mutex::default(),
                replays: Mutex::default(),
            }),
            notify,
            #[cfg(not(windows))]
            running: Arc::default(),
        })
    }
    pub fn shared(&self) -> Arc<Shared> {
        self.shared.clone()
    }
    /// Starts the listener after a restart when phone access was left on.
    pub async fn resume(&self, control: Control, label: &str) {
        *lock(&self.shared.host) = host_name(label);
        if lock(&self.shared.book).enabled
            && let Err(e) = self.start(control).await
        {
            eprintln!("Phone access did not start: {e:#}");
        }
    }
    #[cfg(not(windows))]
    async fn start(&self, control: Control) -> Result<()> {
        let mut running = self.running.lock().await;
        if let Some(previous) = running.take() {
            previous.stop();
        }
        let result = super::server::start(self.shared.clone(), control).await;
        let mut runtime = lock(&self.shared.runtime);
        match result {
            Ok((server, cert_der)) => {
                *running = Some(server);
                *runtime = Runtime {
                    listening: true,
                    problem: None,
                    addresses: Vec::new(),
                    cert_der: Some(cert_der),
                };
                Ok(())
            }
            Err(e) => {
                runtime.listening = false;
                runtime.problem = Some(format!("{e:#}"));
                Err(e)
            }
        }
    }
    #[cfg(windows)]
    async fn start(&self, _control: Control) -> Result<()> {
        let problem = "Phone access is not available on Windows yet".to_owned();
        lock(&self.shared.runtime).problem = Some(problem.clone());
        Err(anyhow!(problem))
    }
    async fn stop(&self) {
        #[cfg(not(windows))]
        if let Some(server) = self.running.lock().await.take() {
            server.stop();
        }
        self.shared.end_sessions(None);
        *lock(&self.shared.offer) = None;
        let mut runtime = lock(&self.shared.runtime);
        runtime.listening = false;
        runtime.problem = None;
    }
    /// The admin page's phone requests; anything else is an error.
    pub async fn answer(&self, request: Request, control: Control, label: &str) -> Result<Reply> {
        *lock(&self.shared.host) = host_name(label);
        match request {
            Request::Phones => {}
            Request::PhoneAccess { enabled, port } => {
                if port == Some(0) {
                    anyhow::bail!("Choose a port from 1 to 65534");
                }
                let mut next = lock(&self.shared.book).clone();
                next.enabled = enabled;
                if port.is_some() {
                    next.port = port;
                }
                self.shared.save_book(next)?;
                if enabled {
                    self.start(control).await?;
                } else {
                    self.stop().await;
                }
            }
            Request::PhoneOffer => {
                anyhow::ensure!(
                    lock(&self.shared.runtime).listening,
                    "Turn phone access on first"
                );
                let code = PairingCode::from_bytes(random_bytes::<CODE_BYTES>()?);
                *lock(&self.shared.offer) = Some(Offer::new(Uuid::new_v4(), code, now()));
            }
            Request::PhoneConfirm { id, accept } => {
                let mut offer = lock(&self.shared.offer);
                let current = offer
                    .as_ref()
                    .filter(|o| o.id == id)
                    .ok_or(PhoneError::NoOffer)?;
                let book = lock(&self.shared.book).clone();
                let (next_offer, next_book) =
                    current.confirm(accept, Uuid::new_v4(), &book, now())?;
                if next_book != book {
                    self.shared.save_book(next_book)?;
                }
                // Every pairing shows on the laptop, also one that a local
                // program confirmed through the admin page.
                if let super::model::OfferState::Paired { device_id, .. } = &next_offer.state
                    && let Some(device) = lock(&self.shared.book).device(*device_id)
                {
                    let body = format!(
                        "{} can now read and send your messages. Unpair it on the admin page if this was not you.",
                        device.name
                    );
                    let enabled = self.notify;
                    tokio::spawn(async move {
                        crate::lan::notify::show(enabled, "Phone paired", &body).await
                    });
                }
                *offer = Some(next_offer);
            }
            Request::PhoneUnpair { device_id } => {
                let next = lock(&self.shared.book).remove(device_id)?;
                self.shared.save_book(next)?;
                self.shared.end_sessions(Some(device_id));
            }
            _ => anyhow::bail!("Not a phone request"),
        }
        Ok(Reply::Phones {
            phones: self.shared.view(),
        })
    }
}
