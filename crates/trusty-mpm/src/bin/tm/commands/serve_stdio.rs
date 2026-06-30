//! `tm serve --stdio` — MCP stdio bridge for the trusty-mpm daemon (#1221).
//!
//! Why: Claude Code launches MCP servers as stdio subprocesses, but the
//! trusty-mpm daemon must be a durable 24/7 process (it runs the supervisor, the
//! reaper, the file watcher, and the Telegram bot — a stdio child would die with
//! its client). So, exactly like `trusty-memory serve --stdio`, this is a thin
//! PROXY: it ensures the HTTP daemon is up, then forwards every JSON-RPC request
//! to the daemon's loopback `POST /rpc` and relays the response. It never holds
//! daemon state itself; it exits when Claude Code closes the pipe, leaving the
//! daemon running.
//!
//! What: [`run_stdio_bridge`] (1) ensures the daemon is reachable via the shared
//! `trusty_common::mcp::ensure_daemon_up` helper (auto-starting `tm daemon`
//! detached if absent, polling the lock file for the real dynamic port); (2)
//! enters `run_stdio_loop`, forwarding each non-notification request to
//! `POST /rpc` and returning the daemon's response verbatim. Transport errors
//! become a JSON-RPC internal error rather than crashing the loop, so the bridge
//! survives a daemon restart and the next request reconnects.
//!
//! STDOUT hygiene: NEVER write to stdout — it is the JSON-RPC channel. All
//! diagnostics go to stderr (via `eprintln!` inside the shared helper).
//!
//! Test: `is_notification_*`, `value_to_mcp_response_*`, and
//! `forward_rpc_errors_on_refused` unit tests below; the daemon-side `POST /rpc`
//! dispatch is covered by `daemon::api_tests::rpc_*`.

use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use trusty_common::mcp::{self, DaemonBridgeConfig, Request, Response};

/// Per-request forwarding timeout (60 s — headroom for slow tmux/provision ops).
///
/// Why: a session spawn provisions a workspace (git clone) and a tmux host,
/// which can take many seconds; a generous ceiling prevents a slow-but-valid
/// call from being cut off while still capping a truly hung request.
/// Test: `forward_rpc_errors_on_refused` exercises the error path within bound.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// Build the shared reqwest client used for every forwarded RPC call.
///
/// Why: one client enables HTTP keep-alive to the daemon, reducing latency.
/// What: a blocking-free async client with the request + connect timeout set.
/// Test: `build_rpc_client_succeeds`.
fn build_rpc_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .connect_timeout(REQUEST_TIMEOUT)
        .build()
        .context("build reqwest client for trusty-mpm stdio bridge")
}

/// Build the `DaemonBridgeConfig` for the trusty-mpm stdio bridge.
///
/// Why: the daemon discovers its own (possibly ephemeral) port and records it in
/// `~/.trusty-mpm/daemon.lock`. `resolve_daemon_url(None)` re-reads that lock on
/// every call, so passing it as `base_url_fn` lets `ensure_daemon_up` track a
/// dynamic port as soon as the daemon writes the lock. Unlike trusty-memory
/// (which sets `no_spawn` to avoid squatting a redb write lock), the trusty-mpm
/// daemon is HTTP-only and singleton-guarded, so auto-spawn is safe and matches
/// the existing `tm start` behaviour.
/// What: returns a config that probes `/health`, auto-spawns `tm daemon` when
/// absent, and resolves the base URL from the lock file each poll.
/// Test: covered indirectly; `ensure_daemon_up` itself is unit-tested in
/// `trusty_common::mcp::daemon_bridge`.
fn build_bridge_config() -> DaemonBridgeConfig {
    DaemonBridgeConfig {
        service_name: "trusty-mpm".to_string(),
        // The daemon picks its own port and records it in the lock file; no addr
        // arg is passed (matches `tm start`).
        spawn_args: vec!["daemon".to_string()],
        health_path: "/health".to_string(),
        // INTENTIONALLY kept on the sync direct-lock resolver (#1849 Phase 2):
        // the stdio bridge calls this closure on every reconnect to track the
        // daemon's current port. The gateway resolver is async and requires a
        // reqwest::Client; the `base_url_fn` signature is synchronous. Routing
        // the MCP bridge through the gateway would also add latency on every
        // JSON-RPC call and create a dependency on the console being up — an
        // unacceptable regression for the core MCP path.
        base_url_fn: Box::new(|| trusty_mpm::core::resolve_daemon_url(None)),
        startup_timeout: None, // shared 30s default
        poll_interval: None,   // shared 500ms default
        no_spawn: false,
    }
}

