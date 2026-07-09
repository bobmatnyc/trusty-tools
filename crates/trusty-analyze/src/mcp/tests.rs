//! Dispatch-level tests for the MCP server.
//!
//! Why: moved out of `mcp/mod.rs` so that file stays under the 500-SLOC
//! production cap (see #1195); test files carry the 1500-SLOC cap.
//! What: exercises `dispatch`, `tools/list`, `resources/list`, and the
//! per-tool handlers directly.
//! Test: this *is* the test module.

use super::*;

fn req(method: &str, params: Value) -> Request {
    Request {
        jsonrpc: "2.0".into(),
        id: Some(Value::from(1u64)),
        method: method.into(),
        params,
    }
}

#[tokio::test]
async fn tools_list_contains_full_surface() {
    let server = AnalyzerMcpServer::new("http://127.0.0.1:1");
    let resp = server.dispatch(req("tools/list", Value::Null)).await;
    let result = resp.result.expect("expected result");
    let tools = result
        .get("tools")
        .and_then(Value::as_array)
        .expect("array");
    let names: Vec<&str> = tools
        .iter()
        .filter_map(|t| t.get("name").and_then(Value::as_str))
        .collect();
    for required in [
        "complexity_hotspots",
        "find_smells",
        "analyze_quality",
        "run_diagnostics",
        "list_facts",
        "upsert_fact",
        "delete_fact",
        "analyzer_health",
        "ingest_scip",
        "list_analyze_indexes",
    ] {
        assert!(
            names.contains(&required),
            "missing tool '{required}' (got {names:?})"
        );
    }
}

#[tokio::test]
async fn tools_list_includes_review_diff() {
    let server = AnalyzerMcpServer::new("http://127.0.0.1:1");
    let resp = server.dispatch(req("tools/list", Value::Null)).await;
    let result = resp.result.expect("expected result");
    let tools = result
        .get("tools")
        .and_then(Value::as_array)
        .expect("array");
    let names: Vec<&str> = tools
        .iter()
        .filter_map(|t| t.get("name").and_then(Value::as_str))
        .collect();
    assert!(names.contains(&"review_diff"), "got {names:?}");
}

#[tokio::test]
async fn review_diff_requires_diff_param() {
    // Missing 'diff' → InvalidParams before any HTTP call is attempted.
    let server = AnalyzerMcpServer::new("http://127.0.0.1:1");
    let err = server
        .handle_review_diff(&serde_json::json!({ "index_id": "x" }))
        .await
        .expect_err("missing diff param should fail");
    assert!(matches!(err, DispatchError::InvalidParams(_)));
}

#[tokio::test]
async fn review_diff_requires_index_id() {
    // Missing 'index_id' → InvalidParams: review is backed by trusty-search
    // and needs an index to cross-reference against.
    let server = AnalyzerMcpServer::new("http://127.0.0.1:1");
    let err = server
        .handle_review_diff(&serde_json::json!({ "diff": "+++ b/x.rs\n" }))
        .await
        .expect_err("missing index_id param should fail");
    assert!(matches!(err, DispatchError::InvalidParams(_)));
}

#[tokio::test]
async fn review_diff_with_args_attempts_post_to_review() {
    // Daemon unreachable — a Transport error mentioning /review proves the
    // handler built the right URL (with index_id) and method.
    let server = AnalyzerMcpServer::new("http://127.0.0.1:1");
    let err = server
        .handle_review_diff(&serde_json::json!({
            "diff": "+++ b/x.rs\n",
            "index_id": "my-idx",
        }))
        .await
        .expect_err("daemon unreachable");
    match err {
        DispatchError::Transport(msg) => {
            assert!(msg.contains("/review"), "got {msg}");
            assert!(msg.contains("index_id=my-idx"), "got {msg}");
        }
        other => panic!("expected Transport, got {other:?}"),
    }
}

#[tokio::test]
async fn tools_list_includes_review_github_pr() {
    let server = AnalyzerMcpServer::new("http://127.0.0.1:1");
    let resp = server.dispatch(req("tools/list", Value::Null)).await;
    let result = resp.result.expect("expected result");
    let tools = result
        .get("tools")
        .and_then(Value::as_array)
        .expect("array");
    let names: Vec<&str> = tools
        .iter()
        .filter_map(|t| t.get("name").and_then(Value::as_str))
        .collect();
    assert!(names.contains(&"review_github_pr"), "got {names:?}");
}

