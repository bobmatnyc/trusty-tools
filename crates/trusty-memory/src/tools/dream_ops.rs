//! On-demand dream / consolidation MCP handlers (spec-001 Phase 3, issue #1721).
//!
//! Why: the idle dream cycle consolidates a whole palace on a ~300s timer. An
//! application managing chat history wants to compact a single room's older
//! turns on demand and have the superseded originals evicted so history
//! actually shrinks. This handler exposes that scoped, synchronous pipeline.
//! What: `handle_dream_consolidate_room` resolves the palace, parses the
//! optional room + age window, builds a `DreamConfig` from the daemon's user
//! config (OpenRouter key, local-model flag, and local-model id — issue
//! #2593) via `dream_config_from_user_config`, and delegates to
//! `dream::consolidate_scoped`. Task drawers are skipped inside that helper.
//! `handle_palace_dream` is an alias for `handle_dream_consolidate_room` with
//! the same parameters, exposed as `palace_dream` in the MCP tool surface.
//! Test: `crates/trusty-memory/tests/dream_room_mcp.rs` (wiring + no-op) and
//! trusty-common's `dream::tests::consolidate_scoped_*` (behaviour).

use crate::AppState;
use anyhow::{Context, Result};
use serde_json::{json, Value};
use trusty_common::memory_core::dream::{consolidate_scoped, detect_fading};

use super::helpers::{open_palace_handle, parse_room, resolve_palace};

/// Default age window: only consolidate facts older than this many days.
const DEFAULT_MAX_AGE_DAYS: i64 = 7;

/// Trigger LLM-driven consolidation for one room (or all rooms) of a palace.
///
/// Why: gives applications an explicit "compact this room's older history now"
/// control instead of waiting for the idle dreamer; returns the work done so
/// the caller can log progress.
/// What: parses `room` (null/omitted = all rooms) and `max_age_days`
/// (default 7), builds a `DreamConfig` seeded from the daemon's user config
/// (OpenRouter key, local-model flag, local-model id — issue #2593), opens
/// the palace, and runs `consolidate_scoped`. When no inference backend is
/// configured the helper is a graceful no-op (zero counts). Also computes
/// the palace-wide fading-memories resurface list (issue
/// #2352) — high-value memories that have decayed below the resurface threshold,
/// surfaced (never auto-boosted) so the caller can touch or `memory_forget`
/// them. Returns
/// `{ palace, room, summary_facts_created, facts_evicted, fading }`.
/// Test: `dream_consolidate_room_returns_shape` (no-op path) and
/// `palace_dream_response_includes_fading` in `tests/dream_room_mcp.rs`.
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
    // inference backend (OpenRouter key / local model / local model id)
    // matches the idle dream cycle. Everything else uses the dream defaults
    // (semantic enabled). `dream_config_from_user_config` (issue #2593) also
    // forwards `local_model.model` into `semantic.model` — previously only
    // the key and the enabled flag were forwarded, leaving `semantic.model`
    // on the OpenRouter-style default even when the local Ollama backend was
    // what actually resolved.
    //
    // Use `crate::service::load_user_config` (the axum-free home of the loader,
    // issue #226) rather than the `crate::web::` re-export: this `tools` module
    // is compiled unconditionally, but `mod web` is `#[cfg(feature =
    // "axum-server")]`-gated, so the re-export vanishes when a dependent builds
    // trusty-memory without that feature — which broke the build (E0433).
    let cfg = crate::service::load_user_config().unwrap_or_default();
    let dream_cfg = crate::service::dream_config_from_user_config(&cfg);

    let stats = consolidate_scoped(&handle, &dream_cfg, room, max_age_days, None).await?;

    // Fading-memories resurface pass (issue #2352): palace-wide, read-only.
    // Surfaced here so the on-demand caller sees the same list the idle dream
    // cycle records in dream_stats.json.
    let fading = detect_fading(&handle, &dream_cfg.fading);
    let fading = serde_json::to_value(&fading).context("serialize fading list")?;

    Ok(json!({
        "palace": palace,
        "room": room_arg,
        "summary_facts_created": stats.summary_facts_created,
        "facts_evicted": stats.facts_evicted,
        "fading": fading,
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
