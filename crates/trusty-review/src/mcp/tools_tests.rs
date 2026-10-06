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
    // #6686: the per-index probe the gate decides on. This stand-in reports a
    // fully-ready index so the fake exercises the branch under test, not this one.
    async fn index_status(
        &self,
        index_id: &str,
    ) -> Result<crate::integrations::search_client::IndexStatusResponse, SearchClientError> {
        Ok(crate::integrations::search_client::IndexStatusResponse::ready(index_id))
    }

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
    // #6686: the per-index probe the gate decides on. This stand-in reports a
    // fully-ready index so the fake exercises the branch under test, not this one.
    async fn index_status(
        &self,
        index_id: &str,
    ) -> Result<crate::integrations::search_client::IndexStatusResponse, SearchClientError> {
        Ok(crate::integrations::search_client::IndexStatusResponse::ready(index_id))
    }

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
    // #6686: the per-index probe the gate decides on. This stand-in reports a
    // fully-ready index so the fake exercises the branch under test, not this one.
    async fn index_status(
        &self,
        index_id: &str,
    ) -> Result<crate::integrations::search_client::IndexStatusResponse, SearchClientError> {
        Ok(crate::integrations::search_client::IndexStatusResponse::ready(index_id))
    }

    async fn health(&self) -> Result<SearchHealth, SearchClientError> {
        Ok(SearchHealth {
            status: "degraded".into(),
            embedder: EmbedderState::Bool(true),
            warmboot_summary: Some(crate::integrations::health::WarmBootSummary {
                warm_boot_degraded: false,
                ..Default::default()
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

// ── reviewer_model override: no silent substitution (#6114) ────────────────────

/// #6114: no envelope can announce that the review ran on a substitute model,
/// because no review runs on one.
///
/// Why: #1357 item 2 made the silent-wrong-backend case DETECTABLE by adding a
/// `reviewer_model_fallback` field. #6114 removes the case instead —
/// `deps_from_state` errors rather than reviewing a diff with a model the caller
/// did not ask for — so the field must be gone from both the envelope and the
/// payload the LLM reads. A field that can never be set is a promise the surface
/// cannot keep.
/// What: wraps a plain `ReviewResult` and asserts the key is absent from both.
/// Test: this test itself; the refusal it depends on is
/// `deps_from_state_build_failure_is_an_error` (tools_dispatch_tests.rs).
#[test]
fn wrap_result_never_carries_a_reviewer_model_fallback() {
    let result = ReviewResult::new("acme", "backend", 8, "Add Y", "https://example/pr/8");
    let envelope = wrap_result(&result);
    assert!(
        envelope.get("reviewer_model_fallback").is_none(),
        "the envelope must not carry a substitution marker (#6114)"
    );
    let text = envelope["content"][0]["text"].as_str().expect("text field");
    let payload: Value = serde_json::from_str(text).expect("valid JSON payload");
    assert!(
        payload.get("reviewer_model_fallback").is_none(),
        "the payload must not carry a substitution marker (#6114)"
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

    let envelope = wrap_result(&result);

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

    let envelope = wrap_result(&result);

    assert_eq!(
        envelope["isError"], false,
        "a policy skip (no infra outage) must not be flagged as an MCP error"
    );
    assert!(
        envelope.get("mcp_status").is_none(),
        "a policy skip must not carry the infra-unavailable sentinel"
    );
}

#[test]
fn wrap_result_names_a_suppressed_reject_without_is_error() {
    // #9188 K: a review whose every finding was withheld says so on the
    // envelope with typed counts; `isError` stays false (Architect ruling
    // 2026-10-05: isError is for real failures only). #9310: the blocking
    // review is REQUEST_CHANGES with `suppressed_reject`, no longer UNKNOWN
    // with `no_verified_findings`.
    let mut result = ReviewResult::new("local", "diff", 0, "local diff", "");
    result.verdict = Verdict::RequestChanges;
    result.verdict_status = Some(crate::models::VerdictStatus::SuppressedReject);
    result
        .withheld_findings
        .push(crate::models::WithheldFinding {
            finding: crate::models::Finding::new(
                "src/a.rs",
                "overflow",
                "`a + b` overflows",
                "",
                0.9,
                crate::models::Effort::Medium,
            ),
            reason: "refuted by the verifier".to_string(),
            missing_fragment: None,
        });

    let envelope = wrap_result(&result);

    assert_eq!(envelope["isError"], false, "{envelope}");
    assert_eq!(envelope["withheld"]["count"], 1, "{envelope}");
    assert_eq!(
        envelope["withheld"]["by_reason"]["refuted"], 1,
        "{envelope}"
    );
    assert_eq!(
        envelope["verdict_status"], "suppressed_reject",
        "{envelope}"
    );
}

/// AQ-7t (Bob 2026-10-05): an APPROVE whose every finding was withheld keeps
/// APPROVE, and the envelope names it `all_withheld` (#9310, formerly
/// `no_verified_findings`).
#[test]
fn wrap_result_names_an_all_withheld_approve() {
    let mut result = ReviewResult::new("local", "diff", 0, "local diff", "");
    result.verdict = Verdict::Approve;
    result.verdict_status = Some(crate::models::VerdictStatus::AllWithheld);
    result.grade = Some("A+".to_string());
    result
        .withheld_findings
        .push(crate::models::WithheldFinding {
            finding: crate::models::Finding::new(
                "src/a.rs",
                "style",
                "`a + b` is slow",
                "",
                0.3,
                crate::models::Effort::Low,
            ),
            reason: "unverifiable".to_string(),
            missing_fragment: None,
        });

    let envelope = wrap_result(&result);

    assert_eq!(envelope["isError"], false, "{envelope}");
    assert_eq!(envelope["verdict_status"], "all_withheld", "{envelope}");
    assert_eq!(envelope["withheld"]["by_reason"]["unverifiable"], 1);
}

/// #9310: every finalized result names its status on the envelope, a clean
/// one included; nothing withheld still means no `withheld` key.
#[test]
fn wrap_result_names_the_status_of_a_clean_review() {
    let mut result = ReviewResult::new("local", "diff", 0, "local diff", "");
    result.verdict = Verdict::Approve;
    result.verdict_status = Some(crate::models::VerdictStatus::Parsed);
    let envelope = wrap_result(&result);
    assert_eq!(envelope["verdict_status"], "parsed", "{envelope}");
    assert!(envelope.get("withheld").is_none(), "{envelope}");
}

/// #9188 compatibility (Bob, 2026-10-05 02:48Z): a review that withheld
/// nothing serializes byte-identically to the pre-#9188 envelope.
#[test]
fn a_review_with_nothing_withheld_serializes_as_before() {
    let mut result = ReviewResult::new("acme", "api", 7, "Add X", "https://example/pr/7");
    result.verdict = Verdict::Approve;
    result.grade = Some("A".to_string());
    result.review_body = "LGTM".to_string();
    result.timestamp = "2026-10-05T00:00:00Z".to_string();
    result.review_version = "tr-test".to_string();
    let mut finding = crate::models::Finding::new(
        "src/a.rs",
        "overflow",
        "`a + b` overflows",
        "use checked_add",
        0.5,
        crate::models::Effort::Low,
    );
    finding.line = Some(3);
    result.findings = vec![finding];
    result.findings_count = 1;

    assert_eq!(wrap_result(&result).to_string(), PRE_9188_ENVELOPE);
}

/// The envelope `a_review_with_nothing_withheld_serializes_as_before` built
/// at 09721c7bbb, before #9188.
const PRE_9188_ENVELOPE: &str = r#"{"content":[{"text":"{\n  \"cost_estimate_usd\": 0.0,\n  \"dry_run\": true,\n  \"findings\": [\n    {\n      \"category\": \"correctness\",\n      \"code_provable\": false,\n      \"confidence\": 0.5,\n      \"consequence\": \"\",\n      \"description\": \"`a + b` overflows\",\n      \"effort\": \"low\",\n      \"file\": \"src/a.rs\",\n      \"issue_eligible\": false,\n      \"kind\": \"overflow\",\n      \"line\": 3,\n      \"suggestion\": \"use checked_add\"\n    }\n  ],\n  \"findings_count\": 1,\n  \"grade\": \"A\",\n  \"head_sha\": \"\",\n  \"input_tokens\": 0,\n  \"latency_ms\": 0,\n  \"model\": \"\",\n  \"output_tokens\": 0,\n  \"owner\": \"acme\",\n  \"posted\": false,\n  \"pr_number\": 7,\n  \"pr_title\": \"Add X\",\n  \"pr_url\": \"https://example/pr/7\",\n  \"repo\": \"api\",\n  \"review_body\": \"LGTM\",\n  \"review_version\": \"tr-test\",\n  \"status\": \"completed\",\n  \"timestamp\": \"2026-10-05T00:00:00Z\",\n  \"unverified_count\": 0,\n  \"verdict\": \"APPROVE\",\n  \"withheld_unverified_count\": 0\n}","type":"text"}],"isError":false}"#;

/// A `Degraded` review (real verdict, non-authoritative banner) must stay
/// `isError: false` — it is a genuine (if loudly-labelled) result, not an
/// infra failure the caller must special-case as an error.
///
/// Why: distinguishes the "opted-in / interactive-surface-defaulted degrade"
/// path from the "infra Skip" path — both are non-authoritative, but only the
/// Skip is a non-result.
/// What: constructs a `Degraded` result; asserts `isError` stays false and the
/// envelope carries the degraded sentinel rather than the infra one.
/// Test: this test itself.
#[test]
fn wrap_result_degraded_stays_is_error_false() {
    let mut result = ReviewResult::new("local", "diff", 0, "local diff", "");
    result.status = ReviewStatus::Degraded;
    result.verdict = Verdict::Approve;

    let envelope = wrap_result(&result);

    assert_eq!(
        envelope["isError"], false,
        "a degraded-but-real verdict must not be flagged as an MCP error"
    );
    assert_eq!(
        envelope["mcp_status"], "degraded_context",
        "a degraded result must carry the degraded sentinel, never the infra one"
    );
}

/// REGRESSION (#4086): a degraded verdict must be distinguishable from a
/// complete one WITHOUT parsing `content[0].text`.
///
/// Why: before this fix, the only difference between an APPROVE produced with
/// full code context and an APPROVE produced with none was buried inside a
/// serialised JSON blob and a Markdown banner. Every programmatic consumer that
/// looked at the envelope — the stable, non-prose part of the response — saw
/// two byte-identical results. A caveat nobody can see is not a caveat.
/// What: builds a `Degraded` APPROVE with a reason, and asserts the envelope
/// alone carries both the sentinel and the human-readable reason.
/// Test: this test itself.
#[test]
fn wrap_result_degraded_sets_sentinel_and_reason() {
    let mut result = ReviewResult::new("acme", "backend", 11, "Add Q", "https://example/pr/11");
    result.status = ReviewStatus::Degraded;
    result.verdict = Verdict::Approve;
    result.error = Some(
        "degraded (non-authoritative): trusty-search at http://localhost:7878: \
         3 index(es) failed to open their corpus"
            .to_string(),
    );

    let envelope = wrap_result(&result);

    assert_eq!(
        envelope["mcp_status"], "degraded_context",
        "a degraded verdict must be machine-detectable from the envelope alone"
    );
    assert!(
        envelope["degraded_reason"]
            .as_str()
            .is_some_and(|r| r.contains("failed to open their corpus")),
        "the envelope must name what was missing; got: {:?}",
        envelope.get("degraded_reason")
    );

    // The control: an otherwise-identical COMPLETE review must be clean, so the
    // sentinel is a real signal and not noise attached to every response.
    let mut complete = ReviewResult::new("acme", "backend", 11, "Add Q", "https://example/pr/11");
    complete.verdict = Verdict::Approve;
    let complete_envelope = wrap_result(&complete);
    assert!(
        complete_envelope.get("mcp_status").is_none(),
        "a complete review must carry no status sentinel"
    );
    assert!(
        complete_envelope.get("degraded_reason").is_none(),
        "a complete review must carry no degraded reason"
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

/// REGRESSION (#4254): `review_health` reports the dry_run the MCP review path
/// executes with, never the raw config flag.
///
/// Why: the reported defect — `review_health` answered `"dry_run": false` while
/// `review_pr` on the same process returned `"dry_run": true, "posted": false`
/// and posted nothing. A caller gating on health believed reviews were reaching
/// GitHub.
/// What: builds a state whose config says LIVE (`dry_run = false`) — the exact
/// configuration that produced the false answer — and asserts health reports
/// `dry_run: true` with a reason naming the gate. Pre-fix this asserted
/// `false == true` and failed.
/// Test: this test itself.
#[tokio::test]
async fn review_health_reports_the_dry_run_the_review_path_executes() {
    let mut config = ReviewConfig::load(None);
    config.dry_run = false;
    let state = AppState::new(config, Arc::new(OkLlmTool), Arc::new(FakeSearchTool), None);

    let result = call_review_health(&state).await;
    let text = result["content"][0]["text"].as_str().expect("text field");
    let health: Value = serde_json::from_str(text).expect("valid JSON");

    assert_eq!(
        health["dry_run"], true,
        "the MCP review tools force dry-run, so health must say so: {health}"
    );
    assert!(
        health["dry_run_reason"]
            .as_str()
            .expect("a reason accompanies a forced dry-run")
            .contains("never posts"),
        "the reason names the gate: {health}"
    );
}

/// #9192: the envelope carries `context_sources` only when the outcome has
/// records, and never changes the `ReviewResult` text it wraps.
#[test]
fn wrap_outcome_adds_context_sources_only_when_present() {
    use crate::models::{ContextSourceRecord, SourceState};
    use crate::pipeline::ReviewOutcome;

    let result = ReviewResult::new("acme", "backend", 8, "Add Y", "https://example/pr/8");
    let mut outcome = ReviewOutcome {
        result: result.clone(),
        context_sources: Vec::new(),
    };
    let off = super::wrap_outcome(&outcome);
    assert_eq!(
        off,
        wrap_result(&result),
        "no records: the envelope is unchanged"
    );
    assert!(off.get("context_sources").is_none());

    outcome.context_sources = vec![ContextSourceRecord::new("pr_body", SourceState::Absent)];
    let on = super::wrap_outcome(&outcome);
    assert_eq!(
        on["context_sources"],
        json!([{"source": "pr_body", "state": "absent"}])
    );
    assert_eq!(on["content"], wrap_result(&result)["content"]);
}

/// #9192: `review_pr` lists the four optional context params, with the new
/// boolean typed as one.
#[test]
fn review_pr_schema_lists_the_optional_context_params() {
    let tools = tool_descriptors();
    let review_pr = tools
        .as_array()
        .and_then(|a| a.iter().find(|t| t["name"] == "review_pr"))
        .expect("review_pr descriptor");
    let props = &review_pr["inputSchema"]["properties"];
    assert_eq!(props["include_pr_body"]["type"], "boolean");
    for name in ["pr_description", "pr_discussion", "referenced_code"] {
        assert_eq!(props[name]["type"], "string", "{name}");
    }
    assert_eq!(
        review_pr["inputSchema"]["required"],
        json!(["owner", "repo", "pr"])
    );
}
