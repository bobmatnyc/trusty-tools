//! The live L1 cache (#9719) and the open-time L1 snapshot gap-fill (#9722).
//!
//! Why: L1 was set only at open, so a drawer written to a live handle never
//! reached `retrieve_l0_l1` (#9719). And the open-time gap-fill, kept for
//! palaces from before drawer persistence (#32), revived any drawer a stale
//! second handle wrote back into `l1_cache.json` (#9722).
//! What: [`PalaceHandle::refresh_l1`] and [`PalaceHandle::save_and_refresh_l1`]
//! keep L1 in step with the drawer table on every write path. At open,
//! [`gap_fill_needed`] runs the gap-fill only until the drawer table is
//! authoritative, and [`maybe_adopt`] makes the gap-fill durable and stamps
//! the marker that retires it.
//! Test: `l1_live_tests`.

use super::handle::PalaceHandle;
use super::open_sweep::{OPEN_WRITE_BUDGET, run_open_write};
use super::scope::RecallScope;
use super::types::{L1_CAP, RecallResult};
use crate::memory_core::palace::{Drawer, PalaceId};
use crate::memory_core::store::kg::KnowledgeGraph;
use crate::memory_core::store::l1_cache::L1Cache;
use anyhow::Result;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;

/// The top [`L1_CAP`] drawers in `drawer_listing_order`.
///
/// Why: #4836 — the same selection `l1_cache.json` persists, so the live L1
/// and the snapshot agree.
/// Test: `refresh_l1_keeps_the_newest_drawers_within_an_importance_tie`.
pub(super) fn select_l1(drawers: &[Drawer]) -> Vec<Drawer> {
    let mut sorted = drawers.to_vec();
    sorted.sort_by(super::types::drawer_listing_order);
    sorted.truncate(L1_CAP);
    sorted
}

/// L0 + L1 for a recall under `scope`.
///
/// Why: #9719 — the live L1 holds the newest drawers of every wing, so an
/// unscoped L1 put another wing's drawer into a wing-scoped recall. A room is a
/// topic, not an ownership boundary (ADR-0027 T7), so a room scope leaves L1 as
/// the palace's baseline, as before.
/// What: a `Wing` scope limits L1 to the rooms that wing owns, resolved
/// fail-closed by [`RecallScope::allowed_room_ids`]; `All` and `Room` admit
/// every room. L0 is never scoped.
/// Test: `a_wing_scoped_recall_drops_another_wings_l1_drawer`,
/// `wing_mcp::wing_scoped_write_then_recall_over_mcp`.
pub fn retrieve_l0_l1_scoped(handle: &PalaceHandle, scope: &RecallScope) -> Vec<RecallResult> {
    let allowed = match scope {
        RecallScope::Wing(_) => scope.allowed_room_ids(&handle.kg),
        RecallScope::All | RecallScope::Room(_) => None,
    };
    super::layers::l0_l1_admitted(handle, &allowed)
}

/// Whether open must gap-fill the drawer table from the L1 snapshot.
///
/// Why: #9722 — once the drawer table is authoritative, a snapshot-only id is
/// stale, not a pre-#32 drawer.
/// What: `false` only when the marker is stamped AND the drawer load was
/// complete. An unreadable marker, or a degraded load, keeps the gap-fill:
/// the snapshot is then the only copy of the drawers it holds.
/// Test: `gap_fill_runs_when_the_marker_is_unreadable`,
/// `gap_fill_runs_on_a_degraded_load_even_when_stamped`.
pub(super) fn gap_fill_needed(authority: &Result<bool>, degraded: bool, palace: &PalaceId) -> bool {
    match authority {
        Ok(stamped) => !stamped || degraded,
        Err(e) => {
            // #9722: fail toward today's behaviour; nothing is deleted.
            tracing::warn!(palace = %palace, "read drawers_authoritative failed, gap-filling: {e:#}");
            true
        }
    }
}

