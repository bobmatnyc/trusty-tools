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
    ToolError, call_review_health, mcp_run_mode, require_str, tool_descriptors, wrap_result,
    wrap_tool_error,
};
use crate::integrations::github::{AuthStrategy, RunMode};
use crate::models::{ReviewResult, ReviewStatus, Verdict};

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

/// A search stub whose `health()` reports `status: "degraded"` purely from a
/// benign, intentional watcher-disable on a network mount
/// (`warm_boot_degraded: false`) — the issue #3693 scenario.
struct DegradedButServingSearchTool;

#[async_trait]
impl SearchClient for DegradedButServingSearchTool {
    async fn health(&self) -> Result<SearchHealth, SearchClientError> {
        Ok(SearchHealth {
            status: "degraded".into(),
            embedder: EmbedderState::Bool(true),
            warmboot_summary: Some(crate::integrations::health::WarmBootSummary {
                warm_boot_degraded: false,
            }),
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

fn make_tool_state_degraded_but_serving_search(llm: Arc<dyn LlmProvider>) -> AppState {
    AppState::new(
        ReviewConfig::load(None),
        llm,
        Arc::new(DegradedButServingSearchTool),
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

/// review_health MCP tool stays `status: "ok"` when trusty-search reports
/// `status: "degraded"` for a benign reason (issue #3693: a network-mount
/// watcher-disable, `warm_boot_degraded: false`) — this tool's own doc
/// comment names it as the primary consumer MPM uses to gate `review_pr`, so
/// this is the call site that most directly reproduces the #3693 symptom for
/// external callers if it were left on `is_healthy()`.
///
/// Why: `call_review_health` shares `probe_deps` with the HTTP `/health`
/// handler, so this exercises the same `is_serving()` gate end-to-end
/// through the MCP path specifically.
/// What: builds AppState with OkLlmTool + DegradedButServingSearchTool; calls
/// call_review_health; asserts status is "ok" and trusty_search.reachable is
/// true.
/// Test: this test itself.
#[tokio::test]
async fn review_health_degraded_but_serving_search_stays_ok() {
    let state = make_tool_state_degraded_but_serving_search(Arc::new(OkLlmTool));
    let result = call_review_health(&state).await;
    let text = result["content"][0]["text"].as_str().expect("text field");
    let health: Value = serde_json::from_str(text).expect("valid JSON");
    assert_eq!(
        health["status"], "ok",
        "degraded-but-serving trusty-search (benign watcher-disable) must not degrade status (#3693)"
    );
    assert_eq!(
        health["deps"]["trusty_search"]["reachable"], true,
        "trusty_search.reachable must be true when only degraded due to benign watcher-disable (#3693)"
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

// ── infra-unavailable Skip must be LOUD (search-unreachable semantics fix) ─────

/// A `ReviewResult` with `status = Skipped` + `infra_unavailable = true` (the
/// required-context gate's ONLY producer of `Skipped`) must come back
/// `isError: true` with the `mcp_status: "infrastructure_unavailable"`
/// sentinel — a caller must be forced to handle it explicitly rather than
/// reading a verdict field, and it must be unambiguously different from BOTH a
/// real verdict AND a policy skip.
///
/// Why: this is the core bug the fix closes — an MCP `tools/call` response
/// with `isError: false` for a Skip is indistinguishable from a real gate
/// verdict.
/// What: constructs a `Skipped` + `infra_unavailable` result directly (as
/// `run_review`'s gate branch does) and asserts both signals on the envelope.
/// Test: this test itself.
#[test]
fn wrap_result_infra_unavailable_sets_error_and_sentinel() {
    let mut result = ReviewResult::new("acme", "backend", 9, "Add Z", "https://example/pr/9");
    result.status = ReviewStatus::Skipped;
    result.infra_unavailable = true;
    result.verdict = Verdict::Unknown;
    result.error = Some("trusty-search unreachable at http://x — start it".to_string());

    let envelope = wrap_result(&result, None);

    assert_eq!(
        envelope["isError"], true,
        "an infra-unavailable Skip must set isError:true so no caller reads it \
         as a successful tool result"
    );
    assert_eq!(
        envelope["mcp_status"], "infrastructure_unavailable",
        "envelope must carry the loud machine-readable sentinel"
    );
}

/// A policy-style outcome (no `infra_unavailable` flag set) must stay
/// `isError: false` even when `status == Skipped` — only a genuine infra
/// outage gets the loud treatment, distinguishing it from a hypothetical
/// future non-infra skip producer.
///
/// Why: guards the "only infra-unavailable gets the loud treatment" half of
/// the fix — a policy skip must not regress into a false-alarm error envelope.
/// What: constructs a `Skipped` result WITHOUT `infra_unavailable`; asserts the
/// envelope stays clean.
/// Test: this test itself.
#[test]
fn wrap_result_policy_skip_without_infra_flag_stays_is_error_false() {
    let mut result = ReviewResult::new("acme", "backend", 10, "Add W", "https://example/pr/10");
    result.status = ReviewStatus::Skipped;
    // infra_unavailable intentionally left false (default) — simulates a
    // hypothetical future policy-driven skip.

    let envelope = wrap_result(&result, None);

    assert_eq!(
        envelope["isError"], false,
        "a policy skip (no infra outage) must not be flagged as an MCP error"
    );
    assert!(
        envelope.get("mcp_status").is_none(),
        "a policy skip must not carry the infra-unavailable sentinel"
    );
}

/// A `Degraded` review (real verdict, non-authoritative banner) must stay
/// `isError: false` — it is a genuine (if loudly-labelled) result, not an
/// infra failure the caller must special-case as an error.
///
/// Why: distinguishes the "opted-in / interactive-surface-defaulted degrade"
/// path from the "infra Skip" path — both are non-authoritative, but only the
/// Skip is a non-result.
/// What: constructs a `Degraded` result; asserts the envelope stays clean.
/// Test: this test itself.
#[test]
fn wrap_result_degraded_stays_is_error_false() {
    let mut result = ReviewResult::new("local", "diff", 0, "local diff", "");
    result.status = ReviewStatus::Degraded;
    result.verdict = Verdict::Approve;

    let envelope = wrap_result(&result, None);

    assert_eq!(
        envelope["isError"], false,
        "a degraded-but-real verdict must not be flagged as an MCP error"
    );
    assert!(
        envelope.get("mcp_status").is_none(),
        "a degraded (non-infra) result must not carry the infra sentinel"
    );
}

// ── mcp_run_mode: local-first auth selection (#1993) ──────────────────────────

/// Both App credentials present → `Serve` (hosted-bot deployment).
///
/// Why: deployments that actually configure a GitHub App must keep using App
/// auth; the local-first default must not regress the hosted path.
/// What: sets both `github_app_id` and `github_app_private_key`; asserts `Serve`.
/// Test: this test itself.
#[test]
fn mcp_run_mode_serve_with_app_creds() {
    let mut config = ReviewConfig::load(None);
    config.github_app_id = Some("123456".to_string());
    config.github_app_private_key = Some("-----BEGIN RSA PRIVATE KEY-----".to_string());
    assert_eq!(mcp_run_mode(&config), RunMode::Serve);
}

/// Neither App credential present → `Cli` (local developer invocation).
///
/// Why: the common MCP case has no GitHub App configured; it must fall back to
/// the developer's `gh` login instead of erroring for missing App creds (#1993).
/// What: clears both App fields; asserts `Cli`.
/// Test: this test itself.
#[test]
fn mcp_run_mode_cli_without_app_creds() {
    let mut config = ReviewConfig::load(None);
    config.github_app_id = None;
    config.github_app_private_key = None;
    assert_eq!(mcp_run_mode(&config), RunMode::Cli);
}

/// Partial or empty App credentials → `Cli`.
///
/// Why: an App is only usable when BOTH id and key are present and non-empty;
/// a half-configured or blank-string App must not select the App strategy.
/// What: exercises id-only, key-only, and both-empty cases; each yields `Cli`.
/// Test: this test itself.
#[test]
fn mcp_run_mode_cli_with_empty_app_creds() {
    // id only.
    let mut config = ReviewConfig::load(None);
    config.github_app_id = Some("123456".to_string());
    config.github_app_private_key = None;
    assert_eq!(mcp_run_mode(&config), RunMode::Cli);

    // key only.
    let mut config = ReviewConfig::load(None);
    config.github_app_id = None;
    config.github_app_private_key = Some("-----BEGIN RSA PRIVATE KEY-----".to_string());
    assert_eq!(mcp_run_mode(&config), RunMode::Cli);

    // both present but whitespace-only.
    let mut config = ReviewConfig::load(None);
    config.github_app_id = Some("   ".to_string());
    config.github_app_private_key = Some("  ".to_string());
    assert_eq!(mcp_run_mode(&config), RunMode::Cli);
}

/// With no App creds, MCP auth resolution selects the CLI strategy.
///
/// Why: this is the end-to-end contract of #1993 — the MCP path must resolve to
/// `AuthStrategy::Cli` (developer `gh`/PAT) when no App is configured, rather
/// than `App` (which would demand `GITHUB_APP_ID`/`GITHUB_APP_PRIVATE_KEY`).
/// What: feeds `mcp_run_mode` into `AuthStrategy::select` (no override) and
/// asserts `Cli`.  Clears `TRUSTY_REVIEW_AUTH_MODE` first so the env override
/// cannot flip the default; serialised to avoid racing other env-reading tests.
/// Test: this test itself.
#[test]
#[serial_test::serial]
fn mcp_run_mode_resolves_cli_strategy() {
    // SAFETY: test-only env mutation, serialised via #[serial].
    unsafe { std::env::remove_var("TRUSTY_REVIEW_AUTH_MODE") };
    let mut config = ReviewConfig::load(None);
    config.github_app_id = None;
    config.github_app_private_key = None;
    assert_eq!(
        AuthStrategy::select(mcp_run_mode(&config), None),
        AuthStrategy::Cli
    );
}
