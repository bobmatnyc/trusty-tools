//! Unit tests for `service::handlers`.
//!
//! Why: split from `handlers.rs` to keep that file under the 500-line cap
//! while preserving full test coverage for all route handlers.
//! What: exercises `resolve_diff_source`, `handle_health`, `handle_status`,
//! and `handle_review` via direct handler invocation.
//! Test: this is the test module; each `#[test]` / `#[tokio::test]` function
//! is a self-contained unit test.

use std::sync::Arc;

use async_trait::async_trait;
use axum::{Json, extract::State, http::StatusCode, response::IntoResponse as _};

use axum::body::to_bytes;

use crate::{
    integrations::{
        analyze_client::{
            AnalyzeClient, AnalyzeClientError, AnalyzeHealthResponse, ComplexityHotspot, Smell,
        },
        search_client::{
            EmbedderState, HealthResponse as SearchHealth, IndexInfo, SearchClient,
            SearchClientError, SearchResult,
        },
    },
    llm::{LlmError, LlmProvider, LlmRequest, LlmResponse},
    pipeline::DiffSource,
    service::handlers::{AppState, ReviewRequest, handle_health, handle_review, handle_status},
};

// ── Fake LLM ─────────────────────────────────────────────────────────────────

pub(super) struct FakeLlm;

#[async_trait]
impl LlmProvider for FakeLlm {
    fn name(&self) -> &str {
        "fake"
    }

