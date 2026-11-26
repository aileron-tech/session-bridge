//! The phone listener (REQ-036): HTTPS on `0.0.0.0:<port>` for the phone app
//! and its API, plain HTTP on `<port + 1>` for the certificate profile only,
//! and an mDNS record for `sb-<label>.local`. Connections from outside the
//! local network are dropped before TLS.
use super::{
    api::{
        Phone, Profile, call, certificate_file, challenge, forget, icon, manifest, page, pair,
        pair_poll, profile_file, script, session, style,
    },
    crypto,
    idle::{IDLE, Idle},
    net::{Interface, is_lan_source},
    service::{Shared, local_interfaces, lock, now},
};
use crate::daemon::Control;
use anyhow::{Context, Result};
use axum::{
    Router,
    extract::{DefaultBodyLimit, connect_info::Connected},
    routing::{get, post},
    serve::{IncomingStream, Listener},
};
use std::{
    collections::BTreeMap,
    net::{IpAddr, SocketAddr},
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context as TaskContext, Poll},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::{TcpListener, TcpStream},
    sync::{OwnedSemaphorePermit, Semaphore, mpsc},
    task::JoinHandle,
};
use tokio_rustls::{TlsAcceptor, server::TlsStream};

const SERVICE: &str = "_session-bridge-phone._tcp.local.";
/// Connections at once, in total and per source address.
const CONNECTIONS: usize = 16;
const PER_ADDRESS: usize = 4;
const HANDSHAKE: Duration = Duration::from_secs(10);
/// A request body: the message limit (16000 bytes) with room for JSON.
const BODY_LIMIT: usize = 64 * 1024;

pub struct Running {
    tasks: Vec<JoinHandle<()>>,
    closed: Arc<AtomicBool>,
    mdns: Option<mdns_sd::ServiceDaemon>,
}
impl Running {
    pub fn stop(self) {
        self.closed.store(true, Ordering::SeqCst);
        for task in self.tasks {
            task.abort();
        }
        if let Some(daemon) = self.mdns
            && let Err(e) = daemon.shutdown()
        {
            eprintln!("Could not stop the phone announcement: {e}");
        }
    }
}

/// The client address of a phone connection.
#[derive(Clone, Copy, Debug)]
pub struct PhoneClient(pub SocketAddr);

/// A TLS connection that holds its connection slots and stops reading and
/// writing once phone access is turned off.
pub struct Guarded {
    inner: Idle<TlsStream<TcpStream>>,
    closed: Arc<AtomicBool>,
    _total: OwnedSemaphorePermit,
    _address: AddressSlot,
}
fn closed_error() -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::BrokenPipe, "Phone access turned off")
}
impl AsyncRead for Guarded {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        if self.closed.load(Ordering::SeqCst) {
            return Poll::Ready(Err(closed_error()));
        }
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}
impl AsyncWrite for Guarded {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        if self.closed.load(Ordering::SeqCst) {
            return Poll::Ready(Err(closed_error()));
        }
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

/// One of `PER_ADDRESS` slots of a source address, released on drop.
pub struct AddressSlot {
    counts: Arc<Mutex<BTreeMap<IpAddr, usize>>>,
    ip: IpAddr,
}
impl AddressSlot {
    fn take(counts: &Arc<Mutex<BTreeMap<IpAddr, usize>>>, ip: IpAddr) -> Option<Self> {
        let mut map = lock(counts);
        let count = map.entry(ip).or_default();
        if *count >= PER_ADDRESS {
            return None;
        }
        *count += 1;
        Some(Self {
            counts: counts.clone(),
            ip,
        })
    }
}
impl Drop for AddressSlot {
    fn drop(&mut self) {
        let mut map = lock(&self.counts);
        if let Some(count) = map.get_mut(&self.ip) {
            *count -= 1;
            if *count == 0 {
                map.remove(&self.ip);
            }
        }
    }
}

/// Connections that passed the source filter and the TLS handshake.
pub struct Incoming {
    connections: mpsc::Receiver<(Guarded, SocketAddr)>,
    local: SocketAddr,
}
impl Listener for Incoming {
    type Io = Guarded;
    type Addr = SocketAddr;
    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        match self.connections.recv().await {
            Some(connection) => connection,
            // The acceptor ended (phone access off): serve nothing more.
            None => std::future::pending().await,
        }
    }
    fn local_addr(&self) -> std::io::Result<Self::Addr> {
        Ok(self.local)
    }
}
impl Connected<IncomingStream<'_, Incoming>> for PhoneClient {
    fn connect_info(stream: IncomingStream<'_, Incoming>) -> Self {
        PhoneClient(*stream.remote_addr())
    }
}

/// This machine's interfaces, read again at most every 10 s.
struct Interfaces {
    read_at: Option<Instant>,
    interfaces: Vec<Interface>,
}
impl Interfaces {
    fn admits(&mut self, ip: IpAddr) -> bool {
        if self
            .read_at
            .is_none_or(|at| at.elapsed() > Duration::from_secs(10))
        {
            self.interfaces = local_interfaces();
            self.read_at = Some(Instant::now());
        }
        is_lan_source(ip, &self.interfaces)
    }
}

