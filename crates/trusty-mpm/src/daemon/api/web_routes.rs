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
///
/// Threat model (review #1503): this page drives the STATE-MUTATING, action-
/// capable `POST /api/v1/sessions/chat` (`actions: true`), which can spawn/stop/
/// mutate managed sessions. The daemon binds **loopback by default**, so the
/// browser surface is normally only reachable from the same machine. If the
/// daemon is ever bound to a non-loopback interface, the CSRF mitigation is the
/// Origin guard on that POST handler
/// ([`origin_allowed`](crate::daemon::api::origin_guard::origin_allowed)): a
/// cross-origin browser POST is rejected with `403`, while same-origin/loopback
/// requests and header-less server-side callers (Telegram/TUI via reqwest,
/// `curl`) pass unchanged. This `GET /web` page itself is a read-only static
/// asset and touches no state, so it carries no CSRF risk on its own.
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
            body.contains(crate::daemon::api::coordinator_routes::COORDINATOR_CHAT_PATH),
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

    /// Why: this is the EXACT drift the #1503 review feared — the browser page
    /// POSTing to a path the router never registered (a silent 404). The fix
    /// routes BOTH the router registration (`api::router`) and this page through
    /// ONE shared constant, `COORDINATOR_CHAT_PATH`; this test pins the page's
    /// `fetch("…")` literal to that constant. Because the router mounts the route
    /// with the SAME constant (`.route(COORDINATOR_CHAT_PATH, …)`), agreement here
    /// proves the page targets a registered route — turning a would-be runtime 404
    /// into a test failure. Kept pure-string (no router/`DaemonState` build) so it
    /// runs without the daemon-spawn blocking-runtime panic that the router-level
    /// `web_route_returns_200_html` hits in the local test env.
    /// What: asserts the embedded page contains `fetch("<COORDINATOR_CHAT_PATH>"`,
    /// the verbatim shared constant the router registers.
    /// Test: this is the test.
    #[test]
    fn web_chat_path_is_registered() {
        use crate::daemon::api::coordinator_routes::COORDINATOR_CHAT_PATH;

        // The page must call the shared constant verbatim. We locate the
        // `fetch("…")` literal so a renamed/typo'd path in the HTML is caught.
        // The router mounts this SAME constant, so a match proves the page POSTs to
        // a registered route (no 404).
        let needle = format!("fetch(\"{COORDINATOR_CHAT_PATH}\"");
        assert!(
            WEB_CHAT_HTML.contains(&needle),
            "web_chat.html must fetch the shared COORDINATOR_CHAT_PATH ({COORDINATOR_CHAT_PATH:?}) \
             that api::router registers; route/HTML drift would otherwise 404"
        );
    }

    /// Why: #1524 audit-trail behavior — the page must RENDER the `actions_taken`
    /// array (not just contain the string). This test asserts the JS actually
    /// checks `Array.isArray(data.actions_taken)` and iterates it — proving the
    /// audit trail is rendered, not merely present as a dead string literal.
    /// What: asserts the embedded JS uses `Array.isArray` on `actions_taken` and
    /// iterates it with a loop that appends list items.
    /// Test: this is the test.
    #[test]
    fn web_chat_renders_actions_taken_array() {
        // The JS must guard with Array.isArray before iterating — a bare string
        // check ("actions_taken") is insufficient to prove it renders the audit trail.
        assert!(
            WEB_CHAT_HTML.contains("Array.isArray(data.actions_taken)"),
            "web_chat.html must check Array.isArray(data.actions_taken) before rendering"
        );
        // The loop must append list items — not just log or discard the array.
        assert!(
            WEB_CHAT_HTML.contains("appendActions(data.actions_taken"),
            "web_chat.html must pass actions_taken to appendActions for rendering"
        );
    }

    /// Why: #1524 conv_id continuity behavior — the page must STORE `conv_id` from
    /// the response AND RE-SEND it on the next turn (session continuity). A bare
    /// string presence check ("conv_id") does not prove the store-and-resend flow.
    /// What: asserts (a) the JS stores the response conv_id in a variable, and (b)
    /// re-sends it on the next POST.
    /// Test: this is the test.
    #[test]
    fn web_chat_stores_and_resends_conv_id() {
        // (a) The response conv_id must be stored: `if (data.conv_id) { convId = data.conv_id; … }`.
        assert!(
            WEB_CHAT_HTML.contains("convId = data.conv_id"),
            "web_chat.html must store data.conv_id in convId for continuity"
        );
        // (b) The stored id must be re-sent on the next POST:
        // `if (convId) body.conv_id = convId`.
        assert!(
            WEB_CHAT_HTML.contains("body.conv_id = convId"),
            "web_chat.html must re-send convId as body.conv_id on the next turn"
        );
    }
}
