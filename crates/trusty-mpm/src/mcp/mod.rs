//! # trusty-mpm-mcp
//!
//! Why: Claude Code sessions (and their subagents) need to talk to the
//! trusty-mpm daemon directly — to enumerate sibling sessions, request agent
//! delegations, protect their own context window, and inspect circuit-breaker
//! state. MCP is the protocol Claude Code already speaks, so trusty-mpm exposes
//! an MCP server rather than inventing a bespoke channel.
//!
//! What: defines the six orchestration tools (`session_list`, `session_status`,
//! `agent_delegate`, `memory_protect`, `circuit_breaker_status`, `hook_event`),
//! the [`OrchestratorBackend`] trait the daemon implements to service them, and
//! [`dispatch`], which routes a JSON-RPC [`Request`] to the backend. The daemon
//! wires [`dispatch`] into `trusty_mcp_core::run_stdio_loop`.
//!
//! Test: `cargo test -p trusty-mpm-mcp` exercises the tool catalog, argument
//! parsing, and dispatch against an in-memory mock backend.

use async_trait::async_trait;
use serde_json::{Value, json};
use trusty_common::mcp::{Request, Response, error_codes};

pub mod tools;

pub use tools::{TOOL_CATALOG, tool_catalog};

/// Server identity reported in the MCP `initialize` handshake.
pub const SERVER_NAME: &str = "trusty-mpm";

/// Server version reported in the MCP `initialize` handshake.
pub const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Orchestration services the MCP layer exposes to Claude Code.
///
/// Why: the MCP crate must stay free of daemon internals (process spawning,
/// tmux, sockets) so it is unit-testable; the daemon supplies a concrete impl.
/// This is the Dependency Inversion seam between protocol and orchestration.
/// What: one async method per MCP tool. Each takes already-parsed arguments
/// and returns a JSON result or an error message.
/// Test: `tests` module provides a `MockBackend` impl driven by `dispatch`.
#[async_trait]
pub trait OrchestratorBackend: Send + Sync {
    /// Back `session_list`: return a JSON array of session summaries.
    async fn session_list(&self) -> Result<Value, String>;

    /// Back `session_status`: return a detailed status object for `session_id`.
    async fn session_status(&self, session_id: &str) -> Result<Value, String>;

    /// Back `agent_delegate`: request a delegation to `agent` with `task`,
    /// optionally on an explicit model `tier`. Returns the new delegation id.
    async fn agent_delegate(
        &self,
        session_id: &str,
        agent: &str,
        task: &str,
        tier: Option<&str>,
    ) -> Result<Value, String>;

    /// Back `memory_protect`: report (and optionally act on) context-window
    /// pressure for `session_id` given `used`/`window` token counts.
    async fn memory_protect(
        &self,
        session_id: &str,
        used_tokens: u64,
        window_tokens: u64,
    ) -> Result<Value, String>;

    /// Back `circuit_breaker_status`: return the breaker state for `agent`
    /// (or all agents when `agent` is `None`).
    async fn circuit_breaker_status(&self, agent: Option<&str>) -> Result<Value, String>;

    /// Back `hook_event`: ingest a Claude Code hook event into the daemon's
    /// observability pipeline. Returns an acknowledgement.
    async fn hook_event(
        &self,
        session_id: &str,
        event: &str,
        payload: Value,
    ) -> Result<Value, String>;
}

/// Route a JSON-RPC request to the backend, returning the MCP response.
///
/// Why: the daemon's stdio loop needs one entry point that handles the MCP
/// handshake (`initialize`), tool discovery (`tools/list`), and tool calls
/// (`tools/call`) uniformly.
/// What: matches on `req.method`; for `tools/call` it extracts the tool name
/// and `arguments` object and forwards to the matching backend method.
/// Notifications (no id) are suppressed per JSON-RPC.
/// Test: `dispatch_*` tests cover initialize, listing, each tool, and errors.
pub async fn dispatch<B: OrchestratorBackend>(backend: &B, req: Request) -> Response {
    let id = req.id.clone();

    match req.method.as_str() {
        "initialize" => Response::ok(
            id,
            trusty_common::mcp::initialize_response(SERVER_NAME, SERVER_VERSION, None),
        ),
        "tools/list" => Response::ok(id, json!({ "tools": tool_catalog() })),
        "tools/call" => dispatch_tool_call(backend, id, req.params).await,
        "ping" => Response::ok(id, json!({})),
        // Notifications carry no id and must not produce a reply.
        _ if id.is_none() => Response::suppressed(),
        other => Response::err(
            id,
            error_codes::METHOD_NOT_FOUND,
            format!("unknown method: {other}"),
        ),
    }
}

