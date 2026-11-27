//! The usage-based resident-index cap's per-tick sweep (issue #2161).
//!
//! Why: split out of `tickers` in #6957, which pushed that file past the
//! 500-SLOC production cap. The scheduling wrapper
//! (`spawn_residency_sweep_ticker`) stays with its sibling tickers; the tick
//! body — which is where the registry / cold-store / reindex-progress
//! interactions actually live — moves here with its own tests.
//! What: [`run_residency_sweep_tick`] and the `#[cfg(test)]` seam
//! [`run_residency_sweep_tick_inner`] it delegates to. #10533: the tick reads
//! tm's active set from `state.residency_pins` — members are never parked,
//! non-members park after the grace period — through the clock-injected
//! [`run_residency_sweep_tick_at`].
//! Test: `residency_sweep_tests`, `residency_pins_10533_tests`.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Instant;

use super::residency_pins::grace_expired;
use super::state::SearchAppState;
use crate::core::registry::IndexId;
use crate::service::persistence::PersistedIndex;
use crate::service::persistence_timestamps::warmboot_sort_key;
use crate::service::reindex::ReindexStatus;

/// One residency-sweep tick: rank resident indexes, cold-park everything
/// beyond the cap. Extracted from `spawn_residency_sweep_ticker` so the
/// per-tick logic can be reasoned about (and, via the pure helpers it calls,
/// tested) independently of the `tokio::spawn` scaffolding.
///
/// Why (TOCTOU / composition with reindex): an index with a `Running`
/// reindex must never be parked. The reindex task holds its OWN `Arc` to the
/// live `IndexHandle` (captured when the reindex started) and keeps mutating
/// it — including `handle.stages`, which lives on that specific instance.
/// Parking would detach the handle from the registry while the reindex is
/// still writing into it; a query landing after that point would lazily
/// rebuild a BRAND NEW `IndexHandle` from disk (via `get_or_load_index`) that
/// the in-flight reindex task knows nothing about, so its completion would
/// update a `stages` `Arc` no future request will ever observe. Skipping any
/// index with a `Running` entry in `reindex_progress` avoids that permanently
/// wedged status.
///
/// Why (watcher lifetime): `cold_park_index` deliberately never touches the
/// file watcher (see its own doc). But leaving the OLD watcher attached to
/// the now-detached indexer would starve the NEXT reload: `WatcherManager`
/// keys `is_watching` purely by `IndexId`, so after a lazy reload builds a
/// FRESH `IndexHandle` + `CodeIndexer`, the search handler's
/// `if !is_watching(id) { spawn_for_index(...) }` wake-up would see `true`
/// (the stale watcher is still registered under that id) and never spawn a
/// watcher pointed at the fresh indexer — silently freezing that index's
/// live-update path until the next full daemon restart. Stopping the watcher
/// here mirrors `spawn_watcher_idle_suspend_ticker` exactly: the query-time
/// wake-up already re-spawns the watcher AND runs `reconcile_one_index` to
/// catch up on anything that changed while unwatched, so nothing is lost —
/// this is the identical, already-tested mechanism idle-suspend relies on.
/// Is a reindex Running for `id` right now?
///
/// Why (#6957): the sweep reads this state TWICE per candidate — once before
/// calling into the park, and once more from inside it immediately before the
/// detach — and the two reads must be the same read. Before #6957 only the
/// first existed, and `persist_before_park`'s filesystem write sat between it
/// and the detach: a reindex arriving in that window spawned against the same
/// `Arc<IndexHandle>` the park then removed from the registry.
/// What: one `DashMap` lookup plus an atomic load. No lock is held across it,
/// so it composes with the park's own registry/cold-store guards rather than
/// introducing a second ordering to reason about.
/// Test: `sweep_never_parks_index_with_running_reindex` (the early read),
/// `sweep_aborts_park_when_a_reindex_starts_inside_the_park_window` (the late
/// read).
fn reindex_running(state: &SearchAppState, id: &IndexId) -> bool {
    state
        .reindex_progress
        .get(id)
        .is_some_and(|p| p.status.load() == ReindexStatus::Running)
}

