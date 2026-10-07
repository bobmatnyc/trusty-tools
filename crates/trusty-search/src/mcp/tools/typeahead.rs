//! MCP `typeahead` tool dispatch.
//!
//! Why: typeahead is a new per-keystroke autocomplete capability. Keeping its
//! dispatch in a dedicated file prevents `search.rs` — already near the 500
//! SLOC cap — from growing further, and maintains the one-concern-per-file
//! pattern used by `search.rs`, `index.rs`, and `misc.rs`.
//! What: `dispatch_typeahead_tool` handles the `"typeahead"` tool name.
//! It resolves the index, validates params, and calls `search.typeahead` on
//! the daemon's socket (#9168).
//! Test: `typeahead_mcp_tool_returns_hits` in `tests/typeahead.rs`
//! (integration-level); `tests_typeahead` unit tests below.

use serde_json::Value;

use super::{
    types::{require_str, DispatchError},
    McpServer,
};
use crate::service::rpc::queries::METHOD_TYPEAHEAD;

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
/// What: resolves `index_id` (explicit, `project` (#9168), or pinned),
/// requires `query`, and calls `search.typeahead` with `{index_id, q, limit?,
/// mode?}`.
/// Test: `tests_typeahead::handle_typeahead_missing_query_returns_invalid_params`.
async fn handle_typeahead(server: &McpServer, args: &Value) -> Result<Value, DispatchError> {
    // #6317: an unpinned call with no id gets the indexes that exist plus a
    // retry hint, as a success — typeahead is a read.
    let Some(index_id) = server.resolve_target(args).await? else {
        return super::index_directory::index_directory(server, "typeahead").await;
    };

    let query = require_str(args, "query")?;

    // #9168: the query-string fields travel as typed JSON beside `index_id`.
    let mut params = serde_json::json!({ "index_id": index_id, "q": query });
    if let Some(limit) = args.get("limit").and_then(Value::as_u64) {
        params["limit"] = Value::from(limit);
    }
    if let Some(mode) = args.get("mode").and_then(Value::as_str) {
        params["mode"] = Value::String(mode.to_string());
    }

    server.call(METHOD_TYPEAHEAD, params).await
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
        // What: creates a server on an absent socket (never dialled — `None`
        // is returned before any daemon call).
        // Test: this test.
        let server = crate::mcp::tools::test_daemon::unreachable_server();
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
    fn handle_typeahead_missing_index_id_consults_the_directory() {
        // Why (#6317): this asserted `InvalidParams`, which was the contract
        // until the owner ruling made an unresolvable index answer with the
        // indexes that exist. What it guards now is that the arm goes to the
        // daemon for that listing — against an absent socket that surfaces as
        // `Transport`, never as a parameter error. The success shape is
        // asserted in `tests_index_directory` against a live mock.
        // What: calls `dispatch_typeahead_tool` with `{"query": "fn"}` and no
        // `index_id` on an unpinned server pointed at an absent socket.
        // Test: this test.
        let server = crate::mcp::tools::test_daemon::unreachable_server();
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
            Some(Err(DispatchError::Transport(_))) => {}
            other => panic!("expected Some(Err(Transport)), got {other:?}"),
        }
    }

    #[test]
    fn handle_typeahead_missing_query_returns_invalid_params() {
        // Why: `query` is required — an omitted `query` must be caught before
        // any daemon call (fast-fail with InvalidParams).
        // What: calls with `{"index_id": "x"}` — no `query` key.
        // Test: this test.
        let server = crate::mcp::tools::test_daemon::unreachable_server();
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
