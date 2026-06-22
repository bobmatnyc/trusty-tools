//! MCP `typeahead` tool dispatch.
//!
//! Why: typeahead is a new per-keystroke autocomplete capability. Keeping its
//! dispatch in a dedicated file prevents `search.rs` — already near the 500
//! SLOC cap — from growing further, and maintains the one-concern-per-file
//! pattern used by `search.rs`, `index.rs`, and `misc.rs`.
//! What: `dispatch_typeahead_tool` handles the `"typeahead"` tool name.
//! It resolves the index, validates params, and forwards a GET request to
//! `GET /indexes/:id/typeahead` on the daemon.
//! Test: `typeahead_mcp_tool_returns_hits` in `tests/typeahead.rs`
//! (integration-level); `tests_typeahead` unit tests below.

use serde_json::Value;

use super::{
    types::{require_str, DispatchError},
    McpServer,
};

/// Route the `"typeahead"` tool to the daemon's typeahead endpoint.
///
/// Why: factoring typeahead dispatch out of `search.rs` keeps that file under
/// the 500 SLOC cap and separates per-keystroke autocomplete concerns from
/// full-search concerns.
/// What: returns `None` when `tool != "typeahead"` so the `call_tool` chain
/// can try other groups. On success, returns the daemon response as-is.
/// Test: `tests_typeahead` below; integration-level tests in
/// `crates/trusty-search/tests/typeahead.rs`.
pub(super) async fn dispatch_typeahead_tool(
    server: &McpServer,
    tool: &str,
    args: &Value,
) -> Option<Result<Value, DispatchError>> {
    if tool != "typeahead" {
        return None;
    }
    Some(handle_typeahead(server, args).await)
}

/// Execute the `typeahead` MCP tool.
///
/// Why: isolated for readability and independent testability.
/// What: resolves `index_id` (pinned or explicit), requires `query`, and
/// issues a GET to `/indexes/{id}/typeahead?q=...&limit=...&mode=...`.
/// Test: `tests_typeahead::typeahead_tool_returns_daemon_response`.
async fn handle_typeahead(server: &McpServer, args: &Value) -> Result<Value, DispatchError> {
    let index_id = server.resolve_index_id(args).ok_or_else(|| {
        DispatchError::InvalidParams("missing required string field: index_id".into())
    })?;

    let query = require_str(args, "query")?;

    // Build query params: q, limit (optional), mode (optional).
    let mut params: Vec<(&str, String)> = vec![("q", query.to_string())];
    if let Some(limit) = args.get("limit").and_then(Value::as_u64) {
        params.push(("limit", limit.to_string()));
    }
    if let Some(mode) = args.get("mode").and_then(Value::as_str) {
        params.push(("mode", mode.to_string()));
    }

    server
        .get_query(&format!("/indexes/{index_id}/typeahead"), &params)
        .await
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests_typeahead {
    use super::*;
    use serde_json::json;

    #[test]
    fn dispatch_returns_none_for_unknown_tool() {
        // `dispatch_typeahead_tool` is async but we can check the `None` case
        // by running it in a simple tokio runtime with a dummy server.
        //
        // Why: confirms the dispatch chain falls through correctly for any tool
        // name that is not `"typeahead"`.
        // What: creates a fake McpServer (real URL doesn't matter — the call
        // never hits the network because `None` is returned before any HTTP).
        // Test: this test.
        let server = McpServer::new("http://127.0.0.1:9999");
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("rt");
        let result = rt.block_on(dispatch_typeahead_tool(
            &server,
            "search_lexical",
            &json!({}),
        ));
        assert!(
            result.is_none(),
            "must return None for a non-typeahead tool name"
        );
    }

    #[test]
    fn handle_typeahead_missing_index_id_returns_invalid_params() {
        // Why: when neither an explicit `index_id` nor a session-pinned index
        // is available, the tool must return `InvalidParams` (not panic).
        // What: calls `dispatch_typeahead_tool` with `{"query": "fn"}` and no
        // `index_id` on an unpinned server — expects `InvalidParams`.
        // Test: this test.
        let server = McpServer::new("http://127.0.0.1:9999");
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("rt");
        let result = rt.block_on(dispatch_typeahead_tool(
            &server,
            "typeahead",
            &json!({ "query": "fn" }),
        ));
        match result {
            Some(Err(DispatchError::InvalidParams(msg))) => {
                assert!(
                    msg.contains("index_id"),
                    "error must mention index_id: {msg}"
                );
            }
            other => panic!("expected Some(Err(InvalidParams)), got {other:?}"),
        }
    }

    #[test]
    fn handle_typeahead_missing_query_returns_invalid_params() {
        // Why: `query` is required — an omitted `query` must be caught before
        // any HTTP call (fast-fail with InvalidParams).
        // What: calls with `{"index_id": "x"}` — no `query` key.
        // Test: this test.
        let server = McpServer::new("http://127.0.0.1:9999");
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("rt");
        let result = rt.block_on(dispatch_typeahead_tool(
            &server,
            "typeahead",
            &json!({ "index_id": "myproject" }),
        ));
        match result {
            Some(Err(DispatchError::InvalidParams(msg))) => {
                assert!(msg.contains("query"), "error must mention query: {msg}");
            }
            other => panic!("expected Some(Err(InvalidParams)), got {other:?}"),
        }
    }
}
