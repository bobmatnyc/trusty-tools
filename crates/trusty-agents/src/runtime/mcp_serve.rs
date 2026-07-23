//! `tagent mcp-serve` — a stdio MCP server exposing trusty-agents to external
//! MCP clients (Claude Code, etc.). Epic #3633 "core" slice.
//!
//! Why: external orchestrators already speak line-delimited JSON-RPC / MCP over
//! stdio to trusty-memory and trusty-search; trusty-agents was the missing
//! surface. Rather than grow a third bespoke stdio loop, this reuses the shared
//! native-MCP framework in `trusty_common::mcp` (`run_stdio_loop` +
//! `initialize_response`) that trusty-memory and trusty-search already ship
//! against — so parse-error handling, notification suppression, flush
//! semantics, and the `2024-11-05` protocol version stay fixed in one place and
//! this module adds zero new dependencies.
//! What: a `run_stdio_loop` dispatcher over a STATIC two-tool surface —
//!   - `list_agents`  (read-only): returns the same roster the `GET /api/agents`
//!     route serves, via `crate::api::server::agent_roster`.
//!   - `dispatch_task`: wraps the existing opaque, RBAC-gated `PmBridgeTool`
//!     (`crate::tools::pm_bridge`) rooted at the server process's cwd.
//! Every `tools/call` result is wrapped in the MCP `{content:[{type:"text",
//! text}], isError}` envelope, mirroring trusty-memory's `handle_message`.
//! What's DEFERRED to epic #3633: HTTP/SSE transport, `rpc.discover`, tool
//! namespacing, auth, and mapping external callers onto RBAC service tiers —
//! this slice registers `dispatch_task` with NO new RBAC wiring (it is already
//! opaque + tier-restricted at its own `restricted_tiers()`).
//! Test: `mcp_serve_tests` drives the `dispatch` fn in-memory
//! (initialize → tools/list → tools/call list_agents → unknown-method error),
//! mirroring `run_stdio_loop`'s own test style; a live smoke pipes JSON-RPC
//! lines into the built `tagent mcp-serve` binary.

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{Value, json};
use trusty_common::mcp::{Request, Response, error_codes, initialize_response, run_stdio_loop};

use crate::tools::pm_bridge::PmBridgeTool;
use crate::tools::pm_bridge_backend::ProcessPmBridge;
use crate::tools::traits::ToolExecutor;

/// Entry point for `tagent mcp-serve`: run the shared stdio MCP loop until the
/// client closes the pipe (EOF), then return.
///
/// Why: dispatched in `runtime::run` BEFORE `run_startup_init` (like the
/// `config` credential CLI) so the JSON-RPC stdout stream is never polluted by
/// startup banners or message-bus side effects, and the server stays
/// daemon-less.
/// What: hands the module-level [`dispatch`] fn to
/// [`trusty_common::mcp::run_stdio_loop`], which reads stdin line-by-line and
/// writes one JSON-RPC response line per request (notifications suppressed).
/// Test: exercised by the live binary smoke; the per-message logic is unit
/// tested via [`dispatch`] directly.
pub async fn run_mcp_serve() -> anyhow::Result<()> {
    run_stdio_loop(dispatch).await
}

/// Handle one JSON-RPC request and produce its response.
///
/// Why: pulled out of the stdio loop so unit tests can drive every method
/// without touching real stdin/stdout — the same seam trusty-memory's
/// `handle_message` exposes.
/// What: routes `initialize`, `tools/list`, `tools/call`, `ping`, and the
/// `notifications/*` notifications (suppressed → no wire reply). Unknown
/// methods return a JSON-RPC `METHOD_NOT_FOUND` error.
/// Test: `dispatch_initialize_reports_protocol_and_server`,
/// `dispatch_lists_both_tools`, `dispatch_calls_list_agents`,
/// `dispatch_unknown_method_errors`.
async fn dispatch(req: Request) -> Response {
    let id = req.id.clone();
    match req.method.as_str() {
        "initialize" => Response::ok(
            id,
            initialize_response("trusty-agents", env!("CARGO_PKG_VERSION"), None),
        ),
        // Notifications carry no id and MUST NOT produce a reply.
        "notifications/initialized" | "notifications/cancelled" => Response::suppressed(),
        "tools/list" => Response::ok(id, tool_definitions()),
        "ping" => Response::ok(id, json!({})),
        "tools/call" => {
            let params = req.params.clone().unwrap_or(Value::Null);
            let name = params
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let args = params.get("arguments").cloned().unwrap_or(Value::Null);
            call_tool(&name, args).await.into_response(id)
        }
        other => Response::err(
            id,
            error_codes::METHOD_NOT_FOUND,
            format!("Method not found: {other}"),
        ),
    }
}

