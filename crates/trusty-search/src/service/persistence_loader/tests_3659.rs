//! Issue #3659 regression tests: concurrent warm-boot corpus load must never
//! panic, and the existing single-clean-open behavior must be unchanged.
//!
//! Why: split out of `tests.rs` (already near the 500-SLOC test-file
//! convention cap used elsewhere in this crate — see `core::indexer::tests`)
//! rather than growing it further. These tests exercise the REAL production
//! call path (`build_indexer_from_entry` → `open_corpus_with_retry` →
//! `core::corpus::open_serialized`), not just the low-level primitive
//! (already unit-tested in `core::corpus::open_guard::tests`) — proving the
//! exact race this issue describes (two callers reaching the same
//! not-yet-registered index's corpus path at once during warm boot) is now
//! panic-safe end-to-end.
//! What: (a) two concurrent `build_indexer_from_entry` calls for the SAME
//! entry — simulating an eager warm-boot restore racing an explicit
//! `POST /indexes` (re)create for the same not-yet-registered id — must both
//! return `Ok` with no panic, exactly one holding a live corpus store; (b) a
//! single clean open is unchanged (already covered extensively by
//! `tests.rs`; kept here as a minimal same-file sanity check).
//! Test: this module.

use super::*;
use tempfile::tempdir;
use trusty_common::embedder::MockEmbedder;

fn mock_embedder() -> Arc<dyn crate::core::embed::Embedder> {
    Arc::new(MockEmbedder::new(8))
}

/// Why: the load-bearing regression test for #3659. Before the fix,
/// `open_corpus_with_retry` called `CorpusStore::open` directly with no
/// per-path serialization — two tasks reaching `build_indexer_from_entry`
/// for the SAME not-yet-registered index at once (the realistic scenario:
/// an eager warm-boot restore racing an explicit create/relocate call, since
/// neither the `ColdIndexStore` loading gate nor the root-path-collision
/// guard in `server::helpers` can see an in-flight, not-yet-registered
/// restore) could race `Database::create` against itself on one file with
/// nothing serializing the two attempts.
/// What: launches two `build_indexer_from_entry` calls for the identical
/// colocated `PersistedIndex` (same id, same root_path — same on-disk
/// `index.redb`) concurrently via `tokio::join!`. Asserts: neither call
/// panics (a raw panic here would fail the test outright — the harness does
/// not swallow it); both return `Ok`; exactly one of the two indexers ends
/// up with a live corpus store, and the other cleanly reports
/// `corpus_open_failed = true` (the expected, non-panicking
/// `DatabaseAlreadyOpen` outcome once the winner's store is opened first —
/// see `core::corpus::open_guard::tests` for why "one winner, clean losers"
/// is the correct end state, not "everyone succeeds").
/// Test: this IS the test.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_build_indexer_for_same_entry_never_panics() {
    let tmp = tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let embedder = mock_embedder();

    let entry = PersistedIndex {
        id: "test-3659-concurrent".to_string(),
        root_path: root.clone(),
        colocated: true,
        ..Default::default()
    };

    // Two concurrent racers against the SAME entry / SAME on-disk redb path.
    let (r1, r2) = tokio::join!(
        build_indexer_from_entry(&entry, &embedder),
        build_indexer_from_entry(&entry, &embedder)
    );

    // Neither call is allowed to return Err here: `build_indexer_from_entry`
    // only returns Err on HNSW-allocator OOM (#954), never on a corpus-open
    // failure — that is folded into `corpus_open_failed` on a still-Ok
    // indexer. If this now returns Err, something regressed the OOM-only
    // contract.
    let idx1 = r1.expect("build_indexer_from_entry must not Err on a corpus race (#954 contract)");
    let idx2 = r2.expect("build_indexer_from_entry must not Err on a corpus race (#954 contract)");

    let winners = [idx1.has_corpus_store(), idx2.has_corpus_store()]
        .iter()
        .filter(|has| **has)
        .count();
    let failed_flags = [idx1.corpus_open_failed, idx2.corpus_open_failed]
        .iter()
        .filter(|f| **f)
        .count();

    assert_eq!(
        winners, 1,
        "exactly one of the two racing builds must end up with a live corpus store \
         (issue #3659): redb allows only one live handle per file"
    );
    assert_eq!(
        failed_flags, 1,
        "exactly one of the two racing builds must cleanly report corpus_open_failed \
         (DatabaseAlreadyOpen), never a panic (issue #3659)"
    );
}

/// Why: regression guard — the #3659 serialization + panic-safety wrapper
/// must not change behavior for the ordinary, non-racing single-open case
/// (the overwhelming common case: one index, one opener).
/// What: a single `build_indexer_from_entry` call against a fresh colocated
/// entry must succeed with a live, empty corpus store, exactly as before
/// this fix.
/// Test: this IS the test.
#[tokio::test]
async fn single_clean_open_is_unchanged() {
    let tmp = tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let embedder = mock_embedder();

    let entry = PersistedIndex {
        id: "test-3659-single".to_string(),
        root_path: root.clone(),
        colocated: true,
        ..Default::default()
    };

    let indexer = build_indexer_from_entry(&entry, &embedder)
        .await
        .expect("a single clean open must still succeed (issue #3659 regression guard)");
    assert!(
        indexer.has_corpus_store(),
        "a single, non-racing open must still wire a live corpus store"
    );
    assert!(
        !indexer.corpus_open_failed,
        "a single, non-racing open must not report corpus_open_failed"
    );
    assert_eq!(
        indexer
            .corpus_store()
            .expect("corpus store must be set")
            .chunk_count()
            .expect("chunk_count must succeed"),
        0,
        "a freshly-created corpus must start empty"
    );
}
