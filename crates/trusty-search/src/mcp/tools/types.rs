//! Shared error type and response-wrapping helpers for the MCP tool dispatcher.
//!
//! Why: the `DispatchError` variants and the `wrap_*` helpers are referenced
//! from multiple submodules (search, index, misc). Centralising them here
//! prevents import cycles and keeps each tool group focused on its own logic.
//! What: exports `DispatchError`, `require_str`, and the four `wrap_*`
//! response-shaping functions consumed by `dispatch` in `mod.rs`.
//! Test: coverage via the public dispatch tests in `tests.rs` and
//! `tests_lane.rs`; the types themselves are structural, not logic-bearing.

use serde_json::Value;

/// Transport-layer and parameter errors that `call_tool` can return.
///
/// Why: a typed enum lets `dispatch` branch on the failure kind and map it
/// to the correct JSON-RPC error code or in-band MCP tool error shape without
/// parsing error strings.
/// What: four variants — `UnknownTool` (no route), `InvalidParams` (bad args),
/// `Transport` (HTTP-level failure), `StageNotReady` (issue #138 pre-flight
/// failure with structured retry hint).
/// Test: every variant is exercised by at least one unit test in `tests.rs` or
/// `tests_lane.rs`.
#[derive(Debug)]
pub(super) enum DispatchError {
    UnknownTool,
    InvalidParams(String),
    Transport(String),
    /// Issue #138 — a per-lane tool was called but its prerequisite stage
    /// is not `Ready` on this index. Carries the resolved stage map and a
    /// retry hint so the LLM can pick a different tool without a second
    /// round-trip.
    StageNotReady {
        message: String,
        current_stages: Value,
        suggested_tools: Vec<&'static str>,
    },
}

/// Extract a required `&str` field from a JSON args object.
///
/// Why: every tool arm needs to pull required string fields and return a
/// uniform `INVALID_PARAMS` error when they are missing or not strings.
/// What: returns `Ok(&str)` on success, `Err(DispatchError::InvalidParams)`
/// with a human-readable message on failure.
/// Test: `missing_params_returns_invalid_params` in `tests.rs`.
pub(super) fn require_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, DispatchError> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| DispatchError::InvalidParams(format!("missing or non-string '{key}'")))
}

/// Wrap a structured JSON result in MCP's `content[]` envelope (bare-method
/// form).
///
/// Why: bare-method callers (not using `tools/call`) expect a plain
/// `{content: [{type, text}]}` wrapper rather than the `isError` form.
/// What: pretty-prints the value into a single text content node.
/// Test: indirect coverage via all search dispatch tests in `tests.rs`.
pub(super) fn wrap_text_content(value: &Value) -> Value {
    serde_json::json!({
        "content": [{
            "type": "text",
            "text": serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string()),
        }]
    })
}

/// Wrap a successful `tools/call` payload with the spec-required `isError`
/// flag.
///
/// Why: the MCP spec's `tools/call` response shape always includes `isError`
/// so MCP clients can branch without parsing text content.
/// What: pretty-prints the value and embeds it alongside `"isError": is_error`.
/// Test: indirect coverage via `tools/call`-form tests in `tests.rs`.
pub(super) fn wrap_tool_result(value: &Value, is_error: bool) -> Value {
    serde_json::json!({
        "content": [{
            "type": "text",
            "text": serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string()),
        }],
        "isError": is_error,
    })
}

/// Wrap a tool execution failure as `{content, isError: true}` per MCP spec.
///
/// Why: the `tools/call` protocol requires in-band error signalling via
/// `isError: true` rather than a JSON-RPC error envelope.
/// What: formats the message into a text content node with `"isError": true`.
/// Test: `search_semantic_tool_returns_stage_not_ready_when_stage_2_missing`
/// in `tests_lane.rs`.
pub(super) fn wrap_tool_error(msg: &str) -> Value {
    serde_json::json!({
        "content": [{
            "type": "text",
            "text": format!("Error: {msg}"),
        }],
        "isError": true,
    })
}

/// Wrap a STAGE_NOT_READY error in MCP's structured tool-error envelope
/// (issue #138).
///
/// Why: MCP `tools/call` failures use `isError: true` rather than the
/// JSON-RPC error envelope. The LLM gets the human-readable text in
/// `content[]` AND a machine-readable `_meta` block with the exact retry
/// hint (`suggested_tools`) and the current stages snapshot so it can
/// pick the right fallback tool without a second probe.
/// What: returns a JSON object matching the spec in issue #138 — `isError:
/// true`, single text content node, and `_meta` carrying `error_code`,
/// `current_stages`, and `suggested_tools`.
/// Test: `search_semantic_tool_returns_stage_not_ready_when_stage_2_missing`
/// in `tests_lane.rs`.
pub(super) fn wrap_stage_not_ready_error(
    message: &str,
    current_stages: &Value,
    suggested_tools: &[&'static str],
) -> Value {
    serde_json::json!({
        "isError": true,
        "content": [{
            "type": "text",
            "text": message,
        }],
        "_meta": {
            "error_code": "STAGE_NOT_READY",
            "current_stages": current_stages,
            "suggested_tools": suggested_tools,
        }
    })
}
