//! Pure daemon-bridge for `trusty-memory serve --stdio` (issue #1078).
//!
//! Why: The prior `serve --stdio` path opened redb directly in the stdio
//! process. When the daemon holds the exclusive write lock the stdio process
//! fell back to a read-only snapshot, causing write failures and stale reads.
//! This module makes the stdio path a pure proxy: every JSON-RPC request is
//! forwarded to the running daemon and **the stdio process never opens redb**.
//! Nothing below may reach for a store — see #1078.
//!
//! What: `run_stdio_bridge` (1) ensures the daemon is running via
//! [`crate::commands::daemon_guard::ensure_daemon_running`], which starts it
//! under an exclusive lock if absent (#5267) and polls the socket for
//! readiness; (2) forwards each non-notification request over that socket and
//! returns the daemon response verbatim to the MCP client.
//!
//! ## What #6286 changed, and the two traps in it
//!
//! The forward hop was `POST {base_url}/rpc` on a `reqwest::Client`, against an
//! address read out of the `http_addr` discovery file. It is now one framed
//! JSON-RPC exchange on `trusty_common::daemon_socket_path("trusty-memory")`.
//! Because the bridge already forwarded a generic envelope byte-for-byte, this
//! is one function rather than a per-method rewrite.
//!
//! **`jsonrpc` is normalised to `"2.0"` before forwarding.** `trusty_mcp::
//! Request` declares `jsonrpc: Option<String>` and SERIALISES it as `null` when
//! the client omitted it. `crate::transport::rpc::JsonRpcRequest` tolerated
//! that; `trusty_common::uds::server::RpcRouter` does not — it refuses any
//! frame whose `jsonrpc` is not exactly `"2.0"` with `CODE_PARSE_ERROR`. So a
//! request that works today would break post-migration for a reason nothing in
//! its body explains. [`normalise_jsonrpc`] is the fix and
//! `forwarded_request_carries_jsonrpc_two_point_zero` is its regression test.
//!
//! **A streamed method is refused rather than mis-answered.** `memory.chat`
//! answers in many frames, and MCP stdio has no frame sequence to put them in:
//! `run_stdio_loop` writes exactly one response per request. Rather than
//! silently returning the first token, or hanging while the client waits for a
//! shape it cannot parse, the bridge refuses a `tools/call` naming a streaming
//! method with a JSON-RPC error that says so. See [`STREAMING_METHODS`].
//!
//! Caller-identity injection (DOC-53 §4.3, critical fix): the daemon this
//! bridge proxies to is ONE shared process serving every concurrently-
//! attached session — it cannot tell which caller a given request came from
//! except from what the request itself carries. THIS bridge process, by
//! contrast, is spawned fresh per `serve --stdio` invocation and genuinely does
//! run inside its caller's own process tree/environment.
//! `inject_caller_context` resolves this bridge's own workstream identity
//! ONCE at startup (via [`crate::attribution::resolve_own_workstream_name`])
//! and stamps it into every forwarded request's tool `arguments` (as
//! `workstream`, alongside the bridge's own `cwd`) — mirroring
//! `inject_default_palace`'s existing `--palace` injection — so the daemon's
//! MCP dispatch handlers (`tools::helpers::attach_mcp_attribution`,
//! `CreatorInfo::new_for_caller`) can attribute the write correctly without
//! ever reading their own (shared, meaningless-per-caller) environment.
//!
//! STDOUT hygiene: NEVER write to stdout -- it is the JSON-RPC channel.
//! All diagnostic output goes to stderr.
//!
//! Test: unit tests below; `tests/serve_stdio_e2e.rs` for the full e2e path.

use anyhow::{anyhow, Result};
use std::path::{Path, PathBuf};
use std::time::Duration;
use trusty_mcp as mcp;

/// Per-request forwarding timeout (60 s -- headroom for cold-start embedding).
///
/// Why: a generous ceiling stops one hung request blocking the stdio loop while
/// still letting a slow embedding operation finish.
/// Test: `forward_reports_a_dead_socket_rather_than_hanging`.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// Methods that answer in many frames and therefore cannot be bridged.
///
/// Why: MCP stdio is one response per request — `mcp::run_stdio_loop` writes
/// exactly one `Response` per `Request`, and there is no frame sequence to put a
/// token stream in. Three things could happen to a streamed call here, and two
/// of them are silent: return the first item as if it were the answer, or hang
/// while the client waits for a shape it will never get. This list is what makes
/// the third — an error that names the problem — the one that happens.
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

