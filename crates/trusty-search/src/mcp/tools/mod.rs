//! MCP tool dispatcher: JSON-RPC 2.0 over a daemon HTTP back-end.
//!
//! Why: Claude Code speaks MCP/JSON-RPC; the trusty-search daemon speaks
//! REST. This module is a pure translator. It owns no state beyond a
//! `reqwest::Client` and a base URL, so the same dispatcher can be driven
//! from `stdio` (one process per session) or `sse` (long-lived axum task).
//!
//! What: [`McpServer::dispatch`] takes a [`Request`] and returns a
//! [`Response`]. Tool calls map 1:1 to daemon endpoints. Tool arms are
//! split across focused submodules:
//! - [`search`]      — `search`, `search_lexical`, `search_semantic`, `search_kg`,
//!   `search_all`, `search_similar`
//! - [`index`]       — `index_file`, `remove_file`, `list_indexes`,
//!   `create_index`, `delete_index`, `reindex`, `index_status`, `list_chunks`
//! - [`misc`]        — `search_health`, `chat`, `get_call_chain`, `grep`,
//!   `upgrade`
//! - [`descriptors`] — static `tool_descriptors()` for `tools/list`
//! - [`http`]        — shared HTTP transport helpers (`get`, `post`, `delete`, …)
//! - [`types`]       — `DispatchError`, `require_str`, response-wrapping helpers
//!
//! Test: `cargo test -p trusty-search` covers JSON-RPC parsing, error
//! shapes (-32600 invalid request, -32601 method not found, -32602 invalid
//! params), and dispatch routing without hitting a real daemon.

use serde_json::Value;

// JSON-RPC 2.0 primitives from the shared `trusty-common` crate.
// Re-exported here to keep `pub use` consumers (and `crate::mcp::tools::error_codes`
// etc.) working.
pub use trusty_common::mcp::{error_codes, initialize_response, JsonRpcError, Request, Response};

pub(crate) mod descriptors;
pub(crate) mod http;
pub(crate) mod index;
pub(crate) mod misc;
pub(crate) mod search;
pub(crate) mod types;

pub use descriptors::tool_descriptors;

use types::{
    wrap_stage_not_ready_error, wrap_text_content, wrap_tool_error, wrap_tool_result, DispatchError,
};

/// Application-level JSON-RPC error code surfaced when a per-lane MCP tool
/// (`search_semantic`, `search_kg`) is invoked but its prerequisite stage
/// is not yet `Ready` on the target index (issue #138).
///
/// Why: Falls inside the JSON-RPC 2.0 "server reserved" range (`-32099` ..
/// `-32000`) so it never collides with transport-level codes (parse error,
/// invalid request, method not found, invalid params, internal error). The
/// LLM and any orchestrator can branch on this code to retry against
/// `search_lexical` instead of asking the user.
/// What: a free integer constant; emitted on bare-method invocations. The
/// `tools/call` form surfaces the same condition via `_meta.error_code =
/// "STAGE_NOT_READY"` per MCP's in-band error convention.
/// Test: covered by `search_semantic_tool_returns_stage_not_ready_when_*`
/// and `search_kg_tool_returns_stage_not_ready_when_*` in `tests_lane.rs`.
pub const STAGE_NOT_READY_CODE: i32 = -32010;

/// Tool dispatcher backed by an HTTP client targeting the daemon.
///
/// Why: decouples the MCP wire protocol from the HTTP daemon API so the
/// same dispatcher can be used from both `stdio` and `sse` transports.
/// What: holds the daemon base URL and a `reqwest::Client`; `dispatch`
/// translates JSON-RPC requests into HTTP calls and wraps the response.
/// Test: instantiated in every test with a fake base URL or a local mock
/// daemon; the `with_client` constructor allows injecting a pre-built
/// client for connection pooling.
#[derive(Clone)]
pub struct McpServer {
    pub(crate) base_url: String,
    pub(crate) http: reqwest::Client,
}

