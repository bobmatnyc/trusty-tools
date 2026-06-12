//! Pure daemon-bridge for `trusty-memory serve --stdio` (issue #1078).
//!
//! Why: The prior `serve --stdio` path opened redb directly in the stdio
//! process.  When the HTTP daemon holds the exclusive write lock the stdio
//! process fell back to a read-only snapshot, causing write failures and
//! stale reads.  This module makes the stdio path a pure proxy: every
//! JSON-RPC request is forwarded to `POST /rpc` on the running daemon;
//! the stdio process never opens redb.
//!
//! What: `run_stdio_bridge` (1) ensures the daemon is running via the shared
//! `trusty_common::mcp::ensure_daemon_up` helper (auto-starting it detached
//! if absent, polling the `http_addr` file for the real dynamic port);
//! (2) forwards each non-notification request to `POST /rpc` on the daemon
//! and returns the daemon response verbatim to the MCP client.
//!
//! STDOUT hygiene: NEVER write to stdout -- it is the JSON-RPC channel.
//! All diagnostic output goes to stderr.
//!
//! Test: unit tests below; `tests/serve_stdio_e2e.rs` for the full e2e path.

use anyhow::{anyhow, Context, Result};
use std::time::Duration;
use trusty_common::mcp::{self, DaemonBridgeConfig};

use crate::commands::daemon_guard::daemon_base_url;

/// Per-request forwarding timeout (60 s -- headroom for cold-start embedding).
///
/// Why: generous ceiling prevents a single hung request from blocking the
/// stdio loop, while still allowing slow embedding operations to complete.
/// Test: `forward_rpc_returns_error_on_connection_refused`.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// Build the shared reqwest client used for every forwarded RPC call.
///
/// Why: one client enables HTTP keep-alive to the daemon, reducing latency.
/// Test: `build_rpc_client_succeeds`.
pub(crate) fn build_rpc_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .connect_timeout(REQUEST_TIMEOUT)
        .build()
        .context("build reqwest client for daemon-bridge")
}

