//! JSON-RPC 2.0 / MCP stdio service for trusty-review.
//!
//! Why: Claude Code speaks MCP over stdio; this module lets operators wire
//! trusty-review into Claude Code via `.mcp.json` without running the HTTP
//! daemon.  The stdio path reuses the same `AppState` and pipeline as the HTTP
//! daemon, so behaviour is identical.
//!
//! What: `run(state)` builds the shared state once and calls
//! `trusty_common::mcp::run_stdio_loop`, which reads JSON-RPC requests
//! line-by-line from stdin and writes responses to stdout.  `run_deferred`
//! answers `initialize` immediately and builds `AppState` lazily so the
//! startup handshake is never delayed by provider/network calls (issue #1739).
//! `dispatch` handles `initialize`, `notifications/initialized`, `tools/list`,
//! `tools/call`, and bare method names.  All tracing goes to stderr; stdout is
//! the transport.
//!
//! Test: `dispatch_initialize_returns_server_info`,
//! `dispatch_tools_list_returns_three_tools`,
//! `dispatch_unknown_tool_returns_method_not_found`,
//! `dispatch_notification_is_suppressed`,
//! `deferred_dispatch_initialize_needs_no_state`,
//! `deferred_dispatch_tool_call_when_ready`.

pub mod console_metrics;
pub mod tools;

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use serde_json::Value;
use tokio::sync::watch;
use tracing::{debug, info, warn};

use trusty_common::mcp::{Request, Response, error_codes, initialize_response, run_stdio_loop};

use crate::config::ReviewConfig;
use crate::integrations::{analyze_client::HttpAnalyzeClient, search_client::HttpSearchClient};
use crate::llm::build_provider;
use crate::mcp::tools::{ToolError, call_tool, tool_descriptors, wrap_tool_error};
use crate::service::AppState;

// Re-export the reusable surface an embedding host (e.g. trusty-analyze, #630)
// needs to delegate into the trusty-review pipeline without depending on the
// binary's serve assembly: the tool descriptors, the `tools/call` router, the
// router's error type, and the shared `AppState`.
pub use crate::mcp::tools::{ToolError as ReviewToolError, call_tool as call_review_tool};
pub use crate::service::AppState as ReviewAppState;

// ─── Entry point ─────────────────────────────────────────────────────────────

/// Start the MCP stdio JSON-RPC loop using the provided `AppState`.
///
/// Why: the `serve --stdio` CLI path needs a single async entry-point that
/// accepts requests on stdin and writes responses to stdout until EOF.
/// What: wraps `AppState` in an `Arc` (so it can be shared across the async
/// closure without lifetime issues), then calls `run_stdio_loop` from
/// `trusty-common` which owns the parse/dispatch/flush cycle.
/// Test: `run` is side-effectful (reads stdin); coverage comes from unit tests
/// on `dispatch` with synthetic requests.
pub async fn run(state: AppState) -> Result<()> {
    let state = Arc::new(state);
    run_stdio_loop(move |req| {
        let state = Arc::clone(&state);
        async move { dispatch(req, &state).await }
    })
    .await
}

// ─── Deferred-startup entry point (issue #1739) ──────────────────────────────

/// The value type carried by the deferred-state watch channel.
///
/// Why: the MCP stdio loop must answer `initialize` before `AppState` is ready
/// (the background build may take 7+ seconds on broken Bedrock creds).  This
/// three-state value lets the dispatcher distinguish "still building" (`None`),
/// "ready" (`Some(Ok(…))`), and "failed" (`Some(Err(reason))`).  `Arc`
/// avoids cloning all 15+ `String` fields of `AppState` on every tool call —
/// the dispatcher does one atomic refcount bump per call instead.
/// What: `None` on channel creation; the background task sends
/// `Some(Ok(Arc::new(state)))` or `Some(Err(reason))` when the build completes.
/// Test: `deferred_dispatch_*` tests in `mcp_tests.rs` cover all three states.
pub type DeferredStateValue = Option<Result<Arc<AppState>, String>>;

/// Start the MCP stdio loop with deferred `AppState` construction (issue #1739).
///
/// Why: the MCP `initialize` handshake has a ~1.5 s deadline — Claude Code
/// marks the server DEGRADED if the response is not received in time.
/// `build_app_state` can take 7+ seconds when Bedrock credentials are broken.
/// This entry-point answers `initialize` in <1 ms and defers provider/liveness
/// calls to a background task, removing the startup race entirely.
/// What: calls `run_stdio_loop` with `dispatch_deferred`, which handles protocol-
/// level methods (`initialize`, `notifications/initialized`, `tools/list`)
/// synchronously and awaits `state_rx` (with a 30 s timeout) for tool calls.
/// The caller is responsible for spawning the background build and feeding
/// results into the matching `watch::Sender<DeferredStateValue>`.
/// Test: `deferred_dispatch_initialize_needs_no_state`,
/// `deferred_dispatch_tool_call_when_ready`,
/// `deferred_dispatch_tool_call_when_build_failed`,
/// `deferred_dispatch_tool_call_when_sender_dropped` in `mcp_tests.rs`.
pub async fn run_deferred(state_rx: watch::Receiver<DeferredStateValue>) -> Result<()> {
    run_stdio_loop(move |req| {
        let rx = state_rx.clone();
        async move { dispatch_deferred(req, rx).await }
    })
    .await
}

