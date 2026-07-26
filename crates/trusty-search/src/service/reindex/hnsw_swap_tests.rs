//! Round-1 and round-2 regression tests for `hnsw_swap.rs` (issue #3970).
//!
//! Why: extracted from `hnsw_swap.rs` to keep that file under the 500-SLOC
//! production cap (`scripts/check_line_cap.sh`) — mirrors the existing
//! `prune.rs` / `prune_tests.rs` split in this same directory.
//! What: exercises `begin_staged_hnsw_swap` / `commit_staged_hnsw_swap` /
//! `abort_staged_hnsw_swap` end-to-end, plus the round-2 adversarial-review
//! regression coverage (torn-swap safety, rename ordering, and the
//! in-flight-persist quiesce race).
//! Test: this file — run via `cargo test -p trusty-search`.

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

/// Why (round-2 review CRITICAL 1 — the core mechanism, proven
/// end-to-end): proves the chosen sidecar-then-binary rename order in
/// `commit_staged_hnsw_swap` produces a SAFE torn state if interrupted
/// between the two renames — never the collision-prone one. Simulates
/// "crash after the sidecar rename, before the binary rename" by
/// performing exactly that first step manually (the same operation
/// `commit_staged_hnsw_swap` performs first) and stopping there, leaving
/// live = OLD binary + NEW sidecar. Before the round-2 fix (binary-first
/// order), the equivalent interrupted state would have been NEW binary +
/// OLD sidecar instead, whose restored `next_key` sits BEHIND actual
/// usage — the exact precondition for a later `upsert` to allocate an
/// already-occupied key and silently overwrite an unrelated vector.
/// What: ONE store instance drives both the "old" (seeded live) and
/// "new" (staged) snapshots, exactly like a real reindex continuing to
/// mutate the same live `UsearchStore` — `next_key` is the SAME
/// monotonic counter throughout, just as in production. After manually
/// applying only the sidecar rename, asserts (1) the torn state still
/// loads (this direction is legitimate/expected, matching the dangling-
/// `id_to_key`-entry case `UsearchStore::remove`'s doc already
/// documents) and (2) — the actual safety property — that every one of
/// the OLD binary's real vectors is still exactly resolvable by search
/// AFTER an upsert of a brand-new id, proving that new id's freshly
/// allocated key could not have collided with and silently destroyed any
/// of them.
/// Test: this IS the test.
#[tokio::test]
#[serial_test::serial]
async fn torn_swap_interrupted_after_sidecar_rename_never_permits_key_collision() {
    let data_dir = tempfile::tempdir().unwrap();
    unsafe { std::env::set_var("TRUSTY_DATA_DIR", data_dir.path()) };
    let root_dir = tempfile::tempdir().unwrap();

    let (id, handle) = build_test_handle("hnsw-swap-torn", root_dir.path(), None);
    let paths = begin_staged_hnsw_swap(&handle, &id)
        .await
        .expect("resolves");

    // Non-collinear vectors so cosine search can uniquely distinguish
    // each old id's top-1 match (a `[k,0,0,0]` family, as used
    // elsewhere in this file, is useless here — every such vector is
    // cosine-identical to every other, since they all point in exactly
    // the same direction).
    let old_ids: Vec<String> = (0..8).map(|i| format!("old-chunk:{i}")).collect();
    let old_vectors: Vec<Vec<f32>> = (0..8).map(|i| vec![1.0, i as f32, 0.0, 0.0]).collect();

    let store = UsearchStore::new(4).unwrap();
    let old_items: Vec<(String, Vec<f32>)> = old_ids
        .iter()
        .cloned()
        .zip(old_vectors.iter().cloned())
        .collect();
    store.upsert_batch(&old_items).await.unwrap();
    store.save(&paths.live).await.expect("seed old live");
    let old_binary_bytes = std::fs::read(&paths.live).expect("read old binary");

    // The "reindex" continues on the SAME store instance — `next_key`
    // keeps advancing from wherever it left off, exactly like a real
    // reindex that keeps mutating the live `self.store` in place.
    let new_items: Vec<(String, Vec<f32>)> = (0..20)
        .map(|i| (format!("new-chunk:{i}"), vec![0.0, 0.0, 1.0, i as f32]))
        .collect();
    store.upsert_batch(&new_items).await.unwrap();
    store.save(&paths.staging).await.expect("save new staging");

    // Simulate "crash after the sidecar rename, before the binary
    // rename" — `commit_staged_hnsw_swap`'s first step, performed
    // manually; the binary rename is deliberately skipped.
    let staging_sidecar = paths.staging.with_extension("keys.json");
    let live_sidecar = paths.live.with_extension("keys.json");
    std::fs::rename(&staging_sidecar, &live_sidecar).expect("simulate sidecar rename");
    assert_eq!(
        std::fs::read(&paths.live).expect("read live binary"),
        old_binary_bytes,
        "sanity: live binary must still be the OLD one (binary rename skipped)"
    );

    // This torn state must NOT be flagged as corrupt — this direction
    // is the legitimate/expected one (see the guard's doc in
    // `UsearchStore::load_from`).
    let torn = UsearchStore::load_from(&paths.live)
        .await
        .expect("load ok")
        .expect("torn state must still load — this is not the dangerous direction");

    // Upsert a brand-new id — its freshly allocated key must never have
    // been used by anything, old or new.
    torn.upsert("brand-new-chunk", vec![0.0, 1.0, 1.0, 0.0])
        .await
        .expect("upsert after torn load must succeed");

    // THE safety property: every OLD vector must still resolve to
    // itself, unharmed. A collision would have silently removed and
    // overwritten one of them with unrelated content.
    for (id, vector) in old_ids.iter().zip(old_vectors.iter()) {
        let hits = torn.search(vector, 1).await.expect("search ok");
        assert_eq!(
            hits.first().map(|h| h.chunk_id.as_str()),
            Some(id.as_str()),
            "{id}'s vector must survive the post-torn-load upsert unharmed — a \
             collision would have silently overwritten it with unrelated content \
             (issue #3970 round-2 CRITICAL 1)"
        );
    }

    unsafe { std::env::remove_var("TRUSTY_DATA_DIR") };
}

