//! JSON-RPC 2.0 request/response envelopes and the transport-agnostic
//! dispatcher that routes a parsed method name onto the underlying
//! [`AppState`] handlers.
//!
//! Why: The daemon used to expose its tool surface exclusively through
//! axum-bound REST routes (browser UI, hook CLIs) and a broken stdio MCP
//! mode whose redb opens collided with the running HTTP daemon's
//! exclusive locks. The fix is to keep the daemon as the single
//! redb-owning process and expose its surface through *multiple*
//! transports — HTTP `POST /rpc` for browser clients, a Unix domain
//! socket for low-overhead local clients (and the MCP stdio bridge).
//! This module is the shared spine: every transport parses bytes into a
//! [`JsonRpcRequest`], hands it to [`dispatch`], and writes a
//! [`JsonRpcResponse`] back on the wire.
//! What:
//!   - [`JsonRpcRequest`] / [`JsonRpcResponse`] / [`JsonRpcError`]: the
//!     envelope types matching the JSON-RPC 2.0 spec
//!     (`{"jsonrpc": "2.0", "id": ..., "method": ..., "params": ...}` →
//!     `{"jsonrpc": "2.0", "id": ..., "result": ...}` or `error`).
//!   - [`dispatch`]: async function that consumes a request and an
//!     `AppState` and returns the response. It routes `initialize`,
//!     `notifications/initialized`, `ping`, `rpc.discover`, `tools/list`,
//!     `tools/call`, `palace_*`, `memory_*`, `kg_*`, `add_alias`,
//!     `discover_aliases`, `get_prompt_context`, `list_prompt_facts`,
//!     `remove_prompt_fact`, `memory_send_message`, and `hook_fired` to the
//!     existing handlers in [`crate::tools`] / [`crate::lib`]. Unknown
//!     methods return `-32601 Method not found`.
//!
//! Test: see `dispatch_palace_list_returns_empty_array_initially`,
//!     `dispatch_unknown_method_returns_method_not_found`,
//!     `dispatch_hook_fired_emits_activity`, and the transport-level
//!     integration tests in `transport::http` and `transport::uds`.

use crate::{ActivitySource, AppState, DaemonEvent, HookType, InjectionKind};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use trusty_common::mcp::initialize_response;

/// JSON-RPC 2.0 standard error codes used by [`dispatch`].
///
/// Why: the spec assigns specific integers to specific failure modes
/// (parse errors, invalid request, method not found, invalid params,
/// internal error). Centralising them as constants prevents drift
/// between transports.
/// Test: `dispatch_unknown_method_returns_method_not_found` asserts
/// the value of [`METHOD_NOT_FOUND`].
pub mod error_codes {
    /// Invalid JSON was received (used by transport parsers, not
    /// [`super::dispatch`] which only sees already-parsed values).
    pub const PARSE_ERROR: i32 = -32700;
    /// The JSON sent is not a valid Request object.
    pub const INVALID_REQUEST: i32 = -32600;
    /// The method does not exist / is not available.
    pub const METHOD_NOT_FOUND: i32 = -32601;
    /// Invalid method parameter(s).
    pub const INVALID_PARAMS: i32 = -32602;
    /// Internal JSON-RPC error.
    pub const INTERNAL_ERROR: i32 = -32603;
}

/// JSON-RPC 2.0 request envelope.
///
/// Why: every transport (HTTP `POST /rpc`, UDS NDJSON, future stdio
/// without the bridge) speaks the same envelope shape, so the type lives
/// here and is reused by every parser.
/// What: serde-deserialised from the JSON wire format. `id` is optional
/// (notifications omit it); `params` is optional and defaults to
/// `Value::Null` on the dispatch path.
/// Test: `jsonrpc_request_round_trip`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    /// Protocol version. Always `"2.0"` for spec-compliant clients;
    /// `dispatch` does not enforce this so legacy clients that omit it
    /// still work.
    #[serde(default)]
    pub jsonrpc: Option<String>,
    /// Request identifier. Notifications (id absent or null) MUST NOT
    /// receive a response.
    #[serde(default)]
    pub id: Option<Value>,
    /// Method name (e.g. `palace_list`, `memory_recall`, `hook_fired`).
    pub method: String,
    /// Method arguments. Defaults to `Null` when omitted.
    #[serde(default)]
    pub params: Option<Value>,
}

