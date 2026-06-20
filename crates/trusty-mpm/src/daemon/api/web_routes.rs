//! Web control-surface route (`GET /web`) — DOC-20 §6 Phase 2 / ONB-3.
//!
//! Why: the chat-core nucleus (DOC-20) makes every operator verb reachable
//! through one action-capable HTTP endpoint, `POST /api/v1/sessions/chat` with
//! `actions: true`. ONB-3 (DOC-18) names a **Web** control surface as a peer of
//! the TUI and Telegram. This module is that surface — but kept deliberately
//! thin: it serves a single static browser chat page that drives the EXISTING
//! endpoint. It embeds **no** session logic, **no** resolver, **no** transport;
//! it is a presentation layer over the same nucleus, exactly as the spec's
//! "adapters are thin" invariant requires.
//! What: the `web_chat_page` handler returns the embedded `web_chat.html` (an
//! `include_str!` so there is no build step and `SKIP_UI_BUILD` is irrelevant —
//! nothing is compiled). The page POSTs to `/api/v1/sessions/chat`, renders the
//! `reply`, and surfaces the `actions_taken` audit trail. It is wired into the
//! router by `api::router`.
//! Test: `web_chat_tests` pins the served status, content-type, and the
//! load-bearing markers in the page body (the endpoint path it calls and the
//! `actions:true` opt-in).

use axum::response::Html;

/// The embedded Web chat page.
///
/// Why: shipping the page as a committed `include_str!` asset keeps the Web
/// adapter zero-cost and zero-dependency — no Svelte/Vite/pnpm build, so a
/// default `cargo build -p trusty-mpm` (and `SKIP_UI_BUILD`) needs nothing
/// extra. The page is a true client of the chat-core endpoint.
/// What: the raw HTML+JS body of `web_chat.html`, served verbatim.
/// Test: `web_chat_page_serves_html` asserts it is non-empty and well-formed.
const WEB_CHAT_HTML: &str = include_str!("web_chat.html");

/// `GET /web` — the browser chat surface over the chat-core nucleus.
///
/// Why: gives the operator a no-install control surface (just a browser) that is
/// a peer to the TUI/Telegram adapters. It is intentionally a static page rather
/// than a server-rendered one so the daemon stays stateless about the Web UI and
/// the page can be cached; all dynamism happens client-side against the existing
/// action-capable chat endpoint.
/// What: returns the embedded [`WEB_CHAT_HTML`] as an `Html` response (axum sets
/// `content-type: text/html; charset=utf-8`). Always `200`; no state is touched,
/// so it works even when the SM/LLM is unconfigured (the page surfaces the
/// endpoint's `503` to the operator at send time).
/// Test: `web_chat_page_serves_html`, and the router-level
/// `web_route_returns_200_html` in `web_chat_tests`.
#[utoipa::path(
    get,
    path = "/web",
    tag = "config",
    responses((status = 200, description = "Browser chat surface over /api/v1/sessions/chat"))
)]
pub async fn web_chat_page() -> Html<&'static str> {
    Html(WEB_CHAT_HTML)
}

#[cfg(test)]
mod web_chat_tests {
    use super::*;
    use crate::daemon::state::DaemonState;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use std::sync::Arc;
    use tower::ServiceExt;

    /// Why: the page must actually exist and carry the load-bearing wiring — the
    /// endpoint it calls and the `actions:true` opt-in — or the adapter silently
    /// breaks. A pure-string assertion needs no daemon, so it is CI-safe.
    /// What: asserts the embedded body is non-empty, is an HTML document, calls
    /// the chat-core endpoint, and opts into the action loop.
    /// Test: this is the test.
    #[tokio::test]
    async fn web_chat_page_serves_html() {
        let Html(body) = web_chat_page().await;
        assert!(!body.is_empty(), "embedded page must not be empty");
        assert!(body.contains("<!DOCTYPE html>"), "must be an HTML document");
        assert!(
            body.contains("/api/v1/sessions/chat"),
            "page must POST to the chat-core endpoint"
        );
        assert!(
            body.contains("actions: true") || body.contains("actions:true"),
            "page must opt into the action-capable loop"
        );
        assert!(
            body.contains("actions_taken"),
            "page must render the audit trail"
        );
        assert!(
            body.contains("conv_id"),
            "page must continue the SM rolling context via conv_id"
        );
    }

    /// Why: pins the wiring into the daemon router — `GET /web` must resolve to
    /// the handler and return a `200` with an HTML content-type, exactly as a
    /// browser expects. This is the router-level contract the other adapters get
    /// for free but the Web one must register explicitly.
    /// What: builds the full router over a default `DaemonState`, drives a
    /// `GET /web` request via `oneshot`, asserts `200` and the `text/html`
    /// content-type. Pure in-memory — no socket bind, CI-safe.
    /// Test: this is the test.
    #[tokio::test]
    async fn web_route_returns_200_html() {
        let state = Arc::new(DaemonState::new());
        let app = crate::daemon::api::router(state);

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/web")
                    .body(Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("router serves /web");

        assert_eq!(resp.status(), StatusCode::OK);
        let ctype = resp
            .headers()
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();
        assert!(
            ctype.starts_with("text/html"),
            "expected text/html, got {ctype:?}"
        );
    }
}
