//! MCP tool schema definitions for the chat-session + dream/consolidation
//! tools (spec-001).
//!
//! Why: `definitions.rs` grew past the 500-SLOC production cap once DOC-53's
//! `cwd`/`workstream` attribution fields were added to `memory_remember`/
//! `memory_note`/`memory_send_message`. This module follows the exact
//! precedent `task_definitions.rs` set (issue #1722): extracting a coherent
//! tool group to a sibling module keeps each file focused and lets the cap
//! be honoured without splitting `definitions.rs` itself.
//! What: exports `chat_tool_definitions(has_default)` — returns a
//! `Vec<Value>` containing the chat-session CRUD tools
//! (`chat_session_create`/`_add_turn`/`_get`/`_list`/`_recall`/`_delete`,
//! `chat_turn_append`) and the on-demand consolidation tools
//! (`dream_consolidate_room`, `palace_dream`), ready to be spliced into the
//! main tools array in `tool_definitions_with`.
//! Test: `tool_definitions_lists_all_tools` in `tools::tests` verifies the
//! names appear in the merged list.

use serde_json::{json, Value};

/// Build the chat-session + dream/consolidation tool schemas conditioned on
/// whether a default palace is configured.
///
/// Why: follows the same `has_default` pattern as every other tool group so
/// the `palace` argument moves out of `required` when the server is bound to
/// a single palace via `--palace`.
/// What: returns the nine tool schemas listed in the module doc as
/// `Vec<Value>`.
/// Test: spliced into `tool_definitions_with` and covered by
/// `tool_definitions_lists_all_tools` in `tools::tests`.
pub(super) fn chat_tool_definitions(has_default: bool) -> Vec<Value> {
    let chat_session_palace_required: Vec<&str> = if has_default { vec![] } else { vec!["palace"] };
    let chat_session_get_required: Vec<&str> = if has_default {
        vec!["session_id"]
    } else {
        vec!["palace", "session_id"]
    };
    let chat_session_add_turn_required: Vec<&str> = if has_default {
        vec!["session_id", "role", "content"]
    } else {
        vec!["palace", "session_id", "role", "content"]
    };
    let dream_consolidate_room_required: Vec<&str> =
        if has_default { vec![] } else { vec!["palace"] };
    // chat_turn_append requires palace + session_id + prompt + response.
    let chat_turn_append_required: Vec<&str> = if has_default {
        vec!["session_id", "prompt", "response"]
    } else {
        vec!["palace", "session_id", "prompt", "response"]
    };
    let chat_session_delete_required: Vec<&str> = if has_default {
        vec!["session_id"]
    } else {
        vec!["palace", "session_id"]
    };

    vec![
        json!({
            "name": "chat_session_create",
            "description": "Create a new chat session in a palace (spec-001 chat-session manager). Returns the session id, its creation timestamp, and the message count (0 for a fresh session). Pass an optional session_id to use a caller-chosen id (idempotent — an existing session is returned unchanged); pass an optional title to name a server-generated session. Sessions are stored in the palace's dedicated redb chat store, NOT the generic memory drawer surface.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "palace":     {"type": "string", "description": "Palace slug (optional if server started with --palace)"},
                    "session_id": {"type": "string", "description": "Optional caller-supplied session id; a UUID is generated when omitted."},
                    "title":      {"type": "string", "description": "Optional session name (applied only when session_id is omitted)."}
                },
                "required": chat_session_palace_required,
            }
        }),
        json!({
            "name": "chat_session_add_turn",
            "description": "Append a message (prompt or response) to a chat session's history. Creates the session if it does not yet exist. Returns the new message_count and updated_at. Bypasses the memory_remember signal/noise + dedup gates so sequential conversational turns persist verbatim.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "palace":     {"type": "string"},
                    "session_id": {"type": "string"},
                    "role":       {"type": "string", "enum": ["user", "assistant", "system"]},
                    "content":    {"type": "string"}
                },
                "required": chat_session_add_turn_required,
            }
        }),
        json!({
            "name": "chat_session_get",
            "description": "Retrieve a full chat session: metadata plus every turn in chronological order. Errors if the session id is unknown.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "palace":     {"type": "string"},
                    "session_id": {"type": "string"}
                },
                "required": chat_session_get_required,
            }
        }),
        json!({
            "name": "chat_session_list",
            "description": "List chat sessions in a palace as paginated metadata (id, title, timestamps, message_count) ordered most-recently-updated first. Does not include message bodies. Returns { sessions, total_count }.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "palace": {"type": "string"},
                    "limit":  {"type": "integer", "default": 50},
                    "offset": {"type": "integer", "default": 0}
                },
                "required": chat_session_palace_required,
            }
        }),
        json!({
            "name": "dream_consolidate_room",
            "description": "Trigger LLM-driven semantic consolidation for one room (or all rooms) of a palace, on demand and synchronously (spec-001). Consolidates facts older than max_age_days into canonical summaries, then evicts the superseded originals so history shrinks. Task drawers are always skipped. No-op (zero counts) when no inference backend (OpenRouter key / local model) is configured. Returns { summary_facts_created, facts_evicted }.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "palace":       {"type": "string"},
                    "room":         {"type": "string", "description": "Room to scope to (e.g. Backend, Planning, or a custom name). Omit or null to consolidate all rooms."},
                    "max_age_days": {"type": "integer", "default": 7, "description": "Only consolidate facts older than this many days."}
                },
                "required": dream_consolidate_room_required,
            }
        }),
        json!({
            "name": "palace_dream",
            "description": "On-demand LLM-driven consolidation for a palace (issue #1721). Alias for dream_consolidate_room with the same parameters; use this name when following the palace_* convention. Triggers a scoped dream/consolidation cycle immediately for the named palace, optionally filtered to one room. Task drawers are always skipped. Gracefully returns zero counts when no inference backend is configured. Returns { palace, room, summary_facts_created, facts_evicted }.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "palace":       {"type": "string"},
                    "room":         {"type": "string", "description": "Room to scope to. Omit or null to consolidate all rooms."},
                    "max_age_days": {"type": "integer", "default": 7, "description": "Only consolidate facts older than this many days."}
                },
                "required": dream_consolidate_room_required,
            }
        }),
        json!({
            "name": "chat_session_recall",
            "description": "Retrieve a full chat session with all turns in order (alias for chat_session_get, preferred name for agent-facing recall). Errors if the session id is unknown.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "palace":     {"type": "string"},
                    "session_id": {"type": "string"}
                },
                "required": chat_session_get_required,
            }
        }),
        json!({
            "name": "chat_session_delete",
            "description": "Delete a chat session (and its full history) from a palace. Idempotent: deleting an unknown session id is a no-op, not an error. Returns { deleted: session_id }.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "palace":     {"type": "string"},
                    "session_id": {"type": "string"}
                },
                "required": chat_session_delete_required,
            }
        }),
        json!({
            "name": "chat_turn_append",
            "description": "Append a prompt/response PAIR to a chat session as two consecutive messages (user role then assistant role). Atomic at the session level — both messages are written together. Creates the session implicitly when it does not exist. Returns { message_count, updated_at }.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "palace":     {"type": "string"},
                    "session_id": {"type": "string"},
                    "prompt":     {"type": "string", "description": "User-side message (stored with role=user)."},
                    "response":   {"type": "string", "description": "Assistant-side message (stored with role=assistant)."}
                },
                "required": chat_turn_append_required,
            }
        }),
    ]
}