    async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
        Ok(LlmResponse {
            text: r#"LGTM.

```json
{"verdict":"APPROVE","summary":"Looks good","findings":[]}
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

pub(super) struct FakeSearch;

#[async_trait]
impl SearchClient for FakeSearch {
    async fn health(&self) -> Result<SearchHealth, SearchClientError> {
        Ok(SearchHealth {
            status: "ok".to_string(),
            embedder: EmbedderState::Bool(true),
        })
    }

    async fn list_indexes(&self) -> Result<Vec<IndexInfo>, SearchClientError> {
        Ok(vec![])
    }

    async fn search(
        &self,
        _index_id: &str,
        _query: &str,
        _top_k: Option<u32>,
    ) -> Result<Vec<SearchResult>, SearchClientError> {
        Ok(vec![])
    }
}

pub(super) struct FailSearch;

#[async_trait]
impl SearchClient for FailSearch {
    async fn health(&self) -> Result<SearchHealth, SearchClientError> {
        Err(SearchClientError::Unavailable("down".to_string()))
    }

    async fn list_indexes(&self) -> Result<Vec<IndexInfo>, SearchClientError> {
        Err(SearchClientError::Unavailable("down".to_string()))
    }

    async fn search(
        &self,
        _: &str,
        _: &str,
        _: Option<u32>,
    ) -> Result<Vec<SearchResult>, SearchClientError> {
        Err(SearchClientError::Unavailable("down".to_string()))
    }
}

// ── Fake LLM that returns auth error ─────────────────────────────────────────

pub(super) struct AuthErrorLlm;

#[async_trait]
impl LlmProvider for AuthErrorLlm {
    fn name(&self) -> &str {
        "auth-error-fake"
    }

    async fn complete(&self, _req: LlmRequest) -> Result<LlmResponse, LlmError> {
        Err(LlmError::AccessDenied("test: invalid credentials".into()))
    }
}

// ── Fake analyze ──────────────────────────────────────────────────────────────

pub(super) struct FakeAnalyze;

#[async_trait]
impl AnalyzeClient for FakeAnalyze {
    async fn health(&self) -> Result<AnalyzeHealthResponse, AnalyzeClientError> {
        Err(AnalyzeClientError::Unavailable("not running".to_string()))
    }

    async fn has_analysis(&self, _: &str) -> bool {
        false
    }

    async fn complexity_hotspots(
        &self,
        _: &str,
        _: Option<u32>,
    ) -> Result<Vec<ComplexityHotspot>, AnalyzeClientError> {
        Ok(vec![])
    }

    async fn smells(&self, _: &str) -> Result<Vec<Smell>, AnalyzeClientError> {
        Ok(vec![])
    }
}

// ── Test state builder ────────────────────────────────────────────────────────

pub(super) fn test_state() -> AppState {
    AppState::new(
        crate::config::ReviewConfig::load(None),
        Arc::new(FakeLlm),
        Arc::new(FakeSearch),
        None,
    )
}

fn test_state_with_failing_search() -> AppState {
    AppState::new(
        crate::config::ReviewConfig::load(None),
        Arc::new(FakeLlm),
        Arc::new(FailSearch),
        Some(Arc::new(FakeAnalyze)),
    )
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[test]
fn resolve_diff_source_requires_owner_repo_pr() {
    use super::resolve_diff_source;
    let req = ReviewRequest {
        owner: None,
        repo: None,
        pr: None,
        local_diff_text: None,
    };
    let result = resolve_diff_source(&req);
    assert!(
        result.is_err(),
        "missing owner/repo/pr must produce an error"
    );
}

#[test]
fn resolve_diff_source_github_all_present() {
    use super::resolve_diff_source;
    let req = ReviewRequest {
        owner: Some("acme".to_string()),
        repo: Some("backend".to_string()),
        pr: Some(42),
        local_diff_text: None,
    };
    let source = resolve_diff_source(&req).expect("should succeed");
    match source {
        DiffSource::Github {
            owner, repo, pr, ..
        } => {
            assert_eq!(owner, "acme");
            assert_eq!(repo, "backend");
            assert_eq!(pr, 42);
        }
        _ => panic!("expected DiffSource::Github"),
    }
}

#[test]
fn resolve_diff_source_local_diff_text() {
    use super::resolve_diff_source;
    let req = ReviewRequest {
        owner: None,
        repo: None,
        pr: None,
        local_diff_text: Some("+fn hello() {}\n".to_string()),
    };
    let source = resolve_diff_source(&req).expect("local_diff_text should succeed");
    assert!(
        matches!(source, DiffSource::LocalFile { .. }),
        "expected DiffSource::LocalFile"
    );
}

#[tokio::test]
async fn health_handler_returns_ok() {
    let state = test_state();
    let response = handle_health(State(state)).await;
    let resp: axum::response::Response = response.into_response();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn health_handler_with_failing_search_still_200() {
    // Even when search is unreachable, /health returns 200 (degraded state
    // is in the body, not via 5xx — spec REV-706).
    let state = test_state_with_failing_search();
    let response = handle_health(State(state)).await;
    let resp: axum::response::Response = response.into_response();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn status_handler_returns_zero_in_flight() {
    let state = test_state();
    let response = handle_status(State(state)).await;
    let resp: axum::response::Response = response.into_response();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn review_handler_bad_request_missing_fields() {
    let state = test_state();
    let req = ReviewRequest {
        owner: None,
        repo: None,
        pr: None,
        local_diff_text: None,
    };
    let response = handle_review(State(state), Json(req)).await;
    let resp: axum::response::Response = response.into_response();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

// ── Inference-probe handler tests (#719) ──────────────────────────────────────

/// /health includes `inference: "ok"` and `status: "ok"` when the LLM succeeds.
///
/// Why: validates the happy-path response shape introduced in #719.
/// What: calls handle_health with FakeLlm (always succeeds); deserialises the
/// response body and asserts `inference == "ok"` and `status == "ok"`.
/// Test: this test itself.
#[tokio::test]
async fn health_inference_ok_when_llm_succeeds() {
    let state = test_state();
    let response = handle_health(State(state)).await;
    let resp: axum::response::Response = response.into_response();
    assert_eq!(resp.status(), StatusCode::OK);

    let body_bytes = to_bytes(resp.into_body(), 65536).await.expect("body bytes");
    let body: serde_json::Value = serde_json::from_slice(&body_bytes).expect("valid JSON");

    assert_eq!(
        body["inference"], "ok",
        "inference must be 'ok' for FakeLlm"
    );
    assert_eq!(
        body["status"], "ok",
        "status must be 'ok' when inference is ok"
    );
    assert!(
        body["reviewer_model"].is_string(),
        "reviewer_model must be present"
    );
    assert!(body["dry_run"].is_boolean(), "dry_run must be present");
    assert!(body["deps"].is_object(), "deps must be present");
}

/// /health sets `status: "degraded"` and `inference: "auth_error"` on LLM auth failure.
///
/// Why: validates the degraded-path response shape introduced in #719 — callers
/// that gate on `status` alone need it to flip to `"degraded"` without also
/// parsing `inference`.
/// What: uses AuthErrorLlm (returns AccessDenied); asserts `inference == "auth_error"`
/// and `status == "degraded"`.  HTTP 200 is still returned (degraded is in the body).
/// Test: this test itself.
#[tokio::test]
async fn health_inference_auth_error_sets_degraded() {
    let state = AppState::new(
        crate::config::ReviewConfig::load(None),
        Arc::new(AuthErrorLlm),
        Arc::new(FakeSearch),
        None,
    );
    let response = handle_health(State(state)).await;
    let resp: axum::response::Response = response.into_response();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "HTTP status must be 200 even when degraded (spec REV-706)"
    );

    let body_bytes = to_bytes(resp.into_body(), 65536).await.expect("body bytes");
    let body: serde_json::Value = serde_json::from_slice(&body_bytes).expect("valid JSON");

    assert_eq!(
        body["inference"], "auth_error",
        "AccessDenied LLM error must map to auth_error"
    );
    assert_eq!(
        body["status"], "degraded",
        "status must be degraded when inference != ok"
    );
}
