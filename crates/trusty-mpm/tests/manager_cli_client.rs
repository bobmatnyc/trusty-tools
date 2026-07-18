//! HTTP-level integration tests for the `tm manager` CLI's `DaemonClient`
//! methods (DOC-36 §3.2/§6 phase 1, epic #2109, WI-6 #2583).
//!
//! Why: `tests/manager_inference.rs` (WI-7 #2584) proves the daemon SIDE of
//! `/api/v1/manager/{digest,chat}`; this file proves the CLIENT side —
//! [`DaemonClient::manager_status`]/[`manager_digest`]/[`manager_chat`] — over
//! real HTTP, driving the REAL `api::router` (mirroring `tests/manager_routes.rs`'s
//! harness) rather than a hand-invented mock. Every mock body used here is
//! either the real router's own response (preferred, byte-exact by
//! construction) or, for the one scenario the real router cannot produce on
//! `origin/main` today (a daemon that PREDATES WI-3/WI-4), a hand-built
//! `/manager/version` stub reporting `available: false` — the exact signal
//! [`DaemonClient::manager_digest`]/[`manager_chat`]'s feature-detection reads.
//!
//! Three review-found behaviors this suite pins (PR #2600 paired review
//! against PR #2601's shapes):
//! 1. The daemon's 503 (no provider) / 502 (call failed) digest degrade body
//!    IS a full `DigestResponse` (narrative + status), not a bare error —
//!    `manager_digest_client_reads_deterministic_fallback_against_real_daemon`.
//! 2. `generated_by` (not a `fallback` boolean, which the real daemon never
//!    sends) is the fallback marker — same test, asserts `outcome.fallback`.
//! 3. A `404` for `scope=project:<name>` naming an unregistered project is the
//!    daemon's OWN message, not a "your daemon is too old" signal —
//!    `manager_digest_client_surfaces_unknown_project_404_against_real_daemon`.
//!    The true old-daemon case is covered separately against a stub `version`
//!    response reporting the routes unavailable.
//!
//! What: `manager_status_client_reads_live_route`,
//! `manager_digest_client_reads_llm_narrative_against_real_daemon`,
//! `manager_digest_client_reads_deterministic_fallback_against_real_daemon`,
//! `manager_digest_client_surfaces_unknown_project_404_against_real_daemon`,
//! `manager_chat_client_reads_llm_reply_against_real_daemon`,
//! `manager_chat_client_surfaces_degrade_message_against_real_daemon`,
//! `manager_digest_and_chat_degrade_cleanly_on_404_against_older_daemon`; plus
//! (coordinator review finding 2, WI-8 #2585) `manager_route_task_client_reads_live_route_against_real_daemon`,
//! `manager_route_task_client_surfaces_400_for_empty_text_against_real_daemon`
//! (the non-404 error path — `DaemonClient::manager_route_task`'s HTTP method,
//! timeout wiring, and `Err` mapping were previously unexercised beyond the
//! `from_body` parser unit tests), and
//! `manager_route_task_client_degrades_cleanly_on_404_against_older_daemon`.
//! Test: this file IS the test; run with
//! `cargo test -p trusty-mpm --test manager_cli_client`.

use std::future::IntoFuture;
use std::sync::Arc;

use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use trusty_common::inference::registry::{ProviderId, capabilities};
use trusty_common::inference::test_support::ScriptedAdapter;
use trusty_common::inference::types::UsageBlock;
use trusty_common::inference::{AssistantMessage, ChatChoice, ChatResponse, InferenceAdapter};
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

/// A fresh, hermetic daemon state under a temp framework root.
async fn fresh_state() -> Arc<DaemonState> {
    let root = tempfile::tempdir().unwrap().keep();
    Arc::new(DaemonState::with_root_isolated_managed(root).await)
}

