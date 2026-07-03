//! Task-drawer MCP tool handlers (spec-001 Phase 4, issue #1722).
//!
//! Why: `DrawerType::Task` drawers already exist in trusty-common with dream-cycle
//! protection (`is_protected()` = true), but there was no way to create, list, or
//! complete them over the MCP surface. Applications driving trusty-memory over MCP
//! must be able to manage task drawers across sessions.
//! What: three `pub(crate) async fn handle_task_{add,list,complete}` handlers that
//! create/query/complete Task drawers via the standard palace storage pipeline.
//! Test: `crates/trusty-memory/tests/task_mcp.rs`.

use crate::{AppState, DaemonReadiness};
use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use serde_json::{json, Value};
use trusty_common::memory_core::palace::DrawerType;
use trusty_common::memory_core::retrieval::RememberOptions;
use trusty_common::memory_core::timeouts;
use uuid::Uuid;

use super::helpers::{
    open_palace_handle, parse_room, parse_tags, resolve_palace, room_label, write_drawer,
    WriteDrawerParams,
};

/// Create a Task drawer in a palace via the MCP surface.
///
/// Why: Task drawers are protected from the dream cycle (never evicted or
/// consolidated), making them suitable for goals, milestones, and any long-lived
/// context an application must retain across sessions. Without this tool, callers
/// had no way to create `DrawerType::Task` drawers over MCP (issue #1722).
/// What: Resolves the palace, parses content/room/tags from args, then calls
/// `write_drawer` with `RememberOptions { force: true, classify_as: Some(Task) }`
/// so the short-content filter is bypassed and the drawer lands with the Task type.
/// MCP attribution is intentionally NOT attached — task drawers represent explicit
/// user intent, not auto-capture events.
/// Returns `{ drawer_id, palace, status: "stored", drawer_type: "Task" }`.
/// Test: `task_add_and_list_via_mcp` in `tests/task_mcp.rs`.
pub(crate) async fn handle_task_add(state: &AppState, args: Value) -> Result<Value> {
    // Issue #1970: same non-blocking write posture as memory_remember — the
    // KG/redb write below never touches the embedder, so a warming daemon
    // defers vector embedding to a background task instead of erroring.
    let defer_embedding = state.readiness() == DaemonReadiness::Warming;
    let palace = resolve_palace(state, &args, "task_add")?;
    let palace = palace.as_str();
    let content = args
        .get("content")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("task_add: missing 'content'"))?
        .to_string();
    let room = parse_room(args.get("room").and_then(|v| v.as_str()));
    let tags = parse_tags(&args);
    let room_label_for_kg = room_label(&room);

    // Serialise the gate-check + write sequence per palace (same pattern as
    // `handle_memory_remember`) so two concurrent task_add calls on the same
    // palace can't race on the redb row insert.
    let write_lock = state.palace_write_lock(palace);
    let _write_guard =
        timeouts::lock_with_timeout(&write_lock, timeouts::write_lock_timeout(), palace)
            .await
            .map_err(|e| anyhow!("task_add: {e:#}"))?;

    let drawer_id = write_drawer(
        state,
        WriteDrawerParams {
            palace_id: palace,
            content,
            tags,
            room,
            importance: 1.0, // tasks are always high-importance
            opts: RememberOptions {
                force: true, // bypass token/noise filters
                enforce_min_tokens: false,
                classify_as: Some(DrawerType::Task),
                defer_embedding,
                ..RememberOptions::default()
            },
            room_label_for_kg,
        },
    )
    .await?;

    Ok(json!({
        "drawer_id": drawer_id.to_string(),
        "palace": palace,
        "status": "stored",
        "drawer_type": "Task",
    }))
}

