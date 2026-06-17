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
            finish_reason: None,
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

// ── reviewer_model provider override (#1233) ───────────────────────────────────

/// Build an offline `AppState` whose STARTUP provider is Bedrock, with a non-empty
/// OpenRouter API key so an `openrouter/...` override can actually build.
///
/// Why: the #1233 regression is that an `openrouter/...` override against a
/// Bedrock-startup state was silently routed to Bedrock.  The test needs a state
/// whose startup provider is unambiguously Bedrock and whose config carries an
/// OpenRouter key so `build_provider` succeeds for the override.
/// What: loads config, forces `role_models.reviewer.provider = Bedrock`, sets a
/// dummy `openrouter_api_key`, and wires the offline stubs.
/// Test: used by `deps_from_state_*` tests below.
fn bedrock_startup_state() -> AppState {
    use crate::config::Provider;
    let mut config = ReviewConfig::load(None);
    config.context.require_search = false;
    config.context.require_analyze = false;
    config.role_models.reviewer.provider = Provider::Bedrock;
    // Non-empty dummy key so build_provider's empty-key guard passes; not a real
    // credential.
    config.openrouter_api_key = "dummy-openrouter-key-for-tests".to_string(); // pragma: allowlist secret
    AppState::new(
        config,
        Arc::new(ApproveLlm), // startup provider stub (name() == "approve-stub")
        Arc::new(FakeSearchDispatch),
        Some(Arc::new(ReadyAnalyzeDispatch)),
    )
}

#[tokio::test]
async fn deps_from_state_openrouter_override_switches_provider() {
    // #1233: an `openrouter/...` reviewer_model override against a Bedrock-startup
    // state must build a fresh OpenRouter provider, NOT silently reuse the startup
    // (Bedrock) provider.
    let state = bedrock_startup_state();
    let (deps, fallback) =
        super::deps_from_state(&state, "openrouter/openai/gpt-5.4-mini-20260317").await;
    assert_eq!(
        deps.llm.name(),
        "openrouter",
        "an openrouter/ override must route to the OpenRouter backend (#1233)"
    );
    assert!(
        fallback.is_none(),
        "a successful override build must NOT report a fallback (#1357)"
    );
}

#[tokio::test]
async fn deps_from_state_no_override_reuses_startup_provider() {
    // Control: a bare model id (no routing prefix) keeps the startup provider —
    // the stub `ApproveLlm`, whose name() is NOT "openrouter".
    let state = bedrock_startup_state();
    let (deps, fallback) = super::deps_from_state(&state, "us.anthropic.claude-sonnet-4-6").await;
    assert_ne!(
        deps.llm.name(),
        "openrouter",
        "a bare model id must reuse the startup (Bedrock) provider, not switch"
    );
    assert!(
        fallback.is_none(),
        "the no-override path must NOT report a fallback (#1357)"
    );
}

/// #1357 item 2: when an override provider fails to build, `deps_from_state` must
/// report a `Some(reason)` fallback so the MCP caller can detect the wrong-backend
/// silent fallback.
///
/// Why: previously the failed-override path only `warn!`d and silently reused the
/// startup provider; an MCP caller had no way to know it got the wrong backend.
/// What: an `openrouter/...` override against a Bedrock-startup state whose config
/// has an EMPTY OpenRouter key makes `build_provider` fail the empty-key guard;
/// the result must fall back to the startup provider AND carry a fallback reason.
/// Test: this test itself.
#[tokio::test]
async fn deps_from_state_build_failure_reports_fallback() {
    let mut state = bedrock_startup_state();
    // Empty the key so OpenRouterProvider::new() fails its empty-key guard.
    state.config.openrouter_api_key = String::new();

    let (deps, fallback) =
        super::deps_from_state(&state, "openrouter/openai/gpt-5.4-mini-20260317").await;
    // Fell back to the startup (Bedrock) provider, not the requested OpenRouter one.
    assert_ne!(
        deps.llm.name(),
        "openrouter",
        "a failed override build must fall back to the startup provider"
    );
    let reason = fallback.expect("a failed override build must report a fallback reason (#1357)");
    assert!(
        reason.contains("reviewer_model") && reason.contains("fell back"),
        "fallback reason must be actionable: {reason}"
    );
}
