//! Miscellaneous tool arms: `search_health`, `chat`, `grep`, `get_call_chain`,
//! and `upgrade`.
//!
//! Why: these five tools share no common theme with the search or index groups
//! but each is too small to justify its own file. Grouping them here keeps
//! the module hierarchy flat while still freeing `mod.rs` from 500+ lines.
//! What: exports `dispatch_misc_tool`, called from `call_tool` in `mod.rs`.
//! Test: `grep_missing_pattern_returns_invalid_params`,
//! `grep_max_count_alias_forwarded_as_max_results`, and
//! `grep_listed_in_tools_with_required_pattern` in `tests.rs`.

use serde_json::Value;

use super::{
    types::{require_str, DispatchError},
    McpServer,
};

/// Route one of the five miscellaneous tool names to the correct daemon call.
///
/// Why: keeping these arms separate from search and index tools lets each
/// submodule stay focused and under the 500-line cap.
/// What: returns `None` when `tool` is not a misc tool, `Some(Ok(value))` on
/// success, or `Some(Err(DispatchError))` on failure.
/// Test: grep tests in `tests.rs`; upgrade and chat are end-to-end tested by
/// integration tests.
pub(super) async fn dispatch_misc_tool(
    server: &McpServer,
    tool: &str,
    args: &Value,
) -> Option<Result<Value, DispatchError>> {
    match tool {
        "search_health" => Some(server.get("/health").await),
        "chat" => {
            let index_id = match require_str(args, "index_id") {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            // Accept either `message` (legacy / UI) or `question` (issue #15 spec).
            let message = args
                .get("message")
                .and_then(Value::as_str)
                .or_else(|| args.get("question").and_then(Value::as_str));
            let message = match message {
                Some(m) => m,
                None => {
                    return Some(Err(DispatchError::InvalidParams(
                        "missing required string field: message (or question)".into(),
                    )))
                }
            };
            let mut body = serde_json::json!({
                "index_id": index_id,
                "message": message,
            });
            if let Some(history) = args.get("history") {
                body["history"] = history.clone();
            }
            if let Some(model) = args.get("model").and_then(Value::as_str) {
                body["model"] = Value::String(model.to_string());
            }
            if let Some(top_k) = args.get("top_k").and_then(Value::as_u64) {
                body["top_k"] = Value::from(top_k);
            }
            if let Some(key) = args.get("api_key").and_then(Value::as_str) {
                body["api_key"] = Value::String(key.to_string());
            }
            Some(server.post("/chat", &body).await)
        }
        "get_call_chain" => {
            // Issue #76 — annotated call tree for an entry-point function.
            // The daemon endpoint returns `text/plain`; we wrap the body in
            // the JSON envelope MCP clients consume.
            let index_id = match require_str(args, "index_id") {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let entry_point = match require_str(args, "entry_point") {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let mut query: Vec<(&str, String)> = vec![("entry_point", entry_point.to_string())];
            if let Some(dir) = args.get("direction").and_then(Value::as_str) {
                query.push(("direction", dir.to_string()));
            }
            if let Some(d) = args.get("max_depth").and_then(Value::as_u64) {
                query.push(("max_depth", d.to_string()));
            }
            if let Some(inc) = args.get("include_source").and_then(Value::as_bool) {
                query.push(("include_source", inc.to_string()));
            }
            Some(
                server
                    .get_text(&format!("/indexes/{index_id}/call_chain"), &query)
                    .await
                    .map(|text| serde_json::json!({ "text": text })),
            )
        }
        "grep" => {
            // grep-parity regex/literal search over an index's files.
            // Mirrors `POST /grep` (global) and `POST /indexes/:id/grep`.
            // `index_id` is optional — when omitted, the daemon fans out
            // across every registered index.
            let pattern = match require_str(args, "pattern") {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let mut body = serde_json::json!({ "pattern": pattern });
            if let Some(v) = args.get("case_insensitive").and_then(Value::as_bool) {
                body["case_insensitive"] = Value::Bool(v);
            }
            if let Some(v) = args.get("context").and_then(Value::as_u64) {
                body["context"] = Value::from(v);
            }
            if let Some(v) = args.get("context_before").and_then(Value::as_u64) {
                body["context_before"] = Value::from(v);
            }
            if let Some(v) = args.get("context_after").and_then(Value::as_u64) {
                body["context_after"] = Value::from(v);
            }
            if let Some(v) = args.get("glob").and_then(Value::as_str) {
                body["glob"] = Value::String(v.to_string());
            }
            if let Some(v) = args.get("multiline").and_then(Value::as_bool) {
                body["multiline"] = Value::Bool(v);
            }
            if let Some(v) = args.get("fixed_strings").and_then(Value::as_bool) {
                body["fixed_strings"] = Value::Bool(v);
            }
            if let Some(v) = args.get("files_with_matches").and_then(Value::as_bool) {
                body["files_with_matches"] = Value::Bool(v);
            }
            if let Some(v) = args.get("invert_match").and_then(Value::as_bool) {
                body["invert_match"] = Value::Bool(v);
            }
            if let Some(v) = args.get("word_regexp").and_then(Value::as_bool) {
                body["word_regexp"] = Value::Bool(v);
            }
            // Issue #447: accept `max_count` as a ripgrep-parity alias for
            // `max_results`. `max_results` wins when both are supplied.
            if let Some(v) = args
                .get("max_results")
                .or_else(|| args.get("max_count"))
                .and_then(Value::as_u64)
            {
                body["max_results"] = Value::from(v);
            }
            match args.get("index_id").and_then(Value::as_str) {
                Some(id) => Some(server.post(&format!("/indexes/{id}/grep"), &body).await),
                None => Some(server.post("/grep", &body).await),
            }
        }
        "upgrade" => {
            // Route to the daemon's /upgrade HTTP endpoint. The body mirrors
            // the MCP schema: check (default true) and confirm (default false).
            let check = args.get("check").and_then(Value::as_bool).unwrap_or(true);
            let confirm = args
                .get("confirm")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let body = serde_json::json!({ "check": check, "confirm": confirm });
            Some(server.post("/upgrade", &body).await)
        }
        _ => None,
    }
}
