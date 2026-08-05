//! MCP tool schemas for the room surface (ADR-0027 D6, ticket T6 / #4805).
//!
//! Why: `definitions.rs` is at 367 SLOC and the three room schemas would push
//! it toward the 500-SLOC production cap; extracting them mirrors what
//! `task_definitions.rs` already does for the task tools, and keeps one
//! coherent group per file.
//! What: exports `room_tool_definitions(has_default)` — a `Vec<Value>` with
//! the `room_list` / `room_create` / `room_rename` schemas, spliced into the
//! main tools array by `tool_definitions_with`.
//! Test: `tool_definitions_lists_all_tools` in `tools::tests` verifies the
//! names appear in the merged list; `every_tool_has_scopes` in `openrpc::tests`
//! verifies scopes are declared.

use serde_json::{json, Value};

/// Build the three room tool schemas conditioned on whether a default palace
/// is configured.
///
/// Why: follows the same `has_default` pattern as every other tool group so
/// the `palace` argument moves out of `required` when the server is bound to
/// a single palace via `--palace`.
/// What: returns `[room_list, room_create, room_rename]` as `Vec<Value>`.
/// Test: spliced into `tool_definitions_with` and covered by
/// `tool_definitions_lists_all_tools` in `tools::tests`.
pub(super) fn room_tool_definitions(has_default: bool) -> Vec<Value> {
    let room_list_required: Vec<&str> = if has_default { vec![] } else { vec!["palace"] };
    let room_create_required: Vec<&str> = if has_default {
        vec!["label"]
    } else {
        vec!["palace", "label"]
    };
    let room_rename_required: Vec<&str> = if has_default {
        vec!["room", "new_label"]
    } else {
        vec!["palace", "room", "new_label"]
    };
    // Reserved until the Wing entity ships (ADR-0027 D2 / ticket T9). Declared
    // now so the argument name is stable, validated strictly so a caller who
    // passes something else gets an error rather than a silently empty list.
    let wing_arg = json!({
        "type": "string",
        "description": "Reserved. Wings are not implemented yet (ADR-0027 T9); the only accepted value is the palace's default wing id, and omitting this is equivalent."
    });

    vec![
        json!({
            "name": "room_list",
            "description": "List every room registered in a palace (ADR-0027). This is the discovery primitive: before it existed, a caller could not find out which rooms a palace had without already knowing their names. Rooms are registered automatically the first time a palace is opened, from the rooms its drawers already sit in — no drawer is ever moved or reclassified. Returns { palace, rooms: [ { room_id, label, room_type, wing_id, drawer_count, created_at, resolved, description } ] }. `resolved: false` means the migration could not recover the room's original name and synthesised an `unresolved-<id>` placeholder — fix it with room_rename.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "palace": {"type": "string", "description": "Palace ID (optional if server started with --palace)"},
                    "wing":   wing_arg.clone()
                },
                "required": room_list_required,
            }
        }),
        json!({
            "name": "room_create",
            "description": "Create a room in a palace, or return the existing one (ADR-0027). Idempotent: room names are matched case-insensitively, so creating `Decisions` when `decisions` exists returns that room rather than minting a second one, and `created` reports whether this call actually wrote the row. Creating a room does not move any drawer into it — write to it with memory_remember/memory_note passing the same `room`. Returns { palace, room_id, label, created }.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "palace":      {"type": "string", "description": "Palace ID (optional if server started with --palace)"},
                    "label":       {"type": "string", "description": "Room name. One of the built-in kinds (Frontend, Backend, Testing, Planning, Documentation, Research, Configuration, Meetings, General) or any custom name; the capitalisation you choose is what room_list shows back."},
                    "description": {"type": "string", "description": "Optional human note about what belongs in this room."},
                    "wing":        wing_arg.clone()
                },
                "required": room_create_required,
            }
        }),
        json!({
            "name": "room_rename",
            "description": "Rename a room (ADR-0027). This is the repair path for rooms listed with `resolved: false` — placeholders the migration created when it could not recover a room's original name. A rename changes ONLY the room's name: no drawer is moved, reassigned, or rewritten, and every drawer keeps the room it is already in. Refuses when the new name already belongs to a different room (merging two rooms is not supported). Returns { palace, room_id, label }.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "palace":    {"type": "string", "description": "Palace ID (optional if server started with --palace)"},
                    "room":      {"type": "string", "description": "The room to rename: either its `room_id` UUID or its current label (case-insensitive), both as shown by room_list."},
                    "new_label": {"type": "string", "description": "The new name. Must not already belong to another room."}
                },
                "required": room_rename_required,
            }
        }),
    ]
}
