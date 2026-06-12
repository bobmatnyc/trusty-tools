//! Unified `rpc.discover` for in-process MCP services (issue #460).
//!
//! Why: trusty-agents hosts multiple MCP services in the same binary. Each linked
//! service implements `trusty_mcp_core::ServiceDescriptor` so the host can
//! collect them and emit one merged OpenRPC document for the `POST /rpc`
//! endpoint. Clients then hit a single discovery URL instead of probing
//! per-service daemons.
//!
//! What:
//!   - `build_unified_discovery()` collects the trusty-memory descriptor
//!     (`MemoryMcpService` from `trusty-memory-mcp`) and the trusty-search
//!     descriptor (`SearchMcpService` from `trusty-search`) and feeds them to
//!     `OpenRpcBuilder::from_services`. Each method ends up annotated with
//!     an `x-service` extension naming its owning service.
//!   - `rpc_handler` is an axum handler that dispatches `rpc.discover` via
//!     `build_unified_discovery()` and returns `-32601 Method not found`
//!     for anything else (in-process `tools/call` proxying is tracked
//!     separately — see TODO at the bottom of this file).
//!
//! Test: `tests::merged_doc_has_all_tools`,
//! `tests::every_method_has_x_service_annotation`,
//! `tests::rpc_discover_not_in_methods_list`.

use axum::Json;
use serde_json::{Value, json};
use trusty_common::mcp::ServiceDescriptor;
use trusty_common::mcp::openrpc::OpenRpcBuilder;
use trusty_memory::MemoryMcpService;
use trusty_search::SearchMcpService;

/// Build the unified OpenRPC discovery document.
///
/// Why: Assembles the unified OpenRPC document covering every in-process MCP
/// service so trusty-agents exposes one `rpc.discover` endpoint instead of forcing
/// clients to probe individual daemons.
/// What: Collects `ServiceDescriptor` impls for memory and search, delegates
/// to `OpenRpcBuilder::from_services`, returns the finalised OpenRPC `Value`.
/// Test: `tests::merged_doc_has_all_tools` asserts ≥ 25 methods (11 memory +
/// 14 search); `tests::every_method_has_x_service_annotation` asserts every
/// method carries `x-service`.
pub fn build_unified_discovery() -> Value {
    let memory = MemoryMcpService;
    let search = SearchMcpService;
    let services: Vec<&dyn ServiceDescriptor> = vec![&memory, &search];
    OpenRpcBuilder::from_services("trusty-agents", env!("CARGO_PKG_VERSION"), &services).build()
}

/// JSON-RPC handler for `POST /rpc`.
///
/// Why: Single entry point for OpenRPC clients. For now only `rpc.discover`
/// is implemented; in-process `tools/call` dispatch is tracked as future
/// work (the per-service tool execution paths already exist but aren't yet
/// surfaced through this endpoint).
/// What: Parses the JSON-RPC envelope, switches on `method`, returns either
/// the discovery doc or `-32601 Method not found`.
/// Test: `tests::handler_returns_discover_doc_with_jsonrpc_envelope`.
pub async fn rpc_handler(Json(req): Json<Value>) -> Json<Value> {
    let method = req.get("method").and_then(|v| v.as_str()).unwrap_or("");
    let id = req.get("id").cloned().unwrap_or(Value::Null);
    match method {
        "rpc.discover" => Json(json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": build_unified_discovery(),
        })),
        _ => Json(json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {
                "code": -32601,
                "message": "Method not found"
            }
        })),
    }
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
        // Why: trusty-agents's service registry must advertise exactly the same
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
        //          issue #1104 added `console_metrics` (→20).
        assert_eq!(svc.tools().len(), 20);
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
        // 11 memory tools + 14 search tools = 25 minimum.
        assert!(
            methods.len() >= 25,
            "expected ≥ 25 merged methods, got {} — services: {:?}",
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

    #[tokio::test]
    async fn handler_returns_discover_doc_with_jsonrpc_envelope() {
        let req = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "rpc.discover"
        });
        let Json(resp) = rpc_handler(Json(req)).await;
        assert_eq!(resp["jsonrpc"], "2.0");
        assert_eq!(resp["id"], 1);
        assert!(resp["result"]["methods"].is_array());
    }

    #[tokio::test]
    async fn handler_returns_method_not_found_for_unknown_method() {
        let req = json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "totally.unknown"
        });
        let Json(resp) = rpc_handler(Json(req)).await;
        assert_eq!(resp["error"]["code"], -32601);
    }
}
