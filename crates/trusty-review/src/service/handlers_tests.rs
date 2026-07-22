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
            EmbedderState, HealthResponse as SearchHealth, HttpSearchClient, IndexInfo,
            SearchClient, SearchClientError, SearchResult,
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
            finish_reason: None,
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

/// `ReviewRequest` deserializes from a body WITHOUT the optional context fields
/// (#1618 back-compat).
///
/// Why: existing callers send only owner/repo/pr (or local_diff_text); adding the
/// new `#[serde(default)]` Option fields must not break them — the missing keys
/// default to None.
#[test]
fn review_request_deserializes_without_optional_context() {
    let body = r#"{"owner":"acme","repo":"backend","pr":42}"#;
    let req: ReviewRequest = serde_json::from_str(body).expect("legacy body must deserialize");
    assert_eq!(req.owner.as_deref(), Some("acme"));
    assert_eq!(req.pr, Some(42));
    assert!(
        req.pr_description.is_none(),
        "pr_description defaults to None"
    );
    assert!(
        req.pr_discussion.is_none(),
        "pr_discussion defaults to None"
    );
    assert!(
        req.referenced_code.is_none(),
        "referenced_code defaults to None"
    );
}

/// `ReviewRequest` deserializes WITH the optional context fields (#1618).
///
/// Why: the new fields must round-trip from the wire so callers can supply PR
/// description, discussion, and referenced code under exactly these names.
#[test]
fn review_request_deserializes_with_optional_context() {
    let body = r#"{
        "local_diff_text": "+fn x() {}\n",
        "pr_description": "Adds a retry guard.",
        "pr_discussion": "Author: checked the source; no values exceed the cap.",
        "referenced_code": "pub const CAP: u32 = 100;"
    }"#;
    let req: ReviewRequest =
        serde_json::from_str(body).expect("body with context must deserialize");
    assert_eq!(req.pr_description.as_deref(), Some("Adds a retry guard."));
    assert_eq!(
        req.pr_discussion.as_deref(),
        Some("Author: checked the source; no values exceed the cap.")
    );
    assert_eq!(
        req.referenced_code.as_deref(),
        Some("pub const CAP: u32 = 100;")
    );
}

