//! Dream cycle passes: content-prune, dedup, prune, compact, closet refresh.
//!
//! Why: Extracted from dream.rs to keep each file under the 500-SLOC cap
//! (#607). Each pass is a focused async function called by `Dreamer::dream_cycle`.
//! The inference-backed semantic half moved on to `semantic.rs` in #7106 so the
//! chunked-embedding rewrite below had room to land.
//! What: `content_prune_pass`, `dedup_pass`, `prune_pass`, `compact_pass`,
//! `refresh_closets`.
//! Test: `dream_cycle_merges_duplicates`, `dream_cycle_prunes_low_importance`,
//! `closet_refresh_builds_index`,
//! `concurrency_tests::dedup_embeds_in_bounded_chunks`.

use super::helpers::{
    build_closet_index, is_low_quality_content, merge_into, rebuild_index_from_drawers,
};
use crate::memory_core::decay::DecayConfig;
use crate::memory_core::embed::Embedder;
use crate::memory_core::palace::Drawer;
use crate::memory_core::retrieval::{PalaceHandle, shared_embedder};
use crate::memory_core::store::vector::VectorStore;
use crate::memory_core::timeouts;
use anyhow::Result;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;

/// Drawers embedded per `embed_batch` call inside [`dedup_pass`] (#7106).
///
/// Why: the pass used to embed a palace's entire corpus in ONE call. Between
/// the caller's `Vec<String>`, `FastEmbedder`'s cache-miss list, the owned copy
/// it hands the blocking task, and the returned vectors, one cycle held roughly
/// six copies of the corpus at peak — multiplied by however many cycles ran at
/// once. Chunking bounds every one of those copies to this many drawers,
/// regardless of palace size.
/// What: 256 drawers per call. Large enough that the per-call overhead
/// (a `spawn_blocking` hop and one ONNX mutex acquisition) stays amortised,
/// small enough that the transient is a few megabytes rather than a few
/// gigabytes.
/// Test: `concurrency_tests::dedup_embeds_in_bounded_chunks`.
pub(super) const DREAM_EMBED_CHUNK: usize = 256;

/// Drop drawers whose content is recognisably noise.
///
/// Why: The write-path blocklist (PR #221) only gates new writes. Pre-
/// existing drawers that slipped through before the gate need periodic
/// cleanup; the dream cycle is the right place for retroactive quality
/// enforcement so palaces self-heal without admin migrations.
/// What: Snapshots the in-memory drawer table, applies the same content
/// rule the write path uses (`filter::blocklist_match` — a prefix-anchored
/// `starts_with` check against `BLOCKLIST_PATTERNS`, not a substring match)
/// plus a word-count floor, and forgets each matching drawer via
/// `PalaceHandle::forget`. Respects the per-cycle wall-clock `budget`
/// deadline.
///
/// #7106: the scan reads the drawer table under its read lock and collects only
/// the victim ids, instead of cloning every `Drawer` in the palace. The scan
/// body is pure — no `.await` runs while the lock is held.
/// Test: `dream_content_prune_drops_blocklist_drawer`,
/// `dream_content_prune_drops_short_drawer`,
/// `dream_content_prune_keeps_good_drawer`.
pub(super) async fn content_prune_pass(
    handle: &Arc<PalaceHandle>,
    started: std::time::Instant,
    budget: Duration,
    min_words: usize,
) -> Result<usize> {
    let victims: Vec<Uuid> = {
        let drawers = handle.drawers.read();
        let mut victims: Vec<Uuid> = Vec::new();
        for drawer in drawers.iter() {
            if started.elapsed() >= budget {
                break;
            }
            // spec-001: Task drawers are protected — never evicted by any pass.
            if drawer.drawer_type.is_protected() {
                continue;
            }
            if is_low_quality_content(drawer.content(), min_words) {
                victims.push(drawer.id);
            }
        }
        victims
    };

    // #5231: report drawers actually dropped. This loop can also exit early on
    // the wall-clock budget, so `victims.len()` over-reported even when every
    // attempted forget succeeded.
    let mut count = 0usize;
    for id in victims {
        if started.elapsed() >= budget {
            break;
        }
        match handle.forget(id).await {
            Ok(outcome) if outcome.is_deleted() => count += 1,
            Ok(_) => {}
            Err(e) => tracing::warn!(?id, "dream content prune: forget failed: {e:#}"),
        }
    }
    Ok(count)
}