/// Register a project fixture on the given daemon state.
async fn register_project(state: &Arc<DaemonState>, name: &str, repo_url: &str) {
    state
        .project_registry()
        .await
        .register(Project {
            name: name.to_string(),
            repo_url: repo_url.to_string(),
            default_branch: "main".to_string(),
            stack_hint: None,
            tags: vec![],
            description: None,
            gh_user: None,
            gh_account: None,
            github: None,
            commit_name: None,
            commit_email: None,
        })
        .await
        .expect("register project");
}

/// Build a scripted OpenAI-shaped response carrying `text`.
fn scripted_reply(text: &str) -> ChatResponse {
    ChatResponse {
        id: "scripted".into(),
        model: "test/model".into(),
        choices: vec![ChatChoice {
            message: AssistantMessage {
                content: Some(text.into()),
                tool_calls: Vec::new(),
            },
            finish_reason: Some("stop".into()),
        }],
        usage: UsageBlock::default(),
    }
}

/// Install a scripted adapter into the served state's manager inference seam —
/// same helper `tests/manager_inference.rs` uses, so the LLM-authored path is
/// deterministic and hermetic (no live provider key, DOC-36 §4).
fn install_scripted(state: &Arc<DaemonState>, reply: &str) {
    let adapter: Arc<dyn InferenceAdapter> = Arc::new(
        ScriptedAdapter::new("scripted", capabilities(ProviderId::OpenRouter))
            .with_response(scripted_reply(reply)),
    );
    state
        .manager_state()
        .inference()
        .set_adapter(adapter, "test/model");
}

/// `DaemonClient::manager_status` against the real, shipped
/// `GET /api/v1/manager/status` route — proves the CLI's status path end to
/// end, not merely the wire-shape unit tests in `client/http_client/manager.rs`.
#[tokio::test]
async fn manager_status_client_reads_live_route() {
    let state = fresh_state().await;
    register_project(&state, "widget", "https://github.com/acme/widget").await;
    let base = serve_real(Arc::clone(&state)).await;
    let client = DaemonClient::new(base);

    let status = client.manager_status().await.expect("manager_status");
    assert_eq!(status.project_count, 1);
    assert_eq!(status.projects[0].project_name, "widget");
}

/// Happy path: a configured (scripted) provider produces a `generated_by:
/// "llm"` narrative — `DaemonClient::manager_digest` returns `Some(outcome)`
/// with `fallback == false`.
#[tokio::test]
async fn manager_digest_client_reads_llm_narrative_against_real_daemon() {
    let state = fresh_state().await;
    register_project(&state, "alpha", "https://github.com/acme/alpha").await;
    install_scripted(
        &state,
        "Alpha has one session provisioning; nothing blocked.",
    );
    let base = serve_real(Arc::clone(&state)).await;
    let client = DaemonClient::new(base);

    let outcome = client
        .manager_digest("portfolio")
        .await
        .expect("call")
        .expect("Some(outcome) on 200");
    assert_eq!(
        outcome.narrative,
        "Alpha has one session provisioning; nothing blocked."
    );
    assert!(!outcome.fallback);
}

/// The review-flagged case: no inference provider configured, so the daemon
/// answers `503` with a FULL `DigestResponse` (`generated_by:
/// "deterministic_fallback"`, narrative + status snapshot) — the client must
/// parse that body into `Some(outcome)` (fallback marked), NOT discard it as
/// a bare HTTP error.
#[tokio::test]
async fn manager_digest_client_reads_deterministic_fallback_against_real_daemon() {
    let state = fresh_state().await;
    register_project(&state, "alpha", "https://github.com/acme/alpha").await;
    // Force the no-provider state independent of ambient credentials/env,
    // exactly as `tests/manager_inference.rs::manager_digest_degrades_without_provider` does.
    state.manager_state().inference().set_unconfigured();
    let base = serve_real(Arc::clone(&state)).await;
    let client = DaemonClient::new(base);

    let outcome = client
        .manager_digest("portfolio")
        .await
        .expect("503 degrade must not be a hard Err — the daemon sends a real body")
        .expect("Some(outcome) on the 503 degrade body");
    assert!(outcome.fallback, "generated_by must mark this as fallback");
    assert!(
        outcome.narrative.contains("deterministic fallback"),
        "narrative: {}",
        outcome.narrative
    );
}