/// Add each snapshot drawer absent from `drawers`; returns the ids added.
pub(super) fn gap_fill(drawers: &mut Vec<Drawer>, snapshot: &[Drawer]) -> HashSet<Uuid> {
    let present: HashSet<Uuid> = drawers.iter().map(|d| d.id).collect();
    let mut added = HashSet::new();
    for d in snapshot {
        if !present.contains(&d.id) && added.insert(d.id) {
            drawers.push(d.clone());
        }
    }
    added
}

/// Persist the gap-filled drawers and stamp the authority marker.
///
/// Why: #9722 — the #32 migration keeps relying on the gap-fill until its
/// rows are durable; after that the gap-fill only revives stale ids.
/// What: runs only for an unstamped, fully loaded palace opened by the data
/// root's maintainer on a writable store. A gap-filled drawer with a
/// `fact_key` is not adopted: `fact_key` postdates #32, so that drawer is a
/// stale id, and adopting it would move a Tier C slot. The write is bounded
/// by [`OPEN_WRITE_BUDGET`]; on failure the open proceeds unstamped and the
/// next open retries. Returns whether the marker was stamped.
/// Test: `a_legacy_snapshot_drawer_is_adopted_and_kept_when_adoption_is_skipped`.
pub(super) fn maybe_adopt(
    kg: &KnowledgeGraph,
    authority: &Result<bool>,
    maintainer: bool,
    degraded: bool,
    drawers: &[Drawer],
    gap_filled: &HashSet<Uuid>,
    palace: &PalaceId,
) -> bool {
    if !maintainer || degraded || kg.is_read_only() || !matches!(authority, Ok(false)) {
        return false;
    }
    let rows: Vec<Drawer> = drawers
        .iter()
        .filter(|d| gap_filled.contains(&d.id) && d.fact_key.is_none())
        .cloned()
        .collect();
    adopt_within(kg, rows, palace, OPEN_WRITE_BUDGET)
}

/// [`maybe_adopt`]'s write, under `budget`.
fn adopt_within(
    kg: &KnowledgeGraph,
    rows: Vec<Drawer>,
    palace: &PalaceId,
    budget: Duration,
) -> bool {
    let store = kg.store();
    let worker = Arc::clone(&store);
    let n = rows.len();
    let work = move || worker.adopt_snapshot_drawers(&rows);
    match run_open_write(&store, palace, "adopt_l1_snapshot", budget, work) {
        Some(Ok(())) => {
            tracing::info!(palace = %palace, adopted = n, "#9722: drawer table is authoritative");
            true
        }
        Some(Err(e)) => {
            // #9722: unstamped, so the gap-fill still covers these drawers.
            tracing::warn!(palace = %palace, "adopt L1 snapshot drawers failed: {e:#}");
            false
        }
        // `run_open_write` logged the skip or the overrun.
        None => false,
    }
}

impl PalaceHandle {
    /// Rebuild L1 from the drawer table.
    ///
    /// Why: L1 is the always-on essential context; #9719 — it must follow the
    /// drawer table on a live handle, not only at open. #4836: a selection,
    /// not just an ordering.
    /// What: stores [`select_l1`] of the drawer table in `l1_drawers`.
    /// Test: `a_drawer_added_after_open_is_served_by_l0_l1`,
    /// `refresh_l1_keeps_the_newest_drawers_within_an_importance_tie`.
    pub fn refresh_l1(&self) {
        let selected = select_l1(&self.drawers.read());
        *self.l1_drawers.write() = selected;
    }

    /// Refresh the live L1, then persist `l1_cache.json`.
    ///
    /// Why: #9719 — every path that rewrites the snapshot changed the drawer
    /// table, so the live L1 follows it in the same step.
    /// What: [`Self::refresh_l1`], then `L1Cache::save_l1_cache` when the
    /// handle has a data directory. The live L1 is refreshed even when the
    /// save fails.
    /// Test: `a_drawer_added_after_open_is_served_by_l0_l1`.
    pub(crate) fn save_and_refresh_l1(&self) -> Result<()> {
        self.refresh_l1();
        if let Some(data_dir) = self.data_dir.as_ref() {
            let snap = self.drawers.read().clone();
            L1Cache::save_l1_cache(&snap, data_dir)?;
        }
        Ok(())
    }
}
