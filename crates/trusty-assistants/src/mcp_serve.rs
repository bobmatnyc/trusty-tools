//! `tagent mcp-serve` — a stdio MCP server exposing trusty-agents to external
//! MCP clients (Claude Code, etc.). Epic #3633 "core" slice.
//!
//! Why: external orchestrators already speak line-delimited JSON-RPC / MCP over
//! stdio to trusty-memory and trusty-search. This reuses the shared native-MCP
//! framework in `trusty_mcp` (`run_stdio_loop` + `initialize_response`) so
//! parse-error handling, notification suppression, flush semantics and the
//! `2024-11-05` protocol version stay fixed in one place. #11632: this is the
//! only serving surface left in the crate — stdio, no network listener.
//! What: a `run_stdio_loop` dispatcher over a STATIC one-tool surface —
//! `dispatch_task`, which wraps the opaque, RBAC-gated `PmBridgeTool`
//! (`crate::tools::pm_bridge`) rooted at the server process's cwd. #11632
//! removed `list_agents`: the agent roster it read was cut with the harness.
//! Every `tools/call` result is wrapped in the MCP `{content:[{type:"text",
//! text}], isError}` envelope; a failing tool stays a normal result with
//! `isError:true`.
//! RBAC: this surface applies NO tier restriction; gating external callers by
//! tier is deferred to epic #3633.
//! Test: `mcp_serve_tests` drives the `dispatch` fn in-memory; a live smoke
//! pipes JSON-RPC lines into the built `tagent mcp-serve` binary.

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{Value, json};
use trusty_mcp::{Request, Response, error_codes, initialize_response, run_stdio_loop};

use crate::tools::pm_bridge::PmBridgeTool;
use crate::tools::pm_bridge_backend::ProcessPmBridge;
use crate::tools::traits::ToolExecutor;

