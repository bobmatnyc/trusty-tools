//! Unit and integration tests for `mcp::dispatch`.
//!
//! Why: split from `mcp/mod.rs` to keep that file under the 500-line cap while
//! preserving full test coverage for the MCP dispatcher, tools/list handler,
//! and the binary stdio smoke test (#950).
//! What: exercises `dispatch` with synthetic `Request` values for initialize,
//! tools/list, tools/call, notification suppression, and error paths; also
//! includes the in-process tools/list name-verification test and the
//! #[ignore]-gated binary stdio spawn test (closes #950).
//! Test: this is the test module; each `#[test]` / `#[tokio::test]` is a
//! self-contained unit or integration test.

use super::*;
use serde_json::json;
use std::collections::HashSet;
use std::sync::Arc;
use trusty_common::mcp::error_codes;

use crate::{
    config::ReviewConfig,
    integrations::search_client::{
        EmbedderState, HealthResponse as SearchHealth, IndexInfo, SearchClient, SearchClientError,
        SearchResult,
    },
    llm::{LlmError, LlmProvider, LlmRequest, LlmResponse},
    service::AppState,
};
use async_trait::async_trait;

// ── Fake LLM ──────────────────────────────────────────────────────────────

struct FakeLlm;

#[async_trait]
impl LlmProvider for FakeLlm {
    fn name(&self) -> &str {
        "fake-mcp-test"
    }

    async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
        Ok(LlmResponse {
            text: r#"{"verdict":"APPROVE","summary":"ok","findings":[]}"#.into(),
            model: req.model.clone(),
            input_tokens: 1,
            output_tokens: 1,
            latency_ms: 0,
            cost_usd: 0.0,
        })
    }
}

// ── Fake search ───────────────────────────────────────────────────────────

struct FakeSearch;

