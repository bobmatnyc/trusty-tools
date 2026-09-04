//! Pure daemon-bridge for `trusty-memory serve --stdio` (issue #1078).
//!
//! Why: the prior `serve --stdio` path opened redb directly in the stdio
//! process. When the daemon holds the exclusive write lock the stdio process
//! fell back to a read-only snapshot, causing write failures and stale reads.
//! This module makes the stdio path a pure proxy: every JSON-RPC request is
//! forwarded to the running daemon and **the stdio process never opens redb**.
//! Nothing below may reach for a store — see #1078.
//!
//! What (#6316): the forwarding itself is no longer written here. This module
//! now (1) ensures the daemon is running via
//! [`crate::commands::daemon_guard::ensure_daemon_running`], and (2) hands
//! [`trusty_mcp::DaemonBridgeJsonRpc`] a [`trusty_mcp::UdsBridgeConfig`] plus
//! one request rewriter. The bridge owns the socket exchange, the `jsonrpc`
//! normalisation, the streaming-method refusal, notification suppression and
//! the reply mapping — every one of which used to be a second copy here.
//!
//! What the rewriter still owns, because it is trusty-memory's and nobody
//! else's:
//!
//! - **`--palace`**: [`inject_default_palace`] stamps the CLI default into the
//!   tool arguments a handler actually reads, for both wire shapes.
//! - **Caller identity (DOC-53 §4.3)**: the daemon this bridge proxies to is
//!   ONE shared process serving every concurrently-attached session — it cannot
//!   tell which caller a request came from except from what the request
//!   carries. THIS process is spawned fresh per `serve --stdio` invocation and
//!   genuinely runs inside its caller's own process tree, so
//!   [`crate::attribution::resolve_own_workstream_name`] resolved HERE is the
//!   correct identity. [`inject_caller_context`] stamps it into every forwarded
//!   request.
//!
//! `run_stdio` does not start the daemon, so the readiness guard stays this
//! module's: [`ensure_daemon_up_for_stdio`] takes the same `StartLock` on the
//! same lock file `trusty-memory start` uses, so the two paths cannot race
//! (#5267).
//!
//! STDOUT hygiene: never write to stdout — it is the JSON-RPC channel. All
//! diagnostic output goes to stderr.
//!
//! Test: unit tests below; `tests/serve_stdio_e2e.rs` for the full e2e path.

use anyhow::{anyhow, Result};
use std::path::PathBuf;
use std::time::Duration;
use trusty_mcp::{DaemonBridgeJsonRpc, UdsBridgeConfig};

/// Per-request forwarding timeout (60 s — headroom for cold-start embedding).
///
/// Why: a generous ceiling stops one hung request blocking the stdio loop while
/// still letting a slow embedding operation finish.
/// Test: `a_transport_failure_names_the_endpoint_and_does_not_hang`.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// Methods that answer in many frames and therefore cannot be bridged.
///
/// Why: MCP stdio is one response per request — the stdio loop writes exactly
/// one response per request, and there is no frame sequence to put a token
/// stream in. Three things could happen to a streamed call here, and two of
/// them are silent: return the first item as if it were the answer, or hang
/// while the client waits for a shape it will never get. This list is what
/// makes the third — an error that names the problem — the one that happens.
///
/// A caller that wants the stream dials the socket itself with
/// `trusty_common::uds::send_framed_stream_request_capped`; the console does
/// exactly that.
///
/// **This list must equal the daemon's `transport::uds::STREAM_METHODS`.** It
/// is a second copy, because the bridge refuses a method BEFORE dialling and so
/// cannot ask the router what it registered. A method the daemon streams and
/// this list omits is exactly the silent case the paragraph above describes:
/// #6286 added `memory.activity_stream` to the daemon and not to this list, so
/// an MCP client calling it would have hung waiting for a shape it was never
/// going to get. `bridge_streaming_methods_match_the_daemon` in
/// `tests/uds_consumer_contract.rs` is what keeps them equal.
///
/// Test: `streaming_method_is_refused_rather_than_half_answered`,
/// `bridge_streaming_methods_match_the_daemon`.
pub const STREAMING_METHODS: &[&str] = &["memory.chat", "memory.activity_stream"];

