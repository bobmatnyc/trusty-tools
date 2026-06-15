//! # trusty-mpm-mcp
//!
//! Why: Claude Code sessions (and their subagents) need to talk to the
//! trusty-mpm daemon directly — to enumerate sibling sessions, request agent
//! delegations, protect their own context window, inspect circuit-breaker
//! state, and (Phase 3) surface / preview / file captured bug reports.
//! MCP is the protocol Claude Code already speaks, so trusty-mpm exposes
//! an MCP server rather than inventing a bespoke channel.
//!
//! What: defines the fifteen MCP tools — nine orchestration/bug-reporting (six
//! core + three bug-reporting: `list_recent_errors`, `preview_bug_report`,
//! `report_bug`) plus six session-lifecycle tools (#1221: `session_new`,
//! `session_stop`, `session_resume`, `session_decommission`, `session_activity`,
//! `session_send`) — the [`OrchestratorBackend`] trait the daemon implements to
//! service them, and [`dispatch`], which routes a JSON-RPC [`Request`] to the
//! backend. The daemon wires [`dispatch`] into both `run_stdio_loop` (the `tm
//! daemon --mcp` path) and the loopback `POST /rpc` endpoint (the `serve
//! --stdio` bridge path).
//!
//! Test: `cargo test -p trusty-mpm` exercises the tool catalog, argument
//! parsing, and dispatch against an in-memory mock backend.

use async_trait::async_trait;
use serde_json::{Value, json};
use trusty_common::mcp::{Request, Response, error_codes};

