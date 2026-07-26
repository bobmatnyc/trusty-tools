//! Staged-write-then-swap for the periodic HNSW snapshot (issue #3970).
//!
//! Why: `core::indexer::persist_hnsw::spawn_incremental_persist` checkpoints
//! the in-memory HNSW graph to disk every [`crate::core::indexer::HNSW_SNAPSHOT_BATCH_INTERVAL`]
//! batches during EVERY reindex, so a crash mid-reindex doesn't lose all
//! progress. Before this fix that checkpoint wrote straight to the LIVE
//! snapshot path. Reindex progress is monotonic, so any reasonably large
//! reindex crossed `UsearchStore::save`'s shrink guard threshold as ordinary
//! healthy progress — from that checkpoint on, the complete pre-reindex
//! snapshot was already gone, and an ungraceful termination (SIGKILL,
//! OOM-kill, abort, power loss) permanently stranded the index. PR #3968
//! closed the equivalent race for the GRACEFUL shutdown path only
//! (`service::shutdown_flush`); this module closes it for the periodic
//! persister itself, mirroring the atomic staged-swap the redb chunk corpus
//! already has (#603 / #839, `corpus_swap.rs`).
//!
//! What: three entry points, called from `runner.rs` / `finish.rs` alongside
//! (not instead of) the existing redb corpus staging:
//! - [`begin_staged_hnsw_swap`] — resolve the (live, staging) HNSW path pair
//!   and flip the indexer's `reindexing` flag so
//!   `spawn_incremental_persist`'s periodic checkpoints redirect to staging.
//! - [`commit_staged_hnsw_swap`] — waits for any still-coalescing periodic
//!   persist task to quiesce, forces one final AWAITED save to staging (so
//!   the tail batches the throttle skipped are captured), renames the
//!   staging sidecar then the staging binary over the live ones, and only
//!   then clears `reindexing`.
//! - [`abort_staged_hnsw_swap`] — same quiesce-wait, then best-effort
//!   deletes the staging files (the live snapshot is left untouched), and
//!   only then clears `reindexing`.
//!
//! Deliberately does NOT route the periodic save through a
//! skip-while-`Running` gate (the shutdown path's fix) — that would disable
//! incremental persistence during the reindex entirely, trading this
//! crash-safety hole for another (loses ALL progress, not just the tail).
//! The periodic persister keeps checkpointing throughout the reindex; only
//! its *destination* changes.
//!
//! Round-2 adversarial review (two CRITICALs, fixed below — read the doc
//! comments on [`commit_staged_hnsw_swap`] / [`abort_staged_hnsw_swap`] for
//! the full mechanics of each):
//! 1. The staging→live swap is two renames, not one — sidecar is now renamed
//!    BEFORE the binary so a crash strictly between them can only leave a
//!    live pairing whose `next_key` is ahead of (never behind) actual usage,
//!    which cannot collide on the next write; `UsearchStore::load_from` also
//!    now refuses a binary reporting MORE vectors than its sidecar describes,
//!    as additional defense-in-depth against a torn pairing from any source.
//! 2. `end_reindex_staging()` is no longer the first thing either function
//!    does — both now wait for `CodeIndexer::wait_for_incremental_persist_drain`
//!    before touching anything, and only clear the flag after the swap (or
//!    abort cleanup) has fully resolved, closing a race where a detached
//!    periodic-persist task that outlived the reindex's batch loop could
//!    still observe the flag flip and write partial state straight to live.
//!
//! Test: `tests` submodule below.

use crate::core::registry::{IndexHandle, IndexId};
use std::path::{Path, PathBuf};

/// The (live, staging) HNSW snapshot path pair for one index.
pub(super) struct HnswSwapPaths {
    pub(super) live: PathBuf,
    pub(super) staging: PathBuf,
}