/// Rewrite a request's `jsonrpc` to `"2.0"` before it is forwarded (#6286).
///
/// Why: `mcp::Request` declares `jsonrpc: Option<String>` and serialises it as
/// `null` when the client omitted it. The old `POST /rpc` path fed
/// `crate::transport::rpc::JsonRpcRequest`, whose `jsonrpc` is `#[serde(default)]
/// Option<String>` and which never checked the value. `RpcRouter` does check,
/// and refuses anything that is not exactly `"2.0"` with `CODE_PARSE_ERROR` —
/// so without this a request that works today becomes a parse error afterwards,
/// for a reason nothing in its body explains.
///
/// What: sets the field unconditionally. A client that sent `"2.0"` is
/// unchanged; one that sent `null`, omitted it, or sent something else gets the
/// only version this transport speaks. Rewriting rather than refusing is
/// deliberate — the bridge's contract with its client is unchanged by ADR-0032,
/// and a version the client never set is not a thing to fail it on.
///
/// Test: `forwarded_request_carries_jsonrpc_two_point_zero`,
/// `forwarded_request_normalises_an_absent_jsonrpc`.
fn normalise_jsonrpc(mut req: serde_json::Value) -> serde_json::Value {
    if let Some(obj) = req.as_object_mut() {
        obj.insert(
            "jsonrpc".to_string(),
            serde_json::Value::String("2.0".to_string()),
        );
    }
    req
}

/// The method a request will actually run, seeing through the `tools/call`
/// envelope.
///
/// Why: a streamed method can arrive either way — as `{"method": "memory.chat"}`
/// from a direct caller, or wrapped as `tools/call` with `params.name`. Checking
/// only the outer field would let the wrapped form through.
fn effective_method(req: &serde_json::Value) -> Option<&str> {
    let method = req.get("method")?.as_str()?;
    if method == "tools/call" {
        return req
            .get("params")
            .and_then(|p| p.get("name"))
            .and_then(|n| n.as_str())
            .or(Some(method));
    }
    Some(method)
}

/// Forward one JSON-RPC request over the daemon's socket and return its answer.
///
/// Why: the core forwarding primitive. It returns the daemon's response
/// verbatim so an MCP client sees the real tool output rather than a
/// bridge-generated error.
///
/// What: normalises `jsonrpc`, refuses a streaming method (see
/// [`STREAMING_METHODS`]), then writes one frame and reads one back. A transport
/// failure — nothing serving, a timeout — is `Err`; a daemon that answered with
/// a JSON-RPC error is `Ok`, because that error is the answer and belongs to the
/// client unaltered.
///
/// # Errors
///
/// Only transport failures. See above for why a refusal is not one.
///
/// Test: `forward_reports_a_dead_socket_rather_than_hanging`,
/// `streaming_method_is_refused_rather_than_half_answered`.
pub(crate) async fn forward_rpc(
    socket: &Path,
    req: serde_json::Value,
) -> Result<serde_json::Value> {
    if let Some(method) = effective_method(&req) {
        if STREAMING_METHODS.contains(&method) {
            return Ok(serde_json::json!({
                "jsonrpc": "2.0",
                "id": req.get("id").cloned().unwrap_or(serde_json::Value::Null),
                "error": {
                    "code": mcp::error_codes::INVALID_REQUEST,
                    "message": format!(
                        "{method} answers as a stream, which MCP stdio cannot carry \
                         (one response per request). Dial {} directly with a framed \
                         streaming client to read it.",
                        socket.display()
                    ),
                },
            }));
        }
    }

    let req = normalise_jsonrpc(req);
    let response: trusty_common::uds::server::RpcResponse =
        trusty_common::uds::send_framed_request_capped(
            socket,
            &req,
            REQUEST_TIMEOUT,
            crate::transport::uds::MAX_FRAME_BYTES,
        )
        .await
        .map_err(|e| anyhow!("connection to the trusty-memory daemon failed: {e}"))?;

    serde_json::to_value(response).map_err(|e| anyhow!("re-encode the daemon response: {e}"))
}

/// Ensure the daemon is running and return the socket to forward on.
///
/// Why (#5267, superseding the `no_spawn: true` posture of #1152): a bridge
/// whose daemon is merely not running should start it, not hard-error. #1152 was
/// an AUTO-SPAWN outage — every bridge independently spawning its own daemon, N
/// bridges producing N daemons racing for redb's write lock. What this does
/// instead is start-if-not-running: the daemon's existence is ensured ONCE,
/// under an exclusive lock, so seven bridges converge on one.
///
/// What (#6286): `trusty_mcp::ensure_daemon_up_single_flight` is no longer
/// reachable from here — its `DaemonBridgeConfig` is built around a `health_path`
/// and a `base_url_fn`, and this daemon has neither. The exclusion it provided
/// is preserved in [`crate::commands::daemon_guard::ensure_daemon_running`],
/// which takes the same `StartLock` on the same lock file `trusty-memory start`
/// uses, so the two paths still cannot race each other. Fails closed if the
/// daemon does not become ready.
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