/// Entry point for `tagent mcp-serve`: run the shared stdio MCP loop until the
/// client closes the pipe (EOF), then return.
///
/// Why: an MCP stdio server speaks JSON-RPC on stdout, so nothing else may
/// write there; tracing goes to stderr.
/// What: installs a stderr tracing subscriber so `dispatch_task`'s
/// diagnostics are not dropped, then hands [`dispatch`] to
/// [`trusty_mcp::run_stdio_loop`].
/// #7609: no channel-write credential is recorded here; the `channel` tool's
/// `set` actions refuse under `mcp-serve` by design.
/// Test: exercised by the live binary smoke; the per-message logic is unit
/// tested via [`dispatch`] directly.
pub async fn run_mcp_serve() -> anyhow::Result<()> {
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_writer(std::io::stderr)
        .try_init();
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
/// `dispatch_lists_the_tool`, `dispatch_unknown_method_errors`.
async fn dispatch(req: Request) -> Response {
    let id = req.id.clone();
    match req.method.as_str() {
        "initialize" => Response::ok(
            id,
            // #11632: the package name, so the rename reaches `serverInfo`.
            initialize_response(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"), None),
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

/// The static `tools/list` result: the one tool this surface ships.
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
/// What: `dispatch_task` constructs a `PmBridgeTool` over `ProcessPmBridge` rooted at
/// the server's cwd — the same project-scoping the CTRL/PM call sites use, never
/// an LLM-supplied path — and maps its `ToolResult` onto the outcome. An unknown
/// tool name is a recoverable error surfaced back to the client.
async fn call_tool(name: &str, args: Value) -> ToolCallOutcome {
    match name {
        "dispatch_task" => {
            // `dispatch_task` always acts on the calling session's own project
            // (the server's cwd), never a tool-argument path — that would be an
            // injection vector. Mirrors `runtime::pm_mode` / `ctrl::pm_task`.
            let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            let tool = PmBridgeTool::new(Arc::new(ProcessPmBridge::from_project(cwd)));
            dispatch_task_outcome(&tool, args).await
        }
        other => ToolCallOutcome {
            text: format!("unknown tool: {other}"),
            is_error: true,
        },
    }
}

/// Run a `dispatch_task` call through an already-constructed [`PmBridgeTool`]
/// and normalise its `ToolResult` to a [`ToolCallOutcome`].
///
/// Why: the DI seam. Production builds the tool over `ProcessPmBridge` (a real
/// subprocess); tests inject a `RecordingBackend` (the same seam
/// `pm_bridge_tests.rs` uses) so the argument-validation and result-mapping
/// path is testable without spawning `tm`/`tcode`.
/// What: forwards `args` to `tool.execute()` and maps `is_error()` /
/// `content()` onto the outcome. `PmBridgeTool::execute` already validates the
/// required `task` argument (missing/empty → recoverable error), which surfaces
/// here as `isError:true`.
/// Test: `dispatch_task_missing_argument_is_error` (through `dispatch`) and
/// `dispatch_task_forwards_to_backend` (through this fn with a RecordingBackend).
async fn dispatch_task_outcome(tool: &PmBridgeTool, args: Value) -> ToolCallOutcome {
    let result = tool.execute(args).await;
    ToolCallOutcome {
        is_error: result.is_error(),
        text: result.content().to_string(),
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
        assert_eq!(result["serverInfo"]["name"], "trusty-assistants");
        assert_eq!(result["serverInfo"]["version"], env!("CARGO_PKG_VERSION"));
    }

    /// Why: the tool surface is the whole contract of this server; a
    /// regression that drops or renames a tool breaks every client.
    /// What: asserts `tools/list` returns exactly `dispatch_task` and that it
    /// carries an object `inputSchema`.
    #[tokio::test]
    async fn dispatch_lists_the_tool() {
        let resp = dispatch(req("tools/list", 2, None)).await;
        let tools = resp.result.expect("tools/list result")["tools"].clone();
        let names: Vec<&str> = tools
            .as_array()
            .expect("tools is an array")
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, vec!["dispatch_task"]);
        for t in tools.as_array().unwrap() {
            assert!(t["inputSchema"].is_object(), "each tool has an inputSchema");
        }
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

    /// Why: `dispatch_task` requires a `task` argument; a call that omits it
    /// must fail SOFT (a `tools/call` result with `isError:true`), not panic or
    /// spawn a backend. This is the argument-validation path
    /// (`PmBridgeTool::execute`'s guard) reached end-to-end through `dispatch`,
    /// and it short-circuits before any subprocess is spawned.
    /// What: calls `dispatch_task` with empty arguments and asserts the error
    /// envelope carries the "missing 'task'" message.
    #[tokio::test]
    async fn dispatch_task_missing_argument_is_error() {
        let params = json!({ "name": "dispatch_task", "arguments": {} });
        let resp = dispatch(req("tools/call", 7, Some(params))).await;
        let result = resp.result.expect("tools/call result");
        assert_eq!(result["isError"], true);
        assert!(
            result["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("missing 'task'")
        );
    }

    /// Why: proves the `dispatch_task` execution path — argument forwarding and
    /// success-result mapping — via the same DI seam `pm_bridge_tests.rs` uses,
    /// without spawning `tm`/`tcode`.
    /// What: builds a `PmBridgeTool` over a `RecordingBackend` that echoes a
    /// fixed transcript, drives it through [`dispatch_task_outcome`], and
    /// asserts the backend saw the task and the outcome is a non-error carrying
    /// the transcript.
    #[tokio::test]
    async fn dispatch_task_forwards_to_backend() {
        use crate::intent::route::BridgeRoute;
        use crate::tools::pm_bridge_backend::PmBridgeBackend;
        use std::sync::Mutex;

        struct RecordingBackend {
            seen: Mutex<Vec<String>>,
        }
        #[async_trait::async_trait]
        impl PmBridgeBackend for RecordingBackend {
            async fn run(
                &self,
                _route: BridgeRoute,
                _target: Option<&str>,
                task: &str,
            ) -> anyhow::Result<String> {
                self.seen.lock().unwrap().push(task.to_string());
                Ok("backend transcript".into())
            }
        }

        let backend = Arc::new(RecordingBackend {
            seen: Mutex::new(Vec::new()),
        });
        let tool = PmBridgeTool::new(backend.clone());
        let outcome = dispatch_task_outcome(&tool, json!({ "task": "check the backlog" })).await;

        assert!(!outcome.is_error, "expected success: {}", outcome.text);
        assert_eq!(outcome.text, "backend transcript");
        let seen = backend.seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0], "check the backlog");
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
