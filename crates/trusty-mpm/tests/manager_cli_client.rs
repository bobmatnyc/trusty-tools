//! HTTP-level integration tests for the `tm manager` CLI's `DaemonClient`
//! methods (DOC-36 §3.2/§6 phase 1, epic #2109, WI-6 #2583).
//!
//! Why: `tests/manager_routes.rs` proves the daemon SIDE of `/api/v1/manager/*`
//! (the routes this WI's CLI calls); this file proves the CLIENT side —
//! [`DaemonClient::manager_status`]/[`manager_digest`]/[`manager_chat`] — over
//! real HTTP, following the same real-axum+reqwest harness
//! (`api::router` on an ephemeral loopback port, driven with `reqwest`) rather
//! than mocking at the transport layer. Two scenarios matter most for this
//! WI: (1) `manager_status` against the REAL daemon router (the endpoint has
//! been live since PR #2598) and (2) `manager_digest`/`manager_chat` against
//! that SAME real router returning a genuine `404` — proving the
//! 404-older-daemon degrade contract against the daemon as it actually stands
//! on `origin/main` today, not a hand-waved assumption. A minimal mock router
//! additionally proves the happy-path loose-parse and the error-surfacing
//! path once a sibling PR's shape is available, without depending on that
//! PR having landed.
//! What: `manager_status_client_reads_live_route`,
//! `manager_digest_and_chat_degrade_cleanly_on_404_against_real_daemon`
//! (against the real router); `manager_digest_client_parses_mock_narrative`,
//! `manager_chat_client_parses_mock_reply`,
//! `manager_digest_client_surfaces_mock_error_body` (against a hand-built
//! mock router standing in for the sibling PR's not-yet-landed shape).
//! Test: this file IS the test; run with
//! `cargo test -p trusty-mpm --test manager_cli_client`.

use std::future::IntoFuture;
use std::sync::Arc;

use axum::extract::Query;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router, http::StatusCode};
use trusty_mpm::client::DaemonClient;
use trusty_mpm::daemon::{api, state::DaemonState};
use trusty_mpm::project::Project;

/// Serve the REAL daemon router on an ephemeral loopback port; return its base URL.
async fn serve_real(state: Arc<DaemonState>) -> String {
    let router = api::router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(axum::serve(listener, router).into_future());
    format!("http://{addr}")
}

/// `DaemonClient::manager_status` against the real, currently-shipped
/// `GET /api/v1/manager/status` route — proves the CLI's status path end to
/// end, not merely the wire-shape unit tests in `client/http_client/manager.rs`.
#[tokio::test]
async fn manager_status_client_reads_live_route() {
    let root = tempfile::tempdir().unwrap().keep();
    let state = Arc::new(DaemonState::with_root_isolated_managed(root).await);
    state
        .project_registry()
        .await
        .register(Project {
            name: "widget".to_string(),
            repo_url: "https://github.com/acme/widget".to_string(),
            default_branch: "main".to_string(),
            stack_hint: None,
            tags: vec![],
            description: None,
            gh_user: None,
            github: None,
            commit_name: None,
            commit_email: None,
        })
        .await
        .expect("register project");

    let base = serve_real(Arc::clone(&state)).await;
    let client = DaemonClient::new(base);

    let status = client.manager_status().await.expect("manager_status");
    assert_eq!(status.project_count, 1);
    assert_eq!(status.projects[0].project_name, "widget");
}

/// `manager_digest`/`manager_chat` against the REAL router, which does not
/// mount `/api/v1/manager/{digest,chat}` yet (WI-3/#2580, WI-4/#2581 are
/// concurrent siblings) — proves the `Ok(None)` 404-degrade contract holds
/// against the daemon as it actually ships today, which is exactly the
/// "older daemon" scenario the CLI handler
/// (`commands::manager::digest`/`chat`) turns into its upgrade message.
#[tokio::test]
async fn manager_digest_and_chat_degrade_cleanly_on_404_against_real_daemon() {
    let root = tempfile::tempdir().unwrap().keep();
    let state = Arc::new(DaemonState::with_root_isolated_managed(root).await);
    let base = serve_real(Arc::clone(&state)).await;
    let client = DaemonClient::new(base);

    let digest = client
        .manager_digest("portfolio")
        .await
        .expect("manager_digest call should not error on 404");
    assert!(
        digest.is_none(),
        "expected None (404 degrade) against a daemon with no digest route mounted"
    );

    let chat = client
        .manager_chat("cli:test", "hello")
        .await
        .expect("manager_chat call should not error on 404");
    assert!(
        chat.is_none(),
        "expected None (404 degrade) against a daemon with no chat route mounted"
    );
}