/// Returns true if the request is a JSON-RPC notification.
///
/// Why: the MCP spec (section 4.1) forbids sending any response for a
/// notification. Suppression must be decided from the REQUEST before forwarding
/// to the daemon -- if we forwarded notifications, the daemon would return a
/// valid `initialize`-like response and the bridge would emit it to stdout,
/// corrupting the MCP channel. This predicate is the single canonical check: a
/// notification has no `id` field, and/or its method begins with
/// `"notifications/"`.
/// What: returns true when `req.id` is `None` (absent in the wire JSON) or
/// the method starts with `"notifications/"`.
/// Test: `notification_requests_are_suppressed` unit test.
fn is_notification(req: &mcp::Request) -> bool {
    req.id.is_none() || req.method.starts_with("notifications/")
}

/// Run the MCP stdio bridge.
///
/// Why: this is the top-level entry point for `trusty-memory serve --stdio`
/// under the daemon-bridge architecture (issue #1078). The prior direct-store
/// path opened redb in the stdio process and hit the write-lock exclusion
/// problem; this path never touches the store at all.
/// What: (1) ensures the daemon is running under an exclusive lock with a 30 s
/// readiness budget (#5267); (2) enters `run_stdio_loop` -- for each JSON-RPC
/// request, detects and suppresses notifications (per MCP spec section 4.1),
/// then forwards non-notification requests over the daemon's socket and returns
/// the response verbatim. Hard-errors if the daemon cannot start.
/// Test: `tests/serve_stdio_e2e.rs` spawns a real child, asserts bounded
/// responses. Bridge-specific unit tests live in this module.
pub async fn run_stdio_bridge(palace: Option<String>) -> Result<()> {
    // Step 1: ensure the daemon is up. All output from this goes to stderr.
    // Failure here is a hard error -- no silent fallback.
    let socket = ensure_daemon_up_for_stdio().await?;

    // If a --palace default was supplied, forward it in every request via the
    // `palace` field in the JSON-RPC `params`. We inject it only when the
    // caller doesn't already include one.
    let default_palace = palace;

    // DOC-53 §4.3 (critical fix): resolve THIS bridge process' own identity
    // once at startup -- see the module doc comment for why this process
    // (unlike the shared daemon it proxies to) is the correct place to read
    // `TM_WORKSTREAM_NAME`/cwd. Resolved once because neither changes for
    // the lifetime of a `serve --stdio` process, mirroring `default_palace`
    // above.
    let caller_cwd = std::env::current_dir()
        .ok()
        .map(|p| p.to_string_lossy().into_owned());
    let caller_workstream = crate::attribution::resolve_own_workstream_name(caller_cwd.as_deref());

    // Step 2: enter the stdio loop. Every non-notification request is
    // forwarded to the daemon. Notifications are suppressed here (per MCP
    // spec section 4.1 -- the server MUST NOT reply to a notification).
    //
    // There is no client to build any more: each forward dials the socket for
    // one exchange and closes it. That is deliberate rather than a regression
    // of the HTTP keep-alive it replaces — the daemon spawns a task per
    // connection, so a per-request connection is what lets concurrent bridges
    // stay concurrent, and a local UDS connect costs microseconds.
    let result = mcp::run_stdio_loop(move |req| {
        let socket = socket.clone();
        let default_palace = default_palace.clone();
        let caller_cwd = caller_cwd.clone();
        let caller_workstream = caller_workstream.clone();

        async move {
            // Decide suppression from the REQUEST before touching the daemon.
            // MCP spec section 4.1: a notification has no id -- the server MUST NOT
            // reply. Forwarding the notification to the daemon would cause
            // the daemon to return a response that we'd emit to stdout,
            // corrupting the MCP channel.
            if is_notification(&req) {
                return mcp::Response::suppressed();
            }

            // Serialise the MCP request envelope into the value we'll POST.
            // We need to potentially inject a default palace into params,
            // then this bridge's own resolved caller identity (DOC-53 §4.3)
            // into the tool arguments so the daemon never has to guess.
            let req_value = inject_default_palace(req_to_value(&req), default_palace.as_deref());
            let req_value = inject_caller_context(
                req_value,
                caller_workstream.as_deref(),
                caller_cwd.as_deref(),
            );

            match forward_rpc(&socket, req_value).await {
                Ok(resp_value) => value_to_mcp_response(resp_value),
                Err(e) => {
                    // Transport-level failure (daemon down, timeout).
                    // Return a JSON-RPC internal error rather than crashing
                    // the loop -- the next request might succeed once the daemon
                    // recovers.
                    tracing::warn!("daemon bridge: transport error: {e:#}");
                    mcp::Response::err(
                        None,
                        mcp::error_codes::INTERNAL_ERROR,
                        format!("trusty-memory daemon unreachable: {e:#}"),
                    )
                }
            }
        }
    })
    .await;

    result
}

