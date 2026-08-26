//! Pure parameter/response helpers for the MCP dispatcher.
//!
//! Why: param extraction, params assembly, and MCP result-envelope wrapping are
//! self-contained pure functions; lifting them out keeps `mcp/mod.rs` under the
//! 500-SLOC production cap (see #1195).
//! What: `require_str` / `index_id_or_default` read args, [`optional_params`]
//! forwards whichever optional arguments a tool call carried, and `wrap_*`
//! build MCP content envelopes.
//!
//! #6287 replaced `build_query` and `urlencode` with [`optional_params`]. Those
//! two existed to put optional arguments into a URL query string; the daemon
//! takes one JSON `params` object now, so an argument is copied across as the
//! JSON value it already was — which also removes the string/u64/bool coercion
//! ladder `build_query` needed, and with it the class of bug where a float
//! `3.9` silently became `3`.
//!
//! Test: `helpers_tests.rs` covers `optional_params` / `index_id_or_default` /
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

/// Start a params object from whichever of `keys` the tool call carried.
///
/// Why (#6287): `find_smells` and `run_diagnostics` accept optional `limit` /
/// `offset` / `omit_content` (#917/#918), and the daemon's request structs give
/// every one of those a serde default. Sending a key the caller did not supply
/// would override that default with a guess, so absence has to be preserved
/// rather than filled in.
/// What: copies each present key's JSON value across verbatim — no coercion,
/// which is what makes the daemon's own `Deserialize` the single arbiter of
/// what a well-typed argument is. A key that is absent, or explicitly `null`,
/// is left out entirely; `null` decodes as "absent" for an `Option` field but
/// as a hard error for a `#[serde(default)]` scalar, and the caller meant the
/// former.
/// Test: `optional_params_copies_only_present_keys`,
/// `optional_params_preserves_value_types`,
/// `optional_params_omits_an_explicit_null`.
pub(super) fn optional_params(
    args: &Value,
    keys: &[&str],
) -> serde_json::Map<String, Value> {
    let mut out = serde_json::Map::new();
    for key in keys {
        match args.get(*key) {
            Some(Value::Null) | None => {}
            Some(v) => {
                out.insert((*key).to_string(), v.clone());
            }
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