#[tokio::test]
async fn review_github_pr_requires_owner() {
    // Missing 'owner' → InvalidParams before any HTTP call.
    let server = AnalyzerMcpServer::new("http://127.0.0.1:1");
    let err = server
        .handle_review_github_pr(&serde_json::json!({
            "repo": "r", "pr": 1, "index_id": "i"
        }))
        .await
        .expect_err("missing owner should fail");
    assert!(matches!(err, DispatchError::InvalidParams(_)));
}

#[tokio::test]
async fn review_github_pr_requires_pr_number() {
    let server = AnalyzerMcpServer::new("http://127.0.0.1:1");
    let err = server
        .handle_review_github_pr(&serde_json::json!({
            "owner": "o", "repo": "r", "index_id": "i"
        }))
        .await
        .expect_err("missing pr should fail");
    assert!(matches!(err, DispatchError::InvalidParams(_)));
}

#[tokio::test]
async fn review_github_pr_posts_to_endpoint() {
    // Daemon unreachable — a Transport error referencing /review/github-pr
    // proves the handler built the right URL after parsing all params.
    let server = AnalyzerMcpServer::new("http://127.0.0.1:1");
    let err = server
        .handle_review_github_pr(&serde_json::json!({
            "owner": "o", "repo": "r", "pr": 7, "index_id": "i"
        }))
        .await
        .expect_err("daemon unreachable");
    match err {
        DispatchError::Transport(msg) => {
            assert!(msg.contains("/review/github-pr"), "got {msg}");
        }
        other => panic!("expected Transport, got {other:?}"),
    }
}

#[tokio::test]
async fn tools_list_includes_deep_analysis() {
    let server = AnalyzerMcpServer::new("http://127.0.0.1:1");
    let resp = server.dispatch(req("tools/list", Value::Null)).await;
    let result = resp.result.expect("expected result");
    let tools = result
        .get("tools")
        .and_then(Value::as_array)
        .expect("array");
    let names: Vec<&str> = tools
        .iter()
        .filter_map(|t| t.get("name").and_then(Value::as_str))
        .collect();
    assert!(names.contains(&"deep_analysis"), "got {names:?}");
}

#[tokio::test]
async fn deep_analysis_requires_index_id() {
    let server = AnalyzerMcpServer::new("http://127.0.0.1:1");
    let err = server
        .handle_deep_analysis(&serde_json::json!({}))
        .await
        .expect_err("missing index_id should fail");
    assert!(matches!(err, DispatchError::InvalidParams(_)));
}

#[tokio::test]
async fn deep_analysis_posts_to_endpoint() {
    // Daemon unreachable — a Transport error referencing /analyze/deep
    // proves the handler built the right URL after parsing index_id.
    let server = AnalyzerMcpServer::new("http://127.0.0.1:1");
    let err = server
        .handle_deep_analysis(&serde_json::json!({ "index_id": "i", "model": "m" }))
        .await
        .expect_err("daemon unreachable");
    match err {
        DispatchError::Transport(msg) => {
            assert!(msg.contains("/analyze/deep"), "got {msg}");
        }
        other => panic!("expected Transport, got {other:?}"),
    }
}

#[tokio::test]
async fn resources_list_returns_envelope() {
    // Daemon unreachable → GET /indexes fails → empty resource list, but
    // the response is still a well-formed `{ resources: [] }` result.
    let server = AnalyzerMcpServer::new("http://127.0.0.1:1");
    let resp = server.dispatch(req("resources/list", Value::Null)).await;
    let result = resp.result.expect("expected result");
    let resources = result
        .get("resources")
        .and_then(Value::as_array)
        .expect("resources array");
    assert!(resources.is_empty(), "expected empty list when daemon down");
}

#[tokio::test]
async fn initialize_advertises_resources_capability() {
    let server = AnalyzerMcpServer::new("http://127.0.0.1:1");
    let resp = server.dispatch(req("initialize", Value::Null)).await;
    let result = resp.result.expect("expected result");
    assert!(result["capabilities"]["resources"].is_object());
}

#[tokio::test]
async fn unknown_tool_returns_method_not_found() {
    let server = AnalyzerMcpServer::new("http://127.0.0.1:1");
    let resp = server
        .dispatch(req(
            "tools/call",
            serde_json::json!({ "name": "no_such_tool", "arguments": {} }),
        ))
        .await;
    let err = resp.error.expect("expected error");
    assert_eq!(err.code, error_codes::METHOD_NOT_FOUND);
}