/// List Task drawers in a palace.
///
/// Why: Applications need to enumerate open (or all) tasks without iterating
/// every drawer via `memory_list`. This tool provides a type-filtered view over
/// the in-memory drawer table (issue #1722).
/// What: Opens the palace handle, reads the drawer table under a read lock,
/// filters to `drawer_type == Task`, then optionally excludes completed drawers
/// when `include_completed` is false (the default). Returns
/// `{ palace, tasks: [ { drawer_id, content, importance, tags, created_at,
///   completed_at, drawer_type } ] }`.
/// Test: `task_add_and_list_via_mcp`, `task_complete_sets_completed_at`
/// in `tests/task_mcp.rs`.
pub(crate) async fn handle_task_list(state: &AppState, args: Value) -> Result<Value> {
    let palace = resolve_palace(state, &args, "task_list")?;
    let include_completed = args
        .get("include_completed")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let handle = open_palace_handle(state, &palace)?;
    let tasks: Vec<Value> = {
        let drawers = handle.drawers.read();
        drawers
            .iter()
            .filter(|d| d.drawer_type == DrawerType::Task)
            .filter(|d| include_completed || d.completed_at.is_none())
            .map(|d| {
                json!({
                    "drawer_id": d.id.to_string(),
                    "content": d.content,
                    "importance": d.importance,
                    "tags": d.tags,
                    "created_at": d.created_at.to_rfc3339(),
                    "completed_at": d.completed_at.map(|t| t.to_rfc3339()),
                    "drawer_type": "Task",
                })
            })
            .collect()
    };

    Ok(json!({ "palace": palace, "tasks": tasks }))
}

/// Mark a Task drawer as completed by setting its `completed_at` timestamp.
///
/// Why: Completed tasks should no longer appear in the default `task_list` view
/// but must be retained on disk (they are never auto-evicted, even after
/// completion). This tool closes the open-task lifecycle without deleting the
/// underlying drawer (issue #1722).
/// What: Resolves the palace, parses `drawer_id` as a UUID, locates the drawer
/// in the in-memory table, verifies it is a Task, sets `completed_at = Utc::now()`,
/// persists the change via `handle.kg.upsert_drawer`, and updates the in-memory
/// table. Returns `{ palace, drawer_id, completed_at, status: "completed" }`.
/// Errors if the drawer is not found or is not a Task.
/// Test: `task_complete_sets_completed_at`, `task_complete_rejects_non_task_drawer`
/// in `tests/task_mcp.rs`.
pub(crate) async fn handle_task_complete(state: &AppState, args: Value) -> Result<Value> {
    let palace = resolve_palace(state, &args, "task_complete")?;
    let drawer_id_str = args
        .get("drawer_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("task_complete: missing 'drawer_id'"))?;
    let drawer_id = Uuid::parse_str(drawer_id_str)
        .map_err(|e| anyhow!("task_complete: invalid drawer_id UUID: {e}"))?;

    let handle = open_palace_handle(state, &palace)?;

    // Locate the drawer and verify it is a Task before mutating anything.
    let drawer = {
        let drawers = handle.drawers.read();
        drawers
            .iter()
            .find(|d| d.id == drawer_id)
            .cloned()
            .ok_or_else(|| anyhow!("task_complete: drawer not found: {drawer_id}"))?
    };

    if drawer.drawer_type != DrawerType::Task {
        return Err(anyhow!(
            "task_complete: drawer {drawer_id} is not a Task (type: {})",
            drawer.drawer_type.as_str()
        ));
    }

    let now = Utc::now();
    let mut updated = drawer;
    updated.completed_at = Some(now);

    // Persist the updated metadata so the completion survives a restart.
    handle
        .kg
        .upsert_drawer(&updated)
        .await
        .context("task_complete: persist updated drawer")?;

    // Update the in-memory table so subsequent task_list calls reflect the change.
    {
        let mut drawers = handle.drawers.write();
        if let Some(d) = drawers.iter_mut().find(|d| d.id == drawer_id) {
            d.completed_at = Some(now);
        }
    }

    Ok(json!({
        "palace": palace,
        "drawer_id": drawer_id_str,
        "completed_at": now.to_rfc3339(),
        "status": "completed",
    }))
}