#[async_trait]
impl SearchClient for FakeSearch {
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

/// Build a minimal `AppState` suitable for unit tests.  Only `review_health`
/// and protocol-level dispatch are exercised here; the FakeLlm is present to
/// satisfy the constructor but is never called.
fn test_state() -> AppState {
    let config = ReviewConfig::load(None);
    AppState::new(config, Arc::new(FakeLlm), Arc::new(FakeSearch), None)
}

fn make_req(method: &str, params: Value) -> Request {
    Request {
        jsonrpc: Some("2.0".into()),
        id: Some(json!(1)),
        method: method.into(),
        params: Some(params),
    }
}

#[tokio::test]
async fn dispatch_initialize_returns_server_info() {
    let state = test_state();
    let req = make_req("initialize", json!({}));
    let resp = dispatch(req, &state).await;
    let result = resp.result.expect("expected result");
    assert_eq!(result["serverInfo"]["name"], "trusty-review");
    assert!(result["serverInfo"]["version"].is_string());
    assert_eq!(result["protocolVersion"], "2024-11-05");
}

#[tokio::test]
async fn dispatch_unknown_tool_returns_method_not_found() {
    let state = test_state();
    let req = make_req("not_a_tool", json!({}));
    let resp = dispatch(req, &state).await;
    let err = resp.error.expect("expected error");
    assert_eq!(err.code, error_codes::METHOD_NOT_FOUND);
}

#[tokio::test]
async fn dispatch_notification_is_suppressed() {
    let state = test_state();
    let req = Request {
        jsonrpc: Some("2.0".into()),
        id: None, // notification — no id
        method: "notifications/initialized".into(),
        params: None,
    };
    let resp = dispatch(req, &state).await;
    assert!(resp.suppress, "notification must be suppressed");
}

#[tokio::test]
async fn dispatch_review_health_via_bare_method() {
    let state = test_state();
    let req = make_req("review_health", json!({}));
    let resp = dispatch(req, &state).await;
    let result = resp.result.expect("expected result");
    // review_health wraps the payload in {content:[{type:text,text:...}]}
    let text = result["content"][0]["text"].as_str().expect("text field");
    let health: Value = serde_json::from_str(text).expect("valid JSON in text");
    assert_eq!(health["status"], "ok");
    assert!(health["version"].is_string());
}

#[tokio::test]
async fn dispatch_review_health_via_tools_call() {
    let state = test_state();
    let req = make_req(
        "tools/call",
        json!({ "name": "review_health", "arguments": {} }),
    );
    let resp = dispatch(req, &state).await;
    let result = resp.result.expect("expected result");
    let text = result["content"][0]["text"].as_str().expect("text field");
    let health: Value = serde_json::from_str(text).expect("valid JSON in text");
    assert_eq!(health["status"], "ok");
}

#[tokio::test]
async fn dispatch_rejects_wrong_jsonrpc_version() {
    let state = test_state();
    let req = Request {
        jsonrpc: Some("1.0".into()),
        id: Some(json!(7)),
        method: "review_health".into(),
        params: None,
    };
    let resp = dispatch(req, &state).await;
    let err = resp.error.expect("expected error");
    assert_eq!(err.code, error_codes::INVALID_REQUEST);
}

#[tokio::test]
async fn dispatch_tools_call_missing_name_returns_invalid_params() {
    let state = test_state();
    let req = make_req("tools/call", json!({ "arguments": {} }));
    let resp = dispatch(req, &state).await;
    let err = resp.error.expect("expected error");
    assert_eq!(err.code, error_codes::INVALID_PARAMS);
}

/// In-process smoke test: `tools/list` response lists exactly 3 tools.
///
/// Why: guarantees the tools/list handler is wired correctly and the tool
/// count matches the documented surface (review_pr, review_diff,
/// review_health) without spawning a subprocess or needing credentials
/// (closes #950 in-process requirement).
/// What: dispatches a `tools/list` request through the in-process
/// `dispatch` function and asserts exactly 3 tools with the expected names.
/// Test: this test itself; always runs in CI (not gated with `#[ignore]`).
#[tokio::test]
async fn dispatch_tools_list_three_tools_names_verified() {
    let state = test_state();
    let req = make_req("tools/list", json!({}));
    let resp = dispatch(req, &state).await;
    let result = resp.result.expect("expected result");
    let tools = result["tools"].as_array().expect("tools must be array");
    let names: HashSet<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
    let expected: HashSet<&str> = ["review_pr", "review_diff", "review_health"]
        .into_iter()
        .collect();
    assert_eq!(
        names, expected,
        "tools/list returned unexpected set: {:?}",
        names
    );
}

/// Binary stdio smoke test: spawn `trusty-review serve --stdio`, send MCP
/// `initialize` + `tools/list`, assert exactly 3 tools are listed.
///
/// Why: validates the end-to-end `serve --stdio` path as an integration
/// test — subprocess spawn, JSON-RPC framing, and tool registration —
/// without needing credentials or a running daemon (closes #950
/// binary-level requirement).
/// What: locates the `trusty-review` binary via `CARGO_BIN_EXE_trusty-review`
/// (set by Cargo when running `cargo test --test`), spawns it in stdio MCP
/// mode, writes two JSON-RPC requests on stdin, reads two responses on
/// stdout, and asserts `tools/list` returns 3 tools.
/// Test: gated `#[ignore]` because it requires a compiled binary; run
/// explicitly with `cargo test -p trusty-review -- --include-ignored
/// stdio_serve_tools_list_returns_three_tools`.
#[tokio::test]
#[ignore = "requires compiled binary (run with --include-ignored)"]
async fn stdio_serve_tools_list_returns_three_tools() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt as _, BufReader};
    use tokio::process::Command;

    // CARGO_BIN_EXE_trusty-review is set by Cargo when running `cargo test`.
    // Fall back to searching $PATH so developers can run this manually too.
    let bin = option_env!("CARGO_BIN_EXE_trusty-review")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("trusty-review"));

    let mut child = Command::new(&bin)
        .arg("serve")
        .arg("--stdio")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("failed to spawn trusty-review binary");

    let mut stdin = child.stdin.take().expect("piped stdin");
    let stdout = child.stdout.take().expect("piped stdout");
    let mut reader = BufReader::new(stdout).lines();

    // Send initialize request.
    let init = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {}
    });
    stdin
        .write_all(format!("{}\n", init).as_bytes())
        .await
        .expect("write initialize");
    // Skip initialize response line.
    let _init_resp = reader.next_line().await.expect("read init response");

    // Send tools/list request.
    let list_req = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/list",
        "params": {}
    });
    stdin
        .write_all(format!("{}\n", list_req).as_bytes())
        .await
        .expect("write tools/list");

    let list_line = reader
        .next_line()
        .await
        .expect("read tools/list response")
        .expect("non-EOF tools/list line");

    // Parse and collect results before killing the child so an assertion
    // panic cannot orphan the subprocess.
    let resp: Value =
        serde_json::from_str(&list_line).expect("tools/list response must be valid JSON");
    let tool_count = resp["result"]["tools"]
        .as_array()
        .map(|a| a.len())
        .unwrap_or(0);

    // Unconditional kill — runs before any assertion can panic.
    child.kill().await.ok();

    assert_eq!(
        tool_count, 3,
        "serve --stdio tools/list must return 3 tools"
    );
}