/// Returns true if the request is a JSON-RPC notification.
///
/// Why: per the MCP spec (section 4.1) the server MUST NOT reply to a
/// notification. Suppression is decided from the REQUEST before forwarding —
/// forwarding a notification would make the daemon emit a response that the
/// bridge would write to stdout, corrupting the MCP channel.
/// What: returns true when `req.id` is absent OR the method begins with
/// `"notifications/"`.
/// Test: `is_notification_detects_missing_id_and_prefix`.
fn is_notification(req: &Request) -> bool {
    req.id.is_none() || req.method.starts_with("notifications/")
}

/// Convert an `mcp::Request` into the JSON value POSTed to the daemon.
///
/// Why: `forward_rpc` sends raw JSON; the request envelope must be serialised.
/// What: `serde_json::to_value`, falling back to an empty object (which the
/// daemon rejects with a parse error — the correct behaviour) on the impossible
/// serialisation failure.
/// Test: covered transitively by the forwarding path.
fn req_to_value(req: &Request) -> serde_json::Value {
    serde_json::to_value(req).unwrap_or_else(|_| serde_json::json!({}))
}

/// Convert the daemon's JSON-RPC response value into an `mcp::Response`.
///
/// Why: `run_stdio_loop` expects `mcp::Response`; the daemon returns a raw value
/// in the standard `{jsonrpc, id, result | error}` shape.
/// What: extracts `id`, returns `ok` when `result` is present, `err` when `error`
/// is present, or an internal error when the response is malformed.
/// Test: `value_to_mcp_response_maps_ok_err_and_malformed`.
fn value_to_mcp_response(v: serde_json::Value) -> Response {
    let id = v
        .get("id")
        .cloned()
        .and_then(|id| if id.is_null() { None } else { Some(id) });

    if let Some(result) = v.get("result").cloned() {
        return Response::ok(id, result);
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
        return Response::err(id, code, message);
    }
    Response::err(
        id,
        mcp::error_codes::INTERNAL_ERROR,
        "daemon returned a response with neither result nor error",
    )
}