/// Begin staged HNSW persistence for a reindex (issue #3970).
///
/// Why: called once, before the batch loop starts, so every periodic
/// checkpoint for the duration of the reindex redirects to staging instead of
/// the live snapshot.
/// What: resolves the live + staging HNSW paths (colocated or legacy,
/// mirroring `begin_staged_corpus_swap`'s own routing), sets
/// `CodeIndexer::begin_reindex_staging`, and returns the pair. Returns `None`
/// only when a path is unresolvable (e.g. an unwritable data dir) — in that
/// case the reindexing flag is left untouched and the periodic persister
/// keeps writing straight to the live path exactly as it did before this fix
/// (a degraded-but-not-worse fallback, matching how `begin_staged_corpus_swap`
/// falls back to direct-write mode on its own path failures).
/// Test: `tests::begin_staged_hnsw_swap_resolves_legacy_paths`.
pub(super) async fn begin_staged_hnsw_swap(
    handle: &IndexHandle,
    index_id: &IndexId,
) -> Option<HnswSwapPaths> {
    let is_colocated = crate::service::colocated_storage::has_colocated_storage(&handle.root_path);
    let live = if is_colocated {
        crate::service::colocated_storage::colocated_hnsw_path(&handle.root_path)
    } else {
        crate::service::persistence::hnsw_path(&index_id.0)
    };
    let live = match live {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(
                "staged hnsw swap: cannot resolve live hnsw path for '{}' ({e}) — periodic \
                 checkpoints will keep writing directly to the live path",
                index_id.0
            );
            return None;
        }
    };
    let staging = if is_colocated {
        crate::service::colocated_storage::colocated_hnsw_staging_path(&handle.root_path)
    } else {
        crate::service::persistence::hnsw_staging_path(&index_id.0)
    };
    let staging = match staging {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(
                "staged hnsw swap: cannot resolve staging hnsw path for '{}' ({e}) — periodic \
                 checkpoints will keep writing directly to the live path",
                index_id.0
            );
            return None;
        }
    };

    handle.indexer.read().await.begin_reindex_staging();
    tracing::debug!(
        "staged hnsw swap: reindex staging began for '{}' (staging={})",
        index_id.0,
        staging.display()
    );
    Some(HnswSwapPaths { live, staging })
}

