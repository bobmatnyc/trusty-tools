//! Index management tool arms: `index_file`, `remove_file`, `list_indexes`,
//! `create_index`, `delete_index`, `reindex`, `index_status`, `list_chunks`.
//!
//! Why: index lifecycle operations (register, populate, inspect, delete) form
//! a cohesive group that changes together when the daemon's index API evolves.
//! Keeping them separate from search and admin tools makes code review and
//! feature additions easier.
//! What: exports `dispatch_index_tool`, called from `call_tool` in `mod.rs`,
//! which routes the eight index-management tool names to their daemon endpoints.
//! Test: `tests.rs` — `missing_params_returns_invalid_params` and the
//! `tools/list` completeness tests cover all eight names.

use serde_json::Value;

use super::{
    types::{require_str, DispatchError},
    McpServer,
};

/// Route one of the eight index-management tool names to the correct daemon
/// call.
///
/// Why: grouping index management separately from search and admin lets each
/// file stay focused and under the 500-line cap.
/// What: returns `None` when `tool` is not an index-management tool (so
/// `call_tool` can try the next group), `Some(Ok(value))` on success, or
/// `Some(Err(DispatchError))` on failure.
/// Test: `tools_list_returns_all_tools` and `missing_params_returns_invalid_params`
/// in `tests.rs` exercise all returned tool names and error paths.
pub(super) async fn dispatch_index_tool(
    server: &McpServer,
    tool: &str,
    args: &Value,
) -> Option<Result<Value, DispatchError>> {
    match tool {
        "index_file" => {
            let index_id = match require_str(args, "index_id") {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let path = match require_str(args, "path") {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let content = match require_str(args, "content") {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            Some(
                server
                    .post(
                        &format!("/indexes/{index_id}/index-file"),
                        &serde_json::json!({ "path": path, "content": content }),
                    )
                    .await,
            )
        }
        "remove_file" => {
            let index_id = match require_str(args, "index_id") {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let path = match require_str(args, "path") {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            Some(
                server
                    .post(
                        &format!("/indexes/{index_id}/remove-file"),
                        &serde_json::json!({ "path": path }),
                    )
                    .await,
            )
        }
        // Issue #312: request details=true so the response includes
        // per-index size_bytes in addition to the id list.
        "list_indexes" => Some(server.get("/indexes?details=true").await),
        "create_index" => {
            let id = match require_str(args, "id") {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let root_path = match require_str(args, "root_path") {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            Some(
                server
                    .post(
                        "/indexes",
                        &serde_json::json!({ "id": id, "root_path": root_path }),
                    )
                    .await,
            )
        }
        "delete_index" => {
            let index_id = match require_str(args, "index_id") {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            Some(server.delete(&format!("/indexes/{index_id}")).await)
        }
        "reindex" => {
            let index_id = match require_str(args, "index_id") {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            // Accept optional root_path override (mirrors the HTTP body).
            let mut body = serde_json::json!({});
            if let Some(rp) = args.get("root_path").and_then(Value::as_str) {
                body["root_path"] = Value::String(rp.to_string());
            }
            Some(
                server
                    .post(&format!("/indexes/{index_id}/reindex"), &body)
                    .await,
            )
        }
        "index_status" => {
            let index_id = match require_str(args, "index_id") {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            Some(server.get(&format!("/indexes/{index_id}/status")).await)
        }
        "list_chunks" => {
            // Issue #54 — paginated enumeration of an index's corpus.
            // Mirrors `GET /indexes/:id/chunks?offset=&limit=`.
            let index_id = match require_str(args, "index_id") {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(0);
            let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(100);
            Some(
                server
                    .get(&format!(
                        "/indexes/{index_id}/chunks?offset={offset}&limit={limit}"
                    ))
                    .await,
            )
        }
        _ => None,
    }
}
