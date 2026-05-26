//! `ServiceDescriptor` impl for the trusty-memory MCP service.
//!
//! Why: open-mpm (and any future host process that links several MCP
//! services into a single binary) needs a uniform way to enumerate every
//! tool a linked service contributes and the scopes each tool requires.
//! The shared `trusty_mcp_core::ServiceDescriptor` trait is that contract:
//! the host collects `&dyn ServiceDescriptor` impls and feeds them to
//! `OpenRpcBuilder::from_services` to emit one merged `rpc.discover`
//! document covering all of them. By implementing the trait here, the
//! host no longer needs to know anything concrete about trusty-memory.
//!
//! What: A zero-sized `MemoryMcpService` struct that delegates to the
//! existing `tool_definitions_with` and `scopes_for_tool` helpers already
//! used by this crate's standalone `rpc.discover` handler. Reusing those
//! functions guarantees the host-merged manifest and the standalone one
//! stay byte-identical.
//!
//! Test: `tests` below assert the tool count (23), the name string, and
//! that the read/write scope split matches what `scopes_for_tool` returns
//! for representative tools (`memory_recall` → `memory.read`,
//! `memory_remember` → `memory.write`).

use trusty_common::mcp::ServiceDescriptor;

use crate::openrpc::scopes_for_tool;
use crate::tools::tool_definitions_with;

/// `ServiceDescriptor` impl that advertises this crate's 23 memory tools.
///
/// Why: Lets open-mpm link trusty-memory-mcp directly and include its
/// tools in a unified `rpc.discover` document without bespoke glue code.
/// What: Wraps the existing tool definitions and the per-tool scope
/// mapping behind the shared trait. The struct is unit-like — there is
/// no per-instance state — so callers can construct it with
/// `MemoryMcpService` at the call site.
/// Test: `tests::tools_returns_eleven`, `tests::scopes_for_read_tool`,
/// `tests::scopes_for_write_tool`, `tests::name_returns_trusty_memory`.
#[derive(Debug, Default, Clone, Copy)]
pub struct MemoryMcpService;

impl ServiceDescriptor for MemoryMcpService {
    fn name(&self) -> &str {
        "trusty-memory"
    }

    fn version(&self) -> &str {
        env!("CARGO_PKG_VERSION")
    }

    fn tools(&self) -> Vec<serde_json::Value> {
        // Why: `tool_definitions_with(false)` matches the schema clients see
        //      when no default palace is configured — the conservative shape
        //      where `palace` is a required argument on every palace-scoped
        //      tool. The host can override later if it wants to surface the
        //      `has_default = true` variant.
        let defs = tool_definitions_with(false);
        defs.get("tools")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default()
    }

    fn scopes_for(&self, tool: &str) -> Vec<String> {
        scopes_for_tool(tool)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_returns_trusty_memory() {
        let svc = MemoryMcpService;
        assert_eq!(svc.name(), "trusty-memory");
    }

    #[test]
    fn version_matches_cargo_pkg_version() {
        let svc = MemoryMcpService;
        assert_eq!(svc.version(), env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn tools_returns_expected_count() {
        let svc = MemoryMcpService;
        let tools = svc.tools();
        assert_eq!(
            tools.len(),
            23,
            "expected 23 memory tools, got {}",
            tools.len()
        );
    }

    #[test]
    fn scopes_for_read_tool() {
        let svc = MemoryMcpService;
        assert_eq!(svc.scopes_for("memory_recall"), vec!["memory.read"]);
    }

    #[test]
    fn scopes_for_write_tool() {
        let svc = MemoryMcpService;
        assert_eq!(svc.scopes_for("memory_remember"), vec!["memory.write"]);
    }

    #[test]
    fn dispatches_through_trait_object() {
        // Why: open-mpm collects services as `Vec<Box<dyn ServiceDescriptor>>`,
        //      so we must confirm dynamic dispatch resolves correctly here.
        let svc: Box<dyn ServiceDescriptor> = Box::new(MemoryMcpService);
        assert_eq!(svc.name(), "trusty-memory");
        assert_eq!(svc.tools().len(), 23);
        assert_eq!(svc.scopes_for("palace_create"), vec!["memory.write"]);
        assert_eq!(svc.scopes_for("palace_delete"), vec!["memory.write"]);
        assert_eq!(svc.scopes_for("palace_update"), vec!["memory.write"]);
        assert_eq!(svc.scopes_for("palace_list"), vec!["memory.read"]);
    }
}