/// POST one JSON-RPC request to `{base_url}/rpc` and return the response body.
///
/// Why: the core forwarding primitive -- returns the daemon's response verbatim
/// so MCP clients see the real tool output, not a bridge-generated error.
/// What: serialises `req`, POSTs to `/rpc`, deserialises response body.
/// Transport errors (refused, timeout) become `Err`.
/// Test: `forward_rpc_returns_error_on_connection_refused`.
pub(crate) async fn forward_rpc(
    client: &reqwest::Client,
    base_url: &str,
    req: serde_json::Value,
) -> Result<serde_json::Value> {
    let url = format!("{base_url}/rpc");
    let resp = client
        .post(&url)
        .json(&req)
        .send()
        .await
        .with_context(|| format!("POST {url}: connection to daemon failed"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(anyhow!(
            "daemon returned HTTP {status} for POST /rpc: {body}"
        ));
    }

    resp.json::<serde_json::Value>()
        .await
        .context("deserialise JSON-RPC response from daemon")
}

/// Build the `DaemonBridgeConfig` for the trusty-memory stdio bridge.
///
/// Why: the shared `ensure_daemon_up` helper is parameterised by a config
/// struct so it can serve trusty-memory, trusty-search, and trusty-analyze
/// without duplication. This function encapsulates the trusty-memory-specific
/// values: health path `/health`, spawn args `serve --foreground --http
/// 127.0.0.1:0` (dynamic port, avoids EADDRINUSE against the real daemon),
/// and a `base_url_fn` that re-reads the `http_addr` file on every call.
/// What: returns a `DaemonBridgeConfig` ready for `ensure_daemon_up`.
/// Test: `ensure_daemon_up` unit tests in `trusty_common::mcp::daemon_bridge`.
fn build_bridge_config() -> DaemonBridgeConfig {
    DaemonBridgeConfig {
        service_name: "trusty-memory".to_string(),
        // `--foreground` skips the self-spawn shortcut in the daemon entry
        // point. `--http 127.0.0.1:0` lets the OS pick a free port, avoiding
        // EADDRINUSE when the real daemon holds :7070. The daemon writes the
        // OS-assigned port to its `http_addr` file; our `base_url_fn` re-reads
        // that file on each poll iteration so the bridge discovers the port
        // as soon as it is available.
        spawn_args: vec![
            "serve".to_string(),
            "--foreground".to_string(),
            "--http".to_string(),
            "127.0.0.1:0".to_string(),
        ],
        health_path: "/health".to_string(),
        base_url_fn: Box::new(daemon_base_url),
        startup_timeout: None, // use the shared 30s default
        poll_interval: None,   // use the shared 500ms default
    }
}

/// Ensure the trusty-memory daemon is running and return its live base URL.
///
/// Why: thin wrapper around the shared `ensure_daemon_up` helper that supplies
/// the trusty-memory-specific `DaemonBridgeConfig`. Kept as a named function so
/// `run_stdio_bridge` reads cleanly and the config details are isolated.
/// What: delegates entirely to `trusty_common::mcp::ensure_daemon_up`.
/// Test: e2e coverage in `tests/serve_stdio_e2e.rs`.
pub(crate) async fn ensure_daemon_up_for_stdio() -> Result<String> {
    let config = build_bridge_config();
    trusty_common::mcp::ensure_daemon_up(&config).await
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
/// What: (1) ensures the daemon is running via the shared `ensure_daemon_up`
/// helper (auto-start with 30 s budget); (2) builds a shared reqwest client;
/// (3) enters `run_stdio_loop` -- for each JSON-RPC request, detects and
/// suppresses notifications (per MCP spec section 4.1), then forwards
/// non-notification requests to `POST /rpc` on the daemon and returns the
/// response verbatim. Hard-errors if the daemon cannot start.
/// Test: `tests/serve_stdio_e2e.rs` spawns a real child, asserts bounded
/// responses. Bridge-specific unit tests live in this module.
pub async fn run_stdio_bridge(palace: Option<String>) -> Result<()> {
    // Step 1: ensure the daemon is up. All output from this goes to stderr.
    // Failure here is a hard error -- no silent fallback.
    let base_url = ensure_daemon_up_for_stdio().await?;

    // If a --palace default was supplied, forward it in every request via the
    // `palace` field in the JSON-RPC `params`. We inject it only when the
    // caller doesn't already include one.
    let default_palace = palace;

    // Step 2: build the shared HTTP client once.
    let client = build_rpc_client()?;

    // Step 3: enter the stdio loop. Every non-notification request is
    // forwarded to the daemon. Notifications are suppressed here (per MCP
    // spec section 4.1 -- the server MUST NOT reply to a notification).
    let result = mcp::run_stdio_loop(move |req| {
        let client = client.clone();
        let base_url = base_url.clone();
        let default_palace = default_palace.clone();

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
            // We need to potentially inject a default palace into params.
            let req_value = inject_default_palace(req_to_value(&req), default_palace.as_deref());

            match forward_rpc(&client, &base_url, req_value).await {
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

/// Convert a `trusty_common::mcp::Request` to a `serde_json::Value`.
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

/// Inject `default_palace` into a JSON-RPC request's params when the caller
/// hasn't already specified a `palace` field.
///
/// Why: `serve --stdio --palace <name>` should behave the same for the bridge
/// path as it did for the direct-store path -- every tool call that accepts a
/// `palace` parameter should see the default. We inject it at the envelope
/// level here, avoiding per-tool-handler coupling.
/// What: if `params` is a JSON object and has no `palace` key, adds
/// `"palace": <default_palace>`. If params is null/absent, wraps it in an
/// object `{"palace": default_palace}`. Leaves the value unchanged if params
/// already contains `palace` or if `default_palace` is None.
/// Test: `inject_default_palace_adds_when_absent`, `inject_default_palace_preserves_existing`.
fn inject_default_palace(
    mut req: serde_json::Value,
    default_palace: Option<&str>,
) -> serde_json::Value {
    let Some(palace) = default_palace else {
        return req;
    };

    // Find or create the params object.
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

    // Only inject if the caller didn't already specify a palace.
    if params.get("palace").is_none() {
        params["palace"] = serde_json::Value::String(palace.to_string());
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
    let id = v
        .get("id")
        .cloned()
        .and_then(|id| if id.is_null() { None } else { Some(id) });

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
    // forward_rpc
    // -----------------------------------------------------------------------
    /// Why: `forward_rpc` against a refused port must return `Err`, not hang.
    /// Test: this test.
    #[tokio::test]
    async fn forward_rpc_returns_error_on_connection_refused() {
        let client = build_rpc_client().expect("build client");
        let result =
            forward_rpc(&client, "http://127.0.0.1:65534", json!({"method": "ping"})).await;
        assert!(result.is_err(), "should fail when no server is listening");
    }

    /// Why: `build_rpc_client` must succeed in all test environments.
    /// Test: this test.
    #[test]
    fn build_rpc_client_succeeds() {
        assert!(build_rpc_client().is_ok());
    }
}