/// The review-flagged case: `scope=project:<name>` naming an UNREGISTERED
/// project is the daemon's own legitimate `404` (`digest.rs`: `"project
/// '{name}' is not registered"`), not a "this daemon predates the route"
/// signal — must surface as `Err` carrying that exact message, never
/// `Ok(None)`.
#[tokio::test]
async fn manager_digest_client_surfaces_unknown_project_404_against_real_daemon() {
    let state = fresh_state().await;
    register_project(&state, "alpha", "https://github.com/acme/alpha").await;
    let base = serve_real(Arc::clone(&state)).await;
    let client = DaemonClient::new(base);

    let err = client
        .manager_digest("project:ghost")
        .await
        .expect_err("an unregistered project must be Err, not Ok(None)");
    assert!(
        err.to_string()
            .contains("project 'ghost' is not registered"),
        "error should carry the daemon's own 404 message: {err}"
    );
}

/// Happy path for chat: a configured (scripted) provider's reply comes back
/// as `Some(outcome)` with the reply text.
#[tokio::test]
async fn manager_chat_client_reads_llm_reply_against_real_daemon() {
    let state = fresh_state().await;
    install_scripted(&state, "Everything looks healthy right now.");
    let base = serve_real(Arc::clone(&state)).await;
    let client = DaemonClient::new(base);

    let outcome = client
        .manager_chat("cli:alice", "what needs my attention?")
        .await
        .expect("call")
        .expect("Some(outcome) on 200");
    assert_eq!(outcome.reply, "Everything looks healthy right now.");
    assert_eq!(outcome.conversation_key, "cli:alice");
}

/// The review-flagged case for chat: no provider configured, so the daemon
/// answers `503` with `{ error: "inference_unavailable", message: "..." }`
/// (`chat.rs`'s `chat_error`) — there is no assistant reply to synthesize,
/// but the client must still surface the daemon's actionable `message` text
/// as the outcome's reply rather than discarding the body as a bare error.
#[tokio::test]
async fn manager_chat_client_surfaces_degrade_message_against_real_daemon() {
    let state = fresh_state().await;
    state.manager_state().inference().set_unconfigured();
    let base = serve_real(Arc::clone(&state)).await;
    let client = DaemonClient::new(base);

    let outcome = client
        .manager_chat("cli:bob", "status?")
        .await
        .expect("503 degrade must not be a hard Err — the daemon sends an actionable body")
        .expect("Some(outcome) on the 503 degrade body");
    assert!(
        outcome.reply.contains("tm config keys"),
        "reply should carry the daemon's actionable message: {}",
        outcome.reply
    );
    // The error body never echoes a conversation_key — falls back to the
    // requested one.
    assert_eq!(outcome.conversation_key, "cli:bob");
}

/// A hand-built stub standing in for a daemon that PREDATES WI-3/WI-4/WI-8 — the
/// one scenario the current `origin/main` router (which now ships
/// digest/chat/route-task) cannot produce. `/manager/version` reports all three
/// routes `available: false` and none is mounted at all, so a request to any of
/// them 404s with an empty body — the genuine "upgrade your daemon" case
/// [`DaemonClient::manager_endpoint_available`] feature-detects via the
/// version probe.
fn older_daemon_stub_router() -> Router {
    async fn version_without_digest_chat_or_route_task() -> impl IntoResponse {
        Json(serde_json::json!({
            "manager_api_version": "0.1.0",
            "crate_version": "0.0.0",
            "phase": 1,
            "endpoints": [
                { "method": "GET", "path": "/api/v1/manager/version", "available": true },
                { "method": "GET", "path": "/api/v1/manager/status", "available": true },
                { "method": "GET", "path": "/api/v1/manager/digest", "available": false },
                { "method": "POST", "path": "/api/v1/manager/chat", "available": false },
                { "method": "POST", "path": "/api/v1/manager/route-task", "available": false },
            ],
            "palace": { "id": "tm-manager-portfolio", "available": false, "reason": null },
        }))
    }
    Router::new().route(
        "/api/v1/manager/version",
        get(version_without_digest_chat_or_route_task),
    )
}

