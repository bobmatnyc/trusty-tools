//! Pure parameter/response helpers for the MCP dispatcher.
//!
//! Why: param extraction, query-string assembly, URL encoding, and MCP
//! result-envelope wrapping are self-contained pure functions; lifting them out
//! keeps `mcp/mod.rs` under the 500-SLOC production cap (see #1195).
//! What: `require_str` / `index_id_or_default` read args, `build_query` /
//! `urlencode` assemble query strings, and `wrap_*` build MCP content
//! envelopes.
//! Test: `helpers_tests.rs` covers `build_query` / `index_id_or_default` /
//! the timeout constant; the `wrap_*` helpers are exercised via dispatch tests.

use super::DispatchError;
use serde_json::Value;

pub(super) fn require_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, DispatchError> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| DispatchError::InvalidParams(format!("missing or non-string '{key}'")))
}

/// Read `index` (preferred) or `index_id` (legacy alias) from `args`,
/// falling back to `"default"`.
///
/// Why: Multiple tools accept either parameter name and need the same
/// fallback behaviour; centralising removes 9 copies of the same chain.
/// What: Tries `index`, then `index_id`, then `"default"`.
/// Test: Covered indirectly by every per-tool handler test.
pub(super) fn index_id_or_default(args: &Value) -> &str {
    args.get("index")
        .or_else(|| args.get("index_id"))
        .and_then(Value::as_str)
        .unwrap_or("default")
}

/// Build a `?key=val&...` query string from whichever of `keys` is present
/// in `args`. Handles string, integer (u64), and bool values; skips keys
/// that are absent or of an unsupported type. Returns an empty string if no
/// keys were found.
///
/// Why: `find_smells` and `run_diagnostics` gained `limit` (number), `offset`
/// (number), and `omit_content` (bool) params (#917/#918); extending this
/// helper avoids duplicating query-string assembly in each handler.
/// What: tries `as_str` first, then `as_u64`, then `as_bool`; uses the first
/// match. The former `as_f64` fallback was removed because JSON integers are
/// already covered by `as_u64`, and float→u64 truncation (e.g. `3.9 → 3`) is
/// silently wrong. Non-string values are formatted without URL encoding because
/// they never contain reserved characters.
/// Test: `build_query_handles_numeric_and_bool` and
/// `build_query_integer_limit_parses_correctly` in `helpers_tests.rs`.
pub(super) fn build_query(args: &Value, keys: &[&str]) -> String {
    let mut q = String::new();
    for key in keys {
        let node = args.get(*key);
        let val: Option<String> = node
            .and_then(Value::as_str)
            .map(urlencode)
            .or_else(|| node.and_then(Value::as_u64).map(|n| n.to_string()))
            .or_else(|| node.and_then(Value::as_bool).map(|b| b.to_string()));
        if let Some(v) = val {
            let sep = if q.is_empty() { '?' } else { '&' };
            q.push(sep);
            q.push_str(key);
            q.push('=');
            q.push_str(&v);
        }
    }
    q
}

/// Minimal URL encoding for the bits we pass through to `/facts?subject=...`.
/// Avoids pulling a full url crate into the MCP server.
pub(super) fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

pub(super) fn wrap_text_content(value: &Value) -> Value {
    serde_json::json!({
        "content": [{
            "type": "text",
            "text": serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string()),
        }]
    })
}

pub(super) fn wrap_tool_result(value: &Value) -> Value {
    serde_json::json!({
        "content": [{
            "type": "text",
            "text": serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string()),
        }],
        "isError": false,
    })
}

pub(super) fn wrap_tool_error(msg: &str) -> Value {
    serde_json::json!({
        "content": [{ "type": "text", "text": format!("Error: {msg}") }],
        "isError": true,
    })
}
