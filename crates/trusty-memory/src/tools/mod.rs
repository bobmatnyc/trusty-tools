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
//! - `memory_forget(palace, drawer_id)`            -> status: deleted|not_found
//! - `palace_create(name, description?)`           -> PalaceId
//! - `palace_list()`                                -> Vec<PalaceId>
//! - `palace_info(palace)`                          -> palace metadata + stats
//! - `room_list(palace)`                            -> Vec<RoomSummary>
//! - `room_create(palace, label, description?)`     -> room_id (idempotent)
//! - `room_rename(palace, room, new_label)`         -> renamed room
//! - `kg_assert(palace, subject, predicate, object, confidence?, provenance?)` -> ()
//! - `kg_retract_triple(palace, subject, predicate, object)` -> closed count
//! - `kg_query(palace, subject)`                    -> Vec<Triple>
//! - `kg_list_subjects(palace, limit?, with_counts?)` -> subjects (#4776)
//! - `wing_list(palace)`                            -> Vec<WingSummary>
//! - `wing_create(palace, label)`                   -> wing_id (idempotent)
//! - `wing_rename(palace, wing, new_label)`         -> WingSummary

pub mod bm25;
pub mod chat_definitions;
pub mod chat_ops;
pub mod definitions;
pub mod dream_ops;
// #5000 / #4786: answer "is this findable?" per id and per estate.
pub mod embed_audit;
pub mod embed_audit_definitions;
pub mod helpers;
pub mod kg_ops;
pub mod memory_ops;
pub mod palace_ops;
pub mod room_definitions;
pub mod room_ops;
pub mod task_definitions;
pub mod task_ops;
// ADR-0027 T9 (#4809): the wing surface ships WITH the wing entity — a level
// nobody reads is the defect the ADR exists to correct.
pub mod wing_definitions;
pub mod wing_ops;

// Re-export the public + cross-module surface so external call sites
// (`crate::tools::X`) and the `super::*` glob in `tools::tests` keep
// resolving exactly as they did against the former monolithic module.
pub use bm25::{spawn_bm25_index_worker, Bm25IndexRequest, BM25_INDEX_QUEUE_CAPACITY};
pub use definitions::{tool_definitions, tool_definitions_with, MemoryMcpServer};
pub(crate) use helpers::{auto_extract_and_assert, room_label};

// Re-exports used only by the in-crate test module (`super::*`).
#[cfg(test)]
pub(crate) use bm25::{bm25_hits_to_recall_results, bm25_index_enqueue};
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
    handle_kg_bootstrap, handle_kg_gaps, handle_kg_list_subjects, handle_kg_query,
    handle_kg_retract_triple, handle_list_prompt_facts, handle_remove_prompt_fact,
    handle_upgrade_tool,
};
use memory_ops::{
    handle_memory_forget, handle_memory_list, handle_memory_note, handle_memory_recall,
    handle_memory_recall_all, handle_memory_recall_deep, handle_memory_remember,
    handle_memory_send_message,
};
use palace_ops::{
    handle_palace_compact, handle_palace_create, handle_palace_delete, handle_palace_info,
    handle_palace_list, handle_palace_reembed, handle_palace_unalias, handle_palace_update,
};
use room_ops::{handle_room_create, handle_room_list, handle_room_rename};
use task_ops::{handle_task_add, handle_task_complete, handle_task_list};
use wing_ops::{handle_wing_create, handle_wing_list, handle_wing_rename};

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
    // #6424: a successful recall, remember or note is what the console's Last
    // Used column means by "used". The args are cloned only for those four
    // tools, since the dispatch below consumes them.
    let stamp_args = USE_STAMPING_TOOLS.contains(&name).then(|| args.clone());
    let result = dispatch_tool_inner(state, name, args).await;
    if let (Ok(_), Some(args)) = (&result, &stamp_args) {
        stamp_palace_use(state, args, name);
    }
    result
}

/// The tools whose success counts as using a palace (#6424).
///
/// Why: the column answers "when did anyone last put something in or take
/// something out of this palace". Housekeeping — `palace_info`, `console_metrics`,
/// the embed-audit sweeps that open every palace on disk — is not use, and
/// stamping on it would make every palace look equally fresh forever.
/// What: the read and write verbs a caller reaches for deliberately.
/// `memory_recall_all` is absent on purpose: it spans every palace, so it says
/// nothing about any one of them.
/// Test: `dispatch_remember_and_recall_stamp_last_used`,
/// `dispatch_palace_info_does_not_stamp_last_used` in `tools::tests`.
const USE_STAMPING_TOOLS: &[&str] = &[
    "memory_remember",
    "memory_note",
    "memory_recall",
    "memory_recall_deep",
];

