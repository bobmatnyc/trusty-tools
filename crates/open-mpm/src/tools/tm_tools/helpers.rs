//! Shared argument-parsing helpers for the TM tool surface.

use serde_json::Value;

use crate::tools::traits::ToolResult;

/// Pull a required string field from a JSON `Value`, returning a tool-friendly
/// error when missing or empty.
///
/// Why: Every TM tool validates a `session_name` (and sometimes `message`) the
/// same way; centralising the check keeps each `execute` body small.
/// What: Returns `Ok(&str)` for a present, non-blank string, else an `Err`
/// carrying a `ToolResult::err` describing the missing param.
/// Test: `missing_session_name_is_recoverable_error`.
pub(super) fn required_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, ToolResult> {
    match args.get(key).and_then(Value::as_str) {
        Some(s) if !s.trim().is_empty() => Ok(s),
        _ => Err(ToolResult::err(format!("missing param: {key}"))),
    }
}
