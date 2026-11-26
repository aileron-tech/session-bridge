//! Local setup page (REQ-018, REQ-019): served on 127.0.0.1 only, started on
//! the first `session-bridge admin`. Every API call needs the per-service token
//! from that command's URL, the page's own Host and Origin, and an allowlisted
//! request: setup, session listing, messages, and sending as the user.
pub mod markdown;

use crate::{
    daemon::Control,
    protocol::{MessageView, Reply, Request},
};
use anyhow::{Result, anyhow};
use axum::{
    Json, Router,
    body::Bytes,
    extract::State,
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use data_encoding::BASE32_NOPAD;
use serde::Serialize;
use std::sync::Arc;
use tokio::{net::TcpListener, task::JoinHandle};

const PAGE: &str = include_str!("admin/index.html");
/// The page with this bridge's version in its header.
static PAGE_HTML: std::sync::LazyLock<String> =
    std::sync::LazyLock::new(|| PAGE.replace("{{version}}", env!("CARGO_PKG_VERSION")));
const SCRIPT: &str = include_str!("admin/admin.js");
const STYLE: &str = include_str!("admin/admin.css");
const ICON: &str = include_str!("admin/icon.svg");
const POLICY: &str = "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

pub struct Admin {
    pub url: String,
    _server: JoinHandle<()>,
}

#[derive(Clone)]
struct Page {
    control: Control,
    token: Arc<String>,
    port: u16,
    /// Sends by `client_id`, answered once (REQ-036).
    replays: Arc<std::sync::Mutex<crate::phone::replay::Replays>>,
}

/// Binds the page on a free loopback port. Synchronous, so the request
/// handler (which can start the page) does not contain its own future.
pub fn start(control: Control) -> Result<Admin> {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
    listener.set_nonblocking(true)?;
    let listener = TcpListener::from_std(listener)?;
    let port = listener.local_addr()?.port();
    let mut secret = [0u8; 32];
    getrandom::fill(&mut secret).map_err(|e| anyhow!("No system randomness: {e}"))?;
    let token = BASE32_NOPAD.encode(&secret);
    let url = format!("http://127.0.0.1:{port}/#token={token}");
    let app = Router::new()
        .route("/", get(page))
        .route("/admin.js", get(script))
        .route("/admin.css", get(style))
        .route("/icon.svg", get(icon))
        .route("/api/call", post(call))
        .with_state(Page {
            control,
            token: Arc::new(token),
            port,
            replays: Arc::default(),
        });
    let server = tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            eprintln!("Admin page stopped: {e}");
        }
    });
    Ok(Admin {
        url,
        _server: server,
    })
}

fn header_is(headers: &HeaderMap, name: header::HeaderName, allowed: &[String]) -> bool {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| allowed.iter().any(|a| a == v))
}
/// Host must name this server, so a rebinding DNS name cannot reach it.
fn local_host(headers: &HeaderMap, port: u16) -> bool {
    header_is(
        headers,
        header::HOST,
        &[format!("127.0.0.1:{port}"), format!("localhost:{port}")],
    )
}
/// A browser's Origin, when sent, must be this page.
fn same_origin(headers: &HeaderMap, port: u16) -> bool {
    !headers.contains_key(header::ORIGIN)
        || header_is(
            headers,
            header::ORIGIN,
            &[
                format!("http://127.0.0.1:{port}"),
                format!("http://localhost:{port}"),
            ],
        )
}
fn authorized(headers: &HeaderMap, token: &str) -> bool {
    let presented = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or_default();
    presented.len() == token.len()
        && presented
            .bytes()
            .zip(token.bytes())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0
}
/// Setup, listing, messages and the user's own sends. Agent sends (which name
/// a sender session), native peer frames and the admin URL stay out of reach.
fn allowed(request: &Request) -> bool {
    matches!(
        request,
        Request::Status
            | Request::Lan
            | Request::Network { .. }
            | Request::Pair { .. }
            | Request::PairCode { .. }
            | Request::Unpair { .. }
            | Request::List { .. }
            | Request::Messages
            | Request::UserSend { .. }
            | Request::ChannelSend { .. }
            | Request::Channels
            | Request::ChannelDefault { .. }
            | Request::Schedules
            | Request::SaveSchedule { .. }
            | Request::DeleteSchedule { .. }
            | Request::RunSchedule { .. }
            | Request::Roles
            | Request::SaveRole { .. }
            | Request::DeleteRole { .. }
            | Request::AssignRole { .. }
            | Request::Remotes
            | Request::AddRemote { .. }
            | Request::RemoveRemote { .. }
            | Request::Spawn { .. }
            | Request::Close { .. }
            | Request::SpawnSettings
            | Request::SaveSpawnSettings { .. }
            | Request::Projects
            | Request::Waiting
            | Request::RemoveWaiting { .. }
            | Request::ImportAliases
            | Request::ProjectMaster { .. }
            | Request::Usage
            | Request::Phones
            | Request::PhoneAccess { .. }
            | Request::PhoneOffer
            | Request::PhoneConfirm { .. }
            | Request::PhoneUnpair { .. }
    )
}

