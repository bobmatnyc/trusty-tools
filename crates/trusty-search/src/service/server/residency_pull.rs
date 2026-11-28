//! The pull ticker that feeds tm's active-project set into residency (#10533).
//!
//! Why: trusty-mpm publishes the active set over `mpm.residency.active`, but no
//! trusty-search code consumed it (design brief 2026-10-30, section 0), so
//! residency stayed recency-only.
//! What: [`spawn_residency_pull_ticker`] pulls the set every
//! `TRUSTY_RESIDENCY_PULL_SECS` (default 30) and hands the result to
//! [`apply_pull`], which records it in `state.residency_pins` and pre-wakes
//! members that are registered but cold-parked, leaving one resident slot for
//! a non-member. `TRUSTY_RESIDENCY=off` or a pull interval of `0` spawns
//! nothing, so the sweep runs as before.
//! Test: `apply_pull_records_a_set_and_keeps_it_across_a_failure`,
//! `prewake_targets_names_only_cold_registered_members`,
//! `prewake_leaves_one_slot_for_a_non_member`,
//! `pull_interval_zero_disables_the_ticker`.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use trusty_common::residency::{
    residency_enabled, residency_pull_secs, ActiveProjectSet, RESIDENCY_ENABLED_ENV,
    RESIDENCY_PULL_SECS_ENV,
};

use super::state::SearchAppState;
use crate::core::registry::{IndexHandle, IndexId};

/// Deadline for one `mpm.residency.active` pull. Matches
/// `trusty_common::memory_rpc::DEFAULT_TIMEOUT`, the sibling UDS client.
/// Test: `active_set_pull_timeout_is_five_seconds`.
pub(super) const ACTIVE_SET_PULL_TIMEOUT: Duration = Duration::from_secs(5);

/// Spawn the active-set pull ticker.
///
/// Why: the sweep reads the set from `state.residency_pins`; something has to
/// keep it fresh, and a parked member should be resident before its first
/// query rather than after it.
/// What: a detached task holding a `Weak` (stops with the state). The first
/// tick fires at once so pins exist before the first sweep. A failed pull is
/// logged at debug and recorded, so the held set survives while fresh and
/// expires after `TRUSTY_RESIDENCY_STALE_SECS`.
/// Test: the tick body is `apply_pull`, covered by
/// `apply_pull_records_a_set_and_keeps_it_across_a_failure`.
pub(super) fn spawn_residency_pull_ticker(state: Arc<SearchAppState>) {
    let Some(period) = pull_interval(
        std::env::var(RESIDENCY_ENABLED_ENV).ok().as_deref(),
        std::env::var(RESIDENCY_PULL_SECS_ENV).ok().as_deref(),
    ) else {
        tracing::info!(
            "residency-pull: disabled via {RESIDENCY_ENABLED_ENV}=off or {RESIDENCY_PULL_SECS_ENV}=0"
        );
        return;
    };
    let weak = Arc::downgrade(&state);
    drop(state);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(period);
        // A slow pre-wake must not trigger a burst of catch-up pulls.
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            let Some(state) = weak.upgrade() else {
                break;
            };
            let pulled = trusty_common::mpm_rpc::fetch_active_projects(ACTIVE_SET_PULL_TIMEOUT)
                .await
                .map_err(|e| e.to_string());
            apply_pull(&state, pulled, Instant::now()).await;
        }
    });
}

/// The pull period, or `None` when `TRUSTY_RESIDENCY` is off or
/// `TRUSTY_RESIDENCY_PULL_SECS` is `0` (the ticker then never spawns).
/// Test: `pull_interval_zero_disables_the_ticker`.
pub(super) fn pull_interval(enabled: Option<&str>, secs: Option<&str>) -> Option<Duration> {
    if !residency_enabled(enabled) {
        return None;
    }
    let secs = residency_pull_secs(secs);
    (secs > 0).then(|| Duration::from_secs(secs))
}