fn bind(port: u16) -> Result<TcpListener> {
    let listener = std::net::TcpListener::bind(("0.0.0.0", port))
        .with_context(|| format!("Port {port} is in use or not allowed"))?;
    listener.set_nonblocking(true)?;
    Ok(TcpListener::from_std(listener)?)
}

type Starting = Pin<Box<dyn Future<Output = Result<(Running, Vec<u8>)>> + Send>>;

/// Starts the listeners; returns them and the certificate. A boxed future:
/// its handlers call back into `Control`, which starts the listener.
pub fn start(shared: Arc<Shared>, control: Control) -> Starting {
    Box::pin(start_listeners(shared, control))
}
async fn start_listeners(shared: Arc<Shared>, control: Control) -> Result<(Running, Vec<u8>)> {
    let host = shared.host();
    let port = lock(&shared.book).port();
    anyhow::ensure!(port < u16::MAX, "Choose a port below 65535");
    let identity = crypto::load_or_create(&shared.identity_file, &host, now() as i64)?;
    let cert_der = identity.cert_der()?;
    let acceptor = TlsAcceptor::from(crypto::server_config(&identity)?);
    let https = bind(port)?;
    let http = bind(port + 1)?;
    let local = https.local_addr()?;
    let closed = Arc::new(AtomicBool::new(false));
    let (sender, connections) = mpsc::channel(CONNECTIONS);

    let accepting = {
        let closed = closed.clone();
        tokio::spawn(async move {
            let total = Arc::new(Semaphore::new(CONNECTIONS));
            let per_address = Arc::new(Mutex::new(BTreeMap::new()));
            let mut interfaces = Interfaces {
                read_at: None,
                interfaces: Vec::new(),
            };
            loop {
                let (stream, address) = match https.accept().await {
                    Ok(accepted) => accepted,
                    Err(e) => {
                        eprintln!("Phone connection not accepted: {e}");
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        continue;
                    }
                };
                if !interfaces.admits(address.ip()) {
                    continue;
                }
                let Ok(permit) = total.clone().try_acquire_owned() else {
                    continue;
                };
                let Some(slot) = AddressSlot::take(&per_address, address.ip()) else {
                    continue;
                };
                let acceptor = acceptor.clone();
                let sender = sender.clone();
                let closed = closed.clone();
                tokio::spawn(async move {
                    match tokio::time::timeout(HANDSHAKE, acceptor.accept(stream)).await {
                        Ok(Ok(inner)) => {
                            let guarded = Guarded {
                                inner: Idle::new(inner, IDLE),
                                closed,
                                _total: permit,
                                _address: slot,
                            };
                            if sender.send((guarded, address)).await.is_err() {
                                eprintln!("Phone connection dropped: listener stopped");
                            }
                        }
                        Ok(Err(e)) => eprintln!("Phone TLS handshake from {address} failed: {e}"),
                        Err(_) => eprintln!("Phone TLS handshake from {address} timed out"),
                    }
                });
            }
        })
    };

    let app = Router::new()
        .route("/", get(page))
        .route("/phone.js", get(script))
        .route("/phone.css", get(style))
        .route("/icon.svg", get(icon))
        .route("/manifest.webmanifest", get(manifest))
        .route("/api/pair", post(pair))
        .route("/api/pair/status", post(pair_poll))
        .route("/api/challenge", post(challenge))
        .route("/api/session", post(session))
        .route("/api/call", post(call))
        .route("/api/forget", post(forget))
        .layer(DefaultBodyLimit::max(BODY_LIMIT))
        .with_state(Phone {
            shared: shared.clone(),
            control,
            closed: closed.clone(),
        });
    let serving = tokio::spawn(async move {
        let incoming = Incoming { connections, local };
        if let Err(e) = axum::serve(
            incoming,
            app.into_make_service_with_connect_info::<PhoneClient>(),
        )
        .await
        {
            eprintln!("Phone app stopped: {e}");
        }
    });

    let profile = Router::new()
        .route("/bridge.mobileconfig", get(profile_file))
        .route("/bridge.crt", get(certificate_file))
        .with_state(Profile {
            cert_der: Arc::new(cert_der.clone()),
            host: host.clone(),
            closed: closed.clone(),
        });
    let profiles = tokio::spawn(async move {
        if let Err(e) = axum::serve(
            http,
            profile.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        {
            eprintln!("Phone certificate download stopped: {e}");
        }
    });

    let mdns = match announce(&host, port) {
        Ok(daemon) => Some(daemon),
        Err(e) => {
            eprintln!("Phone host name not announced: {e:#}");
            None
        }
    };
    Ok((
        Running {
            tasks: vec![accepting, serving, profiles],
            closed,
            mdns,
        },
        cert_der,
    ))
}

/// Answers `sb-<label>.local` over mDNS (IPv4), independent of laptop networking.
fn announce(host: &str, port: u16) -> Result<mdns_sd::ServiceDaemon> {
    let daemon = mdns_sd::ServiceDaemon::new()?;
    daemon.disable_interface(mdns_sd::IfKind::IPv6)?;
    let instance = host.trim_end_matches(".local");
    let info = mdns_sd::ServiceInfo::new(
        SERVICE,
        instance,
        &format!("{host}."),
        "",
        port,
        &[] as &[(&str, &str)],
    )?
    .enable_addr_auto();
    daemon.register(info)?;
    Ok(daemon)
}
