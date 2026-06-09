//! JSON-RPC 2.0 / MCP stdio service for trusty-review.
//!
//! Why: Claude Code speaks MCP over stdio; this module lets operators wire
//! trusty-review into Claude Code via `.mcp.json` without running the HTTP
//! daemon.  The stdio path reuses the same `AppState` and pipeline as the HTTP
//! daemon, so behaviour is identical.
//!
//! What: `run(state)` builds the shared state once and calls
//! `trusty_common::mcp::run_stdio_loop`, which reads JSON-RPC requests
//! line-by-line from stdin and writes responses to stdout.  `dispatch` handles
//! `initialize`, `notifications/initialized`, `tools/list`, `tools/call`, and
//! bare method names.  All tracing goes to stderr; stdout is the transport.
//!
//! Test: `dispatch_initialize_returns_server_info`,
//! `dispatch_tools_list_returns_three_tools`,
//! `dispatch_unknown_tool_returns_method_not_found`,
//! `dispatch_notification_is_suppressed`.

pub mod tools;

use std::sync::Arc;

use anyhow::Result;
use serde_json::Value;
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
    let llm = build_provider(
        &reviewer_model,
        &default_provider,
        &config.openrouter_api_key,
    )
    .await
    .map_err(|e| anyhow::anyhow!("failed to build reviewer LLM provider: {e}"))?;

    // Verifier is optional in the embedded path: degrade to no-verification
    // rather than abort the host daemon if the verifier model cannot be built.
    let verifier = if config.verification.enabled {
        let role = &config.role_models.verifier;
        match build_provider(&role.model, &role.provider, &config.openrouter_api_key).await {
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
