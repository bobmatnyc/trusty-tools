//! MCP `call_tool` dispatch tests for `review_diff` and `review_pr` (#949).
//!
//! Why: split from `tools_tests.rs` to keep that file under the 500-line cap
//! while adding full dispatch-path coverage for the two primary review tools.
//! What: exercises `call_tool("review_diff", ...)` with a stub LLM (fully
//! offline, no credentials) and `call_tool("review_pr", ...)` with no
//! GITHUB_TOKEN (exercises the token-resolution failure path).
//! Test: each `#[tokio::test]` is a self-contained unit test; no network.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::{
    config::ReviewConfig,
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
    service::AppState,
};

use super::{ToolError, call_tool};

// ── Stubs ─────────────────────────────────────────────────────────────────────

/// LLM stub that emits a minimal APPROVE JSON block so the pipeline runs
/// fully offline without a real model.
///
/// Why: `call_tool("review_diff", ...)` invokes the full pipeline; this stub
/// provides a parseable APPROVE verdict without any network call.
/// What: always returns the APPROVE JSON wrapped in markdown code fences.
/// Test: `call_tool_review_diff_returns_non_empty_verdict`.
struct ApproveLlm;

#[async_trait]
impl LlmProvider for ApproveLlm {
    fn name(&self) -> &str {
        "approve-dispatch-stub"
    }

    async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
        Ok(LlmResponse {
            text: r#"Looks good.

```json
{"verdict":"APPROVE","summary":"LGTM","findings":[]}
```"#
                .into(),
            model: req.model.clone(),
            input_tokens: 5,
            output_tokens: 5,
            latency_ms: 0,
            cost_usd: 0.0,
        })
    }
}

/// Search stub that reports itself healthy; satisfies the required-context gate
/// when `require_search = false`.
///
/// Why: the pipeline probes the search client; a healthy stub keeps tests fast.
/// What: health returns ok; search returns empty results.
/// Test: `call_tool_review_diff_returns_non_empty_verdict`.
struct FakeSearchDispatch;

#[async_trait]
impl SearchClient for FakeSearchDispatch {
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

/// Analyze client stub that reports healthy and ready.
///
/// Why: `AppState::new` takes `Option<Arc<dyn AnalyzeClient>>`; a ready stub
/// ensures the pipeline proceeds even when `require_analyze = false`.
/// What: health returns ok + search_reachable=true; `has_analysis` returns true.
/// Test: `call_tool_review_diff_returns_non_empty_verdict`.
struct ReadyAnalyzeDispatch;

#[async_trait]
impl AnalyzeClient for ReadyAnalyzeDispatch {
    async fn health(&self) -> Result<AnalyzeHealthResponse, AnalyzeClientError> {
        Ok(AnalyzeHealthResponse {
            status: "ok".into(),
            search_reachable: true,
        })
    }

    async fn has_analysis(&self, _path: &str) -> bool {
        true
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

/// Build an `AppState` with the required-context gate bypassed and healthy
/// stubs, suitable for fully offline `review_diff` pipeline tests.
///
/// Why: `call_tool("review_diff", ...)` runs the full pipeline including the
/// preflight gate (#590); setting `require_search = false` and
/// `require_analyze = false` lets the pipeline proceed with stub deps.
/// What: constructs `AppState` with `ApproveLlm` + `FakeSearchDispatch` +
/// `ReadyAnalyzeDispatch`, gate flags both false.
/// Test: `call_tool_review_diff_returns_non_empty_verdict`.
fn offline_state() -> AppState {
    let mut config = ReviewConfig::load(None);
    config.context.require_search = false;
    config.context.require_analyze = false;
    AppState::new(
        config,
        Arc::new(ApproveLlm),
        Arc::new(FakeSearchDispatch),
        Some(Arc::new(ReadyAnalyzeDispatch)),
    )
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// `call_tool("review_diff", ...)` with a small inline diff and stub LLM
/// returns `isError: false` and content JSON carrying a non-empty `verdict`.
///
/// Why: exercises the full `review_diff` dispatch path — temp-file creation,
/// `DiffSource::LocalFile`, pipeline run with fake LLM — fully offline, no
/// GitHub token required (closes #949).
/// What: builds an offline `AppState`; calls `call_tool("review_diff", ...)`
/// with a small unified diff; deserialises the MCP content text and asserts
/// `verdict` is non-empty and `isError` is false.
/// Test: this test itself; no network, no credentials needed.
#[tokio::test]
async fn call_tool_review_diff_returns_non_empty_verdict() {
    let state = offline_state();
    let args = json!({
        "diff": "+fn hello() { println!(\"hi\"); }\n",
        "context": "test: trivial add"
    });
    let result = call_tool("review_diff", &args, &state)
        .await
        .expect("call_tool must not return ToolError for a valid review_diff call");

    assert_eq!(
        result["isError"],
        json!(false),
        "review_diff must return isError:false for a valid diff"
    );

    let text = result["content"][0]["text"]
        .as_str()
        .expect("content[0].text must be a string");
    let review: Value = serde_json::from_str(text).expect("content text must be valid JSON");
    let verdict = review["verdict"]
        .as_str()
        .expect("verdict field must be a string");
    assert!(
        !verdict.is_empty(),
        "verdict must be non-empty, got: {verdict:?}"
    );
}

/// `call_tool("review_pr", ...)` with no `GITHUB_TOKEN` in the environment
/// returns an error indicating auth / token failure.
///
/// Why: exercises the token-resolution failure path in `call_review_pr` —
/// the tool must not panic and must surface the auth failure clearly
/// (closes #949).
/// What: blanks out `github_token` / `github_app_id` in the config so there
/// is no token to resolve; calls `call_tool("review_pr", ...)`; asserts the
/// response is `isError: true` or `ToolError::InvalidParams` mentioning auth.
/// Test: this test itself; failure is fast (token resolution fails before
/// any HTTP call, so no network).
#[tokio::test]
async fn call_tool_review_pr_no_token_returns_error() {
    let mut config = ReviewConfig::load(None);
    config.github_token = String::new();
    config.github_app_id = None;
    config.github_app_private_key = None;
    config.context.require_search = false;
    config.context.require_analyze = false;

    let state = AppState::new(
        config,
        Arc::new(ApproveLlm),
        Arc::new(FakeSearchDispatch),
        None,
    );

    let args = json!({
        "owner": "test-owner",
        "repo":  "test-repo",
        "pr":    1
    });

    let result = call_tool("review_pr", &args, &state).await;

    match result {
        Ok(envelope) => {
            // Auth failure delivered as MCP in-band error: isError: true.
            assert_eq!(
                envelope["isError"],
                json!(true),
                "review_pr with no token must return isError:true, got: {envelope}"
            );
            let text = envelope["content"][0]["text"]
                .as_str()
                .expect("content[0].text must be a string");
            let lower = text.to_lowercase();
            assert!(
                lower.contains("auth") || lower.contains("token") || lower.contains("github"),
                "error text must mention auth/token: {text:?}"
            );
        }
        // Err(ToolError::InvalidParams) is also acceptable for auth failure.
        Err(ToolError::InvalidParams(msg)) => {
            let lower = msg.to_lowercase();
            assert!(
                lower.contains("auth") || lower.contains("token") || lower.contains("github"),
                "InvalidParams must mention auth/token: {msg:?}"
            );
        }
        Err(ToolError::UnknownTool) => {
            panic!("review_pr must be a known tool");
        }
    }
}
