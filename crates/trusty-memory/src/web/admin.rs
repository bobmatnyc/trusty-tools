//! Logs tail, admin stop, and fire-and-forget remember handlers.
//!
//! Why: Operational endpoints for debugging, shutdown, and agent-accessible
//! memory writes that do not require MCP. These are grouped together because
//! they all support daemon administration rather than palace data operations.
//! What: `GET /api/v1/logs/tail`, `POST /api/v1/admin/stop`, and
//! `POST /api/v1/remember` handlers.
//! Test: `logs_tail_*`, `admin_stop_returns_ok`, and `remember_async_*` tests
//! in `web::tests`.

use axum::{
    extract::{Query, State},
    http::StatusCode,
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::AppState;

use super::error::ApiError;

/// Default number of log lines returned by `GET /api/v1/logs/tail` when `n`
/// is absent. 100 lines is enough context for a glance without a huge payload.
const DEFAULT_LOGS_TAIL_N: usize = 100;

/// Hard ceiling on `GET /api/v1/logs/tail?n=` — equal to the ring-buffer
/// capacity, so a request can never ask for more lines than the buffer holds.
const MAX_LOGS_TAIL_N: usize = trusty_common::log_buffer::DEFAULT_LOG_CAPACITY;

fn default_logs_tail_n() -> usize {
    DEFAULT_LOGS_TAIL_N
}

/// Query parameters for `GET /api/v1/logs/tail`.
///
/// Why (issue #35): callers ask for a bounded number of recent log lines;
/// `n` defaults to a useful page size and is clamped server-side so a
/// misconfigured client cannot request more lines than the buffer holds.
/// What: `n` is optional; absent → [`DEFAULT_LOGS_TAIL_N`]. Clamped to
/// `[1, MAX_LOGS_TAIL_N]` in the handler.
/// Test: `logs_tail_clamps_n` exercises the clamp.
#[derive(serde::Deserialize)]
pub(super) struct LogsTailParams {
    #[serde(default = "default_logs_tail_n")]
    n: usize,
}

/// `GET /api/v1/logs/tail?n=200` — return the most recent N tracing log lines.
///
/// Why (issue #35): operators debugging a running daemon want recent logs
/// over HTTP without SSHing to the box or restarting with a different
/// `RUST_LOG`. The in-memory ring buffer (fed by the `LogBufferLayer` wired
/// into the subscriber at startup) makes this near-free.
/// What: clamps `n` to `[1, MAX_LOGS_TAIL_N]`, drains the tail of
/// `state.log_buffer`, and returns `{ "lines": [...], "total": <buffered> }`
/// where `total` is the number of lines currently buffered (so callers can
/// tell whether the ring has wrapped).
/// Test: `logs_tail_returns_recent_lines` and `logs_tail_clamps_n`.
pub(super) async fn logs_tail(
    State(state): State<AppState>,
    Query(params): Query<LogsTailParams>,
) -> Json<Value> {
    let n = params.n.clamp(1, MAX_LOGS_TAIL_N);
    let lines = state.log_buffer.tail(n);
    Json(serde_json::json!({
        "lines": lines,
        "total": state.log_buffer.len(),
    }))
}

/// `POST /api/v1/admin/stop` — request a graceful shutdown of the daemon.
///
/// Why (issue #35): the admin UI and operators want a one-call way to stop
/// the daemon without resolving its PID and sending a signal. The daemon is
/// localhost-only and trusts every caller, so no auth is required.
/// What: spawns a detached task that sleeps 200 ms (giving this HTTP response
/// time to flush to the client) and then calls `std::process::exit(0)`.
/// Returns `{ "ok": true, "message": "shutting down" }` immediately.
/// Test: `admin_stop_returns_ok` asserts the response shape (it does not
/// drive the real exit — that would terminate the test process).
pub(super) async fn admin_stop(State(_state): State<AppState>) -> Json<Value> {
    tracing::warn!("admin_stop: shutdown requested via POST /api/v1/admin/stop");
    tokio::spawn(async {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        std::process::exit(0);
    });
    Json(serde_json::json!({ "ok": true, "message": "shutting down" }))
}

// ---------------------------------------------------------------------------
// Fire-and-forget memory save (`POST /api/v1/remember`)
// ---------------------------------------------------------------------------

/// Request body for `POST /api/v1/remember`.
///
/// Why: agents spawned via Claude Code's Agent tool do not inherit any MCP
/// connections, so the `memory_remember` MCP tool is unreachable to them.
/// Exposing a plain HTTP entry point lets those agents shell out via the
/// `trusty-memory note` CLI (or any `curl` call) without learning MCP.
/// What: `content` is the drawer body and is required; `palace` falls back
/// to the daemon's `--palace` default when omitted; `tags` is optional and
/// passed through verbatim to the underlying handler.
/// Test: `remember_async_*` tests in this module.
#[derive(Debug, Deserialize)]
pub(super) struct RememberAsyncBody {
    /// Drawer body. Required.
    pub(super) content: String,
    /// Target palace. When `None`, the daemon's `--palace` default is used;
    /// callers without a default-palace configured must pass this field or
    /// the spawned task logs a warning and drops the request.
    #[serde(default)]
    pub(super) palace: Option<String>,
    /// Optional tag list to attach to the drawer.
    #[serde(default)]
    pub(super) tags: Option<Vec<String>>,
}

/// Minimum word count for content accepted by `POST /api/v1/remember`.
///
/// Why (issue #466): the fire-and-forget endpoint returns `202 Accepted`
/// immediately and dispatches the write on a detached task. Any content that
/// the background worker would reject (e.g. too few tokens) caused silent data
/// loss — the caller believed the memory was stored when it wasn't. Validating
/// the minimum synchronously turns silent drops into explicit `422` rejections
/// so callers know immediately that their content was not queued.
/// What: mirrors `tools::CONTENT_GATE_MIN_WORDS` (4 words) — the same gate
/// `handle_memory_remember` applies via `content_gate` in the background task.
/// Test: `remember_async_rejects_short_content`.
const REMEMBER_MIN_WORDS: usize = 4;

/// `POST /api/v1/remember` — fire-and-forget memory save.
///
/// Why: sub-agents spawned via Claude Code's Agent tool have no MCP
/// connection (MCP servers are not inherited across sub-agent spawns), so
/// they cannot invoke `mcp__trusty-memory__memory_remember` directly. They
/// can, however, run shell commands — this endpoint plus the
/// `trusty-memory note` CLI gives them a writable handle that needs no
/// MCP plumbing. The contract is one-way: the request is parsed, validated,
/// and queued on a `tokio::spawn`, then `202 Accepted` is returned
/// immediately. Failures during the spawned dispatch (palace not found,
/// content gate skip, redb error) are logged at `warn` but never propagate
/// back to the caller because the agent has already exited by then.
/// Issue #466: synchronous validation of obvious rejections (empty content,
/// fewer than [`REMEMBER_MIN_WORDS`] whitespace-delimited words) now returns
/// `422 Unprocessable Entity` before queuing so callers receive a clear error
/// instead of a false `202`. Content that passes the synchronous checks may
/// still be dropped by the background worker's fuller filter set (blocklist,
/// dedup, MCP-level token threshold), but those are less predictable from
/// the HTTP surface.
/// What: deserialises the body, rejects empty content (400) and sub-threshold
/// word count (422), then maps `{content, palace, tags}` → `{text, palace,
/// tags}` (the field names `handle_memory_remember` expects) and dispatches
/// `memory_remember` from a detached task. Returns `202 Accepted` with
/// `{"status":"queued"}`.
/// Test: `remember_async_returns_202_and_persists` (happy path),
/// `remember_async_rejects_empty_content` (400 input validation), and
/// `remember_async_rejects_short_content` (422 for sub-word-count content).
pub(super) async fn remember_async(
    State(state): State<AppState>,
    Json(body): Json<RememberAsyncBody>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let content = body.content.trim();
    if content.is_empty() {
        return Err(ApiError::bad_request("content must not be empty"));
    }
    // Issue #466: synchronous word-count gate so callers learn of obvious
    // rejections without waiting for the detached task to silently drop them.
    let word_count = content.split_whitespace().count();
    if word_count < REMEMBER_MIN_WORDS {
        return Err(ApiError::unprocessable(format!(
            "content too short: {} word(s); minimum is {} words",
            word_count, REMEMBER_MIN_WORDS,
        )));
    }

    // Build the MCP-shaped args once on the request thread so deserialisation
    // errors never end up swallowed by the spawned task.
    // `handle_memory_remember` expects `text` (not `content`).
    let mut args_map = serde_json::Map::new();
    args_map.insert("text".to_string(), Value::String(content.to_string()));
    if let Some(p) = body.palace.clone().or_else(|| state.default_palace.clone()) {
        args_map.insert("palace".to_string(), Value::String(p));
    }
    if let Some(tags) = body.tags.clone() {
        args_map.insert(
            "tags".to_string(),
            Value::Array(tags.into_iter().map(Value::String).collect()),
        );
    }
    let tool_args = Value::Object(args_map);

    let state_for_task = state.clone();
    tokio::spawn(async move {
        match crate::tools::dispatch_tool(&state_for_task, "memory_remember", tool_args).await {
            Ok(v) => {
                tracing::debug!(target: "trusty_memory::remember_async", result = %v, "queued remember succeeded");
            }
            Err(e) => {
                tracing::warn!(
                    target: "trusty_memory::remember_async",
                    "queued remember failed: {e:#}"
                );
            }
        }
    });

    Ok((StatusCode::ACCEPTED, Json(json!({ "status": "queued" }))))
}
