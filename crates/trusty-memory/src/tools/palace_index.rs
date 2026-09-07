//! The no-palace fallback for palace-scoped READ tools (#6318).
//!
//! Why: a caller that does not know which palace to name used to get an error
//! and no way forward — `memory_recall` with no `palace` and no `--palace`
//! default said "missing 'palace'" and stopped there. Owner ruling
//! (2026-08-27): "If a palace or collection is not specified, it should return
//! an index." An index is a safe answer for a READ, because reading nothing
//! and describing what is readable are both truthful. It is NOT a safe answer
//! for a WRITE: `memory_remember` with no palace has no defensible target, so
//! every mutating tool keeps [`super::helpers::resolve_palace`]'s error.
//! What: [`resolve_palace_or_index`] returns the resolved palace when one
//! exists (explicit argument, then `--palace` default — the same precedence
//! `resolve_palace` applies), and otherwise a [`PalaceScope::Index`] carrying
//! the structured index the read handler returns verbatim as its successful
//! result. A palace that was NAMED but does not exist is not this path: it
//! resolves here and fails later at the open, exactly as before.
//! Test: `tools::tests::palace_index_tests`.

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde_json::{json, Value};
use trusty_common::memory_core::room_identity::room_type_tag;
use trusty_common::memory_core::store::rooms::list_room_summaries;
use uuid::Uuid;

use super::helpers::open_palace_handle;
use crate::AppState;

/// How many palaces the index describes in full.
///
/// Why: a full row opens the palace's redb file to count drawers, rooms and
/// wings. That cost is fine for the handful of palaces a host normally has and
/// unbounded for an estate of hundreds, so the index details the most recently
/// used ones and names the rest. Every palace still appears — the cap decides
/// how much is said about each, never whether it is listed.
/// What: the number of leading rows (most recently used first) that carry
/// counts and rooms.
/// Test: `palace_index_details_are_capped_and_the_rest_are_still_listed`.
pub(crate) const PALACE_INDEX_DETAIL_LIMIT: usize = 24;

/// The outcome of resolving a palace for a READ tool (#6318).
pub(crate) enum PalaceScope {
    /// A palace was named, or the server has a default; proceed as before.
    Palace(String),
    /// Neither was available; return this index as the tool's success value.
    Index(Value),
}

/// Resolve a palace for a READ tool, falling back to an index of palaces.
///
/// Why: see the module note — the ruling makes an index the answer to "which
/// palace?", not an error.
/// What: explicit `args["palace"]` wins, then `state.default_palace`, then the
/// index. Same precedence as [`super::helpers::resolve_palace`], which every
/// mutating tool still calls.
/// Test: `every_read_tool_returns_an_index_with_no_palace`,
/// `a_named_palace_that_does_not_exist_still_errors`,
/// `a_default_palace_still_wins_over_the_index`.
pub(crate) async fn resolve_palace_or_index(
    state: &AppState,
    args: &Value,
    tool: &str,
) -> Result<PalaceScope> {
    if let Some(p) = args.get("palace").and_then(|v| v.as_str()) {
        return Ok(PalaceScope::Palace(p.to_string()));
    }
    if let Some(p) = state.default_palace.clone() {
        return Ok(PalaceScope::Palace(p));
    }
    Ok(PalaceScope::Index(palace_index(state, tool).await?))
}

