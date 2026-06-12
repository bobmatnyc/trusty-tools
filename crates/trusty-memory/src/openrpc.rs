//! OpenRPC 1.3.2 service description for `trusty-memory-mcp`.
//!
//! Why: Orchestrators such as open-mpm need a machine-readable manifest of
//! every memory tool the server exposes — including the logical scopes
//! (`memory.read` / `memory.write`) each tool requires — so they can route
//! tasks and enforce per-tool authorisation without bespoke per-server
//! adapters. OpenRPC's `rpc.discover` method is the standard JSON-RPC 2.0
//! discovery surface; no new transport is needed.
//!
//! What: A thin wrapper around `trusty_mcp_core::openrpc::discover_response`
//! that supplies the 11 trusty-memory tools and the per-tool scope mapping.
//! Read-only tools (lookups, queries, info) require `memory.read`; tools
//! that mutate palace state require `memory.write`.
//!
//! Test: `cargo test -p trusty-memory-mcp openrpc` verifies the envelope
//! shape, that every tool gets a non-empty scope list, and that
//! read/write classification is correct.

use serde_json::Value;
use trusty_common::mcp::openrpc::discover_response;

use crate::tools::tool_definitions_with;

/// Logical scopes exposed by the trusty-memory MCP server.
///
/// Why: A central enum-like constant block keeps scope literals in one
/// place; orchestrators key off these exact strings.
/// What: Two scopes — read for non-mutating tools, write for tools that
/// create, mutate, or delete palace data.
/// Test: `every_tool_has_scopes`.
mod scopes {
    pub const MEMORY_READ: &str = "memory.read";
    pub const MEMORY_WRITE: &str = "memory.write";
    /// Why (issue #60): `kg_bootstrap` writes triples derived from the
    /// project filesystem, not just the palace's own data. Operators may
    /// want to authorise it separately from generic memory.write tools
    /// (which only mutate the palace). The dedicated `knowledge.write`
    /// scope makes that distinction explicit.
    pub const KNOWLEDGE_WRITE: &str = "knowledge.write";
}

/// Return the logical scopes a given memory tool requires.
///
/// Why: open-mpm and similar orchestrators need to know whether a tool
/// mutates state so they can enforce least-privilege auth before
/// dispatching the call.
/// What: Read-only tools → `["memory.read"]`; mutating tools →
/// `["memory.write"]`. Unknown names return an empty slice so future tool
/// additions fail open with no scope rather than panicking; the unit test
/// asserts every currently registered tool maps to a non-empty set.
/// Test: `every_tool_has_scopes` and `read_write_classification`.
pub fn scopes_for_tool(name: &str) -> Vec<String> {
    use scopes::*;
    let s: &[&str] = match name {
        // Read-only / query
        "memory_recall" | "memory_recall_deep" | "memory_recall_all" | "memory_list"
        | "palace_list" | "palace_info" | "kg_query" | "kg_gaps" | "list_prompt_facts"
        | "get_prompt_context" | "console_metrics" => &[MEMORY_READ],

        // Mutating
        "memory_remember"
        | "memory_note"
        | "memory_forget"
        | "palace_create"
        | "palace_delete"
        | "palace_update"
        | "palace_compact"
        | "kg_assert"
        | "add_alias"
        | "remove_prompt_fact"
        | "discover_aliases"
        | "memory_send_message" => &[MEMORY_WRITE],

        // Bootstrap mutates the KG from external project files; it belongs
        // in the dedicated knowledge.write scope (issue #60).
        "kg_bootstrap" => &[KNOWLEDGE_WRITE],

        // Upgrade: requires admin-level write access because it modifies the
        // installed binary and may restart the daemon.
        "upgrade" => &[MEMORY_WRITE],

        _ => &[],
    };
    s.iter().map(|x| (*x).to_string()).collect()
}

/// Build the OpenRPC `rpc.discover` response for this server.
///
/// Why: Produces the JSON value used as the `result` of an
/// `rpc.discover` JSON-RPC response so any compliant client can introspect
/// every method, its parameters, and its required scopes.
/// What: Pulls the full tool definition list from `tool_definitions_with` and
/// hands them to the shared `trusty_mcp_core::openrpc::discover_response`
/// builder with `scopes_for_tool` as the scope resolver. `has_default`
/// must match the runtime state so the emitted `required` arrays line up
/// with what the live server accepts.
/// Test: `discover_response_lists_all_tools` and
/// `discover_response_x_scopes_present`.
pub fn build_discover_response(version: &str, has_default: bool) -> Value {
    let defs = tool_definitions_with(has_default);
    let empty: Vec<Value> = Vec::new();
    let tools: &[Value] = defs["tools"]
        .as_array()
        .map(|a| a.as_slice())
        .unwrap_or(&empty);
    discover_response("trusty-memory-mcp", version, tools, scopes_for_tool)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tool_has_scopes() {
        let defs = tool_definitions_with(false);
        let tools = defs["tools"].as_array().expect("tools array");
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
        assert_eq!(scopes_for_tool("memory_recall"), vec!["memory.read"]);
        assert_eq!(scopes_for_tool("memory_remember"), vec!["memory.write"]);
        assert_eq!(scopes_for_tool("kg_query"), vec!["memory.read"]);
        assert_eq!(scopes_for_tool("kg_assert"), vec!["memory.write"]);
        assert_eq!(scopes_for_tool("palace_compact"), vec!["memory.write"]);
        assert_eq!(scopes_for_tool("palace_info"), vec!["memory.read"]);
    }

    #[test]
    fn discover_response_lists_all_tools() {
        let doc = build_discover_response("9.9.9", false);
        assert_eq!(doc["openrpc"], "1.3.2");
        assert_eq!(doc["info"]["title"], "trusty-memory-mcp");
        assert_eq!(doc["info"]["version"], "9.9.9");
        let methods = doc["methods"].as_array().expect("methods array");
        let defs = tool_definitions_with(false);
        let tool_count = defs["tools"].as_array().unwrap().len();
        assert_eq!(methods.len(), tool_count);
    }

    #[test]
    fn discover_response_x_scopes_present() {
        let doc = build_discover_response("0.1.0", false);
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
