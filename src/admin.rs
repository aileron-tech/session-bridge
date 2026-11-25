//! Local setup page (REQ-018): served on 127.0.0.1 only, started on the first
//! `session-bridge admin`. Every API call needs the per-service token from
//! that command's URL, the page's own Host and Origin, and an allowlisted
//! setup request; message sending is not available here.
use crate::{
    daemon::Control,
    protocol::{Reply, Request},
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
use std::sync::Arc;
use tokio::{net::TcpListener, task::JoinHandle};

const PAGE: &str = include_str!("admin/index.html");
const SCRIPT: &str = include_str!("admin/admin.js");
const STYLE: &str = include_str!("admin/admin.css");
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
        .route("/api/call", post(call))
        .with_state(Page {
            control,
            token: Arc::new(token),
            port,
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
/// Setup and status only: sending, listing sessions and native peer frames stay with agents.
fn allowed(request: &Request) -> bool {
    matches!(
        request,
        Request::Status
            | Request::Lan
            | Request::Network { .. }
            | Request::Pair { .. }
            | Request::PairCode { .. }
            | Request::Unpair { .. }
    )
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
    asset(&page, &headers, "text/html; charset=utf-8", PAGE)
}
async fn script(State(page): State<Page>, headers: HeaderMap) -> Response {
    asset(&page, &headers, "text/javascript; charset=utf-8", SCRIPT)
}
async fn style(State(page): State<Page>, headers: HeaderMap) -> Response {
    asset(&page, &headers, "text/css; charset=utf-8", STYLE)
}
async fn call(State(page): State<Page>, headers: HeaderMap, body: Bytes) -> Response {
    if !local_host(&headers, page.port) || !same_origin(&headers, page.port) {
        return StatusCode::FORBIDDEN.into_response();
    }
    if !authorized(&headers, &page.token) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let request: Request = match serde_json::from_slice(&body) {
        Ok(request) => request,
        Err(e) => return (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    };
    if !allowed(&request) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let reply: Reply = page.control.call(request).await;
    Json(reply).into_response()
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

    /* REQ-018: Sending and session listing are not reachable from the page. */
    #[test]
    fn allows_setup_requests_only() {
        assert!(allowed(&Request::Lan));
        assert!(allowed(&Request::Status));
        assert!(!allowed(&Request::Admin));
        assert!(!allowed(&Request::Health));
        assert!(!allowed(&Request::List {
            sender: None,
            project: None
        }));
        assert!(!allowed(&Request::Send {
            sender: String::new(),
            session_id: String::new(),
            message: String::new(),
            project: None
        }));
    }
}
