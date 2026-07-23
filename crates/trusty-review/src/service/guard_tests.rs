//! Router-wide same-origin write guard tests for trusty-review (#3332).
//!
//! Why: trusty-review was missed by #3317's origin-guard rollout across
//! search/memory/analyze/mpm. Issue #3332 adopts
//! `trusty_common::server::with_guarded_middleware` so the destructive
//! `POST /review` route is no longer reachable cross-origin (CSRF). The
//! GitHub webhook route is a special case: GitHub signs requests with
//! `X-Hub-Signature-256` and never sends an `Origin` header, so the guard's
//! fail-open-on-missing-Origin behaviour must keep webhook delivery working
//! unchanged — this file has a dedicated regression test for exactly that.
//!
//! What: boots the real router via `build_router` (loopback-only allowlist,
//! same as production's default bind) and drives it with
//! `tower::ServiceExt::oneshot`, asserting the guard's three outcomes:
//! cross-origin write → 403, loopback/missing-Origin write → allowed,
//! cross-origin webhook (no Origin, as GitHub actually sends it) → allowed.
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

fn test_state_with_secret(secret: &str) -> AppState {
    let mut config = crate::config::ReviewConfig::load(None);
    config.github_webhook_secret = secret.to_string();
    AppState::new(config, Arc::new(FakeLlm), Arc::new(FakeSearch), None)
}

fn router() -> Router {
    crate::service::build_router(test_state())
}

fn make_sig(secret: &str, body: &[u8]) -> String {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(body);
    format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
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

/// Why (#3332, CAUTION in the loopback-only doctrine tranche): GitHub's
/// webhook deliveries are HMAC-signed (`X-Hub-Signature-256`) but carry NO
/// `Origin` header at all — a browser-only header GitHub's server-to-server
/// delivery never sends. If the router-wide guard did not fail open on a
/// missing `Origin`, adopting it here would silently break webhook delivery.
/// This test pins that the webhook path still accepts POSTs without Origin,
/// using a correctly-signed payload so HMAC verification (a separate concern)
/// is not what is under test.
/// Test: this test.
#[tokio::test]
async fn webhook_route_accepts_post_without_origin() {
    let secret = "test-secret"; // pragma: allowlist secret
    let state = test_state_with_secret(secret);
    let router = crate::service::build_router(state);

    let payload = review_requested_payload("review_requested");
    let sig = make_sig(secret, &payload);

    let response = router
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/pr/github/webhook")
                .header("x-github-event", "pull_request")
                .header("x-hub-signature-256", sig)
                .header("content-type", "application/json")
                // Deliberately NO `origin` header — matches real GitHub
                // webhook deliveries exactly.
                .body(Body::from(payload))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_ne!(
        response.status(),
        StatusCode::FORBIDDEN,
        "webhook POST with no Origin header must not be blocked by the write guard"
    );
    // The webhook handler itself accepts and dispatches (202) — confirms the
    // request reached the handler rather than being swallowed by the guard.
    assert_eq!(response.status(), StatusCode::ACCEPTED);
}