/// The page's view of a reply: message text also comes as Markdown blocks.
pub(crate) fn page_reply(reply: Reply) -> serde_json::Value {
    #[derive(Serialize)]
    struct PageMessage {
        #[serde(flatten)]
        view: MessageView,
        blocks: Vec<markdown::Block>,
    }
    match reply {
        Reply::Messages { user, messages } => serde_json::json!({
            "result": "messages",
            "user": user,
            "messages": messages
                .into_iter()
                .map(|view| PageMessage {
                    blocks: markdown::parse(&view.message),
                    view,
                })
                .collect::<Vec<_>>(),
        }),
        other => serde_json::to_value(other).unwrap_or_else(|e| {
            serde_json::json!({"result": "error", "message": format!("Reply not serializable: {e}")})
        }),
    }
}

fn asset(page: &Page, headers: &HeaderMap, kind: &'static str, body: &'static str) -> Response {
    if !local_host(headers, page.port) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let mut response = body.into_response();
    let set = response.headers_mut();
    set.insert(header::CONTENT_TYPE, HeaderValue::from_static(kind));
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
    response
}
async fn page(State(page): State<Page>, headers: HeaderMap) -> Response {
    asset(
        &page,
        &headers,
        "text/html; charset=utf-8",
        PAGE_HTML.as_str(),
    )
}
async fn script(State(page): State<Page>, headers: HeaderMap) -> Response {
    asset(&page, &headers, "text/javascript; charset=utf-8", SCRIPT)
}
async fn style(State(page): State<Page>, headers: HeaderMap) -> Response {
    asset(&page, &headers, "text/css; charset=utf-8", STYLE)
}
async fn icon(State(page): State<Page>, headers: HeaderMap) -> Response {
    asset(&page, &headers, "image/svg+xml", ICON)
}
async fn call(State(page): State<Page>, headers: HeaderMap, body: Bytes) -> Response {
    if !local_host(&headers, page.port) || !same_origin(&headers, page.port) {
        return StatusCode::FORBIDDEN.into_response();
    }
    if !authorized(&headers, &page.token) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let value: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(e) => return (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    };
    let request: Request = match serde_json::from_value(value.clone()) {
        Ok(request) => request,
        Err(e) => return (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    };
    if !allowed(&request) {
        return StatusCode::FORBIDDEN.into_response();
    }
    // A send repeated with its `client_id` gets the first answer (REQ-036).
    use crate::phone::replay::{Begin, client_id, digest};
    let replay = client_id(&value).filter(|_| {
        matches!(
            request,
            Request::UserSend { .. } | Request::ChannelSend { .. }
        )
    });
    let lock = || {
        page.replays
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    };
    if let Some(id) = replay {
        match lock().begin(id, digest(&value), crate::phone::service::now()) {
            Begin::New => {}
            Begin::InFlight => {
                return (
                    StatusCode::CONFLICT,
                    "This message is still being sent: check the conversation",
                )
                    .into_response();
            }
            Begin::Done(answer) => return Json(answer).into_response(),
            Begin::Mismatch => {
                return (
                    StatusCode::CONFLICT,
                    "This client_id was already used for another message",
                )
                    .into_response();
            }
        }
    }
    let answer = page_reply(page.control.call_admin(request).await);
    if let Some(id) = replay {
        lock().finish(id, answer.clone());
    }
    Json(answer).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(header::HeaderName, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(name.clone(), HeaderValue::from_str(value).unwrap());
        }
        map
    }

    /* REQ-018: Only this server's host and origin, with the exact token, pass. */
    #[test]
    fn checks_host_origin_and_token() {
        assert!(local_host(&headers(&[(header::HOST, "127.0.0.1:5")]), 5));
        assert!(local_host(&headers(&[(header::HOST, "localhost:5")]), 5));
        assert!(!local_host(&headers(&[(header::HOST, "127.0.0.1:6")]), 5));
        assert!(!local_host(&headers(&[]), 5));
        assert!(same_origin(&headers(&[]), 5));
        assert!(same_origin(
            &headers(&[(header::ORIGIN, "http://localhost:5")]),
            5
        ));
        assert!(!same_origin(
            &headers(&[(header::ORIGIN, "http://evil.example")]),
            5
        ));
        assert!(authorized(
            &headers(&[(header::AUTHORIZATION, "Bearer ABC")]),
            "ABC"
        ));
        for bad in ["Bearer ABD", "Bearer AB", "ABC", "Basic ABC"] {
            assert!(!authorized(
                &headers(&[(header::AUTHORIZATION, bad)]),
                "ABC"
            ));
        }
        assert!(!authorized(&headers(&[]), "ABC"));
    }

    /* REQ-018: The page shows this bridge's version in its header. */
    #[test]
    fn shows_the_version() {
        assert!(PAGE_HTML.contains(&format!(">{}</span>", env!("CARGO_PKG_VERSION"))));
        assert!(!PAGE_HTML.contains("{{version}}"));
    }
    /* REQ-019: The page lists sessions, reads messages and writes as the user;
    agent sends, native frames and admin URLs stay out of reach. */
    #[test]
    fn allows_setup_listing_and_user_messages() {
        assert!(allowed(&Request::Lan));
        assert!(allowed(&Request::Status));
        assert!(allowed(&Request::Messages));
        assert!(allowed(&Request::List {
            sender: None,
            project: None
        }));
        assert!(allowed(&Request::UserSend {
            session_id: String::new(),
            message: String::new(),
            channel: None,
            scheduled: false,
            team_report: false
        }));
        assert!(allowed(&Request::Schedules));
        assert!(allowed(&Request::ChannelSend {
            project: String::new(),
            message: String::new(),
            team_report: false
        }));
        assert!(allowed(&Request::Channels));
        assert!(allowed(&Request::ChannelDefault {
            project: String::new(),
            name: None
        }));
        assert!(!allowed(&Request::Admin));
        assert!(!allowed(&Request::Shutdown));
        assert!(!allowed(&Request::Health));
        assert!(!allowed(&Request::Send {
            sender: String::new(),
            session_id: String::new(),
            message: String::new(),
            project: None
        }));
    }

    /* REQ-029: The page reads host and session usage. */
    #[test]
    fn allows_usage() {
        assert!(allowed(&Request::Usage));
    }

    /* REQ-019: Message text reaches the page as typed blocks beside the raw text. */
    #[test]
    fn adds_blocks_to_messages() {
        let user = crate::domain::SessionId {
            provider: crate::domain::Provider::User,
            id: uuid::Uuid::nil(),
        };
        let view = crate::protocol::MessageView {
            message_id: uuid::Uuid::nil(),
            sender: user.clone(),
            sender_label: "you".into(),
            recipient: user.clone(),
            recipient_label: "fixer (mono)".into(),
            message: "**done**".into(),
            delivery: crate::domain::Delivery::Delivered,
            created_at: 7,
            channel: None,
            scheduled: false,
        };
        let page = page_reply(Reply::Messages {
            user,
            messages: vec![view],
        });
        assert_eq!(page["result"], "messages");
        let message = &page["messages"][0];
        assert_eq!(message["message"], "**done**");
        assert_eq!(message["sender_label"], "you");
        assert_eq!(message["delivery"]["state"], "delivered");
        assert_eq!(message["blocks"][0]["type"], "paragraph");
        assert_eq!(message["blocks"][0]["content"][0]["type"], "strong");
        let other = page_reply(Reply::Health { pid: 1 });
        assert_eq!(other["result"], "health");
    }
}