/// Convert a `trusty_mcp::Request` to a `serde_json::Value`.
///
/// Why: `forward_rpc` sends raw JSON to the daemon; the mcp::Request struct
/// must be serialised first. Infallible because `mcp::Request` is always
/// serialisable.
/// What: uses `serde_json::to_value` and falls back to an empty object (which
/// the daemon will reject with a parse error, but that's the correct behavior).
/// Test: covered transitively by `forward_rpc_roundtrip`.
fn req_to_value(req: &mcp::Request) -> serde_json::Value {
    serde_json::to_value(req).unwrap_or_else(|_| serde_json::json!({}))
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
/// non-notification request forwarded to `/rpc` gets this stamp, mirroring
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

/// Convert the daemon's JSON-RPC response value into a `mcp::Response`.
///
/// Why: `run_stdio_loop` expects `mcp::Response`; the daemon returns a raw
/// `serde_json::Value` which we must map. The daemon always returns the
/// standard JSON-RPC 2.0 shape `{jsonrpc, id, result | error}`.
/// What: extracts `id`, then returns `mcp::Response::ok` if `result` is
/// present, `mcp::Response::err` if `error` is present, or an internal error
/// if neither.
/// Test: `value_to_mcp_response_ok`, `value_to_mcp_response_err`,
/// `value_to_mcp_response_malformed`.
pub(crate) fn value_to_mcp_response(v: serde_json::Value) -> mcp::Response {
    let id = v.get("id").cloned().filter(|id| !id.is_null());

    if let Some(result) = v.get("result").cloned() {
        return mcp::Response::ok(id, result);
    }

    if let Some(err) = v.get("error") {
        let code = err
            .get("code")
            .and_then(|c| c.as_i64())
            .map(|c| c as i32)
            .unwrap_or(mcp::error_codes::INTERNAL_ERROR);
        let message = err
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("unknown daemon error")
            .to_string();
        return mcp::Response::err(id, code, &message);
    }

    // Neither result nor error -- malformed response from daemon.
    mcp::Response::err(
        id,
        mcp::error_codes::INTERNAL_ERROR,
        "daemon returned a response with neither result nor error",
    )
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
    // value_to_mcp_response
    // -----------------------------------------------------------------------
    /// Why: ok/err/malformed/null-id responses must map correctly.
    /// Test: this test.
    #[test]
    fn value_to_mcp_response_variants() {
        // ok path
        let ok = value_to_mcp_response(json!({"jsonrpc":"2.0","id":42,"result":{"tools":[]}}));
        assert!(!ok.suppress);
        assert_eq!(ok.id, Some(json!(42)));
        assert!(ok.error.is_none());
        // err path
        let err = value_to_mcp_response(
            json!({"jsonrpc":"2.0","id":7,"error":{"code":-32601,"message":"Not found"}}),
        );
        assert_eq!(err.error.unwrap().code, -32601);
        // malformed -- neither result nor error
        let bad = value_to_mcp_response(json!({"jsonrpc":"2.0","id":1}));
        assert_eq!(bad.error.unwrap().code, mcp::error_codes::INTERNAL_ERROR);
        // null id -> None
        let null_id = value_to_mcp_response(json!({"jsonrpc":"2.0","id":null,"result":{}}));
        assert_eq!(null_id.id, None);
    }

    // -----------------------------------------------------------------------
    // is_notification
    // -----------------------------------------------------------------------
    /// Why: notifications must be suppressed so the bridge never emits a
    /// response for them -- that would corrupt the MCP stdio channel.
    /// Test: this test.
    #[test]
    fn notification_requests_are_suppressed() {
        // Normal request with id -- not a notification.
        let normal = mcp::Request {
            jsonrpc: Some("2.0".to_string()),
            id: Some(json!(1)),
            method: "tools/list".to_string(),
            params: None,
        };
        assert!(!is_notification(&normal));
        // No id -> notification.
        let notif = mcp::Request {
            jsonrpc: Some("2.0".to_string()),
            id: None,
            method: "notifications/initialized".to_string(),
            params: None,
        };
        assert!(is_notification(&notif));
        // notifications/ prefix even with id -> notification.
        let notif_with_id = mcp::Request {
            jsonrpc: Some("2.0".to_string()),
            id: Some(json!(99)),
            method: "notifications/cancelled".to_string(),
            params: None,
        };
        assert!(is_notification(&notif_with_id));
    }

    // -----------------------------------------------------------------------
    // jsonrpc normalisation (#6286)
    // -----------------------------------------------------------------------

    /// Why: this is the trap the migration introduced. `mcp::Request`
    /// serialises `jsonrpc: null` when the client omitted the field, the old
    /// `POST /rpc` path never checked it, and `RpcRouter` refuses anything that
    /// is not exactly `"2.0"`. Without normalisation a request that works today
    /// becomes `CODE_PARSE_ERROR` afterwards.
    /// What: serialises a real `mcp::Request` with `jsonrpc: None` — the exact
    /// value a client that omits the field produces — and asserts the forwarded
    /// object carries `"2.0"`.
    /// Test: itself.
    #[test]
    fn forwarded_request_carries_jsonrpc_two_point_zero() {
        let req = mcp::Request {
            jsonrpc: None,
            id: Some(json!(1)),
            method: "tools/list".to_string(),
            params: None,
        };
        let raw = req_to_value(&req);
        assert!(
            raw["jsonrpc"].is_null(),
            "the fixture must reproduce the null this fix exists for, got {raw}"
        );
        let forwarded = normalise_jsonrpc(raw);
        assert_eq!(forwarded["jsonrpc"], "2.0");
        assert_eq!(forwarded["method"], "tools/list", "nothing else changes");
        assert_eq!(forwarded["id"], json!(1));
    }

    /// Why: a client that sends a wrong version, or none at all, must still be
    /// forwarded rather than refused — the bridge's contract with its client is
    /// unchanged by ADR-0032, and a version the client never set is not a thing
    /// to fail it on.
    /// Test: itself.
    #[test]
    fn forwarded_request_normalises_an_absent_jsonrpc() {
        let absent = normalise_jsonrpc(json!({"id": 1, "method": "ping"}));
        assert_eq!(absent["jsonrpc"], "2.0");
        let wrong = normalise_jsonrpc(json!({"jsonrpc": "1.0", "id": 1, "method": "ping"}));
        assert_eq!(wrong["jsonrpc"], "2.0");
    }

    // -----------------------------------------------------------------------
    // streaming refusal
    // -----------------------------------------------------------------------

    /// Why: `memory.chat` answers in many frames and MCP stdio writes one
    /// response per request. The two silent outcomes — returning the first
    /// token as the answer, or hanging — are what this refusal prevents. Both
    /// wire shapes are checked because a caller can name the method directly or
    /// wrap it in `tools/call`, and checking only the outer field would let the
    /// wrapped form through.
    /// Test: itself.
    #[tokio::test]
    async fn streaming_method_is_refused_rather_than_half_answered() {
        let socket = std::path::Path::new("/nonexistent/trusty-memory.sock");
        for req in [
            json!({"jsonrpc": "2.0", "id": 1, "method": "memory.chat", "params": {}}),
            json!({
                "jsonrpc": "2.0", "id": 2, "method": "tools/call",
                "params": {"name": "memory.chat", "arguments": {}}
            }),
        ] {
            // The socket does not exist, so a refusal that reached the wire
            // would be a transport error instead.
            let answer = forward_rpc(socket, req)
                .await
                .expect("the refusal is an answer, not a transport failure");
            let message = answer["error"]["message"]
                .as_str()
                .expect("the refusal must carry a message");
            assert!(
                message.contains("stream"),
                "the refusal must say why: {message}"
            );
        }
    }

    /// Why: every other method must still reach the wire, and a dead socket has
    /// to be reported promptly rather than by waiting out the 60 s budget — the
    /// stdio loop is single-file, so a hung forward blocks every later request.
    /// Test: itself.
    #[tokio::test]
    async fn forward_reports_a_dead_socket_rather_than_hanging() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let started = std::time::Instant::now();
        let result = forward_rpc(
            &tmp.path().join("absent.sock"),
            json!({"jsonrpc": "2.0", "id": 1, "method": "ping"}),
        )
        .await;
        assert!(result.is_err(), "no listener means no answer");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "a refused dial must not wait out REQUEST_TIMEOUT: {:?}",
            started.elapsed()
        );
    }
}