impl McpServer {
    /// Construct a dispatcher pointing at the daemon's base URL
    /// (e.g. `http://127.0.0.1:7878`).
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            http: reqwest::Client::new(),
        }
    }

    /// Inject a pre-built reqwest client (useful for tests / pooling).
    pub fn with_client(base_url: impl Into<String>, http: reqwest::Client) -> Self {
        Self {
            base_url: base_url.into(),
            http,
        }
    }

    /// Daemon base URL.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Translate a JSON-RPC request into a daemon HTTP call and wrap the
    /// response.
    ///
    /// Why: all MCP clients go through a single entry point so protocol
    /// concerns (version check, notification suppression, `tools/call`
    /// envelope handling) are in one place.
    /// What: validates the `jsonrpc` field, handles MCP lifecycle methods
    /// (`initialize`, `notifications/initialized`), routes `tools/call` and
    /// `tools/list`, delegates bare tool names to `call_tool`, and maps
    /// `DispatchError` variants to the correct JSON-RPC or in-band error shape.
    /// Always returns a `Response`; transport / daemon failures are reported as
    /// `INTERNAL_ERROR` rather than panicking.
    /// Test: `rejects_wrong_jsonrpc_version`, `unknown_tool_returns_method_not_found`,
    /// `missing_params_returns_invalid_params`, `tools_list_returns_all_tools` in
    /// `tests.rs`.
    pub async fn dispatch(&self, req: Request) -> Response {
        let is_notification = req.id.is_none();
        let id = req.id.clone();

        if req.jsonrpc.as_deref() != Some("2.0") {
            if is_notification {
                return Response::suppressed();
            }
            return Response::err(id, error_codes::INVALID_REQUEST, "jsonrpc must be \"2.0\"");
        }

        // MCP lifecycle methods. `initialize` exchanges capabilities;
        // `notifications/initialized` confirms the client finished setup
        // and is silenced (per JSON-RPC 2.0 notification semantics — no
        // `id`, no reply).
        match req.method.as_str() {
            "initialize" => {
                return Response::ok(
                    id,
                    initialize_response("trusty-search", env!("CARGO_PKG_VERSION"), None),
                );
            }
            "notifications/initialized" | "initialized" => {
                return Response::suppressed();
            }
            _ => {}
        }

        // MCP "tools/call" wraps tool name + arguments. We also accept the
        // bare method name for ergonomics (`search` directly).
        let params = req.params.clone().unwrap_or(Value::Null);
        let (tool, arguments, via_tools_call) = match req.method.as_str() {
            "tools/call" => {
                let name = params
                    .get("name")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                let args = params
                    .get("arguments")
                    .cloned()
                    .unwrap_or(Value::Object(Default::default()));
                match name {
                    Some(n) => (n, args, true),
                    None => {
                        return Response::err(
                            id,
                            error_codes::INVALID_PARAMS,
                            "tools/call requires a 'name' field",
                        )
                    }
                }
            }
            "tools/list" => {
                return Response::ok(id, serde_json::json!({ "tools": tool_descriptors() }));
            }
            // OpenRPC 1.3.2 discovery — see `mcp::openrpc`. Returns the
            // full service description so orchestrators (open-mpm, etc.)
            // can introspect every tool and its required
            // `search.read`/`search.write` scope without bespoke adapters.
            "rpc.discover" => {
                return Response::ok(
                    id,
                    crate::mcp::openrpc::build_discover_response(env!("CARGO_PKG_VERSION")),
                );
            }
            other => (other.to_string(), params, false),
        };

        let outcome = self.call_tool(&tool, &arguments).await;

        if via_tools_call {
            // Per MCP spec, tool execution failures are reported in-band as
            // `{content: [...], isError: true}` rather than JSON-RPC errors —
            // the protocol-level error space is reserved for malformed
            // requests / unknown tools.
            match outcome {
                Ok(value) => Response::ok(id, wrap_tool_result(&value, false)),
                Err(DispatchError::UnknownTool) => Response::err(
                    id,
                    error_codes::METHOD_NOT_FOUND,
                    format!("unknown tool: {tool}"),
                ),
                Err(DispatchError::InvalidParams(msg)) => Response::ok(id, wrap_tool_error(&msg)),
                Err(DispatchError::Transport(msg)) => Response::ok(id, wrap_tool_error(&msg)),
                Err(DispatchError::StageNotReady {
                    message,
                    current_stages,
                    suggested_tools,
                }) => Response::ok(
                    id,
                    wrap_stage_not_ready_error(&message, &current_stages, &suggested_tools),
                ),
            }
        } else {
            match outcome {
                Ok(value) => Response::ok(id, wrap_text_content(&value)),
                Err(DispatchError::UnknownTool) => Response::err(
                    id,
                    error_codes::METHOD_NOT_FOUND,
                    format!("unknown tool: {tool}"),
                ),
                Err(DispatchError::InvalidParams(msg)) => {
                    Response::err(id, error_codes::INVALID_PARAMS, msg)
                }
                Err(DispatchError::Transport(msg)) => {
                    Response::err(id, error_codes::INTERNAL_ERROR, msg)
                }
                Err(DispatchError::StageNotReady {
                    message,
                    current_stages,
                    suggested_tools,
                }) => {
                    // Bare-method form has no `_meta` slot, so we surface
                    // the structured payload as the error `data` field and
                    // keep the message human-readable. JSON-RPC `code` -32010
                    // is in the server-defined range reserved for app-level
                    // semantics (issue #138).
                    let data = serde_json::json!({
                        "error_code": "STAGE_NOT_READY",
                        "current_stages": current_stages,
                        "suggested_tools": suggested_tools,
                    });
                    let mut resp = Response::err(id, STAGE_NOT_READY_CODE, message);
                    if let Some(ref mut e) = resp.error {
                        e.data = Some(data);
                    }
                    resp
                }
            }
        }
    }

    /// Route a tool name to the correct tool-group dispatcher.
    ///
    /// Why: splitting tool arms across `search`, `index`, and `misc` submodules
    /// keeps each file under the 500-line cap; `call_tool` is the thin
    /// router that tries each group in sequence.
    /// What: delegates to `dispatch_search_tool`, `dispatch_index_tool`, then
    /// `dispatch_misc_tool`; returns `DispatchError::UnknownTool` when no
    /// group claims the name.
    /// Test: all tool-dispatch tests in `tests.rs` and `tests_lane.rs` exercise
    /// this routing.
    async fn call_tool(&self, tool: &str, args: &Value) -> Result<Value, DispatchError> {
        if let Some(result) = search::dispatch_search_tool(self, tool, args).await {
            return result;
        }
        if let Some(result) = index::dispatch_index_tool(self, tool, args).await {
            return result;
        }
        if let Some(result) = misc::dispatch_misc_tool(self, tool, args).await {
            return result;
        }
        Err(DispatchError::UnknownTool)
    }
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_lane;
// Issue #138: tools/list completeness and per-lane dispatch validation.
#[cfg(test)]
mod tests_tools_list;
// Issue #882: empty-query MCP tests (separate file — line-cap budget).
#[cfg(test)]
mod tests_empty_query;
