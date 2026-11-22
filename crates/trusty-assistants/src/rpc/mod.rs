//! Unified `rpc.discover` for in-process MCP services (issue #460).
//!
//! Why: trusty-assistants hosts multiple MCP services in the same binary. Each linked
//! service implements `trusty_mcp_core::ServiceDescriptor` so the host can
//! collect them and emit one merged OpenRPC document. #11632: the `POST /rpc`
//! route that served it (`rpc_handler`) was cut with the HTTP API; the
//! document builder stays.
//!
//! What:
//!   - `build_unified_discovery()` collects the trusty-memory descriptor
//!     (`MemoryMcpService` from `trusty-memory-mcp`) and the trusty-search
//!     descriptor (`SearchMcpService` from `trusty-search`) and feeds them to
//!     `OpenRpcBuilder::from_services`. Each method ends up annotated with
//!     an `x-service` extension naming its owning service.
//!
//! Test: `tests::merged_doc_has_all_tools`,
//! `tests::every_method_has_x_service_annotation`,
//! `tests::rpc_discover_not_in_methods_list`.

use serde_json::Value;
use trusty_mcp::ServiceDescriptor;
use trusty_mcp::openrpc::OpenRpcBuilder;
use trusty_memory::MemoryMcpService;
use trusty_search::SearchMcpService;

/// Build the unified OpenRPC discovery document.
///
/// Why: Assembles the unified OpenRPC document covering every in-process MCP
/// service so trusty-assistants exposes one `rpc.discover` endpoint instead of forcing
/// clients to probe individual daemons.
/// What: Collects `ServiceDescriptor` impls for memory and search, delegates
/// to `OpenRpcBuilder::from_services`, returns the finalised OpenRPC `Value`.
/// Test: `tests::merged_doc_has_all_tools` asserts ≥ 26 methods (11 memory +
/// 15 search); `tests::every_method_has_x_service_annotation` asserts every
/// method carries `x-service`.
pub fn build_unified_discovery() -> Value {
    let memory = MemoryMcpService;
    let search = SearchMcpService;
    let services: Vec<&dyn ServiceDescriptor> = vec![&memory, &search];
    OpenRpcBuilder::from_services(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"), &services)
        .build()
}

// TODO(#460): in-process `tools/call` dispatch. The merged manifest already
// annotates every method with `x-service`; the dispatcher can use that field
// to route to either MemoryMcpService's tool runner (in trusty-memory-mcp) or
// SearchMcpService's tool runner (in trusty-search). Not blocking discovery,
// so deferred.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_service_tool_count() {
        let svc = SearchMcpService;
        // Why: trusty-assistants's service registry must advertise exactly the same
        //      tools that trusty-search exposes.  A count mismatch means the
        //      descriptor in `trusty-search/src/service/mcp_descriptor.rs`
        //      changed without a corresponding update here, which would cause
        //      silent tool-routing failures at runtime.
        // Source of truth: `SearchMcpService::tools()` delegates to
        //      `trusty_search::mcp::tools::tool_descriptors()`.  When that
        //      list changes, update this assertion and the comment below.
        // History: started at 12; issue #76 added `get_call_chain` (→13);
        //          the `/grep` MCP tool added another (→14 … →15); bundling
        //          trusty-bm25-daemon + trusty-embedderd surfaces via
        //          PR #190/#191 added 3 more (→18 — closes #174);
        //          issue #537 added the `upgrade` MCP tool (→19);
        //          issue #1104 added `console_metrics` (→20);
        //          issue #1557 added the `typeahead` MCP tool (→21);
        //          #9169 removed the uncalled `upgrade` tool (→20);
        //          #7434 added the `add_root` multi-root tool (→21);
        //          #10110 added the `remove_files` batch removal (→22);
        //          #10551 added the `embed_missing` resume (→23);
        //          #10531 added the `ensure_project` find-or-create (→24).
        assert_eq!(svc.tools().len(), 24);
    }

    #[test]
    fn search_service_scopes_for_read_tool() {
        let svc = SearchMcpService;
        // The hybrid-search read tool is named `search` (with `search_all` as
        // its all-indexes variant); both map to the read scope.
        assert_eq!(svc.scopes_for("search"), vec!["search.read"]);
    }

    #[test]
    fn search_service_scopes_for_write_tool() {
        let svc = SearchMcpService;
        assert_eq!(svc.scopes_for("index_file"), vec!["search.write"]);
    }

    #[test]
    fn search_service_unknown_tool_has_no_scope() {
        let svc = SearchMcpService;
        assert!(svc.scopes_for("nonexistent_tool").is_empty());
    }

    #[test]
    fn merged_doc_has_all_tools() {
        let doc = build_unified_discovery();
        let methods = doc
            .get("methods")
            .and_then(|v| v.as_array())
            .expect("methods array");
        // 11 memory tools + 15 search tools = 26 minimum.
        assert!(
            methods.len() >= 26,
            "expected ≥ 26 merged methods, got {} — services: {:?}",
            methods.len(),
            methods
                .iter()
                .map(|m| m.get("name").and_then(|n| n.as_str()).unwrap_or("?"))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn every_method_has_x_service_annotation() {
        let doc = build_unified_discovery();
        let methods = doc
            .get("methods")
            .and_then(|v| v.as_array())
            .expect("methods array");
        for m in methods {
            let svc = m
                .get("x-service")
                .and_then(|v| v.as_str())
                .expect("every method must carry x-service");
            assert!(
                svc == "trusty-memory" || svc == "trusty-search",
                "unexpected x-service value: {svc}"
            );
        }
    }

    #[test]
    fn rpc_discover_not_in_methods_list() {
        // Why: rpc.discover is meta (the discovery operation itself), not a
        //      tool that clients can call. It must not appear in `methods`.
        let doc = build_unified_discovery();
        let methods = doc
            .get("methods")
            .and_then(|v| v.as_array())
            .expect("methods array");
        for m in methods {
            let name = m.get("name").and_then(|v| v.as_str()).unwrap_or("");
            assert_ne!(name, "rpc.discover");
        }
    }
}