#[test]
fn resolve_diff_source_requires_owner_repo_pr() {
    use super::resolve_diff_source;
    let req = ReviewRequest {
        owner: None,
        repo: None,
        pr: None,
        local_diff_text: None,
        ..Default::default()
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
        ..Default::default()
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
        ..Default::default()
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
        ..Default::default()
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
    // #3658: fast/healthy dep path must report the new tri-state field as
    // "ok" without disturbing the pre-existing `reachable` boolean.
    assert_eq!(
        body["deps"]["trusty_search"]["state"], "ok",
        "healthy fast dep must report state:ok (#3658)"
    );
    assert_eq!(
        body["deps"]["trusty_search"]["reachable"], true,
        "healthy fast dep must still report reachable:true (back-compat)"
    );
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

// ── Bounded dep-probe tests (#3658) ───────────────────────────────────────────
//
// Why: trusty-review's /health hung with no internal timeout when
// trusty-search was slow (not down) — the dep probe was unbounded. These
// tests prove: (1) a stalled dep is bounded and reported distinctly as
// `state:"timeout"`; (2) the fast/healthy path is unchanged; (3) a hard-down
// dep still reports `reachable:false` (covered above via
// `health_status_degraded_required_dep_down` / `health_required_dep_down_sets_degraded`
// in `handlers_status_tests.rs`, extended with a `state:"unreachable"` check).

/// `/health` returns within the bound, reporting `state:"timeout"`, when
/// trusty-search accepts a connection but never responds (#3658).
///
/// Why: this is the exact prod repro from #3658 — a memory-pressured
/// trusty-search that is slow, not down. Post-#722 the endpoint eventually
/// reported `reachable:false`, but the probe itself was unbounded, so the
/// whole handler could hang indefinitely. This test uses a real TCP listener
/// that accepts but never writes a response — reproducing the actual hang at
/// the transport layer, not just a mocked async delay — and asserts the
/// handler still returns promptly.
/// What: binds a listener, spawns a task that accepts the connection and then
/// awaits forever (`std::future::pending`) without responding; points an
/// `HttpSearchClient` at it; overrides `TRUSTY_REVIEW_DEP_PROBE_TIMEOUT_SECS=1`
/// so the test is fast and deterministic; calls `handle_health` and asserts
/// wall-clock elapsed stays well under the old 30 s client-level timeout,
/// and that the response reports `deps.trusty_search.state == "timeout"` with
/// `reachable == false`.
/// Test: this test itself (condition: bounded elapsed time + response shape,
/// no arbitrary sleep-as-assertion).
#[tokio::test]
#[serial_test::serial]
async fn health_stalled_dep_returns_timeout_state_within_bound() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local_addr");
    let base_url = format!("http://{addr}");

    // Accept the connection but never read or write anything — the listener
    // simply holds it open, simulating a memory-pressured trusty-search that
    // is slow (not down): it answers the TCP handshake but never completes
    // the HTTP response.
    let _mock_handle = tokio::spawn(async move {
        let _sock = listener.accept().await.expect("accept");
        std::future::pending::<()>().await;
    });

    // SAFETY: #[serial_test::serial] ensures no other thread mutates env vars
    // concurrently in this process during this test. A short override keeps
    // the test fast without depending on the 2 s production default.
    unsafe { std::env::set_var("TRUSTY_REVIEW_DEP_PROBE_TIMEOUT_SECS", "1") };

    let search = Arc::new(HttpSearchClient::new(base_url).expect("client construction"));
    let state = AppState::new(
        crate::config::ReviewConfig::load(None),
        Arc::new(FakeLlm),
        search,
        None,
    );

    let start = std::time::Instant::now();
    let response = handle_health(State(state)).await;
    let elapsed = start.elapsed();

    unsafe { std::env::remove_var("TRUSTY_REVIEW_DEP_PROBE_TIMEOUT_SECS") };

    assert!(
        elapsed < std::time::Duration::from_secs(5),
        "/health must return within the bound even when trusty-search stalls \
         (took {elapsed:?}); the whole point of #3658 is that it must NOT wait \
         out the client's 30 s HTTP timeout"
    );

    let resp: axum::response::Response = response.into_response();
    assert_eq!(resp.status(), StatusCode::OK);
    let body_bytes = to_bytes(resp.into_body(), 65536).await.expect("body bytes");
    let body: serde_json::Value = serde_json::from_slice(&body_bytes).expect("valid JSON");

    assert_eq!(
        body["deps"]["trusty_search"]["state"], "timeout",
        "stalled dep must report a distinct state:timeout, not collapsed into reachable:false-only (#3658)"
    );
    assert_eq!(
        body["deps"]["trusty_search"]["reachable"], false,
        "reachable must still be false for back-compat when the dep times out"
    );
}

// ── dep_probe_timeout() unit tests (#3658) ────────────────────────────────────

/// Returns 2 s when the env var is absent.
///
/// Why: verifies the documented default (short and decoupled from the
/// search/analyze clients' own 30 s / 5 s HTTP-transport timeouts).
/// What: calls `dep_probe_timeout()` with the env var unset.
/// Test: this test (serial to prevent env-var races with sibling tests).
#[test]
#[serial_test::serial]
fn dep_probe_timeout_default() {
    use super::dep_probe_timeout;
    // SAFETY: serial_test::serial ensures no other thread mutates env vars
    // concurrently in this process during this test.
    unsafe { std::env::remove_var("TRUSTY_REVIEW_DEP_PROBE_TIMEOUT_SECS") };
    assert_eq!(
        dep_probe_timeout(),
        std::time::Duration::from_secs(2),
        "default dep-probe timeout must be 2 s (#3658)"
    );
}

/// Returns the caller-supplied value when the env var is a valid non-zero u64.
///
/// Why: operators must be able to tune the dep-probe timeout without
/// recompiling (e.g. a slower network path than the 2 s default assumes).
/// What: sets `TRUSTY_REVIEW_DEP_PROBE_TIMEOUT_SECS=5` and checks the result.
/// Test: this test.
#[test]
#[serial_test::serial]
fn dep_probe_timeout_env_override() {
    use super::dep_probe_timeout;
    // SAFETY: serial_test::serial ensures no other thread mutates env vars
    // concurrently in this process during this test.
    unsafe { std::env::set_var("TRUSTY_REVIEW_DEP_PROBE_TIMEOUT_SECS", "5") };
    let t = dep_probe_timeout();
    unsafe { std::env::remove_var("TRUSTY_REVIEW_DEP_PROBE_TIMEOUT_SECS") };
    assert_eq!(
        t,
        std::time::Duration::from_secs(5),
        "env-var override must be honoured"
    );
}

/// Falls back to 2 s when the env var contains a non-numeric value.
///
/// Why: a mis-typed env var must not panic or hang every probe; fallback to
/// the safe default is the correct behaviour.
/// What: sets an invalid value and asserts the default is used.
/// Test: this test.
#[test]
#[serial_test::serial]
fn dep_probe_timeout_env_invalid_falls_back() {
    use super::dep_probe_timeout;
    // SAFETY: serial_test::serial ensures no other thread mutates env vars
    // concurrently in this process during this test.
    unsafe { std::env::set_var("TRUSTY_REVIEW_DEP_PROBE_TIMEOUT_SECS", "not-a-number") };
    let t = dep_probe_timeout();
    unsafe { std::env::remove_var("TRUSTY_REVIEW_DEP_PROBE_TIMEOUT_SECS") };
    assert_eq!(
        t,
        std::time::Duration::from_secs(2),
        "invalid env var must fall back to 2 s default"
    );
}

/// Falls back to 2 s when the env var is zero (prevents a zero timeout from
/// making every probe instantly report `timeout`).
///
/// Why: a zero timeout would make `/health` always report every dep as
/// timed-out, which is as unhelpful as the original unbounded hang.
/// What: sets `TRUSTY_REVIEW_DEP_PROBE_TIMEOUT_SECS=0` and asserts the default.
/// Test: this test.
#[test]
#[serial_test::serial]
fn dep_probe_timeout_env_zero_falls_back() {
    use super::dep_probe_timeout;
    // SAFETY: serial_test::serial ensures no other thread mutates env vars
    // concurrently in this process during this test.
    unsafe { std::env::set_var("TRUSTY_REVIEW_DEP_PROBE_TIMEOUT_SECS", "0") };
    let t = dep_probe_timeout();
    unsafe { std::env::remove_var("TRUSTY_REVIEW_DEP_PROBE_TIMEOUT_SECS") };
    assert_eq!(
        t,
        std::time::Duration::from_secs(2),
        "zero env var must fall back to 2 s default"
    );
}
