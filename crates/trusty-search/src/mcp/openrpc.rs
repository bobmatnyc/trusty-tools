//! OpenRPC 1.3.2 service description for `trusty-search` MCP.
//!
//! Why: Orchestrators such as open-mpm need a machine-readable manifest of
//! every search tool the daemon exposes — including the logical scopes
//! (`search.read` / `search.write`) each tool requires — so they can route
//! tasks and enforce per-tool authorisation without bespoke per-server
//! adapters. OpenRPC's `rpc.discover` method is the standard JSON-RPC 2.0
//! discovery surface; it reuses the existing transport.
//!
//! What: A thin wrapper around
//! `trusty_mcp_core::openrpc::discover_response` that feeds it the
//! `tool_descriptors()` array and a scope resolver. Read-only tools
//! (queries, lookups, health, listings) require `search.read`; tools that
//! mutate index state (index_file, remove_file, create_index, delete_index,
//! reindex) require `search.write`. The `chat` tool is treated as
//! `search.read` because it only consumes existing indexes.
//!
//! Test: `cargo test --lib mcp::openrpc` checks the envelope shape, that
//! every tool gets a non-empty scope list, and that mutating tools land
//! in `search.write`.

use serde_json::Value;
use trusty_common::mcp::openrpc::discover_response;

use crate::mcp::tools::tool_descriptors;

/// Logical scopes exposed by the trusty-search MCP server.
///
/// Why: Centralising the scope literals here keeps them aligned with what
/// orchestrators key off; any drift would surface in
/// `read_write_classification`.
/// What: Two scopes — read for queries/listings/health, write for tools
/// that create, modify, or delete index state.
/// Test: `every_tool_has_scopes`, `read_write_classification`.
mod scopes {
    pub const SEARCH_READ: &str = "search.read";
    pub const SEARCH_WRITE: &str = "search.write";
}

/// Return the logical scopes a given search tool requires.
///
/// Why: open-mpm needs to know whether a tool mutates index state so it
/// can enforce least-privilege authorisation before dispatching the call.
/// What: Read-only tools → `["search.read"]`; index-mutating tools →
/// `["search.write"]`. Unknown names return an empty slice; the unit
/// test asserts every currently registered tool maps to a non-empty set
/// so future additions surface immediately.
/// Test: `every_tool_has_scopes`, `read_write_classification`.
pub fn scopes_for_tool(name: &str) -> Vec<String> {
    use scopes::*;
    let s: &[&str] = match name {
        // Read-only / query / introspection. Issue #138 added the per-lane
        // search tools (`search_lexical`, `search_semantic`, `search_kg`)
        // alongside the existing `search_all` (now polymorphic between
        // cross-project fan-out and per-index hybrid).
        "search_all" | "search" | "search_lexical" | "search_semantic" | "search_kg"
        | "search_similar" | "search_health" | "list_indexes" | "index_status" | "list_chunks"
        | "chat" | "get_call_chain" | "grep" | "console_metrics" => &[SEARCH_READ],

        // Mutating
        "index_file" | "remove_file" | "create_index" | "delete_index" | "reindex" => {
            &[SEARCH_WRITE]
        }

        // Upgrade: requires admin write access — installs a new binary and
        // may restart the daemon (issue #537).
        "upgrade" => &[SEARCH_WRITE],

        _ => &[],
    };
    s.iter().map(|x| (*x).to_string()).collect()
}

/// Build the OpenRPC `rpc.discover` response for this server.
///
/// Why: Produces the JSON value used as the `result` field of an
/// `rpc.discover` JSON-RPC response so any compliant client can introspect
/// every method, its parameters, and required scopes.
/// What: Pulls the tool descriptors from `tool_descriptors()` (which
/// returns a bare JSON array, distinct from trusty-memory's
/// `{"tools": [...]}` envelope) and hands them to the shared
/// `trusty_mcp_core::openrpc::discover_response` builder with
/// `scopes_for_tool` as the scope resolver.
/// Test: `discover_response_lists_all_tools` and
/// `discover_response_x_scopes_present`.
pub fn build_discover_response(version: &str) -> Value {
    let defs = tool_descriptors();
    let empty: Vec<Value> = Vec::new();
    let tools: &[Value] = defs.as_array().map(|a| a.as_slice()).unwrap_or(&empty);
    discover_response("trusty-search-mcp", version, tools, scopes_for_tool)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tool_has_scopes() {
        let defs = tool_descriptors();
        let tools = defs.as_array().expect("tool_descriptors returns array");
        for tool in tools {
            let name = tool["name"].as_str().unwrap();
            let scopes = scopes_for_tool(name);
            assert!(
                !scopes.is_empty(),
                "tool {name} must declare at least one scope"
            );
        }
    }

    #[test]
    fn read_write_classification() {
        assert_eq!(scopes_for_tool("search"), vec!["search.read"]);
        assert_eq!(scopes_for_tool("search_all"), vec!["search.read"]);
        assert_eq!(scopes_for_tool("index_file"), vec!["search.write"]);
        assert_eq!(scopes_for_tool("delete_index"), vec!["search.write"]);
        assert_eq!(scopes_for_tool("list_indexes"), vec!["search.read"]);
        assert_eq!(scopes_for_tool("chat"), vec!["search.read"]);
    }

    #[test]
    fn discover_response_lists_all_tools() {
        let doc = build_discover_response("9.9.9");
        assert_eq!(doc["openrpc"], "1.3.2");
        assert_eq!(doc["info"]["title"], "trusty-search-mcp");
        assert_eq!(doc["info"]["version"], "9.9.9");
        let methods = doc["methods"].as_array().expect("methods array");
        let defs = tool_descriptors();
        let tool_count = defs.as_array().unwrap().len();
        assert_eq!(methods.len(), tool_count);
    }

    #[test]
    fn discover_response_x_scopes_present() {
        let doc = build_discover_response("0.1.0");
        let methods = doc["methods"].as_array().unwrap();
        for m in methods {
            let scopes = m["x-scopes"].as_array().expect("x-scopes array");
            assert!(
                !scopes.is_empty(),
                "method {} must carry at least one scope",
                m["name"]
            );
        }
    }
}
