//! Room-tool handlers for the trusty-memory MCP surface (ADR-0027 T6 / #4805).
//!
//! Why: rooms have been live in the data for a long time — a quarter of the
//! drawers in the largest palace sit in eleven non-`General` rooms — but had no
//! registry, so a caller could not list them, could not repair a name, and
//! could not learn a room existed without already knowing the word. These three
//! tools are the door onto the `ROOMS` table ADR-0027 added.
//! What: `room_list` (discovery), `room_create` (idempotent), and `room_rename`
//! (the repair path for `unresolved-*` labels). Every redb call runs on the
//! blocking pool; none of them writes to the `DRAWERS` table.
//! Test: `dispatch_room_list_reports_rooms_with_drawer_counts`,
//! `dispatch_room_list_rejects_an_unknown_wing`,
//! `dispatch_room_create_is_idempotent`,
//! `dispatch_room_rename_leaves_drawers_in_place`,
//! `dispatch_room_rename_rejects_a_taken_name` in `tools::tests`.

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use std::collections::HashMap;
use trusty_common::memory_core::room_identity::{
    parse_room_preserving_case, room_type_tag, DEFAULT_WING_ID,
};
use trusty_common::memory_core::store::rooms::{
    create_room, list_room_summaries, rename_room, resolve_room_selector, RoomSummary,
};
use uuid::Uuid;

use super::helpers::{open_palace_handle, resolve_palace};
use crate::AppState;

/// Validate the reserved `wing` argument.
///
/// Why (ADR-0027 D2): the argument name is declared now so it is stable, but
/// the Wing entity is explicitly gated on ticket T9 and its #3064 consumer.
/// Silently ignoring a wing the caller named would be wrong, and silently
/// returning an empty list would be worse — both are invisible failures. So a
/// wing that is not the palace's default is a loud error.
/// What: `Ok(())` when absent, empty, or exactly `DEFAULT_WING_ID`.
/// Test: `dispatch_room_list_rejects_an_unknown_wing`.
fn check_wing(args: &Value, tool: &str) -> Result<()> {
    let Some(wing) = args
        .get("wing")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return Ok(());
    };
    if Uuid::parse_str(wing) == Ok(DEFAULT_WING_ID) {
        return Ok(());
    }
    Err(anyhow!(
        "{tool}: wings are not implemented yet (ADR-0027 T9); omit `wing`, \
         or pass the default wing id {DEFAULT_WING_ID}"
    ))
}

/// Serialise one room row for the wire.
fn room_json(room: &RoomSummary, drawer_count: usize) -> Value {
    json!({
        "room_id": room.id.to_string(),
        "label": room.label,
        "room_type": room_type_tag(&room.room_type),
        "wing_id": room.wing_id.to_string(),
        "drawer_count": drawer_count,
        "created_at": chrono::DateTime::from_timestamp_millis(room.created_at_ms)
            .map(|t| t.to_rfc3339()),
        "resolved": room.resolved,
        "description": room.description,
    })
}

pub(crate) async fn handle_room_list(state: &AppState, args: Value) -> Result<Value> {
    check_wing(&args, "room_list")?;
    let palace = resolve_palace(state, &args, "room_list")?;
    let handle = open_palace_handle(state, &palace)?;
    // Counted from the live drawer table rather than stored on the row: a
    // stored count would be a second source of truth that could drift from the
    // drawers themselves, and ADR-0027's whole premise is that the drawers are
    // authoritative about which room they are in.
    let counts: HashMap<Uuid, usize> =
        handle
            .drawers
            .read()
            .iter()
            .fold(HashMap::new(), |mut acc, d| {
                *acc.entry(d.room_id).or_insert(0) += 1;
                acc
            });
    let store = handle.kg.store();
    let rooms = tokio::task::spawn_blocking(move || list_room_summaries(&store))
        .await
        .context("join room_list")?
        .context("list rooms")?;
    let payload: Vec<Value> = rooms
        .iter()
        .map(|r| room_json(r, counts.get(&r.id).copied().unwrap_or(0)))
        .collect();
    Ok(json!({ "palace": palace, "rooms": payload }))
}

pub(crate) async fn handle_room_create(state: &AppState, args: Value) -> Result<Value> {
    check_wing(&args, "room_create")?;
    let palace = resolve_palace(state, &args, "room_create")?;
    let label = args
        .get("label")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("room_create: missing 'label'"))?
        .to_string();
    let description = args
        .get("description")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let handle = open_palace_handle(state, &palace)?;
    let store = handle.kg.store();
    // ADR-0027 D4.1: classification goes through the one parser, so
    // `room_create("backend")` and a `Backend` write land in the same room.
    let room = parse_room_preserving_case(&label);
    let (summary, created) =
        tokio::task::spawn_blocking(move || create_room(&store, &room, description))
            .await
            .context("join room_create")?
            .context("create room")?;
    Ok(json!({
        "palace": palace,
        "room_id": summary.id.to_string(),
        "label": summary.label,
        "created": created,
    }))
}

pub(crate) async fn handle_room_rename(state: &AppState, args: Value) -> Result<Value> {
    let palace = resolve_palace(state, &args, "room_rename")?;
    let selector = args
        .get("room")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("room_rename: missing 'room' (a room_id or a label)"))?
        .to_string();
    let new_label = args
        .get("new_label")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("room_rename: missing 'new_label'"))?
        .to_string();
    let handle = open_palace_handle(state, &palace)?;
    let store = handle.kg.store();
    let summary = tokio::task::spawn_blocking(move || {
        let id = resolve_room_selector(&store, &selector)?;
        rename_room(&store, id, &new_label)
    })
    .await
    .context("join room_rename")?
    .context("rename room")?;
    Ok(json!({
        "palace": palace,
        "room_id": summary.id.to_string(),
        "label": summary.label,
    }))
}