/// Ensure the daemon is running and return the socket to forward on.
///
/// Why (#5267, superseding the `no_spawn: true` posture of #1152): a bridge
/// whose daemon is merely not running should start it, not hard-error. #1152 was
/// an AUTO-SPAWN outage — every bridge independently spawning its own daemon, N
/// bridges producing N daemons racing for redb's write lock. What this does
/// instead is start-if-not-running: the daemon's existence is ensured ONCE,
/// under an exclusive lock, so seven bridges converge on one.
///
/// What (#6316): [`trusty_mcp::DaemonBridgeJsonRpc::run_stdio`] deliberately
/// does not probe or start anything, so this guard stays the caller's. It takes
/// the same `StartLock` on the same lock file `trusty-memory start` uses, so
/// the two paths still cannot race each other. Fails closed if the daemon does
/// not become ready.
///
/// # Errors
///
/// The data directory cannot be resolved, or the daemon does not become ready
/// inside `ensure_daemon_running`'s budget.
///
/// Test: e2e in `tests/serve_stdio_e2e.rs` and
/// `tests/serve_stdio_concurrent_e2e.rs`.
pub(crate) async fn ensure_daemon_up_for_stdio() -> Result<PathBuf> {
    let lock_path = crate::commands::start::start_lock_path()
        .ok_or_else(|| anyhow!("could not resolve the trusty-memory data directory"))?;
    let socket = crate::transport::uds::socket_path()?;
    crate::commands::daemon_guard::ensure_daemon_running(&socket, &lock_path).await?;
    Ok(socket)
}

/// Build the shared bridge with trusty-memory's own request rewriter (#6316).
///
/// Why: the bridge is constructed in two places — [`run_stdio_bridge`], and
/// every unit test that drives `answer` without a live daemon. Building it once
/// here is what keeps those tests exercising the same streaming list, frame
/// budget and injection chain the real bridge runs.
///
/// What: sets the daemon label, the streaming refusal list, [`REQUEST_TIMEOUT`]
/// and the daemon's own 32 MiB frame budget, then attaches a rewriter that runs
/// [`inject_default_palace`] followed by [`inject_caller_context`]. The rewriter
/// sees the normalised envelope and the bridge re-stamps `jsonrpc` after it, so
/// neither injection can invalidate the frame.
///
/// Test: `streaming_method_is_refused_rather_than_half_answered`,
/// `a_transport_failure_answers_the_request_that_caused_it`,
/// `the_rewriter_reaches_the_forwarded_envelope`.
pub(crate) fn build_bridge(
    socket: PathBuf,
    default_palace: Option<String>,
    caller_workstream: Option<String>,
    caller_cwd: Option<String>,
) -> DaemonBridgeJsonRpc {
    // #6316: the transport, the streaming refusal and the reply mapping live in
    // trusty-mcp now; this crate supplies only what is its own.
    let config = UdsBridgeConfig::new(socket, "trusty-memory")
        .with_streaming_methods(STREAMING_METHODS.iter().copied())
        .with_request_timeout(REQUEST_TIMEOUT)
        .with_max_frame_bytes(crate::transport::uds::MAX_FRAME_BYTES);

    DaemonBridgeJsonRpc::new(config).with_request_rewriter(move |envelope| {
        let envelope = inject_default_palace(envelope, default_palace.as_deref());
        inject_caller_context(
            envelope,
            caller_workstream.as_deref(),
            caller_cwd.as_deref(),
        )
    })
}

