//! MCP tool definitions for the trusty-mpm orchestration server.
//!
//! Why: `tools/list` must advertise a JSON Schema for every tool so Claude Code
//! knows how to call it. Keeping the catalog in one module — separate from the
//! dispatch logic — makes the tool surface easy to audit and version.
//! What: [`tool_catalog`] builds the nine MCP tool descriptors (name, human
//! description, `inputSchema`); [`TOOL_CATALOG`] lists their names for tests.
//! The three bug-reporting tools (`list_recent_errors`, `preview_bug_report`,
//! `report_bug`) are added in Phase 3.
//! Test: `cargo test -p trusty-mpm` asserts the catalog has nine well-formed
//! entries whose names match [`TOOL_CATALOG`].

use serde_json::{Value, json};

/// Canonical names of every tool the server exposes, in catalog order.
///
/// Why: tests and the daemon's startup log both want the authoritative list
/// without re-parsing the JSON schema.
/// What: a static slice of the nine tool names (six orchestration + three
///       bug-reporting added in Phase 3).
/// Test: `catalog_names_match_constant`.
pub const TOOL_CATALOG: [&str; 9] = [
    "session_list",
    "session_status",
    "agent_delegate",
    "memory_protect",
    "circuit_breaker_status",
    "hook_event",
    "list_recent_errors",
    "preview_bug_report",
    "report_bug",
];