/// Remove orphaned vectors from the HNSW index whose drawer row no longer exists.
///
/// Why: Dedup and prune remove drawers via `handle.forget`, which removes
/// the matching vector. But over a palace's lifetime, vectors can also be
/// orphaned by partial writes, schema migrations, or pre-fix bugs that
/// dropped drawer rows without removing the corresponding vector. This
/// pass closes the gap and clears the `index_vectors >> drawer_records`
/// cold-start warning (issue #33).
/// What: Snapshots drawer ids into a `HashSet`, asks the vector store for
/// every id it currently tracks, and removes any vector whose id is not
/// in the drawer set. Respects the per-cycle wall-clock budget. Returns 0
/// silently when the vector store can't enumerate ids (e.g. cold reload
/// before any upsert this session).
/// Test: `dream_cycle_compacts_orphaned_vectors`.
pub(super) async fn compact_pass(
    handle: &Arc<PalaceHandle>,
    started: std::time::Instant,
    budget: Duration,
) -> Result<usize> {
    let mut removed: usize = 0;
    // #6208: snapshot the drawer set and reclaim orphans while holding the
    // palace write mutex — the same lock `remember`/`forget` hold. Without it,
    // a `remember` mid-flight (vector upserted at handle.rs:~686, drawer not yet
    // pushed at ~708) exposes a vector with no matching drawer, which this pass
    // would remove as a false orphan and lose permanently. The guard makes that
    // intermediate state unobservable here; it is released before the rebuild
    // below, which re-takes it independently (no reentrancy — tokio::Mutex is
    // not reentrant). `remember` never holds this across `compact_pass`, and the
    // idle dreamer holds only the `is_compacting` flag, so this cannot deadlock.
    let (drawer_count, index_size_after) = {
        let _write_guard = timeouts::lock_with_timeout(
            &handle.write_mutex,
            timeouts::write_lock_timeout(),
            handle.id.as_str(),
        )
        .await?;
        let drawer_ids: HashSet<Uuid> = handle.drawers.read().iter().map(|d| d.id).collect();

        // Addressable pass: walk every id our key_map knows about and drop
        // anything missing from the drawer table.
        let vector_ids = handle.vector_store.all_ids();
        for vid in vector_ids {
            if started.elapsed() >= budget {
                break;
            }
            if drawer_ids.contains(&vid) {
                continue;
            }
            match handle.vector_store.remove(vid).await {
                Ok(()) => removed += 1,
                Err(e) => tracing::warn!(?vid, "dream compact: vector remove failed: {e:#}"),
            }
        }
        (drawer_ids.len(), handle.vector_store.index_size())
    };

    // Fallback rebuild: if the index still reports significantly more
    // vectors than the drawer table holds (e.g. pre-fix orphans we can't
    // enumerate via key_map), reset the index and re-upsert every drawer
    // from scratch. Costly but bounded — only runs when the divergence is
    // material, and re-embedding 100s of drawers takes <1s on the local
    // ONNX model.
    // Only rebuild when we have drawers to re-embed AND the index has at
    // least 1 + 2*drawer_count entries (well past noise). Avoids tight
    // rebuild loops on a healthy small palace.
    if drawer_count > 0 && index_size_after > drawer_count.saturating_mul(2) + 1 {
        let rebuilt = rebuild_index_from_drawers(handle, started, budget)
            .await
            .map_err(|e| e.context("dream compact rebuild"))?;
        // `rebuilt` counts every drawer we re-upserted; the number of
        // orphans removed via rebuild is `index_size_before - drawer_count`.
        // Surface a conservative `removed` increment by counting the
        // delta as orphans dropped from the index.
        let delta = index_size_after.saturating_sub(rebuilt);
        removed = removed.saturating_add(delta);
    }

    Ok(removed)
}