/// The static `tools/list` result: the two tools this slice ships.
///
/// Why: the surface is fixed at compile time (no dynamic tool registry yet), so
/// a plain literal is the honest representation — and keeps the schemas the
/// single source of truth for what `tools/call` accepts.
/// What: MCP tool descriptors (`{name, description, inputSchema}`).
/// `dispatch_task`'s description + input schema mirror `PmBridgeTool::schema`'s
/// opaque wording (it never names a backend), translated from the OpenAI
/// function-call shape into MCP's `inputSchema`.
fn tool_definitions() -> Value {
    json!({
        "tools": [
            {
                "name": "list_agents",
                "description": "List the agent personas available in this project (Assistant, Izzie, CTO Bot, and the specialist roster), each with its role, model, and provider. Read-only — takes no arguments.",
                "inputSchema": {
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false
                }
            },
            {
                "name": "dispatch_task",
                "description": "Hand off a self-contained unit of work — a coding change in a repo, a multi-step coordination or planning task, a status check — so it actually gets done. The system inspects the task and automatically picks the right way to execute it; you never need to say how. Returns the full result once the work completes.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "task": {
                            "type": "string",
                            "description": "A concrete, self-contained description of the work to hand off."
                        }
                    },
                    "required": ["task"],
                    "additionalProperties": false
                }
            }
        ]
    })
}

/// Outcome of a `tools/call`: the text payload plus whether it was an error, so
/// the caller can build the MCP `{content, isError}` envelope with the right id.
struct ToolCallOutcome {
    text: String,
    is_error: bool,
}

impl ToolCallOutcome {
    /// Wrap this outcome in an MCP `tools/call` result envelope.
    ///
    /// What: `{content: [{type:"text", text}], isError}` — the shape trusty
    /// clients (and Claude Code) expect from a tool call. `isError` is always
    /// present so callers never have to infer success from its absence.
    fn into_response(self, id: Option<Value>) -> Response {
        Response::ok(
            id,
            json!({
                "content": [{ "type": "text", "text": self.text }],
                "isError": self.is_error,
            }),
        )
    }
}

/// Execute a named tool and normalise its result to a [`ToolCallOutcome`].
///
/// Why: keeps method routing (`dispatch`) separate from per-tool execution so
/// each tool's wiring is independently readable and testable.
/// What: `list_agents` returns the shared roster JSON (pretty-printed) as text;
/// `dispatch_task` constructs a `PmBridgeTool` over `ProcessPmBridge` rooted at
/// the server's cwd — the same project-scoping the CTRL/PM call sites use, never
/// an LLM-supplied path — and maps its `ToolResult` onto the outcome. An unknown
/// tool name is a recoverable error surfaced back to the client.
async fn call_tool(name: &str, args: Value) -> ToolCallOutcome {
    match name {
        "list_agents" => {
            let roster = crate::api::server::agent_roster().await;
            let text = serde_json::to_string_pretty(&roster)
                .unwrap_or_else(|_| roster.to_string());
            ToolCallOutcome {
                text,
                is_error: false,
            }
        }
        "dispatch_task" => {
            // `dispatch_task` always acts on the calling session's own project
            // (the server's cwd), never a tool-argument path — that would be an
            // injection vector. Mirrors `runtime::pm_mode` / `ctrl::pm_task`.
            let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            let tool = PmBridgeTool::new(Arc::new(ProcessPmBridge::from_project(cwd)));
            let result = tool.execute(args).await;
            ToolCallOutcome {
                is_error: result.is_error(),
                text: result.content().to_string(),
            }
        }
        other => ToolCallOutcome {
            text: format!("unknown tool: {other}"),
            is_error: true,
        },
    }
}

#[cfg(test)]
mod mcp_serve_tests {
    use super::*;

    /// Build a `Request` for a method with an optional params body.
    fn req(method: &str, id: i64, params: Option<Value>) -> Request {
        Request {
            jsonrpc: Some("2.0".into()),
            id: Some(json!(id)),
            method: method.into(),
            params,
        }
    }

