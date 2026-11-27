//! The phone app's HTTP handlers (REQ-036): the app's files, pairing,
//! challenges and sessions, calls through the phone allowlist, and the
//! certificate download.
use super::{
    allow::{phone_allowed, phone_reply},
    crypto,
    limits::{self, Bucket, Kind},
    model::{
        Candidate, DeviceKey, NONCE_SECONDS, PairingCode, PhoneError, pair_status, session_message,
        session_valid, short_hex_fingerprint, validate_name,
    },
    net::{host_allowed, is_lan_source, origin_allowed, request_host, site_names},
    profile::mobileconfig,
    replay::{Begin, client_id, digest, phone_key},
    server::PhoneClient,
    service::{Shared, local_interfaces, lock, now, now_ms, random_bytes},
};
use crate::{daemon::Control, protocol::Request};
use axum::{
    Json,
    body::Bytes,
    extract::{ConnectInfo, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use data_encoding::{BASE32_NOPAD, BASE64URL_NOPAD};
use serde::Deserialize;
use serde_json::json;
use std::{
    net::{IpAddr, SocketAddr},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

const PAGE: &str = include_str!("web/index.html");
const SCRIPT: &str = include_str!("web/phone.js");
const STYLE: &str = include_str!("web/phone.css");
const MANIFEST: &str = include_str!("web/manifest.webmanifest");
const ICON: &str = include_str!("../admin/icon.svg");
const POLICY: &str = "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self'; manifest-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

#[derive(Clone)]
pub(super) struct Phone {
    pub(super) shared: Arc<Shared>,
    pub(super) control: Control,
    pub(super) closed: Arc<AtomicBool>,
}
impl Phone {
    fn port(&self) -> u16 {
        lock(&self.shared.book).port()
    }
    /// Host names this listener (one of this laptop's current LAN addresses,
    /// or its `.local` name), and a browser's Origin is this app.
    fn same_site(&self, headers: &HeaderMap) -> bool {
        let names = site_names(&self.shared.addresses(), &self.shared.host());
        let port = self.port();
        let named = host_allowed(
            headers.get(header::HOST).and_then(|v| v.to_str().ok()),
            &names,
            port,
        );
        let origin = origin_allowed(
            headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()),
            &names,
            port,
        );
        named && origin && !self.closed.load(Ordering::SeqCst)
    }
}

fn secure(mut response: Response) -> Response {
    let set = response.headers_mut();
    set.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(POLICY),
    );
    set.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    set.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    set.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    set.insert(
        header::STRICT_TRANSPORT_SECURITY,
        HeaderValue::from_static("max-age=31536000"),
    );
    set.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response
}
fn error(status: StatusCode, message: impl ToString) -> Response {
    secure(
        (
            status,
            Json(json!({"result": "error", "message": message.to_string()})),
        )
            .into_response(),
    )
}
fn asset(phone: &Phone, headers: &HeaderMap, kind: &'static str, body: &'static str) -> Response {
    if !phone.same_site(headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let mut response = body.into_response();
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(kind));
    secure(response)
}
pub(super) async fn page(State(phone): State<Phone>, headers: HeaderMap) -> Response {
    asset(&phone, &headers, "text/html; charset=utf-8", PAGE)
}
pub(super) async fn script(State(phone): State<Phone>, headers: HeaderMap) -> Response {
    asset(&phone, &headers, "text/javascript; charset=utf-8", SCRIPT)
}
pub(super) async fn style(State(phone): State<Phone>, headers: HeaderMap) -> Response {
    asset(&phone, &headers, "text/css; charset=utf-8", STYLE)
}
pub(super) async fn icon(State(phone): State<Phone>, headers: HeaderMap) -> Response {
    asset(&phone, &headers, "image/svg+xml", ICON)
}
pub(super) async fn manifest(State(phone): State<Phone>, headers: HeaderMap) -> Response {
    asset(&phone, &headers, "application/manifest+json", MANIFEST)
}

/// Before authentication: same site, the address not blocked, and a token
/// from its bucket.
fn admit_unauthenticated(phone: &Phone, headers: &HeaderMap, ip: IpAddr) -> Option<Response> {
    if !phone.same_site(headers) {
        return Some(StatusCode::FORBIDDEN.into_response());
    }
    let now = now_ms();
    let mut limits = lock(&phone.shared.limits);
    if limits.failures.get(&ip).is_some_and(|f| f.blocked(now)) {
        return Some(retry(limits::FAILURE_WAIT_MS / 1000));
    }
    let bucket = *limits
        .unauthenticated
        .entry(ip)
        .or_insert_with(|| Bucket::full(limits::UNAUTHENTICATED, now));
    let (next, ok) = bucket.take(limits::UNAUTHENTICATED, now);
    limits.unauthenticated.insert(ip, next);
    (!ok).then(|| retry(next.retry_after(limits::UNAUTHENTICATED)))
}
fn retry(seconds: u64) -> Response {
    let mut response = error(
        StatusCode::TOO_MANY_REQUESTS,
        format!("Too many requests: wait {seconds} s"),
    );
    if let Ok(value) = HeaderValue::from_str(&seconds.to_string()) {
        response.headers_mut().insert(header::RETRY_AFTER, value);
    }
    response
}
fn failed(phone: &Phone, ip: IpAddr) {
    let mut limits = lock(&phone.shared.limits);
    let next = limits
        .failures
        .get(&ip)
        .copied()
        .unwrap_or_default()
        .fail(now_ms());
    limits.failures.insert(ip, next);
}
fn parse<T: serde::de::DeserializeOwned>(body: &Bytes) -> Result<T, Box<Response>> {
    serde_json::from_slice(body).map_err(|e| Box::new(error(StatusCode::BAD_REQUEST, e)))
}

#[derive(Deserialize)]
struct PairBody {
    code: String,
    name: String,
    public_key: String,
}
/// A phone typed the code: it now waits for the user on the laptop.
pub(super) async fn pair(
    State(phone): State<Phone>,
    ConnectInfo(client): ConnectInfo<PhoneClient>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let ip = client.0.ip();
    if let Some(refused) = admit_unauthenticated(&phone, &headers, ip) {
        return refused;
    }
    let body: PairBody = match parse(&body) {
        Ok(body) => body,
        Err(response) => return *response,
    };
    let parsed = PairingCode::parse(&body.code).and_then(|code| {
        Ok((
            code,
            validate_name(&body.name)?,
            DeviceKey::parse(&body.public_key)?,
        ))
    });
    let (code, name, key) = match parsed {
        Ok(parsed) => parsed,
        Err(e) => return error(StatusCode::BAD_REQUEST, e),
    };
    let candidate = Candidate {
        name,
        key: key.clone(),
        address: ip.to_string(),
        at: now(),
    };
    let result = {
        let mut offer = lock(&phone.shared.offer);
        let book = lock(&phone.shared.book).clone();
        match offer.as_ref() {
            None => Err(PhoneError::NoOffer),
            Some(current) => {
                let (next, result) = current.redeem(&code, candidate, &book);
                *offer = Some(next);
                result
            }
        }
    };
    if let Err(e) = result {
        failed(&phone, ip);
        return error(StatusCode::FORBIDDEN, e);
    }
    let cert = lock(&phone.shared.runtime)
        .cert_der
        .clone()
        .unwrap_or_default();
    secure(
        Json(json!({
            "result": "scanned",
            "key": key.short(),
            "fingerprint": super::model::hex_fingerprint(&cert).replace(' ', ""),
            "fingerprint_short": short_hex_fingerprint(&cert),
        }))
        .into_response(),
    )
}

#[derive(Deserialize)]
struct PollBody {
    public_key: String,
}
pub(super) async fn pair_poll(
    State(phone): State<Phone>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if !phone.same_site(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let body: PollBody = match parse(&body) {
        Ok(body) => body,
        Err(response) => return *response,
    };
    let key = match DeviceKey::parse(&body.public_key) {
        Ok(key) => key,
        Err(e) => return error(StatusCode::BAD_REQUEST, e),
    };
    let status = pair_status(lock(&phone.shared.offer).as_ref(), &key, now());
    secure(Json(status).into_response())
}

pub(super) async fn challenge(
    State(phone): State<Phone>,
    ConnectInfo(client): ConnectInfo<PhoneClient>,
    headers: HeaderMap,
) -> Response {
    if let Some(refused) = admit_unauthenticated(&phone, &headers, client.0.ip()) {
        return refused;
    }
    let nonce = match random_bytes::<32>() {
        Ok(bytes) => BASE64URL_NOPAD.encode(&bytes),
        Err(e) => return error(StatusCode::INTERNAL_SERVER_ERROR, e),
    };
    let now = now();
    let mut challenges = lock(&phone.shared.challenges);
    challenges.retain(|_, at| now.saturating_sub(*at) < NONCE_SECONDS);
    if challenges.len() >= 64 {
        return retry(NONCE_SECONDS);
    }
    challenges.insert(nonce.clone(), now);
    secure(Json(json!({"result": "challenge", "nonce": nonce})).into_response())
}

#[derive(Deserialize)]
struct SessionBody {
    device_id: uuid::Uuid,
    nonce: String,
    signature: String,
}
/// A paired phone signed a fresh nonce: it gets a session token.
pub(super) async fn session(
    State(phone): State<Phone>,
    ConnectInfo(client): ConnectInfo<PhoneClient>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let ip = client.0.ip();
    if let Some(refused) = admit_unauthenticated(&phone, &headers, ip) {
        return refused;
    }
    let body: SessionBody = match parse(&body) {
        Ok(body) => body,
        Err(response) => return *response,
    };
    // The phone signs the host it reached (`location.hostname`); the Host
    // header was checked to name this listener.
    let signed_host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(request_host)
        .unwrap_or_default();
    let now = now();
    let fresh = lock(&phone.shared.challenges)
        .remove(&body.nonce)
        .is_some_and(|at| now.saturating_sub(at) < NONCE_SECONDS);
    let device = lock(&phone.shared.book).device(body.device_id).cloned();
    let signature = BASE64URL_NOPAD
        .decode(body.signature.trim_end_matches('=').as_bytes())
        .unwrap_or_default();
    let verified = fresh
        && device.as_ref().is_some_and(|device| {
            crypto::verify(
                device.key.raw(),
                &session_message(&signed_host, &body.nonce),
                &signature,
            )
        });
    let Some(device) = device.filter(|_| verified) else {
        failed(&phone, ip);
        return error(
            StatusCode::UNAUTHORIZED,
            "Not a paired phone, or the signature is wrong",
        );
    };
    let token = match random_bytes::<32>() {
        Ok(bytes) => BASE32_NOPAD.encode(&bytes),
        Err(e) => return error(StatusCode::INTERNAL_SERVER_ERROR, e),
    };
    {
        let mut sessions = lock(&phone.shared.sessions);
        sessions.retain(|_, s| session_valid(s.created, s.last, now));
        sessions.insert(
            token.clone(),
            super::service::PhoneSession {
                device: device.id,
                created: now,
                last: now,
            },
        );
    }
    let seen = lock(&phone.shared.book).seen(device.id, now, &ip.to_string());
    if let Err(e) = phone.shared.save_book(seen) {
        eprintln!("Phone last use not saved: {e:#}");
    }
    secure(
        Json(json!({
            "result": "session",
            "token": token,
            "expires_in": super::model::SESSION_IDLE,
            "name": device.name,
        }))
        .into_response(),
    )
}

/// The session of a bearer token, renewed by this use.
fn authenticate(phone: &Phone, headers: &HeaderMap) -> Result<uuid::Uuid, Box<Response>> {
    if !phone.same_site(headers) {
        return Err(Box::new(StatusCode::FORBIDDEN.into_response()));
    }
    let presented = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or_default();
    let now = now();
    let mut sessions = lock(&phone.shared.sessions);
    // Tokens are compared through the map's key lookup after a length check;
    // they are 256-bit random values, so timing reveals nothing useful.
    let found = (presented.len() == 52)
        .then(|| sessions.get_mut(presented))
        .flatten();
    match found {
        Some(session) if session_valid(session.created, session.last, now) => {
            session.last = now;
            let device = session.device;
            drop(sessions);
            let used = lock(&phone.shared.book).used(device, now);
            if let Some(next) = used
                && let Err(e) = phone.shared.save_book(next)
            {
                eprintln!("Phone last use not saved: {e:#}");
            }
            if lock(&phone.shared.book).device(device).is_some() {
                Ok(device)
            } else {
                Err(Box::new(error(
                    StatusCode::UNAUTHORIZED,
                    "This phone is no longer paired",
                )))
            }
        }
        _ => Err(Box::new(error(
            StatusCode::UNAUTHORIZED,
            "Session ended: sign in again",
        ))),
    }
}
fn admit_device(phone: &Phone, device: uuid::Uuid, kind: Kind) -> Option<Response> {
    let now = now_ms();
    let mut limits = lock(&phone.shared.limits);
    let mut take = |kind: Kind| {
        let rate = limits::rate(kind);
        let bucket = *limits
            .devices
            .entry((device, kind))
            .or_insert_with(|| Bucket::full(rate, now));
        let (next, ok) = bucket.take(rate, now);
        limits.devices.insert((device, kind), next);
        (!ok).then(|| retry(next.retry_after(rate)))
    };
    take(Kind::Read).or_else(|| (kind != Kind::Read).then(|| take(kind)).flatten())
}

/// One control request from a paired phone, through the phone allowlist.
pub(super) async fn call(State(phone): State<Phone>, headers: HeaderMap, body: Bytes) -> Response {
    let device = match authenticate(&phone, &headers) {
        Ok(device) => device,
        Err(response) => return *response,
    };
    let value: serde_json::Value = match parse(&body) {
        Ok(value) => value,
        Err(response) => return *response,
    };
    let request: Request = match serde_json::from_value(value.clone()) {
        Ok(request) => request,
        Err(e) => return error(StatusCode::BAD_REQUEST, e),
    };
    if !phone_allowed(&request) {
        return error(StatusCode::FORBIDDEN, "Not available from the phone");
    }
    let kind = limits::kind(&request);
    if let Some(refused) = admit_device(&phone, device, kind) {
        return refused;
    }
    let replay = client_id(&value)
        .filter(|_| kind == Kind::Send)
        .map(|id| phone_key(device, id));
    if let Some(key) = replay {
        match lock(&phone.shared.replays).begin(key, digest(&value), now()) {
            Begin::New => {}
            Begin::InFlight => {
                return error(
                    StatusCode::CONFLICT,
                    "This message is still being sent: check the conversation",
                );
            }
            Begin::Done(answer) => return secure(Json(answer).into_response()),
            Begin::Mismatch => {
                return error(
                    StatusCode::CONFLICT,
                    "This client_id was already used for another message",
                );
            }
        }
    }
    let reply = phone_reply(phone.control.call_phone(request).await);
    // Changed since the phone read it (REQ-037): the phone reads again.
    if let crate::protocol::Reply::Conflict { message } = &reply {
        return error(StatusCode::CONFLICT, message);
    }
    let answer = crate::admin::page_reply(reply);
    if let Some(id) = replay {
        lock(&phone.shared.replays).finish(id, answer.clone());
    }
    secure(Json(answer).into_response())
}

/// The phone unpairs itself.
pub(super) async fn forget(State(phone): State<Phone>, headers: HeaderMap) -> Response {
    let device = match authenticate(&phone, &headers) {
        Ok(device) => device,
        Err(response) => return *response,
    };
    let next = lock(&phone.shared.book).remove(device);
    match next
        .map_err(anyhow::Error::from)
        .and_then(|next| phone.shared.save_book(next))
    {
        Ok(()) => {
            phone.shared.end_sessions(Some(device));
            secure(Json(json!({"result": "forgotten"})).into_response())
        }
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")),
    }
}

#[derive(Clone)]
pub(super) struct Profile {
    pub(super) cert_der: Arc<Vec<u8>>,
    pub(super) host: String,
    pub(super) closed: Arc<AtomicBool>,
}
fn download(
    profile: &Profile,
    client: SocketAddr,
    kind: &'static str,
    name: &str,
    body: Vec<u8>,
) -> Response {
    if profile.closed.load(Ordering::SeqCst) || !is_lan_source(client.ip(), &local_interfaces()) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let mut response = body.into_response();
    let set = response.headers_mut();
    set.insert(header::CONTENT_TYPE, HeaderValue::from_static(kind));
    if let Ok(value) = HeaderValue::from_str(&format!("attachment; filename=\"{name}\"")) {
        set.insert(header::CONTENT_DISPOSITION, value);
    }
    set.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}
pub(super) async fn profile_file(
    State(profile): State<Profile>,
    ConnectInfo(client): ConnectInfo<SocketAddr>,
) -> Response {
    let body = mobileconfig(&profile.cert_der, &profile.host).into_bytes();
    download(
        &profile,
        client,
        "application/x-apple-aspen-config",
        "session-bridge.mobileconfig",
        body,
    )
}
pub(super) async fn certificate_file(
    State(profile): State<Profile>,
    ConnectInfo(client): ConnectInfo<SocketAddr>,
) -> Response {
    download(
        &profile,
        client,
        "application/x-x509-ca-cert",
        "session-bridge.crt",
        profile.cert_der.to_vec(),
    )
}