/// Handle a `tools/call` request: pick the tool, parse args, call the backend.
async fn dispatch_tool_call<B: OrchestratorBackend>(
    backend: &B,
    id: Option<Value>,
    params: Option<Value>,
) -> Response {
    let params = params.unwrap_or(Value::Null);
    let name = match params.get("name").and_then(Value::as_str) {
        Some(n) => n,
        None => {
            return Response::err(
                id,
                error_codes::INVALID_PARAMS,
                "tools/call requires a `name` field",
            );
        }
    };
    let args = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));

    let result = match name {
        "session_list" => backend.session_list().await,
        "session_status" => match required_str(&args, "session_id") {
            Ok(sid) => backend.session_status(&sid).await,
            Err(e) => Err(e),
        },
        "agent_delegate" => {
            match (
                required_str(&args, "session_id"),
                required_str(&args, "agent"),
                required_str(&args, "task"),
            ) {
                (Ok(sid), Ok(agent), Ok(task)) => {
                    let tier = args.get("tier").and_then(Value::as_str);
                    backend.agent_delegate(&sid, &agent, &task, tier).await
                }
                (Err(e), _, _) | (_, Err(e), _) | (_, _, Err(e)) => Err(e),
            }
        }
        "memory_protect" => {
            match (
                required_str(&args, "session_id"),
                required_u64(&args, "used_tokens"),
                required_u64(&args, "window_tokens"),
            ) {
                (Ok(sid), Ok(used), Ok(window)) => backend.memory_protect(&sid, used, window).await,
                (Err(e), _, _) | (_, Err(e), _) | (_, _, Err(e)) => Err(e),
            }
        }
        "circuit_breaker_status" => {
            let agent = args.get("agent").and_then(Value::as_str);
            backend.circuit_breaker_status(agent).await
        }
        "hook_event" => {
            match (
                required_str(&args, "session_id"),
                required_str(&args, "event"),
            ) {
                (Ok(sid), Ok(event)) => {
                    let payload = args.get("payload").cloned().unwrap_or(Value::Null);
                    backend.hook_event(&sid, &event, payload).await
                }
                (Err(e), _) | (_, Err(e)) => Err(e),
            }
        }
        other => Err(format!("unknown tool: {other}")),
    };

    match result {
        // MCP wraps tool results in a `content` array of typed blocks.
        Ok(value) => Response::ok(
            id,
            json!({
                "content": [{ "type": "text", "text": value.to_string() }],
                "isError": false,
            }),
        ),
        Err(message) => Response::ok(
            id,
            json!({
                "content": [{ "type": "text", "text": message }],
                "isError": true,
            }),
        ),
    }
}

/// Extract a required string argument or produce a descriptive error.
fn required_str(args: &Value, key: &str) -> Result<String, String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| format!("missing required string argument: `{key}`"))
}