/// Build the structured index a no-palace read returns (#6318).
///
/// Why: the caller needs enough to pick a palace and retry in one more call —
/// the ids, how much each holds, which rooms are in it, and when it was last
/// used so the likely one sorts first.
/// What: lists every palace under `state.data_root`, orders them most-recently-
/// used first (then by id), and describes the first [`PALACE_INDEX_DETAIL_LIMIT`]
/// in full. Runs the per-palace opens on the blocking pool.
/// Test: `palace_index_reports_counts_and_rooms`,
/// `palace_index_on_an_empty_estate_is_still_a_success`.
pub(crate) async fn palace_index(state: &AppState, tool: &str) -> Result<Value> {
    let root = state.data_root.clone();
    let palaces = tokio::task::spawn_blocking(move || {
        trusty_common::memory_core::PalaceRegistry::list_palaces(&root)
    })
    .await
    .context("join list_palaces")??;

    let total = palaces.len();
    let dirs: Vec<(String, PathBuf)> = palaces
        .iter()
        .map(|p| (p.id.as_str().to_string(), p.data_dir.clone()))
        .collect();
    let state_for_rows = state.clone();
    let rows = tokio::task::spawn_blocking(move || index_rows(&state_for_rows, dirs))
        .await
        .context("join palace index rows")?;

    Ok(json!({
        "status": "no_palace_specified",
        "tool": tool,
        "hint": format!(
            "No 'palace' argument and no server default, so this is an index of \
             the palaces on this host rather than an error. Retry {tool} with \
             palace set to one of the ids below."
        ),
        "palace_count": total,
        "detail_limit": PALACE_INDEX_DETAIL_LIMIT,
        "palaces": rows,
    }))
}

/// Order the palaces and build one row each, blocking throughout.
///
/// Why: reading each palace's last-used stamp is a file read and describing one
/// opens its redb file, so the whole pass belongs on the blocking pool rather
/// than split across it.
/// What: reads every stamp, sorts most-recently-used first (an unstamped palace
/// after every stamped one, then by id so the order is stable across calls), and
/// describes the leading [`PALACE_INDEX_DETAIL_LIMIT`]. Every palace past that
/// is still listed, marked `detail: "omitted"`.
/// Test: `palace_index_details_are_capped_and_the_rest_are_still_listed`.
fn index_rows(state: &AppState, dirs: Vec<(String, PathBuf)>) -> Vec<Value> {
    let mut ordered: Vec<(Option<u64>, String)> = dirs
        .into_iter()
        .map(|(id, dir)| (crate::palace_last_used::read(&dir), id))
        .collect();
    ordered.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    ordered
        .into_iter()
        .enumerate()
        .map(|(i, (last_used, id))| {
            if i < PALACE_INDEX_DETAIL_LIMIT {
                palace_row(state, &id, last_used)
            } else {
                json!({ "palace": id, "last_used_unix": last_used, "detail": "omitted" })
            }
        })
        .collect()
}

/// Describe one palace for the index.
///
/// Why: the counts are the whole reason an index beats a bare list of names —
/// they are how a caller tells the palace it wants from four it does not.
/// What: opens the palace and reports drawer / room / wing counts plus a row
/// per room. A palace that cannot be opened is reported with `unreadable` and
/// its error text rather than dropped: a palace whose bytes are on disk and
/// unreadable must stay visible (the same posture `PalaceRegistry` takes for a
/// hydration skip, #4911).
/// Test: `palace_index_reports_counts_and_rooms`.
fn palace_row(state: &AppState, id: &str, last_used_unix: Option<u64>) -> Value {
    let handle = match open_palace_handle(state, id) {
        Ok(h) => h,
        Err(e) => {
            return json!({
                "palace": id,
                "last_used_unix": last_used_unix,
                "unreadable": format!("{e:#}"),
            })
        }
    };
    let drawer_count = handle.list_drawers(None, None, usize::MAX).len();
    // Counted from the live drawer table, the same way `room_list` counts them
    // — the drawers are authoritative about which room they are in (ADR-0027).
    let per_room: HashMap<Uuid, usize> =
        handle
            .drawers
            .read()
            .iter()
            .fold(HashMap::new(), |mut acc, d| {
                *acc.entry(d.room_id).or_insert(0) += 1;
                acc
            });
    let store = handle.kg.store();
    let wing_count = store.list_wings().map(|w| w.len()).unwrap_or(0);
    let rooms = list_room_summaries(&store).unwrap_or_default();
    let room_rows: Vec<Value> = rooms
        .iter()
        .map(|r| {
            json!({
                "label": r.label,
                "room_type": room_type_tag(&r.room_type),
                "drawer_count": per_room.get(&r.id).copied().unwrap_or(0),
            })
        })
        .collect();
    json!({
        "palace": id,
        "drawer_count": drawer_count,
        "room_count": rooms.len(),
        "wing_count": wing_count,
        "last_used_unix": last_used_unix,
        "rooms": room_rows,
    })
}