/// Why (round-2 review CRITICAL 2): `abort_staged_hnsw_swap` must not
/// clear the `reindexing` flag while a periodic persist task could still
/// be running — see the function's doc for the full race. This proves
/// the ordering: with `PersistState::in_flight` simulated `true` (as if
/// a real detached task were still mid-coalescing-loop), the abort call
/// must genuinely block until that condition clears, not return
/// immediately and clear the flag regardless.
/// What: sets the simulated in-flight condition, races
/// `abort_staged_hnsw_swap` against a delayed task that clears it after
/// a known interval, and asserts the abort call's total elapsed time is
/// at least that interval — proving it actually waited rather than
/// racing past the still-in-flight condition. A regression back to
/// clearing the flag first (or omitting the wait) would make this
/// function return near-instantly regardless of the simulated
/// condition, failing the timing assertion.
/// Test: this IS the test.
#[tokio::test]
#[serial_test::serial]
async fn abort_staged_hnsw_swap_waits_for_in_flight_persist_before_clearing_flag() {
    let data_dir = tempfile::tempdir().unwrap();
    unsafe { std::env::set_var("TRUSTY_DATA_DIR", data_dir.path()) };
    let root_dir = tempfile::tempdir().unwrap();

    let (id, handle) = build_test_handle(
        "hnsw-swap-abort-race",
        root_dir.path(),
        Some(store_with_vectors(10).await),
    );
    let paths = begin_staged_hnsw_swap(&handle, &id)
        .await
        .expect("resolves");
    partial_checkpoint_to(&paths.staging).await;

    handle
        .indexer
        .read()
        .await
        .simulate_persist_in_flight_for_tests(true);

    // Release the simulated in-flight condition from a GENUINELY
    // independent background task (not raced via `tokio::join!` with the
    // call under test — `join!` waits for BOTH futures regardless of
    // which finishes first, so timing its combined completion can't
    // distinguish "the call under test actually waited" from "it
    // returned instantly but `join!` waited for this task anyway"). Only
    // the call under test is timed below.
    const HOLD: std::time::Duration = std::time::Duration::from_millis(150);
    let indexer_arc = handle.indexer.clone();
    let release_task = tokio::spawn(async move {
        tokio::time::sleep(HOLD).await;
        indexer_arc
            .read()
            .await
            .simulate_persist_in_flight_for_tests(false);
    });

    let start = std::time::Instant::now();
    abort_staged_hnsw_swap(&handle, &id, &paths).await;
    let elapsed = start.elapsed();
    release_task.await.expect("release task must not panic");

    assert!(
        elapsed >= HOLD,
        "abort_staged_hnsw_swap must wait for the in-flight persist task to \
         quiesce before resolving — elapsed {elapsed:?} was under the {HOLD:?} hold, \
         meaning it raced past the still-in-flight condition (issue #3970 round-2 \
         CRITICAL 2)"
    );
    assert!(
        !handle.indexer.read().await.is_reindex_staging(),
        "the flag must still end up cleared once the wait resolves"
    );

    unsafe { std::env::remove_var("TRUSTY_DATA_DIR") };
}