/// Extract a required unsigned-integer argument or produce a descriptive error.
fn required_u64(args: &Value, key: &str) -> Result<u64, String> {
    args.get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("missing required integer argument: `{key}`"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// In-memory backend that records calls and returns canned values.
    struct MockBackend;

    #[async_trait]
    impl OrchestratorBackend for MockBackend {
        async fn session_list(&self) -> Result<Value, String> {
            Ok(json!([{ "id": "s1", "status": "active" }]))
        }
        async fn session_status(&self, session_id: &str) -> Result<Value, String> {
            Ok(json!({ "id": session_id, "status": "active" }))
        }
        async fn agent_delegate(
            &self,
            _session_id: &str,
            agent: &str,
            _task: &str,
            _tier: Option<&str>,
        ) -> Result<Value, String> {
            Ok(json!({ "delegation_id": "d1", "agent": agent }))
        }
        async fn memory_protect(
            &self,
            _session_id: &str,
            used_tokens: u64,
            window_tokens: u64,
        ) -> Result<Value, String> {
            Ok(json!({ "fraction": used_tokens as f64 / window_tokens as f64 }))
        }
        async fn circuit_breaker_status(&self, _agent: Option<&str>) -> Result<Value, String> {
            Ok(json!({ "state": "closed" }))
        }
        async fn hook_event(
            &self,
            _session_id: &str,
            event: &str,
            _payload: Value,
        ) -> Result<Value, String> {
            Ok(json!({ "received": event }))
        }
    }

    fn call(name: &str, args: Value) -> Request {
        Request {
            jsonrpc: Some("2.0".into()),
            id: Some(json!(1)),
            method: "tools/call".into(),
            params: Some(json!({ "name": name, "arguments": args })),
        }
    }

    #[tokio::test]
    async fn dispatch_initialize_returns_server_info() {
        let req = Request {
            jsonrpc: Some("2.0".into()),
            id: Some(json!(1)),
            method: "initialize".into(),
            params: None,
        };
        let resp = dispatch(&MockBackend, req).await;
        let result = resp.result.unwrap();
        assert_eq!(result["serverInfo"]["name"], SERVER_NAME);
    }

    #[tokio::test]
    async fn dispatch_tools_list_returns_six_tools() {
        let req = Request {
            jsonrpc: Some("2.0".into()),
            id: Some(json!(1)),
            method: "tools/list".into(),
            params: None,
        };
        let resp = dispatch(&MockBackend, req).await;
        let tools = resp.result.unwrap()["tools"].clone();
        assert_eq!(tools.as_array().unwrap().len(), 6);
    }

    #[tokio::test]
    async fn dispatch_session_list_tool() {
        let resp = dispatch(&MockBackend, call("session_list", json!({}))).await;
        let result = resp.result.unwrap();
        assert_eq!(result["isError"], false);
        assert!(
            result["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("s1")
        );
    }

    #[tokio::test]
    async fn dispatch_agent_delegate_tool() {
        let resp = dispatch(
            &MockBackend,
            call(
                "agent_delegate",
                json!({ "session_id": "s1", "agent": "research", "task": "find" }),
            ),
        )
        .await;
        let result = resp.result.unwrap();
        assert_eq!(result["isError"], false);
        assert!(
            result["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("research")
        );
    }

    #[tokio::test]
    async fn dispatch_missing_argument_is_tool_error() {
        // `session_id` is required; omitting it yields an isError result.
        let resp = dispatch(&MockBackend, call("session_status", json!({}))).await;
        let result = resp.result.unwrap();
        assert_eq!(result["isError"], true);
        assert!(
            result["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("session_id")
        );
    }

    #[tokio::test]
    async fn dispatch_unknown_tool_is_error() {
        let resp = dispatch(&MockBackend, call("nope", json!({}))).await;
        assert_eq!(resp.result.unwrap()["isError"], true);
    }

    #[tokio::test]
    async fn dispatch_unknown_method_returns_jsonrpc_error() {
        let req = Request {
            jsonrpc: Some("2.0".into()),
            id: Some(json!(1)),
            method: "frobnicate".into(),
            params: None,
        };
        let resp = dispatch(&MockBackend, req).await;
        assert_eq!(resp.error.unwrap().code, error_codes::METHOD_NOT_FOUND);
    }

    #[tokio::test]
    async fn dispatch_notification_is_suppressed() {
        let req = Request {
            jsonrpc: Some("2.0".into()),
            id: None,
            method: "notifications/initialized".into(),
            params: None,
        };
        let resp = dispatch(&MockBackend, req).await;
        assert!(resp.suppress);
    }
}
