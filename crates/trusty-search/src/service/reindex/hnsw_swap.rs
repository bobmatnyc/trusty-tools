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
//! - [`commit_staged_hnsw_swap`] — force one final, AWAITED save to staging
//!   (so the tail batches the throttle skipped are captured), then atomically
//!   rename staging → live (binary + sidecar).
//! - [`abort_staged_hnsw_swap`] — clear the `reindexing` flag and
//!   best-effort delete the staging files; the live snapshot is left
//!   untouched.
//!
//! Deliberately does NOT route the periodic save through a
//! skip-while-`Running` gate (the shutdown path's fix) — that would disable
//! incremental persistence during the reindex entirely, trading this
//! crash-safety hole for another (loses ALL progress, not just the tail).
//! The periodic persister keeps checkpointing throughout the reindex; only
//! its *destination* changes.
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

/// Finalize (commit) the staged HNSW swap after a successful reindex
/// (issue #3970).
///
/// Why: publishes the reindex's final HNSW state to the live path in exactly
/// one atomic step, mirroring `commit_staged_corpus_swap`.
/// What: forces one last, AWAITED save to `paths.staging` (capturing any tail
/// batches the throttle skipped since the last periodic checkpoint), clears
/// the indexer's `reindexing` flag, then renames the staging binary and
/// sidecar over the live ones. Both renames are same-directory (same
/// filesystem) and individually atomic at the syscall level; a crash
/// strictly between them can leave a live binary/sidecar pair that no longer
/// match — `UsearchStore::load_from`'s existing corruption/mismatch guards
/// (issue #2922) already treat that as "discard and fall back to a fresh
/// store", so a botched swap loses this reindex's gains rather than
/// corrupting or crashing. A failure at any step is logged and leaves the
/// live snapshot at whatever it already was — never partially written.
/// Test: `tests::commit_staged_hnsw_swap_publishes_final_state`.
pub(super) async fn commit_staged_hnsw_swap(
    handle: &IndexHandle,
    index_id: &IndexId,
    paths: &HnswSwapPaths,
) {
    // Issue #29 analogue: force a final checkpoint so the tail batches since
    // the last throttled save are durable — but AWAITED (unlike
    // `force_incremental_persist`, which only spawns a detached task) so the
    // rename below is guaranteed to publish this up-to-date state, not a
    // stale periodic one. `UsearchStore::save`'s own `save_lock` serializes
    // this against any in-flight periodic checkpoint task, so this call
    // simply waits its turn and then writes the truly-final state.
    let save_result = {
        let indexer = handle.indexer.read().await;
        indexer.save_vector_store(&paths.staging).await
    };

    // Clear the reindexing flag regardless of outcome so any future commit
    // (outside this reindex) resumes checkpointing straight to the live path.
    handle.indexer.read().await.end_reindex_staging();

    match save_result {
        Ok(false) => {
            // No vector store wired (BM25-only index) — nothing to swap.
            return;
        }
        Err(e) => {
            tracing::warn!(
                "staged hnsw swap: final staging save failed for '{}' ({e}) — live snapshot \
                 left at its last periodic-persist state (issue #3970)",
                index_id.0
            );
            return;
        }
        Ok(true) => {}
    }

    if !paths.staging.exists() {
        // Nothing was ever written to staging this run (e.g. the index had
        // zero vectors throughout) — no swap needed.
        return;
    }

    let staging_sidecar = paths.staging.with_extension("keys.json");
    let live_sidecar = paths.live.with_extension("keys.json");
    let live = paths.live.clone();
    let staging = paths.staging.clone();
    let index_id_for_task = index_id.0.clone();
    let rename_result = tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        std::fs::rename(&staging, &live)?;
        // The sidecar may be missing only in a degenerate empty-index case;
        // tolerate `NotFound` so it doesn't mask the binary rename's success.
        match std::fs::rename(&staging_sidecar, &live_sidecar) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
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
}

