//! Router-wide same-origin write guard tests for trusty-review (#3332).
//!
//! Why: trusty-review was missed by #3317's origin-guard rollout across
//! search/memory/analyze/mpm. Issue #3332 adopts
//! `trusty_common::server::with_guarded_middleware` so the destructive
//! `POST /review` route is no longer reachable cross-origin (CSRF).
//!
//! What: boots the real router via `build_router` (loopback-only allowlist,
//! same as production's default bind) and drives it with
//! `tower::ServiceExt::oneshot`, asserting the guard's outcomes: cross-origin
//! write → 403, loopback/missing-Origin write → allowed, cross-origin GET →
//! allowed. It also pins that the retired `POST /pr/github/webhook` path
//! answers 404 (#5181).
//!
//! Test: this is the test module.

use std::sync::Arc;

use async_trait::async_trait;
use axum::{
    Router,
    body::Body,
    http::{Method, Request, StatusCode},
};
use tower::ServiceExt as _;

use crate::{
    integrations::search_client::{
        EmbedderState, HealthResponse as SearchHealth, IndexInfo, SearchClient, SearchClientError,
        SearchResult,
    },
    llm::{LlmError, LlmProvider, LlmRequest, LlmResponse},
    service::handlers::AppState,
};

// ── Fake LLM ─────────────────────────────────────────────────────────────────

struct FakeLlm;

#[async_trait]
impl LlmProvider for FakeLlm {
    fn name(&self) -> &str {
        "fake"
    }

    async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
        Ok(LlmResponse {
            text: r#"LGTM.
```json
{"verdict":"APPROVE","summary":"ok","findings":[]}
```"#
                .to_string(),
            model: req.model.clone(),
            input_tokens: 10,
            output_tokens: 5,
            latency_ms: 1,
            cost_usd: 0.0,
            finish_reason: None,
        })
    }
}

// ── Fake search ───────────────────────────────────────────────────────────────

struct FakeSearch;

#[async_trait]
impl SearchClient for FakeSearch {
    async fn health(&self) -> Result<SearchHealth, SearchClientError> {
        Ok(SearchHealth {
            status: "ok".to_string(),
            embedder: EmbedderState::Bool(true),
            warmboot_summary: None,
        })
    }

    async fn list_indexes(&self) -> Result<Vec<IndexInfo>, SearchClientError> {
        Ok(vec![])
    }

    async fn search(
        &self,
        _: &str,
        _: &str,
        _: Option<u32>,
    ) -> Result<Vec<SearchResult>, SearchClientError> {
        Ok(vec![])
    }
}

// ── Test state builders ────────────────────────────────────────────────────────

fn test_state() -> AppState {
    AppState::new(
        crate::config::ReviewConfig::load(None),
        Arc::new(FakeLlm),
        Arc::new(FakeSearch),
        None,
    )
}

fn router() -> Router {
    crate::service::build_router(test_state())
}

fn review_requested_payload(action: &str) -> Vec<u8> {
    serde_json::json!({
        "action": action,
        "pull_request": {
            "number": 42,
            "user": { "login": "alice" },
            "head": { "sha": "abc123" }
        },
        "repository": {
            "name": "backend",
            "owner": { "login": "acme" }
        },
        "requested_reviewer": { "login": "trusty-review[bot]" }
    })
    .to_string()
    .into_bytes()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// Why (#3332): `POST /review` is a destructive write (triggers an LLM-backed
/// review pipeline); a malicious cross-origin page must be rejected with `403`
/// by the router-wide same-origin guard before the handler runs (CSRF defence).
/// Test: this test.
#[tokio::test]
async fn write_route_rejects_cross_origin() {
    let resp = router()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/review")
                .header("origin", "http://evil.example.com")
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "cross-origin POST /review must be rejected by the write guard"
    );
}

/// Why (#3332): trusty-review's own loopback-served UI/tooling and
/// server-side callers (the console proxy, `curl`, which send NO `Origin`)
/// must keep driving writes — loopback and missing-Origin writes must NOT
/// 403.
/// Test: this test.
#[tokio::test]
async fn write_route_allows_loopback_and_missing_origin() {
    let app = router();
    // Loopback origin → allowed.
    let loopback = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/review")
                .header("origin", "http://127.0.0.1:7891")
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_ne!(
        loopback.status(),
        StatusCode::FORBIDDEN,
        "loopback-origin POST /review must pass the guard"
    );
    // No Origin (server-side caller) → allowed.
    let missing = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/review")
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_ne!(
        missing.status(),
        StatusCode::FORBIDDEN,
        "missing-Origin POST /review (server-side caller) must pass the guard"
    );
}

/// Why (#3332): the guard is method-gated — a cross-origin GET read leaks no
/// destructive capability and must NOT be blocked.
/// Test: this test.
#[tokio::test]
async fn read_route_allows_cross_origin() {
    let resp = router()
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/health")
                .header("origin", "http://evil.example.com")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_ne!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "cross-origin GET /health must not be blocked by the write guard"
    );
}

/// The retired webhook route answers 404, not 200-and-discard (#5181).
///
/// Why: the guard fails open on a missing `Origin` — exactly the shape of a
/// real GitHub delivery — so a route left registered behind a gutted handler
/// would sail through the middleware and answer 2xx while dropping the
/// payload. GitHub never retries an acknowledged delivery, so that is silent
/// loss with every health signal green. Asserting `handle_github_webhook` was
/// deleted proves nothing about the router; this drives the real router and
/// asserts the real response.
///
/// What: sends the request GitHub actually sends (POST, `X-GitHub-Event`, a
/// signature header, no `Origin`) at the retired path and requires 404. Before
/// #5181 the same request returned 202 — never 404 — so this test fails
/// against the pre-fix state.
/// Test: this is the test.
#[tokio::test]
async fn retired_webhook_route_is_not_registered() {
    let response = router()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/pr/github/webhook")
                .header("x-github-event", "pull_request")
                .header("x-hub-signature-256", "sha256=deadbeef")
                .header("content-type", "application/json")
                // Deliberately NO `origin` header — matches real GitHub
                // webhook deliveries exactly, and the guard fails open on it.
                .body(Body::from(review_requested_payload("review_requested")))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::NOT_FOUND,
        "the retired webhook route must be unreachable, not silently accepting"
    );
}