/// Build the MCP tool descriptor list returned by `tools/list`.
///
/// Why: Claude Code reads `inputSchema` to validate calls; a single builder
/// keeps the schemas and the dispatch argument-parsing in lockstep.
/// What: returns nine JSON objects, each `{ name, description, inputSchema }`.
/// Test: `catalog_has_nine_tools` and `every_tool_has_input_schema`.
pub fn tool_catalog() -> Vec<Value> {
    vec![
        tool(
            "session_list",
            "List all Claude Code sessions the trusty-mpm daemon is managing, \
             with status, working directory, and active delegation count.",
            json!({ "type": "object", "properties": {}, "additionalProperties": false }),
        ),
        tool(
            "session_status",
            "Get detailed status for one session: uptime, token usage, current \
             agent, memory pressure, and last activity.",
            json!({
                "type": "object",
                "properties": {
                    "session_id": {
                        "type": "string",
                        "description": "Target session id (UUID)."
                    }
                },
                "required": ["session_id"],
                "additionalProperties": false
            }),
        ),
        tool(
            "agent_delegate",
            "Request that the daemon delegate a task to a named agent. The \
             daemon applies circuit-breaker and depth limits before spawning.",
            json!({
                "type": "object",
                "properties": {
                    "session_id": { "type": "string", "description": "Calling session id." },
                    "agent": { "type": "string", "description": "Target agent name." },
                    "task": { "type": "string", "description": "Task description for the agent." },
                    "tier": {
                        "type": "string",
                        "enum": ["haiku", "sonnet", "opus"],
                        "description": "Optional explicit model tier; daemon picks a default if omitted."
                    }
                },
                "required": ["session_id", "agent", "task"],
                "additionalProperties": false
            }),
        ),
        tool(
            "memory_protect",
            "Report current context-window token usage for a session. The \
             daemon classifies pressure (ok/warn/alert/compact) and may trigger \
             a trusty-memory snapshot or auto-compaction.",
            json!({
                "type": "object",
                "properties": {
                    "session_id": { "type": "string", "description": "Target session id." },
                    "used_tokens": {
                        "type": "integer", "minimum": 0,
                        "description": "Tokens currently in the context window."
                    },
                    "window_tokens": {
                        "type": "integer", "minimum": 1,
                        "description": "Total context window size for the model."
                    }
                },
                "required": ["session_id", "used_tokens", "window_tokens"],
                "additionalProperties": false
            }),
        ),
        tool(
            "circuit_breaker_status",
            "Inspect circuit-breaker state. With no `agent`, returns every \
             agent's breaker; with `agent`, returns just that one.",
            json!({
                "type": "object",
                "properties": {
                    "agent": {
                        "type": "string",
                        "description": "Optional agent name to scope the query."
                    }
                },
                "additionalProperties": false
            }),
        ),
        tool(
            "hook_event",
            "Forward a Claude Code hook event to the daemon's observability \
             pipeline (live dashboard feed, Telegram alerts, memory tracking).",
            json!({
                "type": "object",
                "properties": {
                    "session_id": { "type": "string", "description": "Originating session id." },
                    "event": {
                        "type": "string",
                        "description": "Hook event name, e.g. PreToolUse, SessionStart."
                    },
                    "payload": {
                        "description": "Raw event payload (shape varies per event)."
                    }
                },
                "required": ["session_id", "event"],
                "additionalProperties": false
            }),
        ),
        // ── Bug-reporting tools (Phase 2 surface + Phase 3 filing) ───────────
        tool(
            "list_recent_errors",
            "List recently captured ERROR-level events across all trusty-* daemons \
             (trusty-search, trusty-memory, trusty-analyze, trusty-mpm). Each entry \
             includes a fingerprint for deduplication, an occurrence count, the \
             originating crate, and a one-line summary. Use `preview_bug_report` to \
             see the full scrubbed body before filing.",
            json!({
                "type": "object",
                "properties": {
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 100,
                        "description": "Maximum number of errors to return (default 20)."
                    }
                },
                "additionalProperties": false
            }),
        ),
        tool(
            "preview_bug_report",
            "Preview the exact scrubbed GitHub issue body that would be filed for \
             a specific error fingerprint. Shows what data is included, what was \
             redacted (paths, tokens, secrets), and the proposed labels. Nothing \
             is filed — call `report_bug` with `confirm: true` to actually file.",
            json!({
                "type": "object",
                "properties": {
                    "fingerprint": {
                        "type": "string",
                        "description": "64-char hex SHA-256 fingerprint from list_recent_errors."
                    }
                },
                "required": ["fingerprint"],
                "additionalProperties": false
            }),
        ),
        tool(
            "report_bug",
            "File a GitHub issue in bobmatnyc/trusty-tools for the error identified \
             by `fingerprint`. Requires explicit user consent: `confirm` must be true \
             or nothing is filed. If an open issue with the same fingerprint already \
             exists, posts a '+1 occurrence' comment instead of creating a duplicate. \
             Returns `{ filed, deduped, issue_url, issue_number }` on success, or an \
             actionable error message if no token is configured \
             (set TRUSTY_BUGREPORT_GITHUB_TOKEN). Always call `preview_bug_report` \
             first so the user can review the scrubbed content.",
            json!({
                "type": "object",
                "properties": {
                    "fingerprint": {
                        "type": "string",
                        "description": "64-char hex SHA-256 fingerprint from list_recent_errors."
                    },
                    "confirm": {
                        "type": "boolean",
                        "description": "Must be true to actually file; false or absent → preview only."
                    }
                },
                "required": ["fingerprint", "confirm"],
                "additionalProperties": false
            }),
        ),
    ]
}

/// Assemble one MCP tool descriptor object.
fn tool(name: &str, description: &str, input_schema: Value) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": input_schema,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_has_nine_tools() {
        assert_eq!(tool_catalog().len(), 9);
    }

    #[test]
    fn catalog_names_match_constant() {
        let names: Vec<String> = tool_catalog()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(names, TOOL_CATALOG);
    }

    #[test]
    fn every_tool_has_input_schema() {
        for t in tool_catalog() {
            assert!(t["name"].is_string());
            assert!(t["description"].is_string());
            assert_eq!(t["inputSchema"]["type"], "object");
        }
    }

    #[test]
    fn bug_reporting_tools_present() {
        let catalog = tool_catalog();
        let names: Vec<&str> = catalog.iter().filter_map(|t| t["name"].as_str()).collect();
        assert!(names.contains(&"list_recent_errors"), "{names:?}");
        assert!(names.contains(&"preview_bug_report"), "{names:?}");
        assert!(names.contains(&"report_bug"), "{names:?}");
    }
}