#[tokio::test]
async fn handle_analyzer_health_calls_health_endpoint() {
    // Direct handler invocation, bypassing dispatch. Daemon is unreachable,
    // so we expect a Transport error referencing /health, which proves the
    // handler constructed the right URL without us going through tools/call.
    let server = AnalyzerMcpServer::new("http://127.0.0.1:1");
    let err = server
        .handle_analyzer_health(&Value::Null)
        .await
        .expect_err("daemon unreachable, expected transport error");
    match err {
        DispatchError::Transport(msg) => {
            assert!(
                msg.contains("/health"),
                "expected transport error to mention /health, got: {msg}"
            );
        }
        other => panic!("expected DispatchError::Transport, got {other:?}"),
    }
}

#[tokio::test]
async fn rejects_wrong_jsonrpc_version() {
    let server = AnalyzerMcpServer::new("http://127.0.0.1:1");
    let r = Request {
        jsonrpc: "1.0".into(),
        id: Some(Value::from(7u64)),
        method: "tools/list".into(),
        params: Value::Null,
    };
    let resp = server.dispatch(r).await;
    let err = resp.error.expect("expected error");
    assert_eq!(err.code, error_codes::INVALID_REQUEST);
}

// ── #630: trusty-review LLM tools (feature `review`) ─────────────────────

/// With the `review` feature ON, `tools/list` advertises all three
/// `tr_review_*` tools alongside the base analyzer tools.
#[cfg(feature = "review")]
#[tokio::test]
async fn tools_list_includes_tr_review_tools() {
    let server = AnalyzerMcpServer::new("http://127.0.0.1:1");
    let resp = server.dispatch(req("tools/list", Value::Null)).await;
    let result = resp.result.expect("expected result");
    let tools = result
        .get("tools")
        .and_then(Value::as_array)
        .expect("array");
    let names: Vec<&str> = tools
        .iter()
        .filter_map(|t| t.get("name").and_then(Value::as_str))
        .collect();
    for required in ["tr_review_pr", "tr_review_diff", "tr_review_health"] {
        assert!(names.contains(&required), "missing {required} in {names:?}");
    }
    // The base surface must still be present (no descriptors lost in the
    // extract-to-descriptors.rs refactor).
    assert!(names.contains(&"complexity_hotspots"), "got {names:?}");
    assert!(names.contains(&"deep_analysis"), "got {names:?}");
}

/// With the `review` feature ON, the `tr_review_*` names route into the
/// embedded pipeline rather than falling through to `UnknownTool`. We do not
/// assert success (the credential-bound build path needs live AWS /
/// OpenRouter config); we only assert the dispatcher did NOT return
/// METHOD_NOT_FOUND, i.e. the name was recognised and routed.
#[cfg(feature = "review")]
#[tokio::test]
async fn tr_review_health_routes_not_unknown_tool() {
    let server = AnalyzerMcpServer::new("http://127.0.0.1:1");
    let resp = server
        .dispatch(req(
            "tools/call",
            serde_json::json!({ "name": "tr_review_health", "arguments": {} }),
        ))
        .await;
    // tools/call form: routed tools return a result (possibly an in-band
    // isError envelope on build failure); an *unrecognised* name returns a
    // JSON-RPC METHOD_NOT_FOUND error. Assert we are not in the latter case.
    if let Some(err) = resp.error {
        assert_ne!(
            err.code,
            error_codes::METHOD_NOT_FOUND,
            "tr_review_health must route, not be unknown: {err:?}"
        );
    }
}

/// With the `review` feature OFF, the `tr_review_*` names are neither
/// advertised in `tools/list` nor routable — they return METHOD_NOT_FOUND.
#[cfg(not(feature = "review"))]
#[tokio::test]
async fn tr_review_tools_absent_when_feature_off() {
    let server = AnalyzerMcpServer::new("http://127.0.0.1:1");

    // Not advertised.
    let resp = server.dispatch(req("tools/list", Value::Null)).await;
    let tools = resp
        .result
        .expect("result")
        .get("tools")
        .and_then(Value::as_array)
        .expect("array")
        .clone();
    let names: Vec<String> = tools
        .iter()
        .filter_map(|t| t.get("name").and_then(Value::as_str))
        .map(str::to_owned)
        .collect();
    for absent in ["tr_review_pr", "tr_review_diff", "tr_review_health"] {
        assert!(
            !names.iter().any(|n| n == absent),
            "{absent} must be absent when feature off; got {names:?}"
        );
    }

    // Not routable.
    let resp = server
        .dispatch(req(
            "tools/call",
            serde_json::json!({ "name": "tr_review_health", "arguments": {} }),
        ))
        .await;
    let err = resp.error.expect("expected METHOD_NOT_FOUND error");
    assert_eq!(err.code, error_codes::METHOD_NOT_FOUND);
}