/// Run the MCP stdio bridge.
///
/// Why: this is the top-level entry point for `trusty-memory serve --stdio`
/// under the daemon-bridge architecture (issue #1078). The prior direct-store
/// path opened redb in the stdio process and hit the write-lock exclusion
/// problem; this path never touches the store at all.
///
/// What: (1) ensures the daemon is running under an exclusive lock (#5267);
/// (2) resolves this process' own caller identity once, because neither the cwd
/// nor `TM_WORKSTREAM_NAME` changes for the lifetime of a `serve --stdio`
/// process; (3) hands both, plus the `--palace` default, to the shared bridge
/// and runs its stdio loop. Hard-errors if the daemon cannot start.
///
/// # Errors
///
/// The daemon could not be started or did not become ready, or the stdio loop
/// failed on an I/O error.
///
/// Test: `tests/serve_stdio_e2e.rs` spawns a real child, asserts bounded
/// responses. Bridge-specific unit tests live in this module.
pub async fn run_stdio_bridge(palace: Option<String>) -> Result<()> {
    let socket = ensure_daemon_up_for_stdio().await?;

    // DOC-53 §4.3: resolved HERE, once — see the module doc for why this
    // process rather than the shared daemon is the correct place to read it.
    let caller_cwd = std::env::current_dir()
        .ok()
        .map(|p| p.to_string_lossy().into_owned());
    let caller_workstream = crate::attribution::resolve_own_workstream_name(caller_cwd.as_deref());

    build_bridge(socket, palace, caller_workstream, caller_cwd)
        .run_stdio()
        .await
}

/// Inject `default_palace` into a JSON-RPC request's arguments when the
/// caller hasn't already specified a `palace` field.
///
/// Why: `serve --stdio --palace <name>` should behave the same for the bridge
/// path as it did for the direct-store path -- every tool call that accepts a
/// `palace` parameter should see the default. A real MCP client (Claude Code)
/// sends the standard `tools/call` envelope (`method: "tools/call"`, `params:
/// {name, arguments}`) and tool handlers read `arguments.palace`, NOT
/// top-level `params.palace` -- injecting only at the top level (the
/// pre-existing bug: issue reported live during demo prep) left every real
/// `tools/call` request without a palace, surfacing as `-32603: memory_recall:
/// missing 'palace'` even with `--palace` configured. This mirrors
/// [`inject_caller_context`]'s dispatch-shape handling exactly.
/// What: for `method: "tools/call"`, finds or creates `params.arguments` and
/// injects `"palace": <default_palace>` there when absent. For any other
/// (legacy direct method-per-tool, `params` IS the argument object) request
/// shape, keeps the original top-level `params.palace` injection. Leaves the
/// value unchanged if the target already contains `palace` or if
/// `default_palace` is `None`.
/// Test: `inject_default_palace_adds_when_absent`,
/// `inject_default_palace_preserves_existing`,
/// `inject_default_palace_tools_call_adds_when_absent`,
/// `inject_default_palace_tools_call_preserves_existing`,
/// `inject_default_palace_noop_when_none`.
fn inject_default_palace(
    mut req: serde_json::Value,
    default_palace: Option<&str>,
) -> serde_json::Value {
    let Some(palace) = default_palace else {
        return req;
    };

    let is_tools_call = req.get("method").and_then(|m| m.as_str()) == Some("tools/call");

    // Find or create the params object (same three-way shape as
    // `inject_caller_context`).
    let params = match req.get_mut("params") {
        Some(p) if p.is_object() => p,
        Some(p) if p.is_null() => {
            *p = serde_json::json!({});
            p
        }
        None => {
            req["params"] = serde_json::json!({});
            req.get_mut("params").expect("just inserted")
        }
        // Non-object params (array or scalar) -- don't touch them.
        _ => return req,
    };

    // For `tools/call`, the tool's actual argument object -- the one tool
    // handlers actually read `palace` from -- is nested one level deeper at
    // `params.arguments`. For legacy direct method-per-tool dispatch,
    // `params` IS the argument object already.
    let target = if is_tools_call {
        match params.get_mut("arguments") {
            Some(a) if a.is_object() => a,
            Some(a) if a.is_null() => {
                *a = serde_json::json!({});
                a
            }
            None => {
                params["arguments"] = serde_json::json!({});
                params.get_mut("arguments").expect("just inserted")
            }
            _ => return req,
        }
    } else {
        params
    };

    // Only inject if the caller didn't already specify a palace.
    if target.get("palace").is_none() {
        target["palace"] = serde_json::Value::String(palace.to_string());
    }

    req
}

