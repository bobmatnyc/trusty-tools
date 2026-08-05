//! Wing MCP tool handlers (ADR-0027 D2 / ticket T9, issue #4809).
//!
//! Why: ADR-0027's central finding is that `Wing` had zero construction sites
//! since the day it was written — "a level nobody reads is the defect". So the
//! wing entity ships WITH the surface that reads it: `wing_list` is the
//! discovery primitive, `wing_create` the way a scope comes into existence, and
//! `wing_rename` the repair path. Without these, `WINGS` would be a second dark
//! table and this ticket would have reproduced the exact defect it corrects.
//! What: three `pub(crate) async fn handle_wing_*` handlers, plus
//! [`resolve_wing_arg`] — the shared parser the wing-scoped read tools use.
//! Test: `crates/trusty-memory/tests/wing_mcp.rs`.

use crate::AppState;
use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use trusty_common::memory_core::store::wings::{
    list_wings, rename_wing, resolve_or_create_wing, resolve_wing_selector, WingSummary,
};
use trusty_common::memory_core::PalaceHandle;
use uuid::Uuid;

use super::helpers::{open_palace_handle, resolve_palace};

/// Render a `WingSummary` as the MCP wire shape.
///
/// Why: `wing_list`, `wing_create`, and `wing_rename` all answer with the same
/// object, so a single projection keeps them from drifting.
/// What: ids as strings, the creation stamp as RFC3339 (millis are an
/// implementation detail of the row, not a contract).
fn wing_json(w: &WingSummary) -> Value {
    json!({
        "wing_id": w.id.to_string(),
        "label": w.label,
        "description": w.description,
        "room_count": w.room_count,
        "is_default": w.is_default,
        "created_at": chrono::DateTime::from_timestamp_millis(w.created_at_ms)
            .map(|t| t.to_rfc3339()),
    })
}

/// Resolve an optional `wing` argument to a wing id.
///
/// Why: an unknown wing must FAIL LOUD at the tool boundary, even though the
/// library scope layer fails closed. A typo'd wing that quietly returned zero
/// results would be precisely the invisible failure ADR-0027 D4.4 rejects
/// content inference for — the caller would read "no memories" as fact rather
/// than as a mistake. `wing_list` is one call away, so naming it in the error
/// costs the caller nothing.
/// What: `Ok(None)` when the caller supplied no `wing` (the "wing is never
/// required" guarantee — this is the path every pre-T9 caller takes);
/// `Ok(Some(id))` for a known wing id or label; `Err` for an unknown one.
/// Test: `recall_rejects_an_unknown_wing`,
/// `a_caller_that_never_names_a_wing_is_unaffected`.
pub(crate) fn resolve_wing_arg(
    handle: &PalaceHandle,
    args: &Value,
    tool: &str,
) -> Result<Option<Uuid>> {
    let Some(selector) = args.get("wing").and_then(|v| v.as_str()) else {
        return Ok(None);
    };
    if selector.trim().is_empty() {
        return Ok(None);
    }
    resolve_wing_selector(&handle.kg, selector)
        .map_err(|e| anyhow!("{tool}: resolve wing {selector:?}: {e:#}"))?
        .map(Some)
        .ok_or_else(|| anyhow!("{tool}: unknown wing {selector:?} (try wing_list)"))
}

/// List every wing in a palace with its room population.
///
/// Why: the discovery primitive that did not exist. Before this, a caller could
/// not find out what scopes a palace has without reading its redb file.
/// What: opens the palace (which seeds the default wing if absent) and returns
/// `{ palace, wings: [ { wing_id, label, description, room_count, is_default,
/// created_at } ] }`.
/// Test: `wing_list_shows_the_default_wing`, `wing_create_then_list`.
pub(crate) async fn handle_wing_list(state: &AppState, args: Value) -> Result<Value> {
    let palace = resolve_palace(state, &args, "wing_list")?;
    let handle = open_palace_handle(state, &palace)?;
    let wings = list_wings(&handle.kg).map_err(|e| anyhow!("wing_list: {e:#}"))?;
    let payload: Vec<Value> = wings.iter().map(wing_json).collect();
    Ok(json!({ "palace": palace, "wings": payload }))
}

/// Create a wing, or return the existing one with that label.
///
/// Why: idempotent by construction so a caller can call it unconditionally on
/// startup — the #3064 "room-per-agent-type" consumer will want exactly that,
/// with the agent type as the wing.
/// What: normalises the label, mints a UUIDv5 for a new wing, and returns
/// `{ palace, wing_id, label, created }`. `created` is `false` when the wing
/// already existed. Creating `"default"` returns the palace's default wing id
/// rather than a second one.
/// Test: `wing_create_is_idempotent_over_mcp`, `wing_create_rejects_blank`.
pub(crate) async fn handle_wing_create(state: &AppState, args: Value) -> Result<Value> {
    let palace = resolve_palace(state, &args, "wing_create")?;
    // Trim and reject blank AT the boundary. `normalize_wing_label` in the
    // store already refuses a blank, so this is defence in depth rather than
    // the only guard — but it keeps the rejection local to the tool that
    // caused it, and it means the `label` echoed below is the exact string the
    // store was handed, so the response can never disagree with what was
    // stored.
    let label = args
        .get("label")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("wing_create: missing or blank 'label'"))?;
    let handle = open_palace_handle(state, &palace)?;
    let (id, created) = resolve_or_create_wing(&handle.kg, label)
        .await
        .map_err(|e| anyhow!("wing_create: {e:#}"))?;
    Ok(json!({
        "palace": palace,
        "wing_id": id.to_string(),
        "label": label,
        "created": created,
    }))
}

/// Rename a wing, retiring its old label.
///
/// Why: the repair path — and the only way to fix a scope that was named badly
/// once agents already write into it. Because rooms reference a wing by id,
/// this provably cannot move a room or touch a drawer.
/// What: resolves `wing` (id or current label), applies the rename, and returns
/// the updated summary. Errors when the wing is unknown or the new label is
/// already held by a different wing.
/// Test: `wing_rename_over_mcp`, `wing_rename_rejects_taken_label_over_mcp`.
pub(crate) async fn handle_wing_rename(state: &AppState, args: Value) -> Result<Value> {
    let palace = resolve_palace(state, &args, "wing_rename")?;
    // Same boundary guard as `wing_create`: trim, and reject blank here rather
    // than only in the store, so both tools fail identically on the same input.
    let new_label = args
        .get("new_label")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("wing_rename: missing or blank 'new_label'"))?;
    let handle = open_palace_handle(state, &palace)?;
    // `wing` is required here (unlike the read tools, where its absence means
    // "unscoped"), so route it through the same resolver and then demand it.
    let id = resolve_wing_arg(&handle, &args, "wing_rename")?
        .ok_or_else(|| anyhow!("wing_rename: missing 'wing'"))?;
    let summary = rename_wing(&handle.kg, id, new_label)
        .await
        .map_err(|e| anyhow!("wing_rename: {e:#}"))?;
    Ok(json!({ "palace": palace, "wing": wing_json(&summary) }))
}
