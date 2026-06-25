//! MCP tool surface for trusty-memory.
//!
//! Why: Concentrates the public tool contract in one file so changes are
//! auditable and the MCP schema stays in sync with the implementation.
//! What: Defines `MemoryMcpServer`, `tool_definitions()` (the MCP
//! `tools/list` payload), and the in-process tool dispatcher wired to the
//! real `PalaceRegistry` + retrieval / KG APIs.
//! Test: `cargo test -p trusty-memory-mcp` validates the schema and dispatch.
//!
//! Tools exposed:
//! - `memory_remember(palace, text, room?, tags?)` -> drawer_id
//! - `memory_recall(palace, query, top_k?)`        -> Vec<Drawer> (L0+L1+L2)
//! - `memory_recall_deep(palace, query, top_k?)`   -> Vec<Drawer> (L3 deep)
//! - `memory_list(palace, room?, tag?, limit?)`    -> Vec<Drawer>
//! - `memory_forget(palace, drawer_id)`            -> ()
//! - `palace_create(name, description?)`           -> PalaceId
//! - `palace_list()`                                -> Vec<PalaceId>
//! - `palace_info(palace)`                          -> palace metadata + stats
//! - `kg_assert(palace, subject, predicate, object, confidence?, provenance?)` -> ()
//! - `kg_query(palace, subject)`                    -> Vec<Triple>

pub mod bm25;
pub mod chat_ops;
pub mod definitions;
pub mod dream_ops;
pub mod helpers;
pub mod kg_ops;
pub mod memory_ops;
pub mod palace_ops;
pub mod task_definitions;
pub mod task_ops;

// Re-export the public + cross-module surface so external call sites
// (`crate::tools::X`) and the `super::*` glob in `tools::tests` keep
// resolving exactly as they did against the former monolithic module.
pub use bm25::{spawn_bm25_index_worker, Bm25IndexRequest, BM25_INDEX_QUEUE_CAPACITY};
pub use definitions::{tool_definitions, tool_definitions_with, MemoryMcpServer};
pub(crate) use helpers::{auto_extract_and_assert, room_label};

// Re-exports used only by the in-crate test module (`super::*`).
#[cfg(test)]
pub(crate) use bm25::bm25_index_enqueue;
#[cfg(test)]
pub(crate) use helpers::{blocklist_gate, content_gate, dedup_gate, open_palace_handle};

use crate::AppState;
use anyhow::Result;
use serde_json::Value;

use chat_ops::{
    handle_chat_session_add_turn, handle_chat_session_create, handle_chat_session_delete,
    handle_chat_session_get, handle_chat_session_list, handle_chat_session_recall,
    handle_chat_turn_append,
};
use dream_ops::{handle_dream_consolidate_room, handle_palace_dream};
use kg_ops::{
    handle_add_alias, handle_discover_aliases, handle_get_prompt_context, handle_kg_assert,
    handle_kg_bootstrap, handle_kg_gaps, handle_kg_query, handle_list_prompt_facts,
    handle_remove_prompt_fact, handle_upgrade_tool,
};
use memory_ops::{
    handle_memory_forget, handle_memory_list, handle_memory_note, handle_memory_recall,
    handle_memory_recall_all, handle_memory_recall_deep, handle_memory_remember,
    handle_memory_send_message,
};
use palace_ops::{
    handle_palace_compact, handle_palace_create, handle_palace_delete, handle_palace_info,
    handle_palace_list, handle_palace_update,
};
use task_ops::{handle_task_add, handle_task_complete, handle_task_list};

/// Dispatch a tool call by name to its real handler.
///
/// Why: Centralises the name → handler mapping; every handler now performs a
/// real read/write against the live `PalaceRegistry` instead of returning a
/// stub. After issue #227 the body is a thin router — every tool's logic
/// lives in its own `handle_*` function above so the dispatcher itself is
/// auditable at a glance.
/// What: Returns `Ok(Value)` on success, `Err` on unknown tool / bad args /
/// underlying failure.
/// Test: `dispatch_palace_create_persists`, `dispatch_remember_then_recall`,
/// `dispatch_kg_assert_then_query`, `dispatch_unknown_tool_errors`.
pub async fn dispatch_tool(state: &AppState, name: &str, args: Value) -> Result<Value> {
    match name {
        "memory_remember" => handle_memory_remember(state, args).await,
        "memory_note" => handle_memory_note(state, args).await,
        "memory_recall" => handle_memory_recall(state, args).await,
        "memory_recall_deep" => handle_memory_recall_deep(state, args).await,
        "palace_create" => handle_palace_create(state, args).await,
        "palace_list" => handle_palace_list(state, args).await,
        "palace_delete" => handle_palace_delete(state, args).await,
        "palace_update" => handle_palace_update(state, args).await,
        "kg_assert" => handle_kg_assert(state, args).await,
        "add_alias" => handle_add_alias(state, args).await,
        "list_prompt_facts" => handle_list_prompt_facts(state, args).await,
        "remove_prompt_fact" => handle_remove_prompt_fact(state, args).await,
        "kg_query" => handle_kg_query(state, args).await,
        "memory_list" => handle_memory_list(state, args).await,
        "memory_forget" => handle_memory_forget(state, args).await,
        "palace_info" => handle_palace_info(state, args).await,
        "palace_compact" => handle_palace_compact(state, args).await,
        "kg_gaps" => handle_kg_gaps(state, args).await,
        "memory_recall_all" => handle_memory_recall_all(state, args).await,
        "get_prompt_context" => handle_get_prompt_context(state, args).await,
        "discover_aliases" => handle_discover_aliases(state, args).await,
        "kg_bootstrap" => handle_kg_bootstrap(state, args).await,
        "memory_send_message" => handle_memory_send_message(state, args).await,
        "upgrade" => handle_upgrade_tool(state, args).await,
        "console_metrics" => crate::console_metrics::handle_console_metrics(state, args).await,
        "chat_session_create" => handle_chat_session_create(state, args).await,
        "chat_session_add_turn" => handle_chat_session_add_turn(state, args).await,
        "chat_session_get" => handle_chat_session_get(state, args).await,
        "chat_session_recall" => handle_chat_session_recall(state, args).await,
        "chat_session_list" => handle_chat_session_list(state, args).await,
        "chat_session_delete" => handle_chat_session_delete(state, args).await,
        "chat_turn_append" => handle_chat_turn_append(state, args).await,
        "dream_consolidate_room" => handle_dream_consolidate_room(state, args).await,
        "palace_dream" => handle_palace_dream(state, args).await,
        "task_add" => handle_task_add(state, args).await,
        "task_list" => handle_task_list(state, args).await,
        "task_complete" => handle_task_complete(state, args).await,
        other => anyhow::bail!("unknown tool: {other}"),
    }
}

#[cfg(test)]
mod tests;