/// POST one JSON-RPC request to `{base_url}/rpc` and return the response body.
///
/// Why: the core forwarding primitive — returns the daemon's response verbatim
/// so MCP clients see real tool output, not a bridge-generated stand-in.
/// What: serialises `req`, POSTs to `/rpc`, deserialises the JSON body.
/// Transport errors (refused, timeout) and non-2xx statuses become `Err`.
/// Test: `forward_rpc_errors_on_refused`.
async fn forward_rpc(
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
        .with_context(|| format!("POST {url}: connection to trusty-mpm daemon failed"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(anyhow!(
            "daemon returned HTTP {status} for POST /rpc: {body}"
        ));
    }

    resp.json::<serde_json::Value>()
        .await
        .context("deserialise JSON-RPC response from trusty-mpm daemon")
}

/// Run the trusty-mpm MCP stdio bridge.
///
/// Why: top-level entry point for `tm serve --stdio` (#1221). Mirrors the proven
/// `trusty-memory serve --stdio` pattern so the three daemons share one mental
/// model and the driver skill can call typed MCP tools instead of scraping CLI
/// output.
/// What: (1) ensures the daemon is up (auto-start + dynamic-port discovery via
/// the lock file); (2) builds a shared HTTP client; (3) enters `run_stdio_loop`,
/// suppressing notifications and forwarding every other request to `POST /rpc`,
/// re-resolving the daemon URL each request so a daemon restart on a new port is
/// picked up. A transport error returns a JSON-RPC internal error rather than
/// crashing the loop — the next request reconnects.
/// Test: the helpers are unit-tested below; the end-to-end forward path is
/// covered by the daemon's `rpc_*` integration tests.
pub(crate) async fn run_stdio_bridge() -> Result<()> {
    // Step 1: ensure the daemon is reachable (auto-start if needed). Hard error
    // on failure — no silent fallback. All output goes to stderr.
    let config = build_bridge_config();
    let _ = mcp::ensure_daemon_up(&config).await?;

    // Step 2: shared HTTP client.
    let client = build_rpc_client()?;

    // Step 3: forward loop. Re-resolve the base URL per request so a restarted
    // daemon (possibly on a new ephemeral port) is followed automatically.
    mcp::run_stdio_loop(move |req| {
        let client = client.clone();
        async move {
            if is_notification(&req) {
                return Response::suppressed();
            }
            let base_url = trusty_mpm::core::resolve_daemon_url(None);
            let req_value = req_to_value(&req);
            match forward_rpc(&client, &base_url, req_value).await {
                Ok(resp_value) => value_to_mcp_response(resp_value),
                Err(e) => {
                    tracing::warn!("trusty-mpm stdio bridge: transport error: {e:#}");
                    Response::err(
                        req.id.clone(),
                        mcp::error_codes::INTERNAL_ERROR,
                        format!("trusty-mpm daemon unreachable: {e:#}"),
                    )
                }
            }
        }
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn build_rpc_client_succeeds() {
        assert!(build_rpc_client().is_ok());
    }

    #[test]
    fn is_notification_detects_missing_id_and_prefix() {
        let normal = Request {
            jsonrpc: Some("2.0".into()),
            id: Some(json!(1)),
            method: "tools/list".into(),
            params: None,
        };
        assert!(!is_notification(&normal));

        let no_id = Request {
            jsonrpc: Some("2.0".into()),
            id: None,
            method: "tools/list".into(),
            params: None,
        };
        assert!(is_notification(&no_id));

        let prefixed = Request {
            jsonrpc: Some("2.0".into()),
            id: Some(json!(9)),
            method: "notifications/initialized".into(),
            params: None,
        };
        assert!(is_notification(&prefixed));
    }

    #[test]
    fn value_to_mcp_response_maps_ok_err_and_malformed() {
        let ok = value_to_mcp_response(json!({"jsonrpc":"2.0","id":1,"result":{"tools":[]}}));
        assert!(ok.error.is_none());
        assert_eq!(ok.id, Some(json!(1)));

        let err = value_to_mcp_response(
            json!({"jsonrpc":"2.0","id":2,"error":{"code":-32601,"message":"x"}}),
        );
        assert_eq!(err.error.unwrap().code, -32601);

        let bad = value_to_mcp_response(json!({"jsonrpc":"2.0","id":3}));
        assert_eq!(bad.error.unwrap().code, mcp::error_codes::INTERNAL_ERROR);

        let null_id = value_to_mcp_response(json!({"jsonrpc":"2.0","id":null,"result":{}}));
        assert_eq!(null_id.id, None);
    }

    #[test]
    fn req_to_value_round_trips_method() {
        let req = Request {
            jsonrpc: Some("2.0".into()),
            id: Some(json!(5)),
            method: "session_list".into(),
            params: None,
        };
        let v = req_to_value(&req);
        assert_eq!(v["method"], "session_list");
        assert_eq!(v["id"], 5);
    }

    #[tokio::test]
    async fn forward_rpc_errors_on_refused() {
        let client = build_rpc_client().expect("client");
        let result = forward_rpc(&client, "http://127.0.0.1:65534", json!({"method":"ping"})).await;
        assert!(result.is_err(), "must error when no daemon is listening");
    }
}
