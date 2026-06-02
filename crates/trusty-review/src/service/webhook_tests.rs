//! Unit tests for `service::webhook`.
//!
//! Why: split from `webhook.rs` to keep that file under the 500-line cap while
//! preserving full coverage of HMAC validation, event filtering, and the
//! trigger-classified dispatch path.
//! What: exercises `handle_github_webhook` end-to-end via the router with
//! `tower::ServiceExt::oneshot`, plus payload deserialisation.
//! Test: this is the test module; each function is a self-contained unit test.

use super::*;
use crate::{
    integrations::search_client::{
        HealthResponse as SearchHealth, IndexInfo, SearchClient, SearchClientError, SearchResult,
    },
    llm::{LlmError, LlmProvider, LlmRequest, LlmResponse},
    service::handlers::AppState,
};
use async_trait::async_trait;
use axum::{
    body::Body,
    http::{Method, Request},
};
use tower::ServiceExt as _;

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
            embedder: true,
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

// ── HMAC helper ───────────────────────────────────────────────────────────────

fn make_sig(secret: &str, body: &[u8]) -> String {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(body);
    format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
}

fn test_state_with_secret(secret: &str) -> AppState {
    let mut config = crate::config::ReviewConfig::load(None);
    config.github_webhook_secret = secret.to_string();
    AppState::new(config, Arc::new(FakeLlm), Arc::new(FakeSearch), None)
}

// ── Payload helper ────────────────────────────────────────────────────────────

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

#[test]
fn webhook_payload_deserialises() {
    let payload = review_requested_payload("review_requested");
    let event: PullRequestEvent = serde_json::from_slice(&payload).unwrap();
    assert_eq!(event.action, "review_requested");
    assert_eq!(event.pull_request.number, 42);
    assert_eq!(event.repository.name, "backend");
    assert_eq!(event.repository.owner.login, "acme");
    assert_eq!(
        event.pull_request.head.as_ref().map(|h| h.sha.as_str()),
        Some("abc123"),
        "head SHA must parse for dedup keying"
    );
    assert_eq!(
        event.requested_reviewer.as_ref().map(|r| r.login.as_str()),
        Some("trusty-review[bot]")
    );
}

#[tokio::test]
async fn webhook_rejects_bad_hmac() {
    let secret = "test-secret"; // pragma: allowlist secret
    let state = test_state_with_secret(secret);
    let router = crate::service::build_router(state);
    let payload = review_requested_payload("review_requested");

    let request = Request::builder()
        .method(Method::POST)
        .uri("/pr/github/webhook")
        .header("x-github-event", "pull_request")
        .header("x-hub-signature-256", "sha256=badhex0000")
        .header("content-type", "application/json")
        .body(Body::from(payload))
        .unwrap();

    let response = router.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn webhook_rejects_missing_secret_config() {
    // When GITHUB_WEBHOOK_SECRET is empty in config, all webhooks → 401.
    let state = test_state_with_secret("");
    let router = crate::service::build_router(state);
    let payload = review_requested_payload("review_requested");

    let request = Request::builder()
        .method(Method::POST)
        .uri("/pr/github/webhook")
        .header("x-github-event", "pull_request")
        .header("x-hub-signature-256", "sha256=anything")
        .header("content-type", "application/json")
        .body(Body::from(payload))
        .unwrap();

    let response = router.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn webhook_ignores_non_pull_request_event() {
    let secret = "test-secret"; // pragma: allowlist secret
    let state = test_state_with_secret(secret);
    let router = crate::service::build_router(state);

    let payload = br#"{"zen":"design for failure"}"#;
    let sig = make_sig(secret, payload);

    let request = Request::builder()
        .method(Method::POST)
        .uri("/pr/github/webhook")
        .header("x-github-event", "ping") // not pull_request
        .header("x-hub-signature-256", sig)
        .header("content-type", "application/json")
        .body(Body::from(payload.as_slice()))
        .unwrap();

    let response = router.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn webhook_ignores_non_review_requested_action() {
    let secret = "test-secret"; // pragma: allowlist secret
    let state = test_state_with_secret(secret);
    let router = crate::service::build_router(state);

    let payload = review_requested_payload("opened"); // not review_requested
    let sig = make_sig(secret, &payload);

    let request = Request::builder()
        .method(Method::POST)
        .uri("/pr/github/webhook")
        .header("x-github-event", "pull_request")
        .header("x-hub-signature-256", sig)
        .header("content-type", "application/json")
        .body(Body::from(payload))
        .unwrap();

    let response = router.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn webhook_accepts_review_requested_returns_202() {
    let secret = "test-secret"; // pragma: allowlist secret
    let state = test_state_with_secret(secret);
    let router = crate::service::build_router(state);

    let payload = review_requested_payload("review_requested");
    let sig = make_sig(secret, &payload);

    let request = Request::builder()
        .method(Method::POST)
        .uri("/pr/github/webhook")
        .header("x-github-event", "pull_request")
        .header("x-hub-signature-256", sig)
        .header("content-type", "application/json")
        .body(Body::from(payload))
        .unwrap();

    let response = router.oneshot(request).await.unwrap();
    // 202 Accepted — pipeline spawned in background.
    assert_eq!(response.status(), StatusCode::ACCEPTED);
}
