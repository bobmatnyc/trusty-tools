//! Which served recall hits record an access, and in which palace (#9881).
//!
//! Why: only a recall surface knows what its caller received, after fusion,
//! demotion, the floor and the `top_k` cut. The engine records the access
//! ([`PalaceHandle::record_accesses`]); this module decides what counts.
//! What: a hit counts when a retrieval lane found it — layer 2 or deeper
//! (ruling Q2) — or when it is a folded user-scope ruling, which the fold
//! relabels layer 1 but which its own palace found by search (Q3). L0 is the
//! identity and L1 the essentials every recall returns, so neither counts.
//! Each access is recorded in the palace that owns the drawer (Q3, Q4). The
//! MCP and the HTTP JSON-RPC recall share these handlers, so both count.
//! Test: `served_hits_count_lane_and_ruling_hits_only`;
//! `tools::tests::recall_access_tests`.

use crate::AppState;
use crate::engine::palace::PalaceId;
use crate::engine::retrieval::{CrossPalaceResult, PalaceHandle, RecallResult};
use std::collections::HashMap;
use uuid::Uuid;

use super::helpers::open_palace_handle;

/// The first layer a retrieval lane produces (Q2).
pub(crate) const FIRST_COUNTED_LAYER: u8 = 2;

/// The hits a single-palace recall served, split by owning palace.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct ServedHits {
    /// Lane hits from the palace the caller named.
    pub(crate) lane: Vec<Uuid>,
    /// Folded rulings as `(owning palace, drawer id)`.
    pub(crate) rulings: Vec<(String, Uuid)>,
}

impl ServedHits {
    /// Classify the final, cut `results`; `owners` is the rulings fold's map.
    ///
    /// Why: the classification is the whole of Q2 and Q3; keeping it pure lets
    /// a unit test pin it without a palace.
    /// What: a drawer in `owners` with a non-empty palace is a ruling; any other
    /// hit at [`FIRST_COUNTED_LAYER`] or deeper is a lane hit; the rest are
    /// dropped.
    /// Test: `served_hits_count_lane_and_ruling_hits_only`.
    pub(crate) fn from_cut(results: &[RecallResult], owners: &HashMap<Uuid, String>) -> Self {
        let mut served = Self::default();
        for hit in results {
            let id = hit.drawer.id;
            match owners.get(&id) {
                Some(palace) if !palace.is_empty() => served.rulings.push((palace.clone(), id)),
                Some(_) => {}
                None if hit.layer >= FIRST_COUNTED_LAYER => served.lane.push(id),
                None => {}
            }
        }
        served
    }

    /// Record every served hit in its owning palace.
    ///
    /// Why: the bump is advisory; it must never fail or slow the recall.
    /// What: lane hits go to `handle`; each ruling goes to its palace via
    /// [`record_in_palace`].
    /// Test: `a_served_lane_hit_persists_its_access_count_across_reopen`.
    pub(crate) fn record(self, state: &AppState, handle: &PalaceHandle) {
        handle.record_accesses(&self.lane);
        for (palace, ids) in group(self.rulings) {
            record_in_palace(state, &palace, &ids);
        }
    }
}

/// Record the lane hits a cross-palace recall served, each in its own palace.
///
/// Why (Q4): `memory_recall_all` counts per owning palace.
/// What: groups the cut results' lane hits by `palace_id` and hands each
/// group to [`record_in_palace`].
/// Test: `recall_all_records_each_hit_in_its_own_palace`.
pub(crate) fn record_served_across(state: &AppState, results: &[CrossPalaceResult]) {
    let hits = results
        .iter()
        .filter(|r| r.result.layer >= FIRST_COUNTED_LAYER)
        .map(|r| (r.palace_id.clone(), r.result.drawer.id));
    for (palace, ids) in group(hits) {
        record_in_palace(state, &palace, &ids);
    }
}

/// Group `(palace, id)` pairs by palace.
fn group(pairs: impl IntoIterator<Item = (String, Uuid)>) -> HashMap<String, Vec<Uuid>> {
    let mut by_palace: HashMap<String, Vec<Uuid>> = HashMap::new();
    for (palace, id) in pairs {
        by_palace.entry(palace).or_default().push(id);
    }
    by_palace
}

/// Record `ids` in `palace`, opening it only when it is no longer resident.
///
/// Why: a `scope: "all"` recall releases each batch's palaces before the cut,
/// so a hit's palace may be closed by now (#7125). Reopening is bounded by
/// `top_k` palaces.
/// What: a resident handle records and keeps its debounced flush. A palace
/// opened here records and is released again if nothing else holds it; its
/// drop writes the accesses. An open failure is logged and skipped.
/// Test: `recall_all_records_each_hit_in_its_own_palace`.
fn record_in_palace(state: &AppState, palace: &str, ids: &[Uuid]) {
    let id = PalaceId::new(palace);
    if let Some(handle) = state.registry.peek(&id) {
        handle.record_accesses(ids);
        return;
    }
    match open_palace_handle(state, palace) {
        Ok(handle) => {
            handle.record_accesses(ids);
            drop(handle);
            state.registry.release_if_unreferenced(&id);
        }
        Err(e) => {
            tracing::warn!(palace = %palace, "#9881: reopen to record accesses failed: {e:#}");
        }
    }
}

#[cfg(test)]
#[path = "recall_access_unit_tests.rs"]
mod recall_access_unit_tests;
