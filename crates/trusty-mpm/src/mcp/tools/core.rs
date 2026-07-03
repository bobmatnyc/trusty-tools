//! Core orchestration + bug-reporting MCP tool descriptors.
//!
//! Why: `tools/list` must advertise a JSON Schema for every tool so Claude Code
//! knows how to call it. Splitting the catalog by concept — the six original
//! orchestration tools plus the three bug-reporting tools live here, the
//! session-lifecycle tools live in [`super::session`] — keeps each file well
//! under the 500-SLOC production cap and makes the surface easy to audit.
//! What: [`core_tools`] returns the nine original MCP tool descriptors (name,
//! human description, `inputSchema`); the shared [`tool`] descriptor-builder is
//! re-exported from the parent module.
//! Test: `cargo test -p trusty-mpm` (in [`super`]) asserts the full catalog —
//! core + session — is well-formed and matches the name constant.

use serde_json::{Value, json};

use super::tool;

/// Build the six orchestration + three bug-reporting tool descriptors.
///
/// Why: these nine tools predate the session-lifecycle surface (#1221); keeping
/// their schemas in a dedicated builder isolates them from the new tools so each
/// group can evolve independently without growing one monolithic function past
/// the SLOC cap.
/// What: returns the nine `{ name, description, inputSchema }` objects in catalog
/// order — `session_list`, `session_status`, `agent_delegate`, `memory_protect`,
/// `circuit_breaker_status`, `hook_event`, then the three bug-reporting tools.
/// Test: `super::tests::catalog_has_expected_tool_count` and
/// `super::tests::catalog_names_match_constant`.
pub(super) fn core_tools() -> Vec<Value> {
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
            "Record and gate a delegation to a named agent: applies the \
             circuit-breaker and depth limits and adds the delegation to the \
             session's dashboard tree. This is a TRACKING/GATING companion, NOT \
             an execution path — it does not spawn the agent. Execution happens \
             via the native Agent/Task tool using the deployed agent name (from \
             ~/.claude/agents/), e.g. Agent(subagent_type=\"rust-engineer\").",
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
