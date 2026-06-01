//! ServiceDescriptor implementation for trusty-search.
//!
//! Why: open-mpm (and any future unified host process) needs to link
//! trusty-search directly as a library and include its tools in a single
//! merged `rpc.discover` document. The `trusty_mcp_core::ServiceDescriptor`
//! trait is the cross-service registration contract; implementing it lets
//! the host collect `&dyn ServiceDescriptor` for every linked service
//! without knowing about each one concretely. Closes trusty-search#115.
//!
//! What: A zero-sized `SearchMcpService` struct that delegates to the
//! existing `crate::mcp::tools::tool_descriptors` (14 MCP tool definitions
//! as of issue #76's `get_call_chain`) and
//! `crate::mcp::openrpc::scopes_for_tool` (read/write scope mapping), so
//! there is exactly one source of truth for the tool surface.
//!
//! Test: see `tests` below — verifies `name`, `tools().len() == 14`, and
//! that scope mappings match for both read and write tools.

use trusty_common::mcp::ServiceDescriptor;

/// Why: implements `ServiceDescriptor` so open-mpm can link trusty-search
/// directly and include its tools in a unified `rpc.discover` document.
/// What: wraps the existing MCP tool descriptors and scope mapping; holds
/// no state.
/// Test: tests below verify all 14 tools are present and scopes correct.
#[derive(Debug, Default, Clone, Copy)]
pub struct SearchMcpService;

impl SearchMcpService {
    /// Construct a new descriptor. Zero-sized; cheap to call.
    pub const fn new() -> Self {
        Self
    }
}

impl ServiceDescriptor for SearchMcpService {
    fn name(&self) -> &str {
        "trusty-search"
    }

    fn version(&self) -> &str {
        env!("CARGO_PKG_VERSION")
    }

    fn tools(&self) -> Vec<serde_json::Value> {
        // Reuse the canonical tool_descriptors() so the descriptor and the
        // `tools/list` JSON-RPC response can never drift apart.
        match crate::mcp::tools::tool_descriptors() {
            serde_json::Value::Array(items) => items,
            // tool_descriptors() always returns a JSON array; treat anything
            // else as an empty surface rather than panicking.
            _ => Vec::new(),
        }
    }

    fn scopes_for(&self, tool: &str) -> Vec<String> {
        crate::mcp::openrpc::scopes_for_tool(tool)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_is_trusty_search() {
        assert_eq!(SearchMcpService.name(), "trusty-search");
    }

    #[test]
    fn version_matches_cargo_pkg_version() {
        assert_eq!(SearchMcpService::new().version(), env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn tools_returns_exactly_eighteen() {
        // Issue #76 added `get_call_chain` (→ 14); the grep MCP tool brought
        // the total to 15. Issue #138 added the per-lane search tools
        // (`search_lexical`, `search_semantic`, `search_kg`) for an updated
        // total of 18. `search_all` was already counted in the pre-#138 15.
        let tools = SearchMcpService.tools();
        assert_eq!(
            tools.len(),
            19,
            "expected 19 MCP tools (including upgrade), got {}: {:?}",
            tools.len(),
            tools
                .iter()
                .map(|t| t.get("name").and_then(|n| n.as_str()).unwrap_or("?"))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn every_tool_has_non_empty_scopes() {
        let svc = SearchMcpService;
        for tool in svc.tools() {
            let name = tool
                .get("name")
                .and_then(|n| n.as_str())
                .expect("tool descriptor missing name");
            assert!(
                !svc.scopes_for(name).is_empty(),
                "tool {name} must have at least one scope"
            );
        }
    }

    #[test]
    fn read_tool_scope_mapping() {
        assert_eq!(
            SearchMcpService.scopes_for("search"),
            vec!["search.read".to_string()]
        );
    }

    #[test]
    fn write_tool_scope_mapping() {
        assert_eq!(
            SearchMcpService.scopes_for("index_file"),
            vec!["search.write".to_string()]
        );
    }

    #[test]
    fn trait_object_dispatch() {
        // Why: open-mpm collects services as `Box<dyn ServiceDescriptor>`;
        // make sure SearchMcpService is object-safe and dispatches correctly.
        // Issue #138: total bumped from 15 → 18 with the per-lane search
        // tools.
        let svc: Box<dyn ServiceDescriptor> = Box::new(SearchMcpService);
        assert_eq!(svc.name(), "trusty-search");
        assert_eq!(svc.tools().len(), 19);
    }
}