/// Why (round-2 review CRITICAL 2): same race as the abort test above,
/// but for the commit path — `commit_staged_hnsw_swap` must equally wait
/// for any in-flight persist task before doing its own final save and
/// rename, let alone clearing the flag.
/// What: identical structure to
/// `abort_staged_hnsw_swap_waits_for_in_flight_persist_before_clearing_flag`.
/// Test: this IS the test.
#[tokio::test]
#[serial_test::serial]
async fn commit_staged_hnsw_swap_waits_for_in_flight_persist_before_clearing_flag() {
    let data_dir = tempfile::tempdir().unwrap();
    unsafe { std::env::set_var("TRUSTY_DATA_DIR", data_dir.path()) };
    let root_dir = tempfile::tempdir().unwrap();

    let (id, handle) = build_test_handle(
        "hnsw-swap-commit-race",
        root_dir.path(),
        Some(store_with_vectors(10).await),
    );
    let paths = begin_staged_hnsw_swap(&handle, &id)
        .await
        .expect("resolves");

    handle
        .indexer
        .read()
        .await
        .simulate_persist_in_flight_for_tests(true);

    // See `abort_staged_hnsw_swap_waits_for_in_flight_persist_before_clearing_flag`
    // for why the release runs on a genuinely independent background
    // task rather than being raced via `tokio::join!`.
    const HOLD: std::time::Duration = std::time::Duration::from_millis(150);
    let indexer_arc = handle.indexer.clone();
    let release_task = tokio::spawn(async move {
        tokio::time::sleep(HOLD).await;
        indexer_arc
            .read()
            .await
            .simulate_persist_in_flight_for_tests(false);
    });

    let start = std::time::Instant::now();
    commit_staged_hnsw_swap(&handle, &id, &paths).await;
    let elapsed = start.elapsed();
    release_task.await.expect("release task must not panic");

    assert!(
        elapsed >= HOLD,
        "commit_staged_hnsw_swap must wait for the in-flight persist task to \
         quiesce before resolving — elapsed {elapsed:?} was under the {HOLD:?} hold \
         (issue #3970 round-2 CRITICAL 2)"
    );
    assert!(
        !handle.indexer.read().await.is_reindex_staging(),
        "the flag must still end up cleared once the wait resolves"
    );
    assert!(
        paths.live.exists(),
        "commit must still publish once unblocked"
    );

    unsafe { std::env::remove_var("TRUSTY_DATA_DIR") };
}

/// Why (round-2 review CRITICAL 1 — proves the REAL function's actual
/// order, not a manual simulation of it): the torn-swap safety test
/// above manually replicates a "sidecar renamed, binary not" state to
/// prove that direction is safe; this test instead proves
/// `commit_staged_hnsw_swap` itself really performs the sidecar rename
/// BEFORE the binary rename, so a regression back to the pre-round-2
/// binary-first order would be caught here even if the manual-simulation
/// test above kept passing.
/// What: pre-creates a DIRECTORY at the live binary's path. `fs::rename`
/// of a regular file onto an existing directory always fails (EISDIR on
/// POSIX), so the binary rename is guaranteed to fail while the sidecar
/// rename (an unrelated, untouched path) is free to succeed normally.
/// Calls the real `commit_staged_hnsw_swap` and asserts the live
/// sidecar WAS renamed into place despite the binary rename failing —
/// which is only possible if the sidecar rename ran first (and
/// independently of the later binary-rename failure). If the order were
/// reversed, the binary rename would fail immediately and the sidecar
/// rename would never even be attempted.
/// Test: this IS the test.
#[tokio::test]
#[serial_test::serial]
async fn commit_staged_hnsw_swap_renames_sidecar_before_binary() {
    let data_dir = tempfile::tempdir().unwrap();
    unsafe { std::env::set_var("TRUSTY_DATA_DIR", data_dir.path()) };
    let root_dir = tempfile::tempdir().unwrap();

    let (id, handle) = build_test_handle(
        "hnsw-swap-order-proof",
        root_dir.path(),
        Some(store_with_vectors(5).await),
    );
    let paths = begin_staged_hnsw_swap(&handle, &id)
        .await
        .expect("resolves");

    // Force the binary rename to fail predictably: a regular-file →
    // existing-directory rename always errors.
    std::fs::create_dir_all(&paths.live).expect("pre-create live path as a directory");

    commit_staged_hnsw_swap(&handle, &id, &paths).await;

    let live_sidecar = paths.live.with_extension("keys.json");
    assert!(
        live_sidecar.exists(),
        "the sidecar must be renamed into place BEFORE the (here, deliberately \
         failing) binary rename is attempted — issue #3970 round-2 CRITICAL 1 \
         rename ordering; if this fails, the binary rename ran first, aborted, \
         and the sidecar rename was never reached"
    );
    assert!(
        !paths.staging.with_extension("keys.json").exists(),
        "the staging sidecar must have actually moved (renamed), not been left \
         behind or copied"
    );

    unsafe { std::env::remove_var("TRUSTY_DATA_DIR") };
}

/// Save `store_with_vectors(5)` directly to `path` — a bare helper for
/// tests that need a plausible pre-existing staging checkpoint without
/// caring about its exact content.
async fn partial_checkpoint_to(path: &std::path::Path) {
    store_with_vectors(5)
        .await
        .save(path)
        .await
        .expect("checkpoint save");
}
