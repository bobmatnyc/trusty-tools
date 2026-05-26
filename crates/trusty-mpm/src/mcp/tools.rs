//! MCP tool definitions for the trusty-mpm orchestration server.
//!
//! Why: `tools/list` must advertise a JSON Schema for every tool so Claude Code
//! knows how to call it. Keeping the catalog in one module — separate from the
//! dispatch logic — makes the tool surface easy to audit and version.
//! What: [`tool_catalog`] builds the six MCP tool descriptors (name, human
//! description, `inputSchema`); [`TOOL_CATALOG`] lists their names for tests.
//! Test: `cargo test -p trusty-mpm-mcp` asserts the catalog has six well-formed
//! entries whose names match [`TOOL_CATALOG`].

use serde_json::{Value, json};

/// Canonical names of every tool the server exposes, in catalog order.
///
/// Why: tests and the daemon's startup log both want the authoritative list
/// without re-parsing the JSON schema.
/// What: a static slice of the six tool names.
/// Test: `catalog_names_match_constant`.
pub const TOOL_CATALOG: [&str; 6] = [
    "session_list",
    "session_status",
    "agent_delegate",
    "memory_protect",
    "circuit_breaker_status",
    "hook_event",
];

/// Build the MCP tool descriptor list returned by `tools/list`.
///
/// Why: Claude Code reads `inputSchema` to validate calls; a single builder
/// keeps the schemas and the dispatch argument-parsing in lockstep.
/// What: returns six JSON objects, each `{ name, description, inputSchema }`.
/// Test: `catalog_has_six_tools` and `every_tool_has_input_schema`.
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
    fn catalog_has_six_tools() {
        assert_eq!(tool_catalog().len(), 6);
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
}