async fn serve_mock(router: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(axum::serve(listener, router).into_future());
    format!("http://{addr}")
}

/// Against a daemon whose `/manager/version` reports digest/chat unavailable
/// (and does not mount them at all, so they 404), both client methods must
/// degrade to `Ok(None)` — the genuine "upgrade your daemon" case.
#[tokio::test]
async fn manager_digest_and_chat_degrade_cleanly_on_404_against_older_daemon() {
    let base = serve_mock(older_daemon_stub_router()).await;
    let client = DaemonClient::new(base);

    let digest = client
        .manager_digest("portfolio")
        .await
        .expect("manager_digest call should not error on a genuinely unmounted route");
    assert!(
        digest.is_none(),
        "expected None (404 degrade) against a daemon reporting digest unavailable"
    );

    let chat = client
        .manager_chat("cli:test", "hello")
        .await
        .expect("manager_chat call should not error on a genuinely unmounted route");
    assert!(
        chat.is_none(),
        "expected None (404 degrade) against a daemon reporting chat unavailable"
    );
}

/// `DaemonClient::manager_route_task` against the real, shipped
/// `POST /api/v1/manager/route-task` route (WI-8, #2585) — proves the CLI's
/// route-task path end to end (the HTTP method, the request body shape, and the
/// timeout wiring), not merely the wire-shape unit tests in
/// `client/http_client/manager.rs` (coordinator review finding 2).
#[tokio::test]
async fn manager_route_task_client_reads_live_route_against_real_daemon() {
    let state = fresh_state().await;
    register_project(&state, "alpha", "https://github.com/acme/alpha").await;
    let base = serve_real(Arc::clone(&state)).await;
    let client = DaemonClient::new(base);

    let outcome = client
        .manager_route_task("alpha")
        .await
        .expect("call")
        .expect("Some(outcome) on 200");
    assert_eq!(outcome.project.as_deref(), Some("alpha"));
    assert_eq!(outcome.resolved_by, "resolver");
}

/// The non-404 error path (coordinator review finding 2: previously
/// unexercised): an empty `text` is the daemon's own `400`, which
/// `manager_route_task` must surface as `Err`, never `Ok(None)` — mirroring how
/// `manager_digest_client_surfaces_unknown_project_404_against_real_daemon`
/// pins digest's non-mount-detection error path.
#[tokio::test]
async fn manager_route_task_client_surfaces_400_for_empty_text_against_real_daemon() {
    let state = fresh_state().await;
    let base = serve_real(Arc::clone(&state)).await;
    let client = DaemonClient::new(base);

    let err = client
        .manager_route_task("   ")
        .await
        .expect_err("an empty task must be Err, not Ok(None)");
    assert!(
        err.to_string().contains("text must not be empty"),
        "error should carry the daemon's own 400 message: {err}"
    );
}

/// Against a daemon whose `/manager/version` reports `route-task` unavailable
/// (and does not mount it at all, so it 404s), the client must degrade to
/// `Ok(None)` — the genuine "upgrade your daemon" case, mirroring the
/// digest/chat coverage above.
#[tokio::test]
async fn manager_route_task_client_degrades_cleanly_on_404_against_older_daemon() {
    let base = serve_mock(older_daemon_stub_router()).await;
    let client = DaemonClient::new(base);

    let route = client
        .manager_route_task("fix the flaky auth test")
        .await
        .expect("manager_route_task call should not error on a genuinely unmounted route");
    assert!(
        route.is_none(),
        "expected None (404 degrade) against a daemon reporting route-task unavailable"
    );
}