/// Dispatch an MCP request when `AppState` is being built lazily in the
/// background.
///
/// Why: see `run_deferred` — the dispatcher must answer `initialize` and
/// `tools/list` immediately (they need no `AppState`), while tool calls can
/// legitimately wait for the background build to finish.
/// What: routes protocol-level methods without touching `state_rx`; for all
/// other methods it awaits `state_rx.wait_for(|v| v.is_some())` with a
/// 30-second timeout and then delegates to the regular `dispatch`.  A
/// graceful JSON-RPC error is returned if the build times out or fails instead
/// of panicking or hanging.
/// Test: see `run_deferred` test list above.
pub(crate) async fn dispatch_deferred(
    req: Request,
    mut state_rx: watch::Receiver<DeferredStateValue>,
) -> Response {
    let is_notification = req.id.is_none();
    let id = req.id.clone();

    if req.jsonrpc.as_deref() != Some("2.0") {
        if is_notification {
            return Response::suppressed();
        }
        return Response::err(id, error_codes::INVALID_REQUEST, "jsonrpc must be \"2.0\"");
    }

    // Protocol-level methods that need no AppState → answered immediately,
    // BEFORE waiting for the background build.  This is the invariant that
    // makes the initialize handshake fast even with broken credentials.
    match req.method.as_str() {
        "initialize" => {
            return Response::ok(
                id,
                initialize_response("trusty-review", env!("CARGO_PKG_VERSION"), None),
            );
        }
        "notifications/initialized" | "initialized" => {
            return Response::suppressed();
        }
        "tools/list" => {
            return Response::ok(id, serde_json::json!({ "tools": tool_descriptors() }));
        }
        _ => {}
    }

    // All other methods require AppState.  Await the background build with a
    // generous timeout — a well-provisioned host should finish in < 5 s even
    // with liveness probes; 30 s guards against severe but recoverable delays.
    //
    // IMPORTANT: extract an owned value inside a scoped block so the non-Send
    // watch::Ref (RwLockReadGuard) is definitively dropped before the next
    // .await point — required for the enclosing future to satisfy Send.
    const WARMUP_TIMEOUT: Duration = Duration::from_secs(30);
    let state_outcome: Result<Arc<AppState>, String> = {
        let timed = tokio::time::timeout(WARMUP_TIMEOUT, state_rx.wait_for(|v| v.is_some())).await;
        match timed {
            Err(_elapsed) => {
                warn!(
                    method = req.method.as_str(),
                    "tool call received before AppState was ready (30 s warmup timeout)"
                );
                return Response::err(
                    id,
                    error_codes::INTERNAL_ERROR,
                    "server is still warming up, retry shortly",
                );
            }
            Ok(Err(_recv_err)) => {
                // Sender dropped without ever sending a value — background task
                // exited before it could report success or failure.
                return Response::err(
                    id,
                    error_codes::INTERNAL_ERROR,
                    "server initialization failed — check credentials and restart",
                );
            }
            Ok(Ok(guard)) => {
                // Clone Arc out of the Ref — one atomic refcount bump instead of
                // a full AppState copy.  The guard is dropped at the end of this
                // block (before the dispatch(...).await below).
                match &*guard {
                    Some(Ok(arc)) => Ok(Arc::clone(arc)),
                    Some(Err(reason)) => Err(reason.clone()),
                    None => unreachable!("wait_for guarantees the predicate is satisfied"),
                }
            } // guard (RwLockReadGuard / watch::Ref) dropped here — .await is now safe
        }
    };

    match state_outcome {
        Err(reason) => Response::err(
            id,
            error_codes::INTERNAL_ERROR,
            format!("server initialization failed: {reason}"),
        ),
        // AppState is ready — Arc::deref gives &AppState for the dispatch call.
        Ok(state) => dispatch(req, &state).await,
    }
}

