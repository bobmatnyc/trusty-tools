//! Unit tests for `mcp::tools`.
//!
//! Why: split from `tools.rs` to keep that file under the 500-line cap while
//! preserving full test coverage for all tool handlers and the inference-probe
//! integration (#719/#722).
//! What: exercises `tool_descriptors`, `require_str`, `wrap_tool_error`, and
//! `call_review_health` (happy path, auth-error, and dep-reachability paths).
//! The `call_tool` dispatch tests for `review_diff` / `review_pr` live in the
//! sibling `tools_dispatch_tests.rs` module (#949) to keep each file under the
//! 500-line cap.
//! Test: this is the test module; each `#[test]` / `#[tokio::test]` is a
//! self-contained unit test.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::{
    config::ReviewConfig,
    integrations::search_client::{
        EmbedderState, HealthResponse as SearchHealth, IndexInfo, SearchClient, SearchClientError,
        SearchResult,
    },
    llm::{LlmError, LlmProvider, LlmRequest, LlmResponse},
    service::AppState,
};

use super::{
    ToolError, call_review_health, require_str, tool_descriptors, wrap_result, wrap_tool_error,
};
use crate::models::ReviewResult;

// ── Stub providers ────────────────────────────────────────────────────────────

struct OkLlmTool;

#[async_trait]
impl LlmProvider for OkLlmTool {
    fn name(&self) -> &str {
        "ok-tool-stub"
    }

    async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
        Ok(LlmResponse {
            text: "ok".into(),
            model: req.model.clone(),
            input_tokens: 1,
            output_tokens: 1,
            latency_ms: 0,
            cost_usd: 0.0,
            finish_reason: None,
        })
    }
}

struct AuthErrorLlmTool;

#[async_trait]
impl LlmProvider for AuthErrorLlmTool {
    fn name(&self) -> &str {
        "auth-error-tool-stub"
    }

    async fn complete(&self, _req: LlmRequest) -> Result<LlmResponse, LlmError> {
        Err(LlmError::AccessDenied("bad key".into()))
    }
}

struct FakeSearchTool;

