//! MCP tool schema definitions for the three wing tools (ADR-0027 T9, #4809).
//!
//! Why: `definitions.rs` sits at 367 of its 500-SLOC production cap, so the
//! wing tool JSON lands in a sibling module — the same split
//! `task_definitions.rs` made for the task tools, and for the same reason.
//! What: exports `wing_tool_definitions(has_default)` — a `Vec<Value>` of the
//! three wing tool schemas, spliced into the main tools array in
//! `tool_definitions_with`.
//! Test: `tool_definitions_lists_all_tools` in `tools::tests` verifies the
//! names appear in the merged list.

use serde_json::{json, Value};

/// Build the three wing tool schemas conditioned on whether a default palace
/// is configured.
///
/// Why: follows the same `has_default` pattern as every other tool group so
/// the `palace` argument moves out of `required` when the server is bound to a
/// single palace via `--palace`.
/// What: returns `[wing_list, wing_create, wing_rename]` as `Vec<Value>`.
/// Test: spliced into `tool_definitions_with`, covered by
/// `tool_definitions_lists_all_tools` and `wing_tools_are_listed`.
pub(super) fn wing_tool_definitions(has_default: bool) -> Vec<Value> {
    let wing_list_required: Vec<&str> = if has_default { vec![] } else { vec!["palace"] };
    let wing_create_required: Vec<&str> = if has_default {
        vec!["label"]
    } else {
        vec!["palace", "label"]
    };
    let wing_rename_required: Vec<&str> = if has_default {
        vec!["wing", "new_label"]
    } else {
        vec!["palace", "wing", "new_label"]
    };

    vec![
        json!({
            "name": "wing_list",
            "description": "List the wings of a palace (ADR-0027). A WING is the scope/ownership axis — the 'who' — while a ROOM is the topic axis — the 'what'. Wings let two owners hold same-named rooms (engineer/Planning and pm/Planning are distinct rooms) without name mangling. Every palace has a 'default' wing that every room falls into unless a caller names another, so wings are never required. Returns { palace, wings: [ { wing_id, label, description, room_count, is_default, created_at } ] }.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "palace": {"type": "string", "description": "Palace ID (optional if server started with --palace)"}
                },
                "required": wing_list_required,
            }
        }),
        json!({
            "name": "wing_create",
            "description": "Create a wing (scope) in a palace, or return the existing one with that label (ADR-0027). Idempotent — safe to call unconditionally on startup; label matching is case-insensitive while the first-seen spelling is kept for display. Creating 'default' returns the palace's existing default wing rather than a second one. Creating a wing writes no room and no drawer. Returns { palace, wing_id, label, created }.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "palace": {"type": "string", "description": "Palace ID (optional if server started with --palace)"},
                    "label":  {"type": "string", "description": "Wing name, e.g. an agent type such as `engineer` or `pm`. Must not be empty."}
                },
                "required": wing_create_required,
            }
        }),
        json!({
            "name": "wing_rename",
            "description": "Rename a wing (ADR-0027). The old label stops resolving — this is a rename, not an alias. Rooms reference their wing by id, so a rename never moves a room and never touches a drawer. Errors if the wing is unknown or another wing already holds the new label. Returns { palace, wing: { wing_id, label, ... } }.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "palace":    {"type": "string", "description": "Palace ID (optional if server started with --palace)"},
                    "wing":      {"type": "string", "description": "Wing id or current label to rename."},
                    "new_label": {"type": "string", "description": "New wing name. Must not be empty or already used by another wing."}
                },
                "required": wing_rename_required,
            }
        }),
    ]
}