/// JSON-RPC 2.0 response envelope.
///
/// Why: matches the spec so any compliant client (jsonrpc-client-rs,
/// jayson, hand-rolled `nc`-piped JSON) can talk to the daemon.
/// What: exactly one of `result` or `error` is present. `id` echoes the
/// request id (or is `Null` for parse errors that could not extract one).
/// Test: see helpers below — `ok()` / `err()` / `from_anyhow()`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    /// Always `"2.0"` for spec-compliant responses. Stored as
    /// owned `String` (rather than `&'static str`) so the type can be
    /// deserialised — borrowed-str fields would force every parsed
    /// response to share the lifetime of its source buffer.
    pub jsonrpc: String,
    pub id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

/// JSON-RPC 2.0 error object.
///
/// Why: standardised error shape lets transports surface the same
/// failure modes (parse error, method not found, invalid params, etc.)
/// without inventing per-transport encodings.
/// What: `code` follows the spec; `message` is human-readable; `data`
/// is optional structured detail.
/// Test: `dispatch_unknown_method_returns_method_not_found`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl JsonRpcResponse {
    /// Build a success response with the given result payload.
    ///
    /// Why: every dispatcher arm calls this on its happy path, so the
    /// envelope shape lives in one place.
    /// What: sets `jsonrpc = "2.0"`, `id` to the supplied value, and
    /// `result` to the payload.
    /// Test: covered by every successful dispatch test.
    pub fn ok(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: Some(result),
            error: None,
        }
    }

    /// Build an error response.
    ///
    /// Why: every dispatcher error path calls this so wire format stays
    /// consistent across transports.
    /// What: sets `jsonrpc = "2.0"`, `id`, and the error code + message.
    /// Test: `dispatch_unknown_method_returns_method_not_found`.
    pub fn err(id: Value, code: i32, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: None,
            error: Some(JsonRpcError {
                code,
                message: message.into(),
                data: None,
            }),
        }
    }

    /// Convert an `anyhow::Error` into a JSON-RPC internal-error response.
    ///
    /// Why: tool dispatch returns `anyhow::Result` (the existing
    /// `dispatch_tool` contract); the alternate `{:#}` format walks
    /// the full `Caused by:` chain so the wire surfaces actionable
    /// detail instead of just the outermost context.
    /// What: code = `INTERNAL_ERROR`, message = `format!("{e:#}")`.
    /// Test: covered indirectly when a tool call fails.
    pub fn from_anyhow(id: Value, e: anyhow::Error) -> Self {
        Self::err(id, error_codes::INTERNAL_ERROR, format!("{e:#}"))
    }
}

/// Methods that map directly onto [`crate::tools::dispatch_tool`].
///
/// Why: most of the surface (every `memory_*`, `palace_*`, `kg_*` tool)
/// is already implemented inside `dispatch_tool` with full validation
/// and creator-attribution wiring. Listing them here lets [`dispatch`]
/// forward in one match arm without duplicating the per-tool argument
/// parsing.
/// What: a sorted, exhaustive list of every method name routed through
/// `dispatch_tool`. Adding a tool requires extending this constant.
/// Test: `dispatch_palace_list_returns_empty_array_initially` confirms
/// the forwarding works end-to-end.
const TOOL_METHODS: &[&str] = &[
    "add_alias",
    "discover_aliases",
    "get_prompt_context",
    "kg_assert",
    "kg_bootstrap",
    "kg_gaps",
    "kg_query",
    "list_prompt_facts",
    "memory_forget",
    "memory_list",
    "memory_note",
    "memory_recall",
    "memory_recall_all",
    "memory_recall_deep",
    "memory_remember",
    "memory_send_message",
    "palace_compact",
    "palace_create",
    "palace_info",
    "palace_list",
    "remove_prompt_fact",
];

/// Hook-event payload (mirrors [`crate::hook_emit::HookEventPayload`]).
///
/// Why: parsed inline so the dispatcher does not need a special case
/// for hook events that already lives in the HTTP layer. Keeping the
/// shape identical to the HTTP `POST /api/v1/activity/hook` payload
/// lets the hook CLI helpers send the same JSON body to either
/// transport.
/// What: serde-deserialised from `params` on a `hook_fired` request.
/// Test: `dispatch_hook_fired_emits_activity`.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct HookFiredParams {
    #[serde(default)]
    palace_id: Option<String>,
    #[serde(default)]
    palace_name: Option<String>,
    hook_type: HookType,
    injection_kind: InjectionKind,
    #[serde(default)]
    injection_length: u64,
    #[serde(default)]
    trigger_prompt_excerpt: String,
    #[serde(default)]
    duration_ms: u64,
}