/// Bound on how long [`commit_staged_hnsw_swap`] / [`abort_staged_hnsw_swap`]
/// wait for a still-coalescing periodic persist task to quiesce before
/// resolving the swap (issue #3970 round-2 review, CRITICAL finding 2).
///
/// Why: bounded so a pathological stall in the periodic persister (e.g. a
/// stuck disk) cannot hang the reindex's terminal event forever — matches
/// the order of magnitude of other bounded persistence waits in this crate
/// (`service::shutdown_flush::MIN_FLUSH_TIMEOUT_SECS`).
const PERSIST_DRAIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Finalize (commit) the staged HNSW swap after a successful reindex
/// (issue #3970).
///
/// Why: publishes the reindex's final HNSW state to the live path in exactly
/// one step, mirroring `commit_staged_corpus_swap`.
/// What:
/// 1. Waits (bounded, [`PERSIST_DRAIN_TIMEOUT`]) for any still-coalescing
///    periodic persist task to fully quiesce — see
///    [`crate::core::CodeIndexer::wait_for_incremental_persist_drain`] for
///    why this must happen BEFORE anything else (round-2 CRITICAL 2: without
///    it, a task that outlives the batch loop could still write partial
///    state straight to the live path after the flag below is cleared).
/// 2. Forces one last, AWAITED save to `paths.staging` (capturing any tail
///    batches the throttle skipped), now guaranteed uncontended.
/// 3. Renames the staging sidecar THEN the staging binary over the live
///    ones (round-2 CRITICAL 1 — see the ordering rationale below).
/// 4. ONLY NOW clears the indexer's `reindexing` flag, once the swap is
///    fully resolved — never before.
///
/// Rename ordering (round-2 CRITICAL 1): both renames are same-directory
/// (same filesystem) and individually atomic at the syscall level, but the
/// PAIR is not atomic — a crash strictly between them leaves a "torn" live
/// pair. Renaming the SIDECAR first means the only possible torn state is
/// OLD binary + NEW sidecar (never the reverse): `next_key` restored from
/// that newer sidecar is then guaranteed >= every key ever allocated on this
/// store (`next_key` only ever increases — see `UsearchStore::next_key`'s
/// doc), so a subsequent `upsert` of a genuinely new chunk can never be
/// allocated a key that collides with an existing occupied slot in whichever
/// binary ends up live. The reverse order (binary-first, the pre-round-2
/// shape of this function) can leave NEW binary + OLD sidecar instead — a
/// stale, BEHIND-actual-usage `next_key` that CAN collide, silently
/// overwriting an unrelated live vector on the next write. As additional
/// defense-in-depth, `UsearchStore::load_from` now refuses to load a binary
/// that reports MORE vectors than its paired sidecar's key map describes
/// (the direction only a torn pairing — never legitimate operation — can
/// produce; see that guard's doc), so even a torn pairing from some other
/// source is caught rather than silently trusted.
/// A failure at any step is logged and leaves the live snapshot at whatever
/// it already was — never partially written.
/// Test: `tests::commit_staged_hnsw_swap_publishes_final_state`,
/// `tests::commit_staged_hnsw_swap_waits_for_in_flight_persist_before_clearing_flag`.
pub(super) async fn commit_staged_hnsw_swap(
    handle: &IndexHandle,
    index_id: &IndexId,
    paths: &HnswSwapPaths,
) {
    // Round-2 CRITICAL 2: quiesce BEFORE touching anything else. While we
    // wait, `reindexing` stays `true`, so any still-running task keeps
    // correctly targeting staging.
    //
    // LOAD-BEARING INVARIANTS if this times out (round-3 review — a `false`
    // return does NOT itself make proceeding safe; these two facts do):
    //
    // 1. A still-running task's CURRENT (and, in practice, only) iteration
    //    already fixed its target as `staging` before this wait began —
    //    `spawn_incremental_persist` reads `PersistState::reindexing` fresh
    //    at the top of each coalescing-loop iteration, and `reindexing` was
    //    still `true` when that iteration started. It can only pick a
    //    DIFFERENT target by starting a NEW iteration, which requires
    //    `PersistState::dirty` to be set again — and nothing sets `dirty`
    //    for this index between the reindex's batch loop ending and this
    //    swap resolving (the only caller, `commit_parsed_batch`, is not
    //    invoked again for this index until either a watch-loop event or the
    //    NEXT reindex). So however long that task remains "stuck", it can
    //    only ever finish writing to `staging` — never `live` — for the
    //    stuck iteration itself.
    // 2. `UsearchStore::save`'s `save_lock` (a `tokio::sync::Mutex`, see that
    //    field's doc) additionally serializes our OWN forced final save
    //    (just below) against that same stuck task's save: if the stuck task
    //    is holding `save_lock`, our save queues behind it and only starts
    //    once it releases; if the stuck task hasn't yet acquired it, ours
    //    may run first and the stuck task's save queues behind OURS. Either
    //    way, by the time our save (and the rename further below) completes,
    //    any concurrent save from that task has been folded into a
    //    consistent ordering — our save is never torn by, and never tears,
    //    a concurrent write from it.
    //
    // Fact 1 is what actually prevents the live path from being corrupted by
    // a stuck task in the timeout case; fact 2 is what makes OUR OWN forced
    // save (and therefore the swap we're about to publish) correct and
    // uncorrupted regardless of what that stuck task is doing concurrently.
    // If a future change makes `dirty` reachable for this index during this
    // window (fact 1) — or narrows `save_lock` to not cover the whole save
    // (fact 2) — this reasoning breaks and the CRITICAL 2 race this
    // drain-wait exists to close can reopen on the timeout path specifically.
    if !handle
        .indexer
        .read()
        .await
        .wait_for_incremental_persist_drain(PERSIST_DRAIN_TIMEOUT)
        .await
    {
        tracing::warn!(
            "staged hnsw swap: a periodic persist task for '{}' did not quiesce within {:?} — \
             proceeding with the swap anyway (issue #3970 round-2); safe only because a \
             still-in-flight task's target was already fixed as staging before this wait \
             began (dirty can't be re-triggered for this index in this window) and \
             UsearchStore::save's save_lock serializes our forced final save against it — \
             see this branch's doc comment",
            index_id.0,
            PERSIST_DRAIN_TIMEOUT,
        );
    }

    // Issue #29 analogue: force a final checkpoint so the tail batches since
    // the last throttled save are durable — but AWAITED (unlike
    // `force_incremental_persist`, which only spawns a detached task) so the
    // rename below is guaranteed to publish this up-to-date state, not a
    // stale periodic one. Uncontended now that the drain-wait above has
    // returned (or timed out).
    let save_result = {
        let indexer = handle.indexer.read().await;
        indexer.save_vector_store(&paths.staging).await
    };

    match save_result {
        Ok(false) => {
            // No vector store wired (BM25-only index) — nothing to swap.
            handle.indexer.read().await.end_reindex_staging();
            return;
        }
        Err(e) => {
            tracing::warn!(
                "staged hnsw swap: final staging save failed for '{}' ({e}) — live snapshot \
                 left at its last periodic-persist state (issue #3970)",
                index_id.0
            );
            handle.indexer.read().await.end_reindex_staging();
            return;
        }
        Ok(true) => {}
    }

    if !paths.staging.exists() {
        // Nothing was ever written to staging this run (e.g. the index had
        // zero vectors throughout) — no swap needed.
        handle.indexer.read().await.end_reindex_staging();
        return;
    }

    let staging_sidecar = paths.staging.with_extension("keys.json");
    let live_sidecar = paths.live.with_extension("keys.json");
    let live = paths.live.clone();
    let staging = paths.staging.clone();
    let index_id_for_task = index_id.0.clone();
    // Round-2 CRITICAL 1: sidecar renamed FIRST — see this function's doc
    // for why that ordering is the direction a torn swap can safely fail
    // into, rather than the collision-prone direction the reverse order
    // risks.
    let rename_result = tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        // The sidecar may be missing only in a degenerate empty-index case;
        // tolerate `NotFound` so it doesn't mask the binary rename below.
        match std::fs::rename(&staging_sidecar, &live_sidecar) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }?;
        std::fs::rename(&staging, &live)
    })
    .await;

    match rename_result {
        Ok(Ok(())) => {
            tracing::info!(
                "staged hnsw swap: committed — atomically published the reindexed HNSW \
                 snapshot to {} for '{}' (issue #3970)",
                paths.live.display(),
                index_id_for_task
            );
        }
        Ok(Err(e)) => tracing::warn!(
            "staged hnsw swap: rename failed for '{}' ({e}) — live snapshot left at its \
             last periodic-persist state",
            index_id_for_task
        ),
        Err(e) => tracing::warn!(
            "staged hnsw swap: rename task panicked for '{}': {e}",
            index_id_for_task
        ),
    }

    // Round-2 CRITICAL 2: only clear the flag once the swap has fully
    // resolved (rename attempted and settled either way) — never before.
    handle.indexer.read().await.end_reindex_staging();
}