/// Discard the staged HNSW snapshot after an aborted / failed / memory-
/// aborted reindex (issue #3970).
///
/// Why: mirrors `abort_staged_corpus_swap` — a reindex that does not reach a
/// `Ready` outcome must never publish its (by definition incomplete or
/// invalid) staged snapshot over the live one.
/// What: clears the indexer's `reindexing` flag so future commits resume
/// writing to the live path, then best-effort deletes the staging binary +
/// sidecar. The live snapshot is never touched. A left-behind staging file
/// (e.g. delete failed) is harmlessly overwritten by the next reindex
/// attempt's periodic checkpoints — same accepted trade-off the redb corpus
/// tmp path already has.
/// Test: `tests::abort_staged_hnsw_swap_leaves_live_untouched_and_cleans_staging`.
pub(super) async fn abort_staged_hnsw_swap(
    handle: &IndexHandle,
    index_id: &IndexId,
    paths: &HnswSwapPaths,
) {
    handle.indexer.read().await.end_reindex_staging();

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
mod tests {
    use super::*;
    use crate::core::store::{UsearchStore, VectorStore as _};
    use crate::core::CodeIndexer;

    fn build_test_handle(
        id_str: &str,
        root: &std::path::Path,
        store: Option<UsearchStore>,
    ) -> (IndexId, IndexHandle) {
        let id = IndexId::new(id_str.to_string());
        let mut indexer = CodeIndexer::new(id.0.clone(), root);
        if let Some(store) = store {
            indexer.set_store(std::sync::Arc::new(store));
        }
        let handle = IndexHandle::bare(
            id.clone(),
            std::sync::Arc::new(tokio::sync::RwLock::new(indexer)),
            root.to_path_buf(),
        );
        (id, handle)
    }

    async fn store_with_vectors(count: usize) -> UsearchStore {
        let store = UsearchStore::new(4).unwrap();
        let items: Vec<(String, Vec<f32>)> = (0..count)
            .map(|i| (format!("chunk:{i}"), vec![i as f32 + 1.0, 0.0, 0.0, 0.0]))
            .collect();
        store.upsert_batch(&items).await.unwrap();
        store
    }

    /// Why: `begin_staged_hnsw_swap` must resolve two distinct, correctly
    /// suffixed paths and flip the indexer's `reindexing` flag.
    /// Test: this test.
    #[tokio::test]
    #[serial_test::serial]
    async fn begin_staged_hnsw_swap_resolves_legacy_paths() {
        let data_dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("TRUSTY_DATA_DIR", data_dir.path()) };
        let root_dir = tempfile::tempdir().unwrap();

        let (id, handle) = build_test_handle("hnsw-swap-begin", root_dir.path(), None);
        let paths = begin_staged_hnsw_swap(&handle, &id)
            .await
            .expect("resolves");

        assert_ne!(paths.live, paths.staging, "live and staging must differ");
        assert!(
            paths.staging.to_string_lossy().contains("reindex-staging"),
            "staging path must be distinguishable: {}",
            paths.staging.display()
        );
        assert!(
            handle.indexer.read().await.is_reindex_staging(),
            "begin must flip the reindexing flag"
        );

        unsafe { std::env::remove_var("TRUSTY_DATA_DIR") };
    }

    /// Why (the core #3970 fix, proven end-to-end): committing must publish
    /// the CURRENT in-memory state to the live path via the staged swap, and
    /// clear the reindexing flag so later commits resume direct-to-live.
    /// Test: this test.
    #[tokio::test]
    #[serial_test::serial]
    async fn commit_staged_hnsw_swap_publishes_final_state() {
        let data_dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("TRUSTY_DATA_DIR", data_dir.path()) };
        let root_dir = tempfile::tempdir().unwrap();

        let store = store_with_vectors(50).await;
        let (id, handle) = build_test_handle("hnsw-swap-commit", root_dir.path(), Some(store));
        let paths = begin_staged_hnsw_swap(&handle, &id)
            .await
            .expect("resolves");

        commit_staged_hnsw_swap(&handle, &id, &paths).await;

        assert!(
            !handle.indexer.read().await.is_reindex_staging(),
            "commit must clear the reindexing flag"
        );
        assert!(paths.live.exists(), "commit must publish to the live path");
        assert!(
            !paths.staging.exists(),
            "commit must rename staging away (no leftover staging file)"
        );
        let reloaded = UsearchStore::load_from(&paths.live)
            .await
            .expect("load ok")
            .expect("some");
        assert_eq!(reloaded.len().await.unwrap(), 50);

        unsafe { std::env::remove_var("TRUSTY_DATA_DIR") };
    }

    /// Why: an aborted/rolled-back reindex must never publish its staged
    /// (incomplete or invalid) snapshot — the live snapshot must be exactly
    /// what it was before the reindex began, and staging must be cleaned up.
    /// Test: this test.
    #[tokio::test]
    #[serial_test::serial]
    async fn abort_staged_hnsw_swap_leaves_live_untouched_and_cleans_staging() {
        let data_dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("TRUSTY_DATA_DIR", data_dir.path()) };
        let root_dir = tempfile::tempdir().unwrap();

        // Seed a complete live snapshot BEFORE the reindex begins.
        let seed = store_with_vectors(200).await;
        let (id, handle) = build_test_handle("hnsw-swap-abort", root_dir.path(), None);
        let paths = begin_staged_hnsw_swap(&handle, &id)
            .await
            .expect("resolves");
        seed.save(&paths.live).await.expect("seed save");
        let live_before = std::fs::read(&paths.live).expect("read seeded live");

        // Simulate a partial reindex checkpoint landing in staging.
        let partial = store_with_vectors(30).await;
        partial.save(&paths.staging).await.expect("partial save");
        assert!(paths.staging.exists());

        abort_staged_hnsw_swap(&handle, &id, &paths).await;

        assert!(
            !handle.indexer.read().await.is_reindex_staging(),
            "abort must clear the reindexing flag"
        );
        let live_after = std::fs::read(&paths.live).expect("read live after abort");
        assert_eq!(
            live_after, live_before,
            "abort must leave the live snapshot byte-for-byte unchanged"
        );
        assert!(
            !paths.staging.exists(),
            "abort must delete the staging snapshot"
        );

        unsafe { std::env::remove_var("TRUSTY_DATA_DIR") };
    }

    /// Why (issue #3970 — staging cleanup / restart behavior after an
    /// UNGRACEFUL interruption): an ungraceful crash (SIGKILL/OOM-kill/abort/
    /// power loss) mid-reindex runs neither `commit_staged_hnsw_swap` nor
    /// `abort_staged_hnsw_swap` — there is no boot-time sweep for a stale
    /// staging file, the same accepted trade-off the redb corpus tmp path
    /// already has. This test proves that leftover staging file is inert
    /// (never mistaken for a live/loadable snapshot) and self-cleaning: the
    /// NEXT reindex attempt's own staging writes simply overwrite it, and a
    /// successful commit from that next attempt publishes correctly despite
    /// the orphaned leftover.
    /// What: seeds a live snapshot, simulates an interrupted first reindex
    /// attempt (`begin` + a partial checkpoint written straight to the
    /// staging path, mimicking `spawn_incremental_persist`, then NEITHER
    /// commit nor abort — the crash). Asserts a `load_from(live)` at that
    /// point (simulating a warm-boot right after the crash) restores the
    /// pre-crash complete state, completely unaffected by the orphaned
    /// staging file. Then simulates a second, successful reindex attempt
    /// (`begin` again + a full checkpoint + `commit`) and asserts the final
    /// live state is that second attempt's data, with no leftover staging
    /// file remaining.
    /// Test: this test.
    #[tokio::test]
    #[serial_test::serial]
    async fn interrupted_reindex_leaves_inert_staging_that_next_attempt_overwrites() {
        let data_dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("TRUSTY_DATA_DIR", data_dir.path()) };
        let root_dir = tempfile::tempdir().unwrap();

        // Seed the pre-crash complete live snapshot.
        let seed = store_with_vectors(100).await;
        let (id, handle) = build_test_handle("hnsw-swap-restart", root_dir.path(), None);
        let paths = begin_staged_hnsw_swap(&handle, &id)
            .await
            .expect("resolves");
        seed.save(&paths.live).await.expect("seed save");

        // First reindex attempt: begin staging, write ONE partial checkpoint
        // straight to the staging path (mirrors what
        // `spawn_incremental_persist` does mid-reindex) — then simulate an
        // ungraceful crash: neither commit nor abort ever runs.
        let attempt_one_partial = store_with_vectors(40).await;
        attempt_one_partial
            .save(&paths.staging)
            .await
            .expect("attempt-1 partial checkpoint");
        assert!(
            paths.staging.exists(),
            "orphaned staging file must exist post-crash"
        );

        // Simulate a warm-boot immediately after the crash: only the LIVE
        // path is ever loaded at boot — the orphaned staging file must be
        // completely inert and never consulted.
        let post_crash_boot = UsearchStore::load_from(&paths.live)
            .await
            .expect("load ok")
            .expect("some");
        assert_eq!(
            post_crash_boot.len().await.unwrap(),
            100,
            "warm-boot after an ungraceful crash must restore the last COMPLETE \
             live snapshot, unaffected by the orphaned partial staging file"
        );

        // Second reindex attempt for the SAME index: begin again (as the
        // orchestrator does for every reindex, retried or not), write a
        // fresh full checkpoint — overwriting the orphaned leftover, proving
        // it does not accumulate — and commit successfully this time.
        let paths_attempt_two = begin_staged_hnsw_swap(&handle, &id)
            .await
            .expect("resolves");
        let attempt_two_full = store_with_vectors(150).await;
        handle
            .indexer
            .write()
            .await
            .set_store(std::sync::Arc::new(attempt_two_full));
        commit_staged_hnsw_swap(&handle, &id, &paths_attempt_two).await;

        assert!(
            !paths_attempt_two.staging.exists(),
            "a successful next attempt must leave no leftover staging file — \
             the orphan from the crashed attempt does not accumulate"
        );
        let final_live = UsearchStore::load_from(&paths_attempt_two.live)
            .await
            .expect("load ok")
            .expect("some");
        assert_eq!(
            final_live.len().await.unwrap(),
            150,
            "the retried reindex's complete state must be what's live, not the \
             crashed attempt's partial state nor the pre-crash seed"
        );

        unsafe { std::env::remove_var("TRUSTY_DATA_DIR") };
    }
}