/// A minimal stand-in for the sibling PR's `GET /manager/digest` /
/// `POST /manager/chat` routes, used only to prove the CLIENT's happy-path
/// and error-surfacing behavior without depending on that PR having merged.
fn mock_manager_router() -> Router {
    async fn digest_ok(
        Query(params): Query<std::collections::HashMap<String, String>>,
    ) -> impl IntoResponse {
        let scope = params.get("scope").cloned().unwrap_or_default();
        Json(serde_json::json!({
            "scope": scope,
            "narrative": "3 sessions active across 2 projects",
            "fallback": false,
        }))
    }
    async fn chat_ok(Json(body): Json<serde_json::Value>) -> impl IntoResponse {
        let key = body
            .get("conversation_key")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        Json(serde_json::json!({
            "reply": "everything looks healthy right now",
            "conversation_key": key,
        }))
    }
    Router::new()
        .route("/api/v1/manager/digest", get(digest_ok))
        .route("/api/v1/manager/chat", post(chat_ok))
}

async fn serve_mock(router: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(axum::serve(listener, router).into_future());
    format!("http://{addr}")
}

/// Happy-path digest parse against a mock 200 response carrying a
/// `narrative` field — proves [`DaemonClient::manager_digest`]'s Some(..)
/// branch over real HTTP (not just the in-module unit test against a
/// hand-built `serde_json::Value`).
#[tokio::test]
async fn manager_digest_client_parses_mock_narrative() {
    let base = serve_mock(mock_manager_router()).await;
    let client = DaemonClient::new(base);

    let outcome = client
        .manager_digest("portfolio")
        .await
        .expect("call")
        .expect("Some(outcome) on 200");
    assert_eq!(outcome.narrative, "3 sessions active across 2 projects");
    assert!(!outcome.fallback);
}

/// Happy-path chat parse against a mock 200 response — proves
/// [`DaemonClient::manager_chat`]'s Some(..) branch over real HTTP, and that
/// the conversation key round-trips.
#[tokio::test]
async fn manager_chat_client_parses_mock_reply() {
    let base = serve_mock(mock_manager_router()).await;
    let client = DaemonClient::new(base);

    let outcome = client
        .manager_chat("cli:alice", "what needs my attention?")
        .await
        .expect("call")
        .expect("Some(outcome) on 200");
    assert_eq!(outcome.reply, "everything looks healthy right now");
    assert_eq!(outcome.conversation_key, "cli:alice");
}

/// A `503` carrying a JSON `error` body (the "no inference provider
/// configured" scenario the CLI is meant to render as a clean, actionable
/// error rather than a bare status line) surfaces as `Err` with the daemon's
/// message intact, via the shared `response_or_body_error` helper (#2485) —
/// NOT a `404`-style `Ok(None)` degrade, since the endpoint exists and is
/// answering, it just has nothing useful to say.
#[tokio::test]
async fn manager_digest_client_surfaces_mock_error_body() {
    async fn no_provider_stub() -> impl IntoResponse {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({ "error": "no inference provider configured" })),
        )
    }
    let router = Router::new().route("/api/v1/manager/digest", get(no_provider_stub));
    let base = serve_mock(router).await;
    let client = DaemonClient::new(base);

    let err = client
        .manager_digest("portfolio")
        .await
        .expect_err("503 must surface as Err, not Ok(None)");
    assert!(
        err.to_string().contains("no inference provider configured"),
        "error message should carry the daemon's body text (#2485): {err}"
    );
}
