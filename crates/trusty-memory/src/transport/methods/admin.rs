//! Log tail, shutdown, and the fire-and-forget remember (#6286).
//!
//! Why: three operational endpoints that supported daemon administration
//! rather than palace data, and that had no tool equivalent to fall back on.
//! What: `memory.logs_tail`, `memory.admin_stop`, `memory.remember_async`.
//! Test: `super::super::uds::tests` — `rpc_logs_tail_*`, `rpc_admin_stop_*`,
//! `rpc_remember_async_*`.

use serde::Deserialize;
use serde_json::{json, Value};

use crate::transport::api_error::ApiError;
use crate::AppState;

use super::NoParams;

/// Default tail length: enough context for a glance, small enough to not be a
/// payload.
const DEFAULT_LOGS_TAIL_N: usize = 100;

/// Ceiling on a tail request — the ring-buffer capacity, so a caller can never
/// ask for more lines than the buffer holds.
const MAX_LOGS_TAIL_N: usize = trusty_common::log_buffer::DEFAULT_LOG_CAPACITY;

fn default_logs_tail_n() -> usize {
    DEFAULT_LOGS_TAIL_N
}

/// Params for `memory.logs_tail`.
#[derive(Debug, Deserialize)]
pub struct LogsTailParams {
    /// Lines to return, clamped to `[1, MAX_LOGS_TAIL_N]`.
    #[serde(default = "default_logs_tail_n")]
    pub n: usize,
}

/// `memory.logs_tail` — the most recent N tracing lines (#35).
///
/// `total` is how many lines are currently buffered, so a caller can tell
/// whether the ring has wrapped.
pub async fn logs_tail(state: &AppState, params: LogsTailParams) -> Result<Value, ApiError> {
    let n = params.n.clamp(1, MAX_LOGS_TAIL_N);
    Ok(json!({
        "lines": state.log_buffer.tail(n),
        "total": state.log_buffer.len(),
    }))
}

/// `memory.admin_stop` — ask the daemon to shut down (#35).
///
/// The exit is deferred 200 ms so this response reaches the caller first, and
/// is compiled out under `cfg(test)` (#1100): a detached `process::exit` in a
/// test binary races the test's own return and kills the whole process.
pub async fn admin_stop(_state: &AppState, _params: NoParams) -> Result<Value, ApiError> {
    tracing::warn!("admin_stop: shutdown requested via memory.admin_stop");
    #[cfg(not(test))]
    tokio::spawn(async {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        std::process::exit(0);
    });
    Ok(json!({ "ok": true, "message": "shutting down" }))
}

/// Params for `memory.remember_async`.
///
/// Why this method exists at all: a sub-agent spawned via Claude Code's Agent
/// tool inherits no MCP connections, so `memory_remember` is unreachable to it.
/// It can still run a shell command, which is what `trusty-memory note` is.
#[derive(Debug, Deserialize)]
pub struct RememberAsyncParams {
    /// Drawer body. Required.
    pub content: String,
    /// Target palace; falls back to the daemon's `--palace` default.
    #[serde(default)]
    pub palace: Option<String>,
    /// Optional tags, passed through verbatim.
    #[serde(default)]
    pub tags: Option<Vec<String>>,
}

/// Minimum word count accepted, mirroring `tools::CONTENT_GATE_MIN_WORDS`.
///
/// Why the check is synchronous (#466): the write runs on a detached task, so
/// content the background worker would reject was silently dropped after the
/// caller had already been told the memory was stored.
const REMEMBER_MIN_WORDS: usize = 4;

/// `memory.remember_async` — queue a write and answer immediately.
///
/// The contract is one-way: obvious rejections (empty, too short) are refused
/// here so the caller learns of them, and everything else is dispatched from a
/// detached task whose failures are logged rather than returned — the agent
/// that asked has usually exited by then.
///
/// Test: `rpc_remember_async_rejects_short_content`,
/// `rpc_remember_async_queues_and_persists`.
pub async fn remember_async(
    state: &AppState,
    params: RememberAsyncParams,
) -> Result<Value, ApiError> {
    let content = params.content.trim();
    if content.is_empty() {
        return Err(ApiError::bad_request("content must not be empty"));
    }
    let word_count = content.split_whitespace().count();
    if word_count < REMEMBER_MIN_WORDS {
        return Err(ApiError::unprocessable(format!(
            "content too short: {word_count} word(s); minimum is {REMEMBER_MIN_WORDS} words"
        )));
    }

    // Built on the calling task so a bad shape fails here rather than being
    // swallowed by the detached one. `handle_memory_remember` reads `text`.
    let mut args = serde_json::Map::new();
    args.insert("text".to_string(), Value::String(content.to_string()));
    if let Some(p) = params
        .palace
        .clone()
        .or_else(|| state.default_palace.clone())
    {
        args.insert("palace".to_string(), Value::String(p));
    }
    if let Some(tags) = params.tags.clone() {
        args.insert(
            "tags".to_string(),
            Value::Array(tags.into_iter().map(Value::String).collect()),
        );
    }
    let tool_args = Value::Object(args);

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

    Ok(json!({ "status": "queued" }))
}