/// Inject this bridge's own resolved `workstream` and `cwd` into a JSON-RPC
/// request's TOOL ARGUMENTS, when the caller hasn't already supplied them
/// (DOC-53 §4.3, critical fix).
///
/// Why: the shared daemon this bridge proxies to cannot resolve per-caller
/// identity from its own (one, shared) process — see the module doc
/// comment. This bridge process genuinely IS a fresh, per-session process,
/// so its own `TM_WORKSTREAM_NAME`/cwd resolution
/// ([`crate::attribution::resolve_own_workstream_name`], called once in
/// [`run_stdio_bridge`]) is the correct caller identity to attach. Every
/// non-notification request forwarded to the daemon gets this stamp, mirroring
/// [`inject_default_palace`]'s `--palace` injection.
///
/// What: like [`inject_default_palace`] (which now mirrors this same
/// dispatch-shape branching for `--palace`), this function handles both the
/// "direct method-per-tool" dispatch shape (`method: "<tool-name>"`, `params:
/// <arguments>`) -- where `params` IS the argument object -- and the standard
/// MCP `tools/call` envelope (`method: "tools/call"`, `params: {name,
/// arguments}`) that a real MCP client (Claude Code) actually sends over
/// stdio: it locates `params.arguments` for that shape and injects there
/// instead, since that is the object [`crate::transport::rpc::dispatch`]'s
/// `tools/call` branch actually hands to [`crate::tools::dispatch_tool`].
/// Only injects fields the caller didn't already set — caller intent always
/// wins. A no-op when
/// both `workstream` and `cwd` are `None` (nothing resolvable — DOC-53
/// §4.1's omit-cleanly rule, applied at the injection point too).
/// Test: `inject_caller_context_direct_dispatch_shape`,
/// `inject_caller_context_tools_call_shape`,
/// `inject_caller_context_preserves_existing_caller_values`,
/// `inject_caller_context_noop_when_nothing_resolved`.
fn inject_caller_context(
    mut req: serde_json::Value,
    workstream: Option<&str>,
    cwd: Option<&str>,
) -> serde_json::Value {
    if workstream.is_none() && cwd.is_none() {
        return req;
    }

    let is_tools_call = req.get("method").and_then(|m| m.as_str()) == Some("tools/call");

    // Find or create the params object (same three-way shape as
    // `inject_default_palace`).
    let params = match req.get_mut("params") {
        Some(p) if p.is_object() => p,
        Some(p) if p.is_null() => {
            *p = serde_json::json!({});
            p
        }
        None => {
            req["params"] = serde_json::json!({});
            req.get_mut("params").expect("just inserted")
        }
        _ => return req,
    };

    // For `tools/call`, the tool's actual argument object is nested one
    // level deeper at `params.arguments` -- that's what `dispatch_tool`
    // receives. For direct method-per-tool dispatch, `params` IS the
    // argument object already.
    let args = if is_tools_call {
        match params.get_mut("arguments") {
            Some(a) if a.is_object() => a,
            Some(a) if a.is_null() => {
                *a = serde_json::json!({});
                a
            }
            None => {
                params["arguments"] = serde_json::json!({});
                params.get_mut("arguments").expect("just inserted")
            }
            _ => return req,
        }
    } else {
        params
    };

    if let Some(ws) = workstream {
        if args.get("workstream").is_none() {
            args["workstream"] = serde_json::Value::String(ws.to_string());
        }
    }
    if let Some(c) = cwd {
        if args.get("cwd").is_none() {
            args["cwd"] = serde_json::Value::String(c.to_string());
        }
    }

    req
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use trusty_mcp::Request;

    /// A bridge pointed at a socket nothing is serving.
    fn dead_bridge(socket: PathBuf) -> DaemonBridgeJsonRpc {
        build_bridge(socket, None, None, None)
    }

    /// The request a live session sends once its daemon is gone.
    fn a_request_with_id(id: i64) -> Request {
        Request {
            jsonrpc: Some("2.0".to_string()),
            id: Some(json!(id)),
            method: "tools/call".to_string(),
            params: Some(json!({
                "name": "memory_remember",
                "arguments": {"text": "anything"}
            })),
        }
    }

    // -----------------------------------------------------------------------
    // inject_default_palace
    // -----------------------------------------------------------------------
    /// Why: the default palace must be injected when params is a JSON object
    /// with no existing `palace` key.
    /// What: builds a request with object params, injects, asserts `palace`
    /// was added while existing fields are preserved.
    /// Test: this test.
    #[test]
    fn inject_default_palace_adds_when_absent() {
        let req = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "memory_remember",
            "params": {"content": "hello"}
        });
        let out = inject_default_palace(req, Some("my-palace"));
        assert_eq!(out["params"]["palace"], "my-palace");
        assert_eq!(out["params"]["content"], "hello");
    }

    /// Why: if the caller already provided a palace the bridge must NOT
    /// overwrite it -- the caller's intent takes priority.
    /// Test: this test.
    #[test]
    fn inject_default_palace_preserves_existing() {
        let req = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "memory_remember",
            "params": {"content": "hi", "palace": "caller-palace"}
        });
        let out = inject_default_palace(req, Some("default-palace"));
        assert_eq!(out["params"]["palace"], "caller-palace");
    }

    /// Why: when no default is provided the request must pass through unmodified.
    /// Test: this test.
    #[test]
    fn inject_default_palace_noop_when_none() {
        let req = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "memory_remember",
            "params": {"content": "hi"}
        });
        let out = inject_default_palace(req.clone(), None);
        assert_eq!(out, req);
    }

    /// Why: null params should become an object with the default palace so
    /// handlers that expect a palace field still work.
    /// Test: this test.
    #[test]
    fn inject_default_palace_null_params_becomes_object() {
        let req = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "palace_list",
            "params": null
        });
        let out = inject_default_palace(req, Some("my-palace"));
        assert_eq!(out["params"]["palace"], "my-palace");
    }

    /// Why (root cause of the demo-blocking bug): a real MCP client (Claude
    /// Code) sends the standard `tools/call` envelope, and tool handlers read
    /// `arguments.palace` -- NOT top-level `params.palace`. Before this fix,
    /// `inject_default_palace` only ever wrote to `params.palace`, so every
    /// real `tools/call` request reached the handler with no palace at all,
    /// surfacing as `-32603: memory_recall: missing 'palace' (no --palace
    /// default configured)` even with `--palace` supplied on the CLI.
    /// What: injects with a `tools/call` envelope whose `arguments` has no
    /// `palace`; asserts the default lands at `params.arguments.palace`
    /// (not as a sibling of `name`/`arguments` at the top level).
    /// Test: itself.
    #[test]
    fn inject_default_palace_tools_call_adds_when_absent() {
        let req = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "memory_recall",
                "arguments": {"query": "hello"}
            }
        });
        let out = inject_default_palace(req, Some("owner-profile"));
        assert_eq!(out["params"]["arguments"]["palace"], "owner-profile");
        assert_eq!(out["params"]["arguments"]["query"], "hello");
        assert!(
            out["params"].get("palace").is_none(),
            "must not land as a sibling of name/arguments"
        );
    }

    /// Why: an MCP caller that already set its own `arguments.palace` (e.g.
    /// an explicit `--palace` argument on the tool call itself) must win --
    /// the CLI default is a fallback, not an override.
    /// What: pre-populates `arguments.palace`; asserts it survives unchanged.
    /// Test: itself.
    #[test]
    fn inject_default_palace_tools_call_preserves_existing() {
        let req = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "memory_recall",
                "arguments": {"query": "hi", "palace": "caller-palace"}
            }
        });
        let out = inject_default_palace(req, Some("default-palace"));
        assert_eq!(out["params"]["arguments"]["palace"], "caller-palace");
    }

    /// Why: the legacy direct method-per-tool dispatch shape (`method:
    /// "<tool-name>"`, `params: <arguments>`) must keep receiving the
    /// top-level injection so nothing regresses for callers still using
    /// that shape.
    /// What: injects a non-`tools/call` request; asserts `params.palace` is
    /// still set directly (same assertion as
    /// `inject_default_palace_adds_when_absent`, named here to make the
    /// legacy-shape regression coverage explicit alongside the new
    /// `tools/call` tests).
    /// Test: itself.
    #[test]
    fn inject_default_palace_legacy_direct_shape_still_injects_top_level() {
        let req = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "memory_recall",
            "params": {"query": "hello"}
        });
        let out = inject_default_palace(req, Some("owner-profile"));
        assert_eq!(out["params"]["palace"], "owner-profile");
        assert_eq!(out["params"]["query"], "hello");
    }

    // -----------------------------------------------------------------------
    // inject_caller_context (DOC-53 §4.3, critical fix)
    // -----------------------------------------------------------------------

    /// Why: the "direct method-per-tool" dispatch shape (`method:
    /// "<tool-name>"`, `params: <arguments>`) is the simpler of the two
    /// wire shapes -- `params` itself IS the argument object.
    /// What: injects both workstream and cwd into a request with no
    /// existing values; asserts both land directly under `params`.
    /// Test: itself.
    #[test]
    fn inject_caller_context_direct_dispatch_shape() {
        let req = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "memory_remember",
            "params": {"text": "hello"}
        });
        let out = inject_caller_context(req, Some("feat-x"), Some("/x/.worktrees/feat-x"));
        assert_eq!(out["params"]["workstream"], "feat-x");
        assert_eq!(out["params"]["cwd"], "/x/.worktrees/feat-x");
        assert_eq!(out["params"]["text"], "hello");
    }

    /// Why: a real MCP client (Claude Code) sends the `tools/call` envelope
    /// (`method: "tools/call"`, `params: {name, arguments}`) -- the tool's
    /// actual argument object `dispatch_tool` receives is nested one level
    /// deeper at `params.arguments`, NOT `params` itself. This is the shape
    /// [`inject_default_palace`] gets wrong (injects into the wrong level);
    /// `inject_caller_context` must inject into `arguments`.
    /// What: injects into a `tools/call` envelope; asserts the values land
    /// under `params.arguments`, not as siblings of `name`/`arguments`.
    /// Test: itself.
    #[test]
    fn inject_caller_context_tools_call_shape() {
        let req = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "memory_remember",
                "arguments": {"text": "hello"}
            }
        });
        let out = inject_caller_context(req, Some("feat-x"), Some("/x/.worktrees/feat-x"));
        assert_eq!(out["params"]["arguments"]["workstream"], "feat-x");
        assert_eq!(out["params"]["arguments"]["cwd"], "/x/.worktrees/feat-x");
        assert_eq!(out["params"]["arguments"]["text"], "hello");
        assert!(
            out["params"].get("workstream").is_none(),
            "must not land as a sibling of name/arguments"
        );
    }

    /// Why: an MCP caller that already set its own `workstream`/`cwd`
    /// arguments (e.g. a non-bridge caller, or a test) must win -- the
    /// bridge's resolved identity is a default, not an override.
    /// What: pre-populates both fields in `arguments`; asserts they survive
    /// unchanged after injection with different bridge-resolved values.
    /// Test: itself.
    #[test]
    fn inject_caller_context_preserves_existing_caller_values() {
        let req = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "memory_remember",
                "arguments": {"text": "hello", "workstream": "caller-ws", "cwd": "/caller/cwd"}
            }
        });
        let out = inject_caller_context(req, Some("bridge-ws"), Some("/bridge/cwd"));
        assert_eq!(out["params"]["arguments"]["workstream"], "caller-ws");
        assert_eq!(out["params"]["arguments"]["cwd"], "/caller/cwd");
    }

    /// Why: when the bridge could resolve neither its own workstream nor
    /// cwd, the request must pass through byte-for-byte unmodified --
    /// matches `inject_default_palace_noop_when_none`'s contract.
    /// What: both `None`; asserts the request is unchanged.
    /// Test: itself.
    #[test]
    fn inject_caller_context_noop_when_nothing_resolved() {
        let req = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {"name": "memory_remember", "arguments": {"text": "hello"}}
        });
        let out = inject_caller_context(req.clone(), None, None);
        assert_eq!(out, req);
    }

    // -----------------------------------------------------------------------
    // the shared bridge, wired with this crate's rewriter (#6316)
    // -----------------------------------------------------------------------

    /// Why: `memory.chat` answers in many frames and MCP stdio writes one
    /// response per request. The two silent outcomes — returning the first
    /// token as the answer, or hanging — are what the refusal prevents. Both
    /// wire shapes are checked because a caller can name the method directly or
    /// wrap it in `tools/call`, and checking only the outer field would let the
    /// wrapped form through. The refusal lives in the shared bridge now, so
    /// this also asserts [`build_bridge`] hands it [`STREAMING_METHODS`].
    /// Test: itself.
    #[tokio::test]
    async fn streaming_method_is_refused_rather_than_half_answered() {
        // Nothing is serving, so a refusal that reached the wire would come
        // back as a transport error naming the socket instead.
        let bridge = dead_bridge(PathBuf::from("/nonexistent/trusty-memory.sock"));
        for req in [
            Request {
                jsonrpc: Some("2.0".to_string()),
                id: Some(json!(1)),
                method: "memory.chat".to_string(),
                params: Some(json!({})),
            },
            Request {
                jsonrpc: Some("2.0".to_string()),
                id: Some(json!(2)),
                method: "tools/call".to_string(),
                params: Some(json!({"name": "memory.chat", "arguments": {}})),
            },
        ] {
            let resp = bridge.answer(req).await;
            let message = resp
                .error
                .expect("the refusal is an answer, not a transport failure")
                .message;
            assert!(
                message.contains("stream"),
                "the refusal must say why: {message}"
            );
        }
    }

    /// Why (#6309, the fail-open check): a JSON-RPC client matches a response
    /// to its request by `id`, and `trusty_mcp::Response` omits the field
    /// entirely when the id is `None` — so an error built without one reaches
    /// the client as a frame belonging to no pending call, and the session
    /// reads as hung. A daemon that is up when [`ensure_daemon_up_for_stdio`]
    /// checks and gone by the time a request is forwarded must therefore
    /// produce an error carrying that request's id, never an empty result.
    /// What: forwards to a socket nothing is serving; asserts the answer is an
    /// error, carries the id, and has no result.
    /// Test: itself.
    #[tokio::test]
    async fn a_transport_failure_answers_the_request_that_caused_it() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let resp = dead_bridge(tmp.path().join("vanished.sock"))
            .answer(a_request_with_id(7))
            .await;

        assert!(resp.error.is_some(), "an unreachable daemon is an error");
        assert!(
            resp.result.is_none(),
            "an unreachable daemon must never read as an empty result"
        );
        assert_eq!(
            resp.id,
            Some(json!(7)),
            "the error must carry the id of the request it answers, or the \
             client never matches it and waits forever"
        );
        assert!(!resp.suppress, "a request with an id always gets a reply");
    }

    /// Why (#6309 acceptance): "a bridge whose daemon endpoint is gone returns
    /// an error within the backoff cap (≤30 s) on every request", and the error
    /// has to name the endpoint or the operator cannot tell an upgraded daemon
    /// from a wedged one.
    /// What: measures the vanished-socket round trip and asserts both the bound
    /// and that the socket path appears in the message.
    /// Test: itself.
    #[tokio::test]
    async fn a_transport_failure_names_the_endpoint_and_does_not_hang() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let socket = tmp.path().join("vanished.sock");

        let started = std::time::Instant::now();
        let resp = dead_bridge(socket.clone())
            .answer(a_request_with_id(11))
            .await;
        let elapsed = started.elapsed();

        let message = resp
            .error
            .expect("an unreachable daemon is an error")
            .message;
        assert!(
            message.contains(&socket.display().to_string()),
            "the error must name the endpoint it could not reach, got: {message}"
        );
        assert!(
            elapsed < Duration::from_secs(30),
            "a gone endpoint must answer inside the backoff cap: {elapsed:?}"
        );
    }

    /// Why: MCP spec §4.1 forbids a reply to a notification, and emitting one
    /// would corrupt the stdio channel. This is the one case where an absent id
    /// is correct, and it must be decided before the daemon is dialled.
    /// Test: itself.
    #[tokio::test]
    async fn a_notification_is_suppressed_without_dialling() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let resp = dead_bridge(tmp.path().join("absent.sock"))
            .answer(Request {
                jsonrpc: Some("2.0".to_string()),
                id: None,
                method: "notifications/initialized".to_string(),
                params: None,
            })
            .await;
        assert!(resp.suppress, "a notification must not be answered");
    }

    /// Why: [`inject_default_palace`] and [`inject_caller_context`] are tested
    /// above as pure functions, which proves nothing about whether
    /// [`build_bridge`] hands them to the bridge — and a rewriter that is never
    /// attached fails silently, with `--palace` and every attribution stamp
    /// quietly absent. This drives one real framed exchange and reads the
    /// envelope the daemon actually received.
    /// What: stands up a one-shot Unix listener that echoes the request back as
    /// its `result`, forwards a `tools/call` through the bridge, and asserts the
    /// forwarded arguments carry the palace, the workstream, the cwd, and a
    /// `jsonrpc` of `"2.0"` re-stamped after the rewriter ran.
    /// Test: itself.
    #[tokio::test]
    async fn the_rewriter_reaches_the_forwarded_envelope() {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("sockets").join("echo.sock");
        let listener = trusty_common::uds::bind_hardened(&socket).expect("bind the echo socket");

        // One connection, echoed back; the task dies with the test runtime.
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let Ok((mut conn, _)) = listener.accept().await else {
                return;
            };
            let mut frame = Vec::new();
            let _ = conn.read_to_end(&mut frame).await;
            let seen: serde_json::Value = serde_json::from_slice(&frame).unwrap_or_default();
            let body = json!({
                "jsonrpc": "2.0",
                "id": seen.get("id").cloned().unwrap_or_default(),
                "result": {"seen": seen},
            });
            let mut bytes = serde_json::to_vec(&body).unwrap_or_default();
            bytes.push(b'\n');
            let _ = conn.write_all(&bytes).await;
            let _ = conn.flush().await;
        });

        let bridge = build_bridge(
            socket,
            Some("owner-profile".to_string()),
            Some("feat-x".to_string()),
            Some("/x/.worktrees/feat-x".to_string()),
        );
        let resp = bridge.answer(a_request_with_id(3)).await;

        let result = resp.result.expect("the echo server answered");
        let seen = &result["seen"];
        assert_eq!(
            seen["jsonrpc"], "2.0",
            "jsonrpc is re-stamped after the rewriter"
        );
        assert_eq!(seen["params"]["arguments"]["palace"], "owner-profile");
        assert_eq!(seen["params"]["arguments"]["workstream"], "feat-x");
        assert_eq!(seen["params"]["arguments"]["cwd"], "/x/.worktrees/feat-x");
        assert_eq!(seen["params"]["arguments"]["text"], "anything");
    }
}