/// Build a fully-wired trusty-review [`AppState`] from environment + config file.
///
/// Why: an embedding host (e.g. trusty-analyze's MCP server, #630) needs to run
/// the trusty-review pipeline without copying the binary's `serve` assembly,
/// which lives behind `crate::cli_verify` in the binary target and is therefore
/// not reachable from a library consumer. This entry point reproduces that
/// assembly using only library-public builders so the host can obtain an
/// `AppState` and delegate `tools/call` to [`tools::call_tool`].
/// What: loads [`ReviewConfig`] from env + the default XDG config file, builds
/// the reviewer LLM provider (Bedrock or OpenRouter, per config), optionally
/// builds the verifier provider (degrading to `None` on failure — embedded use
/// must not hard-fail a host daemon over a missing verifier model), builds the
/// HTTP search + analyze clients from config (the analyze client defaults to
/// `http://localhost:7879`, i.e. loopback to the hosting analyze daemon, so
/// embedded reviews get authoritative static-analysis context), and returns the
/// assembled `AppState`. No dedup store is opened — embedded callers do not post
/// comments (`allow_posting=false` in every tool handler), so cross-process
/// dedup is unnecessary.
/// Test: the build path is network/credential-bound (AWS / OpenRouter) and is
/// therefore exercised by the live smoke test rather than a unit test; the
/// dispatch/tools surface it feeds is covered by `mcp::tests` and
/// `tools::tests`.
pub async fn build_review_state() -> Result<AppState> {
    let config = ReviewConfig::load(None);

    let reviewer_model = config.role_models.reviewer.model.clone();
    let default_provider = config.role_models.reviewer.provider.clone();
    let llm = build_provider(&reviewer_model, &default_provider, &config)
        .await
        .map_err(|e| anyhow::anyhow!("failed to build reviewer LLM provider: {e}"))?;

    // Verifier is optional in the embedded path: degrade to no-verification
    // rather than abort the host daemon if the verifier model cannot be built.
    let verifier = if config.verification.enabled {
        let role = &config.role_models.verifier;
        match build_provider(&role.model, &role.provider, &config).await {
            Ok(p) => Some(p),
            Err(e) => {
                warn!("failed to build verifier provider (continuing without verification): {e}");
                None
            }
        }
    } else {
        None
    };

    let search = HttpSearchClient::from_config(&config)
        .map_err(|e| anyhow::anyhow!("failed to build search HTTP client: {e}"))?;
    let analyze = HttpAnalyzeClient::from_config(&config)
        .map_err(|e| anyhow::anyhow!("failed to build analyze HTTP client: {e}"))?;

    info!(
        reviewer_model = %config.role_models.reviewer.model,
        analyzer_url = %config.analyzer_url,
        search_url = %config.search_url,
        "trusty-review embedded AppState built"
    );

    Ok(AppState::with_verifier_and_dedup(
        config,
        llm,
        verifier,
        Arc::new(search),
        Some(Arc::new(analyze)),
        None,
    ))
}

// ─── Dispatcher ──────────────────────────────────────────────────────────────

/// Translate a JSON-RPC 2.0 request into a trusty-review MCP response.
///
/// Why: decouples the dispatch logic from the I/O loop so it can be unit-tested
/// with synthetic `Request` values.
/// What: handles `initialize`, `notifications/initialized`, `tools/list`,
/// `tools/call`, and bare tool names.  Always returns a `Response`; never
/// panics.
/// Test: `dispatch_initialize_returns_server_info`,
/// `dispatch_tools_list_returns_three_tools`,
/// `dispatch_unknown_tool_returns_method_not_found`.
pub async fn dispatch(req: Request, state: &AppState) -> Response {
    let is_notification = req.id.is_none();
    let id = req.id.clone();

    if req.jsonrpc.as_deref() != Some("2.0") {
        if is_notification {
            return Response::suppressed();
        }
        return Response::err(id, error_codes::INVALID_REQUEST, "jsonrpc must be \"2.0\"");
    }

    match req.method.as_str() {
        "initialize" => {
            return Response::ok(
                id,
                initialize_response("trusty-review", env!("CARGO_PKG_VERSION"), None),
            );
        }
        "notifications/initialized" | "initialized" => {
            return Response::suppressed();
        }
        _ => {}
    }

    let params = req.params.clone().unwrap_or(Value::Null);

    // Route `tools/list` and `tools/call`; also accept bare method names for
    // ergonomics (e.g. `review_health` directly).
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
                    );
                }
            }
        }
        "tools/list" => {
            return Response::ok(id, serde_json::json!({ "tools": tool_descriptors() }));
        }
        other => (other.to_string(), params, false),
    };

    debug!(tool, via_tools_call, "mcp dispatch");
    let outcome = call_tool(&tool, &arguments, state).await;

    if via_tools_call {
        // Per MCP spec: tool execution failures are `{content, isError:true}`
        // rather than JSON-RPC errors.
        match outcome {
            Ok(value) => Response::ok(id, value),
            Err(ToolError::UnknownTool) => Response::err(
                id,
                error_codes::METHOD_NOT_FOUND,
                format!("unknown tool: {tool}"),
            ),
            Err(ToolError::InvalidParams(msg)) => Response::ok(id, wrap_tool_error(&msg)),
        }
    } else {
        // Bare-method form: return JSON-RPC errors for protocol-level failures.
        match outcome {
            Ok(value) => Response::ok(id, value),
            Err(ToolError::UnknownTool) => Response::err(
                id,
                error_codes::METHOD_NOT_FOUND,
                format!("unknown tool: {tool}"),
            ),
            Err(ToolError::InvalidParams(msg)) => {
                Response::err(id, error_codes::INVALID_PARAMS, msg)
            }
        }
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "mcp_tests.rs"]
mod tests;
