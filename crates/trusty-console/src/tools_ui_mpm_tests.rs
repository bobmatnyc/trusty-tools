//! Tests for `tools_ui_mpm` (#9979): the page, its redirect, its markers and
//! the loopback-only guard on the page and the chat POST.

use std::net::SocketAddr;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{HeaderValue, Request, StatusCode, header};
use http_body_util::BodyExt;
use tower::ServiceExt;

use super::*;
use crate::routes::chat::MPM_CHAT_API_PATH;
use crate::server::{AppState, build_router};

/// Send `req` through the real router; status, headers and body text.
async fn send(req: Request<Body>) -> (StatusCode, axum::http::HeaderMap, String) {
    let router = build_router(AppState::new(vec![]));
    let resp = router.oneshot(req).await.expect("response");
    let status = resp.status();
    let headers = resp.headers().clone();
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    (
        status,
        headers,
        String::from_utf8_lossy(&bytes).into_owned(),
    )
}

/// A request from `peer`, as the listener's `ConnectInfo<SocketAddr>` reports it.
fn from_peer(method: &str, uri: &str, peer: &str) -> Request<Body> {
    let peer: SocketAddr = peer.parse().expect("peer addr");
    let mut req = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(r#"{"message":"hi","actions":true}"#))
        .expect("request");
    req.extensions_mut().insert(ConnectInfo(peer));
    req
}

fn assert_unframeable(what: &str, headers: &axum::http::HeaderMap) {
    assert_eq!(
        headers.get(header::X_FRAME_OPTIONS),
        Some(&HeaderValue::from_static("DENY")),
        "{what}"
    );
    assert_eq!(
        headers.get(header::CONTENT_SECURITY_POLICY),
        Some(&HeaderValue::from_static("frame-ancestors 'none'")),
        "{what}"
    );
}

/// Why: the page must be served to a browser on this machine, as HTML, and
/// never framed.
/// Test: this test.
#[tokio::test]
async fn chat_page_is_served_as_html_to_a_loopback_client() {
    let (status, headers, body) =
        send(from_peer("GET", MPM_CHAT_PAGE_PATH, "127.0.0.1:50000")).await;
    assert_eq!(status, StatusCode::OK);
    let ctype = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    assert!(ctype.starts_with("text/html"), "got {ctype:?}");
    assert_eq!(body, MPM_CHAT_HTML);
    assert_unframeable("page", &headers);
}

/// Why: like the other `/tools/<tool>` mounts, the bare path redirects to the
/// trailing-slash form.
/// Test: this test.
#[tokio::test]
async fn bare_chat_path_redirects_to_the_slash_form() {
    let req = Request::builder()
        .uri("/tools/mpm/chat")
        .body(Body::empty())
        .expect("request");
    let (status, headers, _) = send(req).await;
    assert_eq!(status, StatusCode::PERMANENT_REDIRECT);
    assert_eq!(
        headers.get(header::LOCATION),
        Some(&HeaderValue::from_static(MPM_CHAT_PAGE_PATH))
    );
}

/// Why: ported from the daemon's `web_chat_page_serves_html` and
/// `web_chat_path_is_registered` — the page must POST to the route the router
/// registers (the router mounts the same `MPM_CHAT_API_PATH` constant; a
/// drift would be a silent 404), opt into the action loop, and render the
/// reply, `conv_id`, the `actions_taken` audit trail and `cost`.
/// Test: this test; `chat_route_is_not_shadowed_by_the_id_capture` proves a
/// POST to that constant reaches the handler.
#[test]
fn chat_page_fetches_the_registered_chat_route() {
    assert!(MPM_CHAT_HTML.contains("<!DOCTYPE html>"));
    let needle = format!("fetch(\"{MPM_CHAT_API_PATH}\"");
    assert!(
        MPM_CHAT_HTML.contains(&needle),
        "the page must fetch {MPM_CHAT_API_PATH:?}, the route the router mounts"
    );
    assert_eq!(
        MPM_CHAT_HTML.matches("fetch(").count(),
        1,
        "the page makes one call"
    );
    assert!(MPM_CHAT_HTML.contains("{ message, actions: true }"));
    assert!(MPM_CHAT_HTML.contains("body.conv_id = convId"));
    assert!(MPM_CHAT_HTML.contains("data.reply"));
    assert!(MPM_CHAT_HTML.contains("Array.isArray(data.actions_taken)"));
    assert!(MPM_CHAT_HTML.contains("data.cost.toFixed(4)"));
    assert!(MPM_CHAT_HTML.contains("--trusty-accent"), "Foundry tokens");
    assert!(!MPM_CHAT_HTML.contains("/api/v1/"), "no daemon HTTP path");
}

/// Why: ruling 2 — a caller off this machine gets neither the page nor a
/// chat turn, and the refusal is unframeable too.
/// What: a non-loopback `ConnectInfo` peer gets 403 with the loopback text on
/// the page, the bare path, and the chat POST.
/// Test: this test.
#[tokio::test]
async fn a_non_loopback_peer_is_refused_the_page_and_the_turn() {
    for (method, uri) in [
        ("GET", MPM_CHAT_PAGE_PATH),
        ("GET", "/tools/mpm/chat"),
        ("POST", MPM_CHAT_API_PATH),
    ] {
        let (status, headers, body) = send(from_peer(method, uri, "100.64.0.7:5555")).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} {uri}");
        assert!(body.contains("loopback"), "{method} {uri}: {body}");
        assert_unframeable(uri, &headers);
    }
}

/// Why: a request the tailnet listener tagged is refused even with no peer
/// address attached.
/// Test: this test.
#[tokio::test]
async fn a_tailnet_tagged_turn_is_refused() {
    let mut req = from_peer("POST", MPM_CHAT_API_PATH, "127.0.0.1:50000");
    req.extensions_mut()
        .insert(crate::tailnet_peer::TailnetRequest);
    let (status, _, body) = send(req).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
}