#[async_trait]
impl SearchClient for FakeSearchTool {
    async fn health(&self) -> Result<SearchHealth, SearchClientError> {
        Ok(SearchHealth {
            status: "ok".into(),
            embedder: EmbedderState::Bool(true),
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

/// A search stub that returns an error on health checks (simulates unreachable dep).
struct FailSearchTool;

#[async_trait]
impl SearchClient for FailSearchTool {
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

fn make_tool_state(llm: Arc<dyn LlmProvider>) -> AppState {
    AppState::new(
        ReviewConfig::load(None),
        llm,
        Arc::new(FakeSearchTool),
        None,
    )
}

fn make_tool_state_fail_search(llm: Arc<dyn LlmProvider>) -> AppState {
    AppState::new(
        ReviewConfig::load(None),
        llm,
        Arc::new(FailSearchTool),
        None,
    )
}

// ── Tool-descriptor tests ─────────────────────────────────────────────────────

#[test]
fn tools_list_has_four_tools() {
    let tools = tool_descriptors();
    let arr = tools.as_array().expect("must be array");
    // Now four tools: review_pr, review_diff, review_health, console_metrics.
    assert_eq!(arr.len(), 4, "expected 4 tools, got {}", arr.len());
    let names: Vec<&str> = arr
        .iter()
        .filter_map(|t| t.get("name").and_then(Value::as_str))
        .collect();
    assert!(names.contains(&"review_pr"), "missing review_pr");
    assert!(names.contains(&"review_diff"), "missing review_diff");
    assert!(names.contains(&"review_health"), "missing review_health");
    assert!(
        names.contains(&"console_metrics"),
        "missing console_metrics"
    );
}

#[test]
fn each_tool_has_input_schema() {
    let tools = tool_descriptors();
    for tool in tools.as_array().unwrap() {
        let name = tool.get("name").and_then(Value::as_str).unwrap_or("?");
        assert!(
            tool.get("inputSchema").is_some(),
            "tool '{name}' is missing inputSchema"
        );
    }
}

// ── Helper tests ──────────────────────────────────────────────────────────────

#[test]
fn require_str_returns_error_on_missing() {
    let args = json!({});
    let result = require_str(&args, "owner");
    assert!(
        matches!(result, Err(ToolError::InvalidParams(_))),
        "expected InvalidParams"
    );
}

#[test]
fn require_str_extracts_value() {
    let args = json!({ "owner": "alice" });
    assert_eq!(require_str(&args, "owner").unwrap(), "alice");
}

#[test]
fn wrap_tool_error_sets_is_error_true() {
    let v = wrap_tool_error("boom");
    assert_eq!(v["isError"], json!(true));
    let text = v["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("boom"));
}

// ── review_health inference-probe tests (#719) ────────────────────────────────

/// review_health MCP tool returns `inference: "ok"` and `status: "ok"` when
/// the provider succeeds.
///
/// Why: validates the happy-path response shape in the MCP path (#719).
/// What: builds AppState with OkLlmTool, calls call_review_health, asserts
/// both fields in the JSON payload.
/// Test: this test itself.
#[tokio::test]
async fn review_health_inference_ok() {
    let state = make_tool_state(Arc::new(OkLlmTool));
    let result = call_review_health(&state).await;
    let text = result["content"][0]["text"].as_str().expect("text field");
    let health: Value = serde_json::from_str(text).expect("valid JSON");
    assert_eq!(health["inference"], "ok");
    assert_eq!(health["status"], "ok");
    assert!(
        health["reviewer_model"].is_string(),
        "reviewer_model must be present"
    );
    assert!(health["dry_run"].is_boolean(), "dry_run must be present");
}

/// review_health MCP tool sets `status: "degraded"` and `inference: "auth_error"`
/// when the provider returns an authentication failure.
///
/// Why: validates the degraded-path response shape in the MCP path (#719).
/// What: builds AppState with AuthErrorLlmTool, calls call_review_health, asserts
/// inference and status fields.
/// Test: this test itself.
#[tokio::test]
async fn review_health_inference_auth_error_degraded() {
    let state = make_tool_state(Arc::new(AuthErrorLlmTool));
    let result = call_review_health(&state).await;
    let text = result["content"][0]["text"].as_str().expect("text field");
    let health: Value = serde_json::from_str(text).expect("valid JSON");
    assert_eq!(health["inference"], "auth_error");
    assert_eq!(health["status"], "degraded");
}

// ── review_health dep-reachability tests (#722) ───────────────────────────────

/// review_health MCP tool sets `status: "degraded"` when the required search dep
/// is unreachable, even if inference itself is healthy.
///
/// Why: validates the #722 fix in the MCP path — callers that gate on `status`
/// must get `"degraded"` when trusty_search is down.
/// What: builds AppState with OkLlmTool (inference ok) + FailSearchTool (health
/// returns Err); calls call_review_health; asserts status is "degraded" and
/// deps.trusty_search.reachable is false.
/// Test: this test itself.
#[tokio::test]
async fn review_health_required_dep_down_degraded() {
    let state = make_tool_state_fail_search(Arc::new(OkLlmTool));
    let result = call_review_health(&state).await;
    let text = result["content"][0]["text"].as_str().expect("text field");
    let health: Value = serde_json::from_str(text).expect("valid JSON");
    assert_eq!(
        health["status"], "degraded",
        "required dep (trusty_search) down → status must be degraded"
    );
    assert_eq!(
        health["inference"], "ok",
        "inference must be ok (OkLlmTool always succeeds)"
    );
    assert_eq!(
        health["deps"]["trusty_search"]["reachable"], false,
        "trusty_search.reachable must be false when search is down"
    );
}

/// review_health MCP tool stays `status: "ok"` when inference is ok and all
/// required deps are reachable — even when analyze (non-required) is absent.
///
/// Why: validates the happy-path of #722 — non-required deps absent/unreachable
/// must not degrade status.
/// What: builds AppState with OkLlmTool + FakeSearchTool (health ok) + no analyze;
/// calls call_review_health; asserts status is "ok" and trusty_search.reachable is true.
/// Test: this test itself.
#[tokio::test]
async fn review_health_optional_dep_down_ok() {
    // No analyze dep configured (analyze = None → analyze_reachable = false).
    let state = make_tool_state(Arc::new(OkLlmTool));
    let result = call_review_health(&state).await;
    let text = result["content"][0]["text"].as_str().expect("text field");
    let health: Value = serde_json::from_str(text).expect("valid JSON");
    assert_eq!(
        health["status"], "ok",
        "optional dep absent → status must remain ok"
    );
    assert_eq!(
        health["deps"]["trusty_search"]["reachable"], true,
        "trusty_search.reachable must be true (FakeSearchTool succeeds)"
    );
    assert_eq!(
        health["deps"]["trusty_analyze"]["reachable"], false,
        "trusty_analyze.reachable must be false (no analyze configured)"
    );
}

// ── reviewer_model override fallback surfacing (#1357 item 2) ───────────────────

/// #1357: when an override provider build failed, `wrap_result` surfaces the
/// fallback reason in BOTH the envelope and the serialised payload text.
///
/// Why: an MCP caller silently getting the wrong backend is the hazard #1357
/// targets; the fallback must be DETECTABLE both programmatically (envelope) and
/// by the LLM reading `content[0].text`.
/// What: wraps a `ReviewResult` with `Some(reason)`; asserts the envelope-level
/// `reviewer_model_fallback` field is present AND that the same key appears in the
/// parsed payload JSON.
/// Test: this test itself.
#[test]
fn wrap_result_surfaces_reviewer_model_fallback() {
    let result = ReviewResult::new("acme", "backend", 7, "Add X", "https://example/pr/7");
    let reason = "failed to build provider for reviewer_model override 'openrouter/x' \
                  (key empty); fell back to the startup 'bedrock' provider";
    let envelope = wrap_result(&result, Some(reason));

    // Envelope-level metadata field for programmatic callers.
    assert_eq!(
        envelope["reviewer_model_fallback"], reason,
        "envelope must carry reviewer_model_fallback for detection"
    );
    assert_eq!(
        envelope["isError"], false,
        "fallback is non-breaking, not an error"
    );

    // The same marker is spliced into the payload the LLM reads.
    let text = envelope["content"][0]["text"].as_str().expect("text field");
    let payload: Value = serde_json::from_str(text).expect("valid JSON payload");
    assert_eq!(
        payload["reviewer_model_fallback"], reason,
        "payload JSON must also carry the fallback so the LLM sees it"
    );
}

/// #1357: the happy path (no fallback) leaves the envelope clean — no extra field.
///
/// Why: only the failure path should advertise a fallback; a clean run must not
/// emit a spurious marker that callers would misread as a degraded backend.
/// What: wraps a `ReviewResult` with `None`; asserts the `reviewer_model_fallback`
/// key is absent from both envelope and payload.
/// Test: this test itself.
#[test]
fn wrap_result_no_fallback_omits_field() {
    let result = ReviewResult::new("acme", "backend", 8, "Add Y", "https://example/pr/8");
    let envelope = wrap_result(&result, None);
    assert!(
        envelope.get("reviewer_model_fallback").is_none(),
        "no fallback → envelope must NOT carry the marker"
    );
    let text = envelope["content"][0]["text"].as_str().expect("text field");
    let payload: Value = serde_json::from_str(text).expect("valid JSON payload");
    assert!(
        payload.get("reviewer_model_fallback").is_none(),
        "no fallback → payload must NOT carry the marker"
    );
}
