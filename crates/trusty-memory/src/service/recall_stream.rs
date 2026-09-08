//! Bounded, streaming palace fan-out for the recall-all family (issue #7125).
//!
//! Why: the three recall-all entry points opened every palace on disk into one
//! `Vec<Arc<PalaceHandle>>` and held all of them for the whole query. On a
//! 94-palace estate that hydrates the drawer table, HNSW graph, KG adjacency
//! and redb page cache of all 94 at once — roughly 13x the on-disk bytes — and
//! when the local `Vec` drops the LRU is left holding its 64 most recent
//! victims, a multi-GB floor that persists until the idle sweep runs. The
//! 64-slot cap bounded the cache, never the peak, and never the residue. Peak
//! residency must track the batch size, not the palace count, and the daemon's
//! steady state after a recall-all must match its steady state before one.
//! What: [`recall_streamed`] walks the palace list in [`RECALL_PALACE_BATCH`]
//! batches. Each batch is opened on the blocking pool, handed to the caller's
//! search, then released — so at most one batch is resident at a time. Palaces
//! the registry already held when the call started are never released, so an
//! operator's working set survives a recall-all untouched. Per-batch results
//! are merged with the same dedup-by-drawer-id/sort/truncate rule
//! `recall_across_palaces` applies within a batch, so the global top-k is
//! unchanged: it is always a subset of the union of the per-batch top-ks.
//! Test: `recall_all_returns_open_palaces_to_baseline` and
//! `recall_all_keeps_palaces_that_were_already_open` in
//! `tests/recall_all_residency.rs`;
//! `recall_all_releases_every_batch_when_the_search_fails` and
//! `recall_streamed_visits_every_palace_in_bounded_batches` in
//! `recall_stream_tests`.

use crate::AppState;
use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::Arc;
use trusty_common::memory_core::palace::{Palace, PalaceId};
use trusty_common::memory_core::retrieval::CrossPalaceResult;
use trusty_common::memory_core::PalaceHandle;
use uuid::Uuid;

use super::helpers::open_palaces_blocking;

/// How many palaces a recall-all holds open at once (issue #7125).
///
/// Why: this is the peak-residency knob. Each open palace costs its drawer
/// table, HNSW graph, KG adjacency and redb page cache — on the estate that
/// prompted #7125, ~13x its on-disk bytes across 94 palaces. Eight keeps the
/// fan-out's own concurrency (`recall_across_palaces` joins the batch) worth
/// having while bounding peak residency at roughly an eighth of the old
/// whole-estate open. Raising it trades RAM for wall-clock; lowering it does
/// the reverse.
/// What: the chunk width [`recall_streamed`] slices the palace list into.
/// Test: `recall_all_returns_open_palaces_to_baseline` exercises an estate
/// wider than one batch, so the batching loop actually iterates.
pub(crate) const RECALL_PALACE_BATCH: usize = 8;