/// Find near-duplicates and merge survivors; returns the merge count.
///
/// Why: The previous implementation initialised `FastEmbedder` once but
/// then called `recall_deep` per drawer — each call does a fresh embed
/// (50–100ms on the local ONNX model) plus an L3 search. On a palace with
/// ~100 drawers that's >5s, which exceeded the per-cycle budget (issue
/// #55). Batch-embedding all drawer contents upfront turns the inner loop
/// into pure vector arithmetic via `vector_store.search`, which is
/// sub-millisecond per query.
/// What: Snapshots drawers once, then walks the snapshot in
/// [`DREAM_EMBED_CHUNK`]-sized chunks — embedding one chunk, using those
/// vectors to search the HNSW index for near-duplicates, and dropping them
/// before the next chunk. `vector_store.search` returns pure cosine similarity
/// (1 - distance), so no importance-renormalisation is required. Survivors are
/// picked by raw `importance`; losers are merged in and forgotten.
///
/// #7106: the chunking is the memory fix. The single whole-corpus `embed_batch`
/// this replaced held the entire palace, in about six copies, for the length of
/// one ONNX call — and every concurrent cycle held its own set.
/// Test: `dream_cycle_merges_duplicates`,
/// `concurrency_tests::dedup_embeds_in_bounded_chunks`.
pub(super) async fn dedup_pass(
    handle: &Arc<PalaceHandle>,
    started: std::time::Instant,
    budget: Duration,
    dedup_threshold: f32,
) -> Result<usize> {
    // Reuse the process-wide shared embedder instead of constructing a
    // fresh ONNX session for every dream cycle (issue #57). The previous
    // per-cycle construction multiplied the daemon's memory footprint by
    // the number of palaces.
    let embedder = shared_embedder()
        .await
        .map_err(|e| e.context("acquire shared embedder for dream dedup"))?;
    dedup_pass_with_embedder(
        handle,
        started,
        budget,
        dedup_threshold,
        embedder.as_ref(),
        timeouts::embed_batch_timeout(),
    )
    .await
}