    /// Why: `initialize` is the MCP handshake — the client refuses to proceed
    /// unless the protocol version and serverInfo come back correctly.
    /// What: asserts the shared `initialize_response` shape is echoed with the
    /// request id, the pinned `2024-11-05` protocol version, and this crate's
    /// name/version in `serverInfo`.
    #[tokio::test]
    async fn dispatch_initialize_reports_protocol_and_server() {
        let resp = dispatch(req("initialize", 1, None)).await;
        assert!(!resp.suppress);
        assert_eq!(resp.id, Some(json!(1)));
        let result = resp.result.expect("initialize returns a result");
        assert_eq!(result["protocolVersion"], "2024-11-05");
        assert!(result["capabilities"]["tools"].is_object());
        assert_eq!(result["serverInfo"]["name"], "trusty-agents");
        assert_eq!(result["serverInfo"]["version"], env!("CARGO_PKG_VERSION"));
    }

    /// Why: the two-tool surface is the whole contract of this slice; a
    /// regression that drops or renames a tool breaks every client.
    /// What: asserts `tools/list` returns exactly `list_agents` + `dispatch_task`
    /// and that each carries an object `inputSchema`.
    #[tokio::test]
    async fn dispatch_lists_both_tools() {
        let resp = dispatch(req("tools/list", 2, None)).await;
        let tools = resp.result.expect("tools/list result")["tools"].clone();
        let names: Vec<&str> = tools
            .as_array()
            .expect("tools is an array")
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, vec!["list_agents", "dispatch_task"]);
        for t in tools.as_array().unwrap() {
            assert!(t["inputSchema"].is_object(), "each tool has an inputSchema");
        }
    }

    /// Why: `list_agents` is the read-only tool demoed first; its `tools/call`
    /// path must return the roster inside the MCP text envelope.
    /// What: calls `list_agents`, asserts the `{content:[{type:"text",text}],
    /// isError:false}` envelope, and that the text parses back to an object
    /// carrying an `agents` array (the scan may be empty in a bare test env —
    /// the SHAPE is what matters).
    #[tokio::test]
    async fn dispatch_calls_list_agents() {
        let params = json!({ "name": "list_agents", "arguments": {} });
        let resp = dispatch(req("tools/call", 3, Some(params))).await;
        let result = resp.result.expect("tools/call result");
        assert_eq!(result["isError"], false);
        let text = result["content"][0]["text"]
            .as_str()
            .expect("content text is a string");
        assert_eq!(result["content"][0]["type"], "text");
        let parsed: Value = serde_json::from_str(text).expect("roster text is JSON");
        assert!(parsed["agents"].is_array(), "roster carries an agents array");
    }

    /// Why: an unknown tool name must fail soft (surfaced to the client as a
    /// tool error), not panic or 500 the loop.
    /// What: calls a bogus tool and asserts `isError:true` with a descriptive
    /// text payload.
    #[tokio::test]
    async fn dispatch_unknown_tool_is_soft_error() {
        let params = json!({ "name": "does_not_exist", "arguments": {} });
        let resp = dispatch(req("tools/call", 4, Some(params))).await;
        let result = resp.result.expect("tools/call result");
        assert_eq!(result["isError"], true);
        assert!(
            result["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("unknown tool")
        );
    }

    /// Why: JSON-RPC requires unknown methods to return a `-32601` error rather
    /// than being silently dropped — clients depend on the error to detect an
    /// unsupported capability.
    /// What: asserts an unrecognised method yields a `METHOD_NOT_FOUND` error
    /// carrying the request id.
    #[tokio::test]
    async fn dispatch_unknown_method_errors() {
        let resp = dispatch(req("frobnicate", 5, None)).await;
        assert!(resp.result.is_none());
        let err = resp.error.expect("unknown method returns an error");
        assert_eq!(err.code, error_codes::METHOD_NOT_FOUND);
        assert_eq!(resp.id, Some(json!(5)));
    }

    /// Why: `run_stdio_loop` decides whether to write a reply from
    /// `Response::suppress`; a notification that produced a normal response
    /// would put an unexpected line on the wire and desync the client.
    /// What: asserts `notifications/initialized` is suppressed.
    #[tokio::test]
    async fn dispatch_suppresses_notifications() {
        let resp = dispatch(req("notifications/initialized", 6, None)).await;
        assert!(resp.suppress, "notifications must not produce a wire reply");
    }
}
