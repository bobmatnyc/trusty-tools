//! The usage-based resident-index cap's per-tick sweep (issue #2161).
//!
//! Why: split out of `tickers` in #6957, which pushed that file past the
//! 500-SLOC production cap. The scheduling wrapper
//! (`spawn_residency_sweep_ticker`) stays with its sibling tickers; the tick
//! body — which is where the registry / cold-store / reindex-progress
//! interactions actually live — moves here with its own tests.
//! What: [`run_residency_sweep_tick`] and the `#[cfg(test)]` seam
//! [`run_residency_sweep_tick_inner`] it delegates to.
//! Test: `residency_sweep_tests`.

use std::collections::HashSet;
use std::sync::Arc;

use super::state::SearchAppState;
use crate::core::registry::IndexId;
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
    // #6821: an unset TRUSTY_MAX_RESIDENT_INDEXES now resolves to the machine
    // tier's default instead of disabling the sweep; `off` is what disables it.
    let resolved =
        crate::service::lazy_loader::resolve_max_resident_indexes_for(state.machine_tier);
    let Some(cap) = resolved.cap else {
        return; // TRUSTY_MAX_RESIDENT_INDEXES=off — nothing is ever parked.
    };

    let resident_ids: HashSet<String> = state.registry.list().into_iter().map(|id| id.0).collect();
    if resident_ids.len() <= cap {
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

    let to_park = crate::service::lazy_loader::ids_to_park(resident_entries, cap);
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
}

#[cfg(test)]
#[path = "residency_sweep_tests.rs"]
mod residency_sweep_tests;