/// Record the palace a just-completed tool call used (#6424).
///
/// Why: one place, so no handler can drift into its own cadence — the throttle
/// and its cost live in [`crate::palace_last_used`].
/// What: resolves the same palace id the handler resolved, follows a palace
/// alias to its target the way the handler's own open did, takes the data
/// directory off the already-resident handle via `peek` (no open, no eviction,
/// no I/O), and hands both to the throttled stamp. Every step is best-effort:
/// an unresolvable palace, a handle the registry has since evicted, or a failed
/// write all leave the column stale rather than fail the caller's operation,
/// which has already succeeded.
///
/// Dropping the alias step breaks the column outright (#6424 review), which is
/// why it is here. `resolve_palace` returns the caller's raw `palace` argument,
/// but
/// `PalaceRegistry::open_palace_bounded` registers the handle under
/// `resolve_palace_alias`'s CANONICAL id, and `peek` is a bare LRU lookup that
/// resolves nothing. Keying on the raw string therefore missed on every
/// alias-addressed call, and the column never advanced for as long as callers
/// used the alias.
/// Test: `dispatch_remember_and_recall_stamp_last_used`,
/// `an_alias_addressed_recall_stamps_the_canonical_palace`.
fn stamp_palace_use(state: &AppState, args: &Value, tool: &str) {
    let Ok(palace) = helpers::resolve_palace(state, args, tool) else {
        return;
    };
    // The same rule `PalaceRegistry::resolve_palace_alias` applies, from the
    // one place that owns it — a second spelling here is how the two drift.
    let palace = trusty_common::palace_alias::alias_target_if_absent(&state.data_root, &palace)
        .unwrap_or(palace);
    let id = trusty_common::memory_core::PalaceId::new(&palace);
    let Some(data_dir) = state.registry.peek(&id).and_then(|h| h.data_dir.clone()) else {
        return;
    };
    crate::palace_last_used::stamp(
        &state.palace_last_used,
        &data_dir,
        &palace,
        crate::palace_last_used::now_unix(),
    );
}

/// The name -> handler match, unwrapped from [`dispatch_tool`]'s stamping.
async fn dispatch_tool_inner(state: &AppState, name: &str, args: Value) -> Result<Value> {
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
        // The inverse of `kg_assert`: closes one (subject, predicate, object)
        // and leaves the pair's other objects live.
        "kg_retract_triple" => handle_kg_retract_triple(state, args).await,
        "add_alias" => handle_add_alias(state, args).await,
        "list_prompt_facts" => handle_list_prompt_facts(state, args).await,
        "remove_prompt_fact" => handle_remove_prompt_fact(state, args).await,
        "kg_query" => handle_kg_query(state, args).await,
        // #4776: subject discovery — the read that makes `kg_query` usable
        // without already knowing a subject.
        "kg_list_subjects" => handle_kg_list_subjects(state, args).await,
        "memory_list" => handle_memory_list(state, args).await,
        "memory_forget" => handle_memory_forget(state, args).await,
        "palace_info" => handle_palace_info(state, args).await,
        "palace_compact" => handle_palace_compact(state, args).await,
        // #4906: report / repair drawers that have no vector.
        "palace_reembed" => handle_palace_reembed(state, args).await,
        // #5005: free drawers destroyed by a vector-id collision.
        "palace_unalias" => handle_palace_unalias(state, args).await,
        // #5000: verify a caller's OWN drawer ids, not the whole missing set.
        "palace_verify_embedded" => embed_audit::handle_palace_verify_embedded(state, args).await,
        // #5000 / #4786: every palace on disk, uncapped — the console report is
        // capped at 20 and shows an uncached palace as 0/0, i.e. healthy.
        "palace_embed_sweep" => embed_audit::handle_palace_embed_sweep(state, args).await,
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
        // ADR-0027 T6 (#4805): the room surface — discovery, idempotent
        // creation, and the rename that repairs an `unresolved-*` label.
        "room_list" => handle_room_list(state, args).await,
        "room_create" => handle_room_create(state, args).await,
        "room_rename" => handle_room_rename(state, args).await,
        // ADR-0027 T9 (#4809): the wing surface — the scope axis over rooms.
        "wing_list" => handle_wing_list(state, args).await,
        "wing_create" => handle_wing_create(state, args).await,
        "wing_rename" => handle_wing_rename(state, args).await,
        other => anyhow::bail!("unknown tool: {other}"),
    }
}

#[cfg(test)]
mod tests;