/// Run `search` over every palace in `palaces`, one bounded batch at a time.
///
/// Why (#7125): see the module doc — the old shape's peak and its residue both
/// scaled with the palace count. This is the shared replacement, so all three
/// recall-all entry points (`MemoryService::recall_all`,
/// `handle_memory_recall_all`, `chat::tools::execute_recall_all`) inherit the
/// bound rather than each growing their own.
/// What: snapshots which palaces the registry already holds, then for each
/// batch opens the handles (on the blocking pool, via `open_palaces_blocking`),
/// awaits `search`, and releases every handle the batch itself brought in.
/// Release runs BEFORE the caller's error is propagated, so a failing search
/// cannot leak a batch. `search` takes the handles by value and its future is
/// dropped before the release, so the registry sees `strong_count == 1` and
/// `release_if_unreferenced` can actually pop; a palace another task is holding
/// stays put by that same guard. Results are merged across batches by drawer
/// id (highest score wins), sorted by score descending, and truncated to
/// `top_k`.
/// Test: `recall_all_returns_open_palaces_to_baseline`,
/// `recall_all_keeps_palaces_that_were_already_open`,
/// `recall_all_releases_every_batch_when_the_search_fails`,
/// `recall_streamed_visits_every_palace_in_bounded_batches`.
pub(crate) async fn recall_streamed<F, Fut>(
    state: &AppState,
    palaces: &[Palace],
    label: &'static str,
    top_k: usize,
    mut search: F,
) -> Result<Vec<CrossPalaceResult>>
where
    F: FnMut(Vec<Arc<PalaceHandle>>) -> Fut,
    Fut: Future<Output = Result<Vec<CrossPalaceResult>>>,
{
    if palaces.is_empty() || top_k == 0 {
        return Ok(Vec::new());
    }
    // #7125: palaces already resident when the query arrived are the operator's
    // working set, not this query's residue — releasing them would evict a warm
    // cache the next request is about to want.
    let pinned = resident_ids(state);

    let mut merged: Vec<CrossPalaceResult> = Vec::new();
    let mut by_drawer: HashMap<Uuid, usize> = HashMap::new();

    for batch in palaces.chunks(RECALL_PALACE_BATCH) {
        let handles = open_palaces_blocking(state, batch, label).await;
        if handles.is_empty() {
            // Every palace in this batch failed to open; `open_palaces_blocking`
            // already warned per palace. Nothing to release, nothing to search.
            continue;
        }
        let opened: Vec<PalaceId> = handles.iter().map(|h| h.id.clone()).collect();
        let outcome = search(handles).await;
        // #7125: release before `?` — an erroring search must not leak a batch.
        release_batch(state, &opened, &pinned);
        merge_batch(&mut merged, &mut by_drawer, outcome?);
    }

    merged.sort_by(|a, b| {
        b.result
            .score
            .partial_cmp(&a.result.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    merged.truncate(top_k);
    Ok(merged)
}

/// Ids the registry holds open right now.
///
/// Why (#7125): the baseline a recall-all must return to. Taken once, before
/// the first batch opens anything, so it names the caller's working set rather
/// than the query's own footprint.
/// What: snapshots `PalaceRegistry::list` into a set for O(1) membership.
/// Test: `recall_all_keeps_palaces_that_were_already_open`.
fn resident_ids(state: &AppState) -> HashSet<PalaceId> {
    state.registry.list().into_iter().collect()
}

/// Hand back the handles this batch opened, keeping the pre-call working set.
///
/// Why (#7125): without this the LRU keeps whatever the fan-out last touched,
/// so the daemon's floor after a recall-all is `min(palace_count, max_open)`
/// fully hydrated palaces regardless of what the operator was actually using.
/// What: for each id the batch opened that was NOT already resident, calls
/// `PalaceRegistry::release_if_unreferenced` (#7106), which pops only when the
/// cache holds the sole reference — so a concurrent recall, remember, or dream
/// cycle holding the same `Arc` is never disturbed. redb stays the source of
/// truth; the next access transparently reopens.
/// Test: `recall_all_returns_open_palaces_to_baseline`,
/// `recall_all_keeps_palaces_that_were_already_open`.
fn release_batch(state: &AppState, opened: &[PalaceId], pinned: &HashSet<PalaceId>) {
    for id in opened {
        if pinned.contains(id) {
            continue;
        }
        state.registry.release_if_unreferenced(id);
    }
}

/// Fold one batch's hits into the running cross-palace result set.
///
/// Why (#7125): batching splits a fan-out that used to merge once, so the
/// dedup-by-drawer-id rule `recall_across_palaces` applies within a batch has
/// to be reapplied across batches or the same drawer could appear twice.
/// What: keeps the highest-scoring occurrence of each drawer id, indexing into
/// `merged` through `by_drawer` so a later higher score replaces in place.
/// Test: `recall_all_returns_open_palaces_to_baseline` drives the merge across
/// more than one batch.
fn merge_batch(
    merged: &mut Vec<CrossPalaceResult>,
    by_drawer: &mut HashMap<Uuid, usize>,
    hits: Vec<CrossPalaceResult>,
) {
    for candidate in hits {
        let drawer_id = candidate.result.drawer.id;
        match by_drawer.get(&drawer_id).copied() {
            Some(idx) if merged[idx].result.score >= candidate.result.score => {}
            Some(idx) => merged[idx] = candidate,
            None => {
                by_drawer.insert(drawer_id, merged.len());
                merged.push(candidate);
            }
        }
    }
}