/// [`dedup_pass`] against a caller-supplied embedder.
///
/// Why: the pass's contract with the embedder — chunk size, and that the
/// wall-clock budget is checked BEFORE each chunk is paid for — is exactly what
/// #7106 changed, and it cannot be observed through the process-wide
/// `shared_embedder` singleton, which a test cannot re-seed per case. Taking
/// the embedder as a parameter gives that contract a direct test seam without
/// putting a test-only hook on the production path.
/// What: identical to [`dedup_pass`] with the embedder and its per-call ceiling
/// already resolved. `embed_timeout` bounds each chunk; a chunk that exceeds it
/// ends the pass with an error rather than holding the cycle's concurrency
/// permit on a wedged embedder. Production passes
/// `timeouts::embed_batch_timeout()`; taking it as a parameter is what lets the
/// timeout be tested without mutating the process environment.
/// Test: `concurrency_tests::dedup_embeds_in_bounded_chunks`,
/// `concurrency_tests::the_cycle_budget_stops_dedup_between_chunks`,
/// `concurrency_tests::a_wedged_embedder_ends_the_dedup_pass_with_an_error`.
pub(super) async fn dedup_pass_with_embedder(
    handle: &Arc<PalaceHandle>,
    started: std::time::Instant,
    budget: Duration,
    dedup_threshold: f32,
    embedder: &(dyn Embedder + Send + Sync),
    embed_timeout: Duration,
) -> Result<usize> {
    let snapshot: Vec<Drawer> = handle.drawers.read().clone();
    if snapshot.len() < 2 {
        return Ok(0);
    }

    let mut merges: usize = 0;
    let mut already_removed: HashSet<Uuid> = HashSet::new();

    for chunk in snapshot.chunks(DREAM_EMBED_CHUNK) {
        // #7106: the budget is spent BETWEEN chunks, before this chunk's embed
        // is paid for. The pre-fix check sat after one whole-corpus call, so a
        // cycle could blow its budget by a factor of minutes in a single step.
        if started.elapsed() >= budget {
            break;
        }
        let contents: Vec<String> = chunk.iter().map(|d| d.content().to_string()).collect();
        // #7106: every other `embed_batch` caller in this crate wraps the call
        // in the shared ceiling (`deferred_embed`, `write_pipeline`,
        // `share::import`). An unwrapped one here would let a wedged embedder
        // hold a dream permit indefinitely, which is exactly the blocking the
        // permit exists to bound. A timed-out chunk ends the pass with an
        // error, so the cycle writes no partial stats and drops its permit.
        let vectors = tokio::time::timeout(embed_timeout, embedder.embed_batch(&contents))
            .await
            .map_err(|_| {
                anyhow::anyhow!(
                    "embed_batch of {} drawers timed out after {embed_timeout:?} \
                     during dream dedup",
                    contents.len()
                )
            })?
            .map_err(|e| e.context("batch embed drawers for dream dedup"))?;
        drop(contents);

        if vectors.len() != chunk.len() {
            // Defensive: embedder must return one vector per input.
            anyhow::bail!(
                "embedder returned {} vectors for {} drawers",
                vectors.len(),
                chunk.len()
            );
        }

        for (drawer, query_vec) in chunk.iter().zip(vectors.iter()) {
            if started.elapsed() >= budget {
                break;
            }
            if already_removed.contains(&drawer.id) {
                continue;
            }
            // spec-001: never merge away a protected Task drawer.
            if drawer.drawer_type.is_protected() {
                continue;
            }
            merges += dedup_one(
                handle,
                &snapshot,
                drawer,
                query_vec,
                dedup_threshold,
                &mut already_removed,
            )
            .await?;
        }
        // `vectors` drops here — the next chunk never overlaps this one's.
    }
    Ok(merges)
}

/// Merge `drawer`'s nearest duplicate, if it has one above the threshold.
///
/// Why: the chunked rewrite (#7106) nests a chunk loop around what used to be
/// one flat loop; hoisting the per-drawer body keeps both [`dedup_pass`] and
/// this file inside their size budgets and leaves the merge rule in one place.
/// What: searches the HNSW index with `query_vec`, takes the first neighbour
/// that is a different, not-yet-removed, unprotected drawer scoring at least
/// `dedup_threshold`, merges the lower-importance side into the higher, and
/// forgets the loser. Returns 1 when a merge happened, 0 otherwise — at most
/// one merge per source, which keeps the pass's behaviour predictable.
/// Test: `dream_cycle_merges_duplicates`,
/// `concurrency_tests::chunking_preserves_dedup_behaviour`.
async fn dedup_one(
    handle: &Arc<PalaceHandle>,
    snapshot: &[Drawer],
    drawer: &Drawer,
    query_vec: &[f32],
    dedup_threshold: f32,
    already_removed: &mut HashSet<Uuid>,
) -> Result<usize> {
    // Top-3 keeps the dedup pass cheap; the first neighbor is `drawer`
    // itself (score ~1.0) so we look at index 1+. `vector_store.search`
    // returns pure cosine similarity — no importance weighting baked
    // in, so we can compare directly to `dedup_threshold`.
    let hits = handle.vector_store.search(query_vec, 3).await?;
    for hit in hits.into_iter() {
        if hit.drawer_id == drawer.id || already_removed.contains(&hit.drawer_id) {
            continue;
        }
        if hit.score < dedup_threshold {
            continue;
        }
        // Resolve the loser's drawer record from the snapshot. If it's
        // not in the snapshot (e.g. orphan vector), skip — the compact
        // pass will clean it up.
        let Some(hit_drawer) = snapshot.iter().find(|d| d.id == hit.drawer_id) else {
            continue;
        };
        // spec-001: a protected Task drawer must never be merged away, even
        // when it is the lower-importance side of a near-duplicate pair.
        if hit_drawer.drawer_type.is_protected() {
            continue;
        }

        // Pick survivor (higher importance wins; ties keep `drawer`).
        let (survivor, loser) = if drawer.importance >= hit_drawer.importance {
            (drawer, hit_drawer)
        } else {
            (hit_drawer, drawer)
        };
        merge_into(handle, survivor, loser);
        // #5231: surface a failed loser-eviction instead of discarding it —
        // silently keeping the loser leaves the merged content duplicated.
        if let Err(e) = handle.forget(loser.id).await {
            tracing::warn!(id = ?loser.id, "dream dedup: loser evict failed: {e:#}");
        }
        already_removed.insert(loser.id);
        return Ok(1);
    }
    Ok(0)
}