/// Discard the staged HNSW snapshot after an aborted / failed / memory-
/// aborted reindex (issue #3970).
///
/// Why: mirrors `abort_staged_corpus_swap` — a reindex that does not reach a
/// `Ready` outcome must never publish its (by definition incomplete or
/// invalid) staged snapshot over the live one.
/// What:
/// 1. Waits (bounded, [`PERSIST_DRAIN_TIMEOUT`]) for any still-coalescing
///    periodic persist task to quiesce — round-2 CRITICAL 2, see
///    [`commit_staged_hnsw_swap`]'s doc for the full race this closes. It
///    matters just as much here: the in-memory store at abort time is, by
///    definition, partial/invalid, so a stale task racing past a
///    too-early flag-clear would write exactly that partial state to LIVE.
/// 2. Best-effort deletes the staging binary + sidecar. The live snapshot is
///    never touched.
/// 3. ONLY NOW clears the indexer's `reindexing` flag so future commits
///    (outside this reindex) resume writing to the live path.
///
/// A left-behind staging file (e.g. delete failed) is harmlessly overwritten
/// by the next reindex attempt's periodic checkpoints — same accepted
/// trade-off the redb corpus tmp path already has.
/// Test: `tests::abort_staged_hnsw_swap_leaves_live_untouched_and_cleans_staging`,
/// `tests::abort_staged_hnsw_swap_waits_for_in_flight_persist_before_clearing_flag`.
pub(super) async fn abort_staged_hnsw_swap(
    handle: &IndexHandle,
    index_id: &IndexId,
    paths: &HnswSwapPaths,
) {
    // Round-2 CRITICAL 2: quiesce before touching anything else — see
    // `commit_staged_hnsw_swap`'s doc comment on its identical wait for the
    // full two-part reasoning. Only INVARIANT 1 there applies to abort
    // (a still-running task's current iteration already fixed `staging` as
    // its target before this wait began, and cannot re-target `live`
    // without a fresh `dirty` trigger that nothing produces for this index
    // in this window): abort never calls `UsearchStore::save` itself, so
    // INVARIANT 2 (the `save_lock` serialization against our OWN forced
    // save) does not apply here — there is no forced save on this path for
    // it to serialize against. That asymmetry is fine: invariant 1 alone is
    // what actually keeps a stuck task off the live path; `save_lock` in
    // commit's case is about that function's OWN save being correct, not an
    // additional requirement for abort's safety.
    if !handle
        .indexer
        .read()
        .await
        .wait_for_incremental_persist_drain(PERSIST_DRAIN_TIMEOUT)
        .await
    {
        tracing::warn!(
            "staged hnsw swap: a periodic persist task for '{}' did not quiesce within {:?} \
             before abort — proceeding anyway (issue #3970 round-2); safe only because a \
             still-in-flight task's target was already fixed as staging before this wait \
             began and dirty can't be re-triggered for this index in this window — see \
             this branch's doc comment",
            index_id.0,
            PERSIST_DRAIN_TIMEOUT,
        );
    }

    let staging = paths.staging.clone();
    let staging_sidecar = paths.staging.with_extension("keys.json");
    let index_id_for_task = index_id.0.clone();
    let removed = tokio::task::spawn_blocking(move || {
        remove_if_exists(&staging)?;
        remove_if_exists(&staging_sidecar)?;
        Ok::<(), std::io::Error>(())
    })
    .await;
    match removed {
        Ok(Ok(())) => {}
        Ok(Err(e)) => tracing::warn!(
            "staged hnsw swap: could not delete staging hnsw snapshot for '{}': {e} — will be \
             overwritten by the next reindex attempt",
            index_id_for_task
        ),
        Err(e) => tracing::warn!(
            "staged hnsw swap: staging-delete task panicked for '{}': {e}",
            index_id_for_task
        ),
    }

    // Round-2 CRITICAL 2: only clear the flag once staging cleanup has fully
    // resolved — never before.
    handle.indexer.read().await.end_reindex_staging();
}

/// Remove `path` if present, treating `NotFound` as success.
fn remove_if_exists(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
#[path = "hnsw_swap_tests.rs"]
mod tests;