/// Dispatch a parsed JSON-RPC request against the daemon's shared state.
///
/// Why: this is the single transport-agnostic seam. HTTP, UDS, and any
/// future transport (gRPC, named pipes on Windows) all funnel through
/// this function. Concentrating the routing here means new tools
/// automatically light up on every transport.
/// What: routes by `req.method`. `initialize` returns the MCP capability
/// handshake required by Claude Code before it will mark the server as
/// connected. `notifications/initialized` and `notifications/cancelled`
/// are client-to-server notifications (no response per MCP spec). `ping`
/// returns `{}`. `rpc.discover` returns the OpenRPC document. `tools/list`
/// returns the MCP tool definitions. `tools/call` extracts `name` +
/// `arguments` from `params` (matching the MCP convention). Any method
/// listed in [`TOOL_METHODS`] is forwarded directly to
/// [`crate::tools::dispatch_tool`] with `params` as the argument object.
/// `hook_fired` emits a [`DaemonEvent::HookFired`] (the JSON-RPC equivalent
/// of `POST /api/v1/activity/hook`). Unknown methods return
/// [`error_codes::METHOD_NOT_FOUND`].
/// Test: see `dispatch_*` tests in this module, especially
/// `dispatch_initialize_returns_capabilities`.
pub async fn dispatch(state: &AppState, req: JsonRpcRequest) -> JsonRpcResponse {
    let id = req.id.clone().unwrap_or(Value::Null);
    let params = req.params.clone().unwrap_or(Value::Null);

    // Built-in protocol methods first — these never touch tool surface.
    match req.method.as_str() {
        // MCP lifecycle: Claude Code sends `initialize` first; without a
        // valid response the client marks the server as failed and refuses
        // to call any tools. `notifications/initialized` is a client
        // notification confirming it finished setup — per spec we MUST NOT
        // send a response (handled by the `is_notification` guard in the
        // UDS handler, but we also explicitly return Null here for safety).
        "initialize" => {
            let extra = state
                .default_palace
                .as_deref()
                .map(|p| json!({"default_palace": p}));
            let result = initialize_response("trusty-memory", &state.version, extra);
            return JsonRpcResponse::ok(id, result);
        }
        "notifications/initialized" | "notifications/cancelled" => {
            // Notifications must not receive a response (MCP spec §4.1).
            // Returning Null here is safe: the UDS handler suppresses the
            // response for any request whose id is absent or Null.
            return JsonRpcResponse::ok(Value::Null, Value::Null);
        }
        "ping" => return JsonRpcResponse::ok(id, json!({})),
        "rpc.discover" => {
            let result = crate::openrpc::build_discover_response(
                &state.version,
                state.default_palace.is_some(),
            );
            return JsonRpcResponse::ok(id, result);
        }
        "tools/list" => {
            let result = crate::tools::tool_definitions_with(state.default_palace.is_some());
            return JsonRpcResponse::ok(id, result);
        }
        "tools/call" => {
            // MCP-style `tools/call` request: params.name + params.arguments.
            let name = params
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let args = params.get("arguments").cloned().unwrap_or(Value::Null);
            return match crate::tools::dispatch_tool(state, &name, args).await {
                Ok(content) => {
                    // MCP convention: wrap the tool result in a content[0].text
                    // block so MCP clients can render it as plain text.
                    let text = match &content {
                        Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    JsonRpcResponse::ok(id, json!({"content": [{"type": "text", "text": text}]}))
                }
                Err(e) => JsonRpcResponse::from_anyhow(id, e),
            };
        }
        "hook_fired" => return dispatch_hook_fired(state, id, params),
        _ => {}
    }

    // Direct tool dispatch: every entry in TOOL_METHODS is forwarded
    // straight into `tools::dispatch_tool` with `params` as the args
    // object. Lets a CLI / curl client call `palace_list` directly
    // without wrapping it in a `tools/call` envelope.
    if TOOL_METHODS.contains(&req.method.as_str()) {
        return match crate::tools::dispatch_tool(state, &req.method, params).await {
            Ok(result) => JsonRpcResponse::ok(id, result),
            Err(e) => JsonRpcResponse::from_anyhow(id, e),
        };
    }

    JsonRpcResponse::err(
        id,
        error_codes::METHOD_NOT_FOUND,
        format!("Method not found: {}", req.method),
    )
}

/// Handle the `hook_fired` method.
///
/// Why: previously the hook ingest path was a bespoke HTTP route
/// (`POST /api/v1/activity/hook`); promoting it to a first-class JSON-RPC
/// method lets the same flow work over UDS (preferred for local hook
/// CLIs because it avoids the TCP-handshake overhead) and HTTP `POST
/// /rpc`. The HTTP route is kept for backwards compatibility.
/// What: parses the params into a [`HookFiredParams`], constructs a
/// [`DaemonEvent::HookFired`] with `source = ActivitySource::Hook`, and
/// emits it via `state.emit`. Returns `{"status": "ok"}` on success or
/// an invalid-params error on bad input.
/// Test: `dispatch_hook_fired_emits_activity`.
fn dispatch_hook_fired(state: &AppState, id: Value, params: Value) -> JsonRpcResponse {
    let parsed: HookFiredParams = match serde_json::from_value(params) {
        Ok(p) => p,
        Err(e) => {
            return JsonRpcResponse::err(
                id,
                error_codes::INVALID_PARAMS,
                format!("hook_fired: invalid params: {e}"),
            );
        }
    };
    state.emit(DaemonEvent::HookFired {
        palace_id: parsed.palace_id,
        palace_name: parsed.palace_name,
        hook_type: parsed.hook_type,
        injection_kind: parsed.injection_kind,
        injection_length: parsed.injection_length,
        trigger_prompt_excerpt: parsed.trigger_prompt_excerpt,
        timestamp: chrono::Utc::now(),
        duration_ms: parsed.duration_ms,
        source: ActivitySource::Hook,
    });
    JsonRpcResponse::ok(id, json!({"status": "ok"}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AppState;
    use serde_json::json;

    /// Build an `AppState` rooted at a fresh tempdir; the tempdir is
    /// leaked so it outlives the test process (tests are short).
    fn test_state() -> AppState {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        AppState::new(root)
    }

    /// Why: round-trips a request through serde to confirm the envelope
    /// shape matches the JSON-RPC 2.0 spec — `jsonrpc`, `id`, `method`,
    /// `params` all surface as expected.
    /// Test: serialise, deserialise, assert equality.
    #[test]
    fn jsonrpc_request_round_trip() {
        let req = JsonRpcRequest {
            jsonrpc: Some("2.0".to_string()),
            id: Some(json!(1)),
            method: "palace_list".to_string(),
            params: Some(json!({})),
        };
        let s = serde_json::to_string(&req).unwrap();
        let back: JsonRpcRequest = serde_json::from_str(&s).unwrap();
        assert_eq!(back.method, "palace_list");
        assert_eq!(back.id, Some(json!(1)));
    }

    /// Why: dispatching `palace_list` against a fresh `AppState` must
    /// return an empty array — the registry has no palaces yet but the
    /// call itself must succeed (not 404). Confirms the dispatcher
    /// reaches `tools::dispatch_tool`.
    /// What: build a state, dispatch, assert the response is `result`
    /// (not `error`) and the payload is an array.
    /// Test: this test.
    #[tokio::test]
    async fn dispatch_palace_list_returns_empty_array_initially() {
        let state = test_state();
        let req = JsonRpcRequest {
            jsonrpc: Some("2.0".to_string()),
            id: Some(json!(1)),
            method: "palace_list".to_string(),
            params: Some(json!({})),
        };
        let resp = dispatch(&state, req).await;
        assert!(resp.error.is_none(), "expected ok, got {:?}", resp.error);
        let result = resp.result.expect("result");
        // `palace_list` returns `{"palaces": [...]}`.
        let palaces = result["palaces"]
            .as_array()
            .expect("result.palaces must be an array");
        assert!(palaces.is_empty(), "fresh state must list zero palaces");
    }

    /// Why: spec compliance — unknown methods must return -32601.
    #[tokio::test]
    async fn dispatch_unknown_method_returns_method_not_found() {
        let state = test_state();
        let req = JsonRpcRequest {
            jsonrpc: Some("2.0".to_string()),
            id: Some(json!(7)),
            method: "definitely_not_a_real_method".to_string(),
            params: None,
        };
        let resp = dispatch(&state, req).await;
        assert!(resp.result.is_none());
        let err = resp.error.expect("error");
        assert_eq!(err.code, error_codes::METHOD_NOT_FOUND);
        assert!(err.message.contains("definitely_not_a_real_method"));
    }

    /// Why: `initialize` is the first method Claude Code sends over the UDS/
    /// bridge path. Without a valid response the MCP client marks the server
    /// as failed. This confirms the dispatcher routes `initialize` to
    /// `trusty_common::mcp::initialize_response` and returns the MCP
    /// capability shape Claude Code expects.
    /// What: dispatch an `initialize` request, assert the result carries
    /// `protocolVersion`, `capabilities.tools`, and `serverInfo.name`.
    /// Test: this test.
    #[tokio::test]
    async fn dispatch_initialize_returns_capabilities() {
        let state = test_state();
        let req = JsonRpcRequest {
            jsonrpc: Some("2.0".to_string()),
            id: Some(json!(1)),
            method: "initialize".to_string(),
            params: Some(json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "test", "version": "1.0"}
            })),
        };
        let resp = dispatch(&state, req).await;
        assert!(
            resp.error.is_none(),
            "initialize must not error: {:?}",
            resp.error
        );
        let result = resp.result.expect("result");
        assert_eq!(
            result["protocolVersion"], "2024-11-05",
            "must echo the negotiated protocol version"
        );
        assert!(
            result["capabilities"]["tools"].is_object(),
            "must advertise tools capability"
        );
        assert_eq!(
            result["serverInfo"]["name"], "trusty-memory",
            "serverInfo.name must be trusty-memory"
        );
    }

    /// Why: ping is a protocol-level keepalive; must return `{}` and
    /// echo the id.
    #[tokio::test]
    async fn dispatch_ping_returns_empty_object() {
        let state = test_state();
        let req = JsonRpcRequest {
            jsonrpc: Some("2.0".to_string()),
            id: Some(json!(42)),
            method: "ping".to_string(),
            params: None,
        };
        let resp = dispatch(&state, req).await;
        assert_eq!(resp.id, json!(42));
        assert_eq!(resp.result, Some(json!({})));
    }

    /// Why: `tools/list` must work via the new dispatcher (parity with
    /// the existing stdio MCP handler).
    #[tokio::test]
    async fn dispatch_tools_list_returns_tool_array() {
        let state = test_state();
        let req = JsonRpcRequest {
            jsonrpc: Some("2.0".to_string()),
            id: Some(json!(2)),
            method: "tools/list".to_string(),
            params: None,
        };
        let resp = dispatch(&state, req).await;
        let result = resp.result.expect("result");
        let tools = result["tools"].as_array().expect("tools array");
        assert!(!tools.is_empty());
    }

    /// Why: PR #144's `POST /api/v1/activity/hook` route is replaced by
    /// the `hook_fired` JSON-RPC method. A successful dispatch must
    /// append a row to the activity log (the persistence side-effect
    /// observable from outside the dispatcher).
    /// What: dispatch one `hook_fired` request, then count the activity
    /// log rows.
    /// Test: this test.
    #[tokio::test]
    async fn dispatch_hook_fired_emits_activity() {
        let state = test_state();
        let req = JsonRpcRequest {
            jsonrpc: Some("2.0".to_string()),
            id: Some(json!(3)),
            method: "hook_fired".to_string(),
            params: Some(json!({
                "palace_id": "p",
                "palace_name": "p",
                "hook_type": "UserPromptSubmit",
                "injection_kind": "prompt-context",
                "injection_length": 100,
                "trigger_prompt_excerpt": "test",
                "duration_ms": 5,
            })),
        };
        let resp = dispatch(&state, req).await;
        assert!(resp.error.is_none(), "expected ok, got {:?}", resp.error);
        // Issue #232: `emit` now fire-and-forgets the redb append on the
        // blocking pool; flush before observing the persisted count.
        state.flush_activity_writes().await;
        let count = state.activity_log.count().unwrap();
        assert_eq!(count, 1, "hook_fired must persist one activity row");
    }

    /// Why: malformed `hook_fired` params must return invalid-params
    /// rather than panicking or emitting a half-formed event.
    #[tokio::test]
    async fn dispatch_hook_fired_invalid_params_errors() {
        let state = test_state();
        let req = JsonRpcRequest {
            jsonrpc: Some("2.0".to_string()),
            id: Some(json!(4)),
            method: "hook_fired".to_string(),
            params: Some(json!({"wrong": "shape"})),
        };
        let resp = dispatch(&state, req).await;
        let err = resp.error.expect("error");
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
    }
}