pub mod session_dispatch;
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
/// and returns a JSON result or an error message. Phase 3 adds three
/// bug-reporting methods (`list_recent_errors`, `preview_bug_report`,
/// `report_bug`).
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

    // ── Phase 3: bug-reporting tools ─────────────────────────────────────────

    /// Back `list_recent_errors`: return a JSON array of recently captured
    /// errors across all trusty-* daemon stores, deduplicated by fingerprint.
    ///
    /// Why: the user must be able to browse what errors have been captured
    ///      before deciding which ones to file.
    /// What: aggregates up to `limit` (default 20) errors from all known
    ///       daemon stores, returns them as a JSON array with summary info.
    /// Test: `dispatch_list_recent_errors_tool` in the `tests` module.
    async fn list_recent_errors(&self, limit: u64) -> Result<Value, String>;

    /// Back `preview_bug_report`: build and return the scrubbed issue preview
    /// for the given fingerprint without filing anything.
    ///
    /// Why: the user must review the exact scrubbed body before consenting.
    /// What: finds the error by fingerprint, runs the scrubber+preview builder,
    ///       returns the title, body, labels, and scrub-change log.
    /// Test: `dispatch_preview_bug_report_tool` in the `tests` module.
    async fn preview_bug_report(&self, fingerprint: &str) -> Result<Value, String>;

    /// Back `report_bug`: file or increment a GitHub issue.
    ///
    /// Why: the consent gate — nothing is filed without `confirm: true`.
    /// What: when `confirm` is `false`, returns a preview-only result (same as
    ///       `preview_bug_report`). When `true`, resolves the token, calls the
    ///       GitHub client, and returns `{ filed, deduped, issue_url,
    ///       issue_number }` or a graceful "no token" message.
    /// Test: `dispatch_report_bug_no_confirm_is_preview` in the `tests` module.
    async fn report_bug(&self, fingerprint: &str, confirm: bool) -> Result<Value, String>;

    // ── #1221: session-lifecycle tools ───────────────────────────────────────

    /// Back `session_new`: spawn a new managed session.
    ///
    /// Why: the driver skill needs a typed, JSON-native way to create an
    ///      isolated, provisioned workspace and launch a harness in it — without
    ///      scraping `tm session new` CLI text (the #842 defect).
    /// What: provisions a workspace cloned from `repo_url` at `git_ref`, creates
    ///       the tmux host, launches the selected `runtime` (default claude-code)
    ///       with `task`, and returns the new session's id / tmux name /
    ///       workspace path / state / attach command. An unknown `runtime` value
    ///       is an error string.
    /// Test: `dispatch_session_new_tool` (mock) + the daemon-side
    ///       `session_new_spawns_via_manager` integration test.
    async fn session_new(
        &self,
        repo_url: &str,
        git_ref: &str,
        task: &str,
        name_hint: Option<&str>,
        runtime: Option<&str>,
    ) -> Result<Value, String>;

    /// Back `session_stop`: stop a session's runtime, keeping its workspace.
    ///
    /// Why: a session endures beyond its running runtime; stopping must preserve
    ///      the workspace so the session can be resumed later.
    /// What: kills the tmux session + harness, marks the record Stopped, returns
    ///       the updated record summary. A missing id is an error string.
    /// Test: `dispatch_session_stop_tool` (mock).
    async fn session_stop(&self, session_id: &str) -> Result<Value, String>;

    /// Back `session_resume`: re-spawn the runtime in the existing workspace.
    ///
    /// Why: the counterpart to `session_stop` — bring a stopped session back
    ///      without re-cloning.
    /// What: re-creates the tmux host rooted at the on-disk workspace, re-spawns
    ///       the SAME runtime backend, and returns the updated record. A missing
    ///       id (or an invalid state transition) is an error string.
    /// Test: `dispatch_session_resume_tool` (mock).
    async fn session_resume(&self, session_id: &str) -> Result<Value, String>;

    /// Back `session_decommission`: full, terminal teardown.
    ///
    /// Why: the only operation that removes the workspace from disk; terminal so
    ///      a session can be fully reclaimed.
    /// What: kills the runtime, removes the workspace dir, marks the record
    ///       Decommissioned (a tombstone is kept), and returns it. A missing id
    ///       is an error string.
    /// Test: `dispatch_session_decommission_tool` (mock).
    async fn session_decommission(&self, session_id: &str) -> Result<Value, String>;

    /// Back `session_activity`: capture pane + lifecycle state.
    ///
    /// Why: the driver must reason about whether a session is working / idle /
    ///      blocked WITHOUT requiring an LLM key, so raw pane content is always
    ///      returned and the LLM classification is an optional overlay.
    /// What: captures the last `lines` (default 60) pane lines, reports
    ///       `runtime_active` + pending-decision fields, and includes an LLM
    ///       `classification` when OPENROUTER_API_KEY is set. A missing id is an
    ///       error string.
    /// Test: `dispatch_session_activity_tool` (mock).
    async fn session_activity(&self, session_id: &str, lines: u32) -> Result<Value, String>;

    /// Back `session_send`: inject a line into a session's pane.
    ///
    /// Why: drive a running session (answer prompts, issue commands) without
    ///      attaching to the tmux pane.
    /// What: sends `text` followed by Enter to the session's pane and returns a
    ///       confirmation with the tmux name. A missing id is an error string.
    /// Test: `dispatch_session_send_tool` (mock).
    async fn session_send(&self, session_id: &str, text: &str) -> Result<Value, String>;
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
        // ── Phase 3: bug-reporting tools ─────────────────────────────────────
        "list_recent_errors" => {
            let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(20);
            backend.list_recent_errors(limit).await
        }
        "preview_bug_report" => match required_str(&args, "fingerprint") {
            Ok(fp) => backend.preview_bug_report(&fp).await,
            Err(e) => Err(e),
        },
        "report_bug" => match required_str(&args, "fingerprint") {
            Ok(fp) => {
                let confirm = args
                    .get("confirm")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                backend.report_bug(&fp, confirm).await
            }
            Err(e) => Err(e),
        },
        // #1221: the six session-lifecycle tools route through a sibling module
        // so this match stays focused and `mod.rs` stays under the SLOC cap.
        other => match session_dispatch::try_dispatch(backend, other, &args).await {
            Some(result) => result,
            None => Err(format!("unknown tool: {other}")),
        },
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
pub(crate) fn required_str(args: &Value, key: &str) -> Result<String, String> {
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
#[path = "tests.rs"]
mod tests;