/// Drop drawers whose effective importance is below `prune_importance`
/// AND that are older than 30 days. Returns the prune count.
///
/// #7106: reads the drawer table under its read lock and collects only the
/// victim ids rather than cloning every `Drawer`. Every computation in the scan
/// is pure, so no `.await` runs while the lock is held.
pub(super) async fn prune_pass(
    handle: &Arc<PalaceHandle>,
    started: std::time::Instant,
    budget: Duration,
    prune_importance: f32,
) -> Result<usize> {
    const MIN_AGE_DAYS: f32 = 30.0;
    let victims: Vec<Uuid> = {
        let drawers = handle.drawers.read();
        let mut victims: Vec<Uuid> = Vec::new();
        for drawer in drawers.iter() {
            if started.elapsed() >= budget {
                break;
            }
            // spec-001: Task drawers are never pruned, regardless of age or
            // decayed importance.
            if drawer.drawer_type.is_protected() {
                continue;
            }
            let age = DecayConfig::age_days(drawer.created_at);
            let boost = drawer.accumulated_boost(&handle.decay_config);
            let eff = handle
                .decay_config
                .effective_importance(drawer.importance, age, boost);
            // `<=` (not `<`): once a drawer's effective importance decays to
            // the floor — meaning it's old and unimportant enough that the
            // decay clamp kicked in — it becomes prunable. Using strict `<`
            // here created the floor-collision bug (#55): with the default
            // `floor = prune_importance = 0.05`, the condition `eff < 0.05`
            // was unsatisfiable, so nothing was ever pruned.
            if eff <= prune_importance && age > MIN_AGE_DAYS {
                victims.push(drawer.id);
            }
        }
        victims
    };

    // #5231: report drawers actually dropped, not candidates considered.
    let mut count = 0usize;
    for id in victims {
        match handle.forget(id).await {
            Ok(outcome) if outcome.is_deleted() => count += 1,
            Ok(_) => {}
            Err(e) => tracing::warn!(?id, "dream prune: forget failed: {e:#}"),
        }
    }
    Ok(count)
}

/// Rebuild closets: simple whitespace tokenization, stop-word filter,
/// keyword -> drawer ids. Returns the number of keywords indexed.
///
/// #7106: builds the index straight from the drawer table's read guard instead
/// of cloning the table first. The build is synchronous, so the read lock is
/// never held across an `.await`, and it is released before the closet write
/// lock is taken.
pub(super) fn refresh_closets(handle: &Arc<PalaceHandle>) -> usize {
    let new_index = {
        let drawers = handle.drawers.read();
        build_closet_index(&drawers)
    };
    let count = new_index.len();
    let mut closets = handle.closets.write();
    *closets = new_index;
    count
}
