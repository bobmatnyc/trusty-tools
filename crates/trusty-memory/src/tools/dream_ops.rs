//! On-demand dream / consolidation MCP handlers (spec-001 Phase 3, issue #1721).
//!
//! Why: the idle dream cycle consolidates a whole palace on a ~300s timer. An
//! application managing chat history wants to compact a single room's older
//! turns on demand and have the superseded originals evicted so history
//! actually shrinks. This handler exposes that scoped, synchronous pipeline.
//! What: `handle_dream_consolidate_room` resolves the palace, parses the
//! optional room + age window, builds a `DreamConfig` from the daemon's user
//! config (OpenRouter key / local-model setting), and delegates to
//! `dream::consolidate_scoped`. Task drawers are skipped inside that helper.
//! `handle_palace_dream` is an alias for `handle_dream_consolidate_room` with
//! the same parameters, exposed as `palace_dream` in the MCP tool surface.
//! Test: `crates/trusty-memory/tests/dream_room_mcp.rs` (wiring + no-op) and
//! `dream::tests::consolidate_scoped_*` in trusty-common (behaviour).

use crate::AppState;
use anyhow::Result;
use serde_json::{json, Value};
use trusty_common::memory_core::dream::{consolidate_scoped, DreamConfig};

use super::helpers::{open_palace_handle, parse_room, resolve_palace};

/// Default age window: only consolidate facts older than this many days.
const DEFAULT_MAX_AGE_DAYS: i64 = 7;

/// Trigger LLM-driven consolidation for one room (or all rooms) of a palace.
///
/// Why: gives applications an explicit "compact this room's older history now"
/// control instead of waiting for the idle dreamer; returns the work done so
/// the caller can log progress.
/// What: parses `room` (null/omitted = all rooms) and `max_age_days`
/// (default 7), builds a `DreamConfig` seeded with the daemon's OpenRouter key
/// and local-model flag, opens the palace, and runs `consolidate_scoped`. When
/// no inference backend is configured the helper is a graceful no-op (zero
/// counts). Returns `{ palace, room, summary_facts_created, facts_evicted }`.
/// Test: `dream_consolidate_room_returns_shape` (no-op path) in
/// `tests/dream_room_mcp.rs`.
pub(crate) async fn handle_dream_consolidate_room(state: &AppState, args: Value) -> Result<Value> {
    let palace = resolve_palace(state, &args, "dream_consolidate_room")?;
    // Absent / null / empty room => all rooms; otherwise scope to that room.
    let room_arg = args
        .get("room")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());
    let room = room_arg.map(|s| parse_room(Some(s)));
    let max_age_days = args
        .get("max_age_days")
        .and_then(|v| v.as_i64())
        .unwrap_or(DEFAULT_MAX_AGE_DAYS);

    let handle = open_palace_handle(state, &palace)?;

    // Seed the consolidation config from the daemon's user config so the
    // inference backend (OpenRouter key / local model) matches the idle dream
    // cycle. Everything else uses the dream defaults (semantic enabled).
    let cfg = crate::web::load_user_config().unwrap_or_default();
    let dream_cfg = DreamConfig {
        openrouter_api_key: cfg.openrouter_api_key,
        local_model_enabled: cfg.local_model.enabled,
        ..DreamConfig::default()
    };

    let stats = consolidate_scoped(&handle, &dream_cfg, room, max_age_days, None).await?;

    Ok(json!({
        "palace": palace,
        "room": room_arg,
        "summary_facts_created": stats.summary_facts_created,
        "facts_evicted": stats.facts_evicted,
    }))
}

/// On-demand LLM-driven consolidation for a palace (issue #1721 `palace_dream`).
///
/// Why: `dream_consolidate_room` was the original tool name from spec-001 Phase
/// 3. Issue #1721 requests the MCP tool be named `palace_dream` to match the
/// naming convention of `palace_compact` / `palace_info`. Both tools expose the
/// same underlying `consolidate_scoped` pipeline; `palace_dream` is the
/// canonical name going forward and `dream_consolidate_room` is retained for
/// backward compatibility.
/// What: delegates directly to `handle_dream_consolidate_room` — the args
/// schema and response shape are identical.
/// Test: `palace_dream_no_inference_returns_gracefully` in
/// `tests/dream_room_mcp.rs`.
pub(crate) async fn handle_palace_dream(state: &AppState, args: Value) -> Result<Value> {
    handle_dream_consolidate_room(state, args).await
}
