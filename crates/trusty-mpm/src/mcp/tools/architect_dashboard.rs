//! The `architect_dashboard` MCP tool descriptor (#10063 PR 3, DOC-82 LDG-3.3).
//!
//! Why: a session without shell reads and writes the Architect's dashboard
//! store through this tool, with the schema, limits and exit classes of
//! `tm architect dashboard`.
//! What: [`architect_dashboard_tools`] returns one descriptor; `op` picks the
//! `mpm.architect.dashboard.*` method and the other arguments are that
//! method's request.
//! Test: `super::tests::catalog_names_match_constant`,
//! `dispatch_architect_dashboard_tool_writes_and_conflicts`.

use serde_json::{Value, json};

use super::tool;

/// Build the `architect_dashboard` descriptor.
pub(super) fn architect_dashboard_tools() -> Vec<Value> {
    let store = json!({
        "type": "object",
        "properties": {
            "root": {"type": "string", "description": "Absolute store root."},
            "format": {"type": "string", "enum": ["dir", "jsonl"]},
            "actor": {"type": "string", "description": "`by` of change and prune lines (default architect)."},
            "prune_log": {"type": "string", "description": "Prune log outside the root (default <root>/../owner-queue-archive/pruned.jsonl)."}
        },
        "required": ["root"],
        "additionalProperties": false
    });
    vec![tool(
        "architect_dashboard",
        "Read or write the Architect's dashboard store (collections items, goals, sessions, \
         questions, meta, ...). Every call names `store` {root, format dir|jsonl}. `op` picks the \
         method: `get` {collection, id}, `list` {collection, status, order_by, desc, limit}, \
         `set` / `update` {collection, id, data, if_version} (`if_version` 0: must not exist; a \
         `set` with no `if_version` on an existing document is a conflict), `delete` \
         {collection, id, if_version} (the record is appended to the prune log first), `batch` \
         {ops: [{op set|update|delete, collection, id, data, if_version}]} (all or none), \
         `pruned` {id, collection}, `validate` {schemas: schema directory}, `fsck`, `tally`, \
         `footer` {tailnet, local}, `ids` {prefix: one capital letter}. Errors end with the CLI \
         exit code: 1 conflict (stale `if_version`), 2 refused input (bad name, `version` in a \
         body, secret-shaped value, case variant), 3 store unreadable or locked. Never put a \
         secret value in a field.",
        json!({
            "type": "object",
            "properties": {
                "op": {"type": "string", "enum": ["get", "list", "set", "update", "delete", "batch",
                    "pruned", "validate", "fsck", "tally", "footer", "ids"]},
                "store": store,
                "collection": {"type": "string"},
                "id": {"type": "string"},
                "data": {"type": "object"},
                "if_version": {"type": "integer", "minimum": 0},
                "ops": {"type": "array", "items": {"type": "object"}},
                "status": {"type": "string"},
                "order_by": {"type": "string"},
                "desc": {"type": "boolean"},
                "limit": {"type": "integer", "minimum": 0},
                "schemas": {"type": "string"},
                "tailnet": {"type": "string"},
                "local": {"type": "string"},
                "prefix": {"type": "string"}
            },
            "required": ["op", "store"],
            "additionalProperties": false
        }),
    )]
}
