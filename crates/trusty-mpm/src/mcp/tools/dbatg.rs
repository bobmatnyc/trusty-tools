//! The `dbatg` MCP tool descriptor (#9758, DOC-82 LDG-3.3).
//!
//! Why: a PM that cannot run shell writes its ledger through this tool (PM
//! rule P10), with the schema, limits and exit classes of `tm dbatg`.
//! What: [`dbatg_tools`] returns one descriptor; `op` picks the
//! `mpm.dbatg.*` method and the other arguments are that method's request.
//! Test: `super::tests::catalog_names_match_constant`,
//! `crate::mcp::dbatg_dispatch::tests::dbatg_tool_writes_and_reads_a_store`.

use serde_json::{Value, json};

use super::tool;

/// Build the `dbatg` descriptor.
pub(super) fn dbatg_tools() -> Vec<Value> {
    let store = json!({
        "type": "object",
        "properties": {
            "project": {"type": "string", "description": "Absolute project path; a worktree maps to its main checkout."},
            "session": {"type": "string", "description": "The session whose store, `^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$`."}
        },
        "required": ["project", "session"],
        "additionalProperties": false
    });
    vec![tool(
        "dbatg",
        "Write or read this PM session's Intent Ledger (goals G, decisions D, asks A, tasks T) \
         in `<project>/.trusty-mpm/dbatg/<session>/`. `op` picks the method: `init` \
         (`project`, `session`, `code`: create the store; same code and session again is a no-op), \
         `batch` (`store`, `request_id`, optional `if_v`, `ops`: apply every op or none; a \
         `request_id` sent again returns the first result and applies nothing), `show` (`store`, \
         optional `brief`). Batch ops, tagged by `op`: `add` {kind G|D|A|T, title, goal, rank, \
         finish, maps, status, to, opts [{l, rec}], id}, `update` {id, title, rank, esc, answer, \
         to, outcome, status, note}, `close` {id, outcome}, `session` {task, next, waiting_on, \
         owed, agents, crons, refs, boundary, ctx_use}, `prune` {dry_run, answered_hours, \
         require_outcome}. Returns `init` {store, v}, `batch` {v, events [{id, ev, note}], \
         archived [ids], warnings}, `show` {text}. Errors end with the CLI exit code: 1 conflict \
         (`if_v`, tamper), 2 refused input (limits, secret-shaped values, unknown ids), 3 store \
         unavailable. Never put a secret value in a field.",
        json!({
            "type": "object",
            "properties": {
                "op": {"type": "string", "enum": ["init", "batch", "show"]},
                "project": {"type": "string"},
                "session": {"type": "string"},
                "code": {"type": "string"},
                "store": store,
                "request_id": {"type": "string"},
                "if_v": {"type": "integer", "minimum": 0},
                "ops": {"type": "array", "items": {"type": "object"}},
                "brief": {"type": "boolean"}
            },
            "required": ["op"],
            "additionalProperties": false
        }),
    )]
}
