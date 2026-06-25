//! Chat-session MCP tool handlers (spec-001 Phase 2).
//!
//! Why: trusty-memory already owns a redb-backed `ChatSessionStore` per palace
//! (used by the HTTP chat UI), but it was unreachable over MCP. Applications
//! driving trusty-memory as a dedicated chat-session manager need to create
//! sessions, append prompt/response turns, and read history back over the same
//! MCP surface they use for everything else. These handlers expose that store
//! directly — deliberately NOT routing through `memory_remember`, whose
//! signal/noise + 5-minute dedup gates are hostile to sequential conversational
//! turns.
//! What: four `pub(crate) async fn handle_chat_session_*` handlers wrapping the
//! existing store methods (`create_session` / `upsert_session` / `get_session`
//! / `list_sessions`). Visibility is `pub(crate)` so the dispatcher in
//! `tools::mod` can route to them.
//! Test: `crates/trusty-memory/tests/chat_mcp.rs`.

use crate::AppState;
use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use trusty_common::memory_core::store::chat_sessions::ChatMessage;

use super::helpers::resolve_palace;

/// Roles accepted on a chat turn. Mirrors the OpenAI/Anthropic message-role
/// vocabulary the spec's `chat_session_add_turn` schema enumerates.
const VALID_ROLES: [&str; 3] = ["user", "assistant", "system"];

/// Create (or reference) a chat session in a palace.
///
/// Why: applications open a session before streaming turns into it; returning
/// the id (and current count) lets the caller thread it through subsequent
/// `chat_session_add_turn` calls.
/// What: when `session_id` is omitted, delegates to `ChatSessionStore::
/// create_session(title)` which mints a fresh UUID and persists the title.
/// When `session_id` is supplied, creates an empty session under that id via
/// `upsert_session(id, &[])` if it does not already exist (idempotent); the
/// optional `title` is only honoured on the generated-id path because the
/// reused `upsert_session` API does not carry a title. Always reads the row
/// back so the response reflects persisted state.
/// Test: `chat_session_create_returns_id`,
/// `chat_session_create_with_explicit_id` in `tests/chat_mcp.rs`.
pub(crate) async fn handle_chat_session_create(state: &AppState, args: Value) -> Result<Value> {
    let palace = resolve_palace(state, &args, "chat_session_create")?;
    let store = state.session_store(&palace)?;
    let title = args
        .get("title")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let session_id = match args.get("session_id").and_then(|v| v.as_str()) {
        Some(id) => {
            // Idempotent: only seed an empty row when the id is new so we never
            // clobber an existing session's history.
            if store.get_session(id)?.is_none() {
                store.upsert_session(id, &[])?;
            }
            id.to_string()
        }
        None => store.create_session(title)?,
    };

    let session = store
        .get_session(&session_id)?
        .ok_or_else(|| anyhow!("chat_session_create: session vanished after write"))?;
    Ok(json!({
        "session_id": session.id,
        "created_at": session.created_at,
        "message_count": session.history.len(),
    }))
}

/// Append one message (prompt or response) to a session's history.
///
/// Why: each conversational turn must persist immediately and survive daemon
/// restarts; appending here (rather than via `memory_remember`) keeps turns out
/// of the noisy generic dedup path.
/// What: validates `role` against [`VALID_ROLES`], loads the existing history
/// (creating the session implicitly when missing, per spec), pushes the new
/// `ChatMessage`, and writes it back via `upsert_session`. Reads the row back
/// to return the authoritative `message_count` and `updated_at`.
/// Test: `chat_session_add_turn_appends`,
/// `chat_session_add_turn_rejects_bad_role` in `tests/chat_mcp.rs`.
pub(crate) async fn handle_chat_session_add_turn(state: &AppState, args: Value) -> Result<Value> {
    let palace = resolve_palace(state, &args, "chat_session_add_turn")?;
    let session_id = args
        .get("session_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("chat_session_add_turn: missing 'session_id'"))?;
    let role = args
        .get("role")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("chat_session_add_turn: missing 'role'"))?;
    if !VALID_ROLES.contains(&role) {
        return Err(anyhow!(
            "chat_session_add_turn: invalid role '{role}' (expected one of {VALID_ROLES:?})"
        ));
    }
    let content = args
        .get("content")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("chat_session_add_turn: missing 'content'"))?;

    let store = state.session_store(&palace)?;
    // Load → append → write. Missing session => start from empty history so the
    // turn implicitly creates the session (spec contract).
    let mut history = store
        .get_session(session_id)?
        .map(|s| s.history)
        .unwrap_or_default();
    history.push(ChatMessage {
        role: role.to_string(),
        content: content.to_string(),
    });
    store.upsert_session(session_id, &history)?;

    let session = store
        .get_session(session_id)?
        .ok_or_else(|| anyhow!("chat_session_add_turn: session vanished after write"))?;
    Ok(json!({
        "message_count": session.history.len(),
        "updated_at": session.updated_at,
    }))
}

/// Fetch a full session (metadata + every turn in order).
///
/// Why: resuming a conversation needs the entire message log in one call.
/// What: reads the row via `get_session`; errors with a clear not-found message
/// when the id is unknown. Serialises the `ChatSession` verbatim.
/// Test: `chat_session_get_round_trips`,
/// `chat_session_get_missing_errors` in `tests/chat_mcp.rs`.
pub(crate) async fn handle_chat_session_get(state: &AppState, args: Value) -> Result<Value> {
    let palace = resolve_palace(state, &args, "chat_session_get")?;
    let session_id = args
        .get("session_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("chat_session_get: missing 'session_id'"))?;
    let store = state.session_store(&palace)?;
    let session = store
        .get_session(session_id)?
        .ok_or_else(|| anyhow!("chat_session_get: session not found: {session_id}"))?;
    Ok(serde_json::to_value(session)?)
}

/// List session metadata in a palace (paginated; no history bodies).
///
/// Why: a session sidebar / management view needs a recent-first list without
/// paying to decode every history blob.
/// What: calls `list_sessions` (already sorted `updated_at` DESC), records the
/// unpaginated `total_count`, then applies `offset` + `limit` (defaults 0 / 50)
/// to the slice it returns.
/// Test: `chat_session_list_paginates` in `tests/chat_mcp.rs`.
pub(crate) async fn handle_chat_session_list(state: &AppState, args: Value) -> Result<Value> {
    let palace = resolve_palace(state, &args, "chat_session_list")?;
    let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(50) as usize;
    let offset = args.get("offset").and_then(|v| v.as_u64()).unwrap_or(0) as usize;

    let store = state.session_store(&palace)?;
    let metas = store.list_sessions()?;
    let total_count = metas.len();
    let page: Vec<_> = metas.into_iter().skip(offset).take(limit).collect();
    Ok(json!({
        "sessions": serde_json::to_value(page)?,
        "total_count": total_count,
    }))
}