pub(super) async fn run_residency_sweep_tick(state: &Arc<SearchAppState>) {
    run_residency_sweep_tick_inner(state, |_| {}).await
}

/// Test seam for [`run_residency_sweep_tick`] (#6957), mirroring the
/// `hook: impl FnOnce()` seam [`cold_park_index_inner`] already carries for
/// #3995: `hook` runs synchronously inside the park for each candidate id,
/// after the cold entry is registered and after the sweep's own pre-park
/// reindex check has already passed — the exact window a `POST
/// /indexes/<id>/reindex` lands in. Production passes a no-op.
async fn run_residency_sweep_tick_inner(state: &Arc<SearchAppState>, hook: impl Fn(&IndexId)) {
    run_residency_sweep_tick_at(
        state,
        Instant::now(),
        crate::core::project_map::now_unix(),
        hook,
    )
    .await
}

/// [`run_residency_sweep_tick_inner`] at an injected clock (#10533).
///
/// Why: the active-set grace period is a time rule, and a test must walk it
/// without sleeping.
/// What: two candidate sources feed one park loop. (1) Grace: when the active
/// set is trusted, a resident non-member whose absence and last activity both
/// reach the grace period parks, even under the cap. (2) Cap: over the cap,
/// [`crate::service::lazy_loader::ids_to_park`] ranks only non-members, and
/// keeps one non-member when members fill the cap (ruling (b)). An untrusted
/// set leaves (1) empty and (2) unpinned, which is the pre-#10533 tick. Warm
/// pins and a `Running` reindex block either source.
/// `TRUSTY_MAX_RESIDENT_INDEXES=off` returns before either source runs.
/// Test: `sweep_keeps_an_active_set_member_resident_past_the_cap`,
/// `non_member_parks_only_after_the_grace_period_with_no_query`,
/// `stale_or_absent_set_sweep_behaves_as_today`,
/// `running_reindex_blocks_an_active_set_grace_park`,
/// `max_resident_off_parks_nothing_not_even_a_grace_park`.
async fn run_residency_sweep_tick_at(
    state: &Arc<SearchAppState>,
    now: Instant,
    now_unix: u64,
    hook: impl Fn(&IndexId),
) {
    // #6821: an unset TRUSTY_MAX_RESIDENT_INDEXES now resolves to the machine
    // tier's default instead of disabling the sweep; `off` is what disables it.
    let resolved =
        crate::service::lazy_loader::resolve_max_resident_indexes_for(state.machine_tier);
    let Some(cap) = resolved.cap else {
        return; // TRUSTY_MAX_RESIDENT_INDEXES=off — nothing is ever parked.
    };

    // #10533: `None` when the active set is stale, absent, empty, or names no
    // registered index.
    let registered = super::residency_pins::registered_in(state);
    let members = state.residency_pins.members(now, &registered);
    let member_count = members.as_ref().map_or(0, HashSet::len);
    // #10533 ruling (b): warn once per change when the pins fill the cap.
    if state
        .residency_pins
        .note_over_cap(member_count > 0 && member_count >= cap)
    {
        tracing::warn!(
            "residency-sweep: {member_count} active-set index(es) fill the resident cap of \
             {cap}; the most recently active non-member keeps one slot"
        );
    }
    let resident_ids: HashSet<String> = state.registry.list().into_iter().map(|id| id.0).collect();
    if resident_ids.len() <= cap && members.is_none() {
        return;
    }

    let toml_entries = match crate::service::persistence::load_index_registry() {
        Ok(entries) => entries,
        Err(e) => {
            tracing::warn!("residency-sweep: could not read indexes.toml: {e}");
            return;
        }
    };
    let resident_entries: Vec<_> = toml_entries
        .into_iter()
        .filter(|e| resident_ids.contains(&e.id))
        .collect();

    // #10533: grace parks first, so the cap ranks what is left after them.
    let pinned = members.unwrap_or_default();
    let (mut to_park, rest): (Vec<_>, Vec<_>) = resident_entries
        .into_iter()
        .partition(|e| grace_candidate(state, e, &pinned, now, now_unix));
    let grace_parks = to_park.len();
    to_park.extend(crate::service::lazy_loader::ids_to_park(rest, cap, &pinned));
    // #9027: a warm-all pin keeps the index resident for its window, even past
    // the cap; the warm's own RSS ceiling bounds what that can cost.
    to_park.retain(|entry| !state.warm.is_pinned(&entry.id));
    if to_park.is_empty() {
        return;
    }

    let mut parked = 0usize;
    for entry in to_park {
        let id = IndexId::new(entry.id.clone());

        // Never park an index with an in-flight reindex — see the function
        // doc for why this composes badly with the residency detach. This is
        // the cheap early-out; #6957 added the authoritative late re-check
        // below, passed into the park as a closure because the park itself
        // must stay free of `SearchAppState` (see the `residency` module doc).
        if reindex_running(state, &id) {
            continue;
        }

        // #6957: the same read, re-run inside the park immediately before the
        // registry detach — after `persist_before_park`'s disk write, which is
        // the window the early check above cannot cover.
        if crate::service::lazy_loader::cold_park_index_inner(
            &id,
            &state.registry,
            &state.cold_store,
            entry,
            || reindex_running(state, &id),
            || hook(&id),
        )
        .await
        {
            // See the function doc: stop the watcher so the next reload's
            // wake-up path re-establishes it (and reconciles) against the
            // FRESH indexer instance instead of leaving a stale one pinned.
            state.watcher_manager.stop_for_index(&id).await;
            parked += 1;
        }
    }

    if parked > 0 {
        tracing::info!(
            "residency-sweep: cold-parked {parked} index(es) beyond top-{cap} resident cap"
        );
    }
    if grace_parks > 0 {
        // #10533: the grace share of the candidates, for the live check.
        tracing::info!(
            "residency-sweep: {grace_parks} candidate(s) idled past the active-set grace period"
        );
    }
}

