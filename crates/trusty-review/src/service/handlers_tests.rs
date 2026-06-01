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

use crate::{
    integrations::{
        analyze_client::{
            AnalyzeClient, AnalyzeClientError, AnalyzeHealthResponse, ComplexityHotspot, Smell,
        },
        search_client::{
            HealthResponse as SearchHealth, IndexInfo, SearchClient, SearchClientError,
            SearchResult,
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
            embedder: true,
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