/// Record one pull result and pre-wake the members it names.
///
/// What: `Ok` is observed at `now`; `Err` is a pull failure at `now`. Then,
/// while the set is trusted, each id from [`prewake_targets`] is loaded
/// through the query path and its watcher resumed. A load that fails (the
/// embedder is not ready, the index is parked in the project map) is logged at
/// debug and retried on the next tick. A change of the set's freshness is
/// logged once at info.
/// Test: `apply_pull_records_a_set_and_keeps_it_across_a_failure`,
/// `freshness_change_reports_each_transition_once`.
pub(super) async fn apply_pull(
    state: &Arc<SearchAppState>,
    pulled: Result<ActiveProjectSet, String>,
    now: Instant,
) {
    let registered = super::residency_pins::registered_in(state);
    let ok = match pulled {
        Ok(set) => {
            state.residency_pins.observe(set, now, &registered);
            true
        }
        Err(e) => {
            tracing::debug!("residency-pull: mpm.residency.active unavailable: {e}");
            state.residency_pins.on_pull_failure(now);
            false
        }
    };
    // #10533: one info line per fresh/stale change, not one per pull.
    if let Some(fresh) = state.residency_pins.freshness_change(now) {
        let word = if fresh { "fresh" } else { "stale" };
        tracing::info!("residency-pull: the active-project set is now {word}");
    }
    if !ok {
        return;
    }
    let Some(members) = state.residency_pins.members(now, &registered) else {
        return;
    };
    let cap = crate::service::lazy_loader::resolve_max_resident_indexes_for(state.machine_tier).cap;
    for id in prewake_targets(state, &members, cap) {
        match super::index_resolve::resolve_or_load_index(state, &id).await {
            Ok(handle) => {
                resume_watcher(state, &handle).await;
                tracing::info!("residency-pull: pre-woke active-set index '{}'", id.0);
            }
            Err((status, body)) => tracing::debug!(
                "residency-pull: could not pre-wake '{}' ({status}): {}",
                id.0,
                body.0
            ),
        }
    }
}

/// The members that are registered but not resident: cold-parked and not
/// permanently failed. Sorted, so the wake order is stable.
///
/// #10533 ruling (b): under a cap, resident members plus the targets stay at
/// most `cap - 1`, so one slot is free for a non-member. `cap: None`
/// (`TRUSTY_MAX_RESIDENT_INDEXES=off`) has no bound.
/// Test: `prewake_targets_names_only_cold_registered_members`,
/// `prewake_leaves_one_slot_for_a_non_member`.
pub(super) fn prewake_targets(
    state: &SearchAppState,
    members: &HashSet<String>,
    cap: Option<usize>,
) -> Vec<IndexId> {
    let mut ids: Vec<IndexId> = members
        .iter()
        .map(|id| IndexId::new(id.clone()))
        .filter(|id| {
            state.registry.get(id).is_none()
                && state.cold_store.contains(id)
                && !state.cold_store.is_failed(id)
        })
        .collect();
    ids.sort_by(|a, b| a.0.cmp(&b.0));
    if let Some(cap) = cap {
        let resident_members = members
            .iter()
            .filter(|id| state.registry.get(&IndexId::new((*id).clone())).is_some())
            .count();
        ids.truncate(cap.saturating_sub(1).saturating_sub(resident_members));
    }
    ids
}

/// Resume the watcher of a handle served without one, and reconcile.
///
/// Why: a lazily restored index never spawns a watcher, and a query against it
/// resumes one; a pre-wake must leave the index in the same state a query
/// does. Moved out of `search.rs` (#10533) so both paths share it.
/// What: gated on `needs_root_resync` (#7434). Only when the spawn actually
/// established every root does it start a background `reconcile_one_index`,
/// so `TRUSTY_DISABLE_WATCHER=1` never spins a reconcile per call.
/// Test: `every_index_root_is_watched`,
/// `a_stuck_root_start_does_not_block_the_other_roots` (the resync gate).
pub(super) async fn resume_watcher(state: &Arc<SearchAppState>, handle: &Arc<IndexHandle>) {
    if state.watcher_manager.needs_root_resync(handle).await {
        state.watcher_manager.spawn_for_index(handle).await;
        if !state.watcher_manager.needs_root_resync(handle).await {
            let woken = Arc::clone(handle);
            let summary = Arc::clone(&state.reconcile_summary);
            tokio::spawn(crate::service::reconcile::reconcile_one_index(
                woken, summary,
            ));
        }
    }
}