/// Is `entry` a non-member that has idled past the active-set grace period?
///
/// Why (#10533): design section 3, ACTIVE to PARKED. A warm pin or a
/// `Running` reindex keeps it out of the grace list up front, so the cap stage
/// still counts it as resident.
/// What: `false` whenever `pinned` is empty (untrusted set). The last
/// activity is the latest of the persisted recency key (last query or
/// reindex), the in-memory `last_queried_write_cache` stamp, and the
/// indexer's own activity clock, which every query, grep, chunk page and
/// ingest advances. An indexer whose lock is held counts as active.
/// Test: `non_member_parks_only_after_the_grace_period_with_no_query`,
/// `running_reindex_blocks_an_active_set_grace_park`,
/// `a_search_without_a_stamp_write_keeps_a_non_member_within_grace`,
/// `grep_and_list_chunks_keep_a_non_member_resident_within_grace`,
/// `a_recent_reindex_counts_as_activity_for_the_grace_period`.
fn grace_candidate(
    state: &SearchAppState,
    entry: &PersistedIndex,
    pinned: &HashSet<String>,
    now: Instant,
    now_unix: u64,
) -> bool {
    if pinned.is_empty() || pinned.contains(&entry.id) {
        return false;
    }
    let id = IndexId::new(entry.id.clone());
    if state.warm.is_pinned(&entry.id) || reindex_running(state, &id) {
        return false;
    }
    // #10533: the stamp is written at most once per 60 s, and grep, chunk
    // pages and the fan-out never write it; the indexer clock sees them all.
    let Some(handle) = state.registry.get(&id) else {
        return false;
    };
    let Ok(indexer) = handle.indexer.try_read() else {
        return false;
    };
    let clock = now_unix.saturating_sub(indexer.idle_duration().as_secs());
    let cached = state
        .last_queried_write_cache
        .get(&id)
        .map_or(0, |stamp| *stamp);
    let last_activity = warmboot_sort_key(entry).max(cached).max(clock);
    let pins = &state.residency_pins;
    grace_expired(
        pins.absent_for(&entry.id, now, &super::residency_pins::registered_in(state)),
        last_activity,
        pins.grace(),
        now_unix,
    )
}

#[cfg(test)]
#[path = "residency_sweep_tests.rs"]
mod residency_sweep_tests;

#[cfg(test)]
#[path = "residency_pins_10533_tests.rs"]
mod residency_pins_10533_tests;
