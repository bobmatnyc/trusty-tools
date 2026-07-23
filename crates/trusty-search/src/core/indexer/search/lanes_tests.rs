//! Regression tests for the reclaim-vs-search race identified in the #2846
//! PR review (MEDIUM): `bm25_search` / `grep_fallback_search` each call
//! `ensure_*_loaded()` and then acquire a SEPARATE read lock, so a
//! memory-pressure reclaim (`CodeIndexer::reclaim_memory_now`) can land in
//! between and clear the structure right after the ensure-check passed —
//! silently losing that lane's hits for the racing query. This was
//! unreachable when only idle-evict could clear these structures (idle-evict
//! only ever fires after 60s of quiet — it can't race an active query), but
//! the memory-pressure ticker fires under active load by design.
//!
//! Why deterministic (not statistical/stress) tests: holding the same
//! `RwLock` write guard the query lane is about to contend for lets us force
//! the exact interleaving — the racing reclaim is applied WHILE the query
//! task is blocked inside its own `read().await` call, i.e. strictly after
//! its `ensure_*_loaded()` returned "already populated, nothing to do" — the
//! precise window the review flagged. This is reproducible on every run, no
//! flakiness budget needed.
//! What: for each lane, spawn the query as a background task while the test
//! holds the lane's write lock; once the background task is confirmed
//! blocked on its own read-lock acquisition, mutate the structure to empty +
//! set the evicted flag (exactly what `reclaim_memory_now` does) and drop the
//! guard. Assert the query still returns the correct hit — proving the retry
//! (added in `lanes.rs`) rehydrates in place instead of surfacing a false
//! empty lane.
//! Test: this module (`bm25_search_survives_reclaim_race_between_ensure_and_read`,
//! `grep_fallback_survives_reclaim_race_between_ensure_and_read`).

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use crate::core::bm25::Bm25Index;
use crate::core::corpus::CorpusStore;
use crate::core::indexer::CodeIndexer;

/// BM25-only indexer with a durable redb corpus wired (mirrors
/// `tests_idle_evict::make_indexer_with_corpus`) — a durable corpus is
/// required for both `ensure_bm25_entities_loaded` and `ensure_chunks_loaded`
/// to have anything to rehydrate from.
fn make_indexer_with_corpus(redb_path: &std::path::Path) -> CodeIndexer {
    let mut idx = CodeIndexer::new("lanes-race-test", "/tmp/lanes-race-test");
    let store = CorpusStore::open(redb_path).expect("open corpus store");
    idx.set_corpus_store(Arc::new(store));
    idx
}

/// Block on `notified` becoming true, polling briefly — used to confirm the
/// background query task has actually reached (and blocked inside) its
/// read-lock acquisition before we mutate the guarded structure out from
/// under it. A fixed sleep is sufficient here (single-threaded `#[tokio::test]`
/// runtime — cooperative scheduling means a `yield_now` + short sleep
/// reliably lets the spawned task run until it genuinely blocks).
async fn let_background_task_reach_its_read_lock() {
    tokio::task::yield_now().await;
    tokio::time::sleep(Duration::from_millis(20)).await;
}

/// `bm25_search` must recover from a reclaim landing strictly between its
/// `ensure_bm25_entities_loaded()` call and its `bm25.read()` acquisition.
#[tokio::test]
#[serial_test::serial]
async fn bm25_search_survives_reclaim_race_between_ensure_and_read() {
    let dir = tempfile::tempdir().unwrap();
    let redb_path = dir.path().join("index.redb");
    let idx = Arc::new(make_indexer_with_corpus(&redb_path));

    idx.index_files_batch(&[
        (
            "src/auth.rs".into(),
            "pub struct MyType { x: u32 }\nfn authenticate() {}".into(),
        ),
        ("src/token.rs".into(), "fn verify_token() {}".into()),
    ])
    .await
    .expect("index batch");
    assert!(
        !idx.bm25.read().await.is_empty(),
        "expected BM25 documents after indexing"
    );

    // Hold the write lock so the spawned `bm25_search` blocks INSIDE its own
    // `bm25.read()` call — i.e. strictly after its `ensure_bm25_entities_loaded()`
    // already observed BM25 populated.
    let write_guard = idx.bm25.write().await;

    let idx_bg = Arc::clone(&idx);
    let search_task =
        tokio::spawn(async move { idx_bg.bm25_search("authenticate", 5, None).await });

    let_background_task_reach_its_read_lock().await;

    // Simulate the reclaim landing in the race window: clear BM25 and mark it
    // evicted, exactly as `CodeIndexer::reclaim_memory_now` does, while still
    // holding the lock the blocked search is waiting on.
    let mut guard = write_guard;
    *guard = Bm25Index::new();
    idx.bm25_entities_evicted.store(true, Ordering::Relaxed);
    drop(guard);

    let results = search_task
        .await
        .expect("bm25_search task panicked")
        .expect("bm25_search returned Err");
    assert!(
        !results.is_empty(),
        "bm25_search must recover from a reclaim landing between its ensure() call and its \
         read-lock acquisition, not silently return an empty lexical lane"
    );
}

/// `grep_fallback_search` must recover from a reclaim landing strictly
/// between its `ensure_chunks_loaded()` call and its `chunks.read()`
/// acquisition — the same race as `bm25_search`, applied to the `chunks` map.
#[tokio::test]
#[serial_test::serial]
async fn grep_fallback_survives_reclaim_race_between_ensure_and_read() {
    let dir = tempfile::tempdir().unwrap();
    let redb_path = dir.path().join("index.redb");
    let idx = Arc::new(make_indexer_with_corpus(&redb_path));

    idx.index_files_batch(&[(
        "src/auth.rs".into(),
        "fn authenticate_user_via_token() {}".into(),
    )])
    .await
    .expect("index batch");
    assert!(
        !idx.chunks.read().await.is_empty(),
        "expected in-memory chunks after indexing"
    );

    let write_guard = idx.chunks.write().await;

    let idx_bg = Arc::clone(&idx);
    let search_task = tokio::spawn(async move {
        idx_bg
            .grep_fallback_search("authenticate_user_via_token", 5, None)
            .await
    });

    let_background_task_reach_its_read_lock().await;

    // Simulate the reclaim landing in the race window: clear the in-memory
    // chunk map and mark it evicted, exactly as `reclaim_memory_now` does.
    let mut guard = write_guard;
    guard.clear();
    idx.chunks_evicted.store(true, Ordering::Relaxed);
    drop(guard);

    let results = search_task
        .await
        .expect("grep_fallback_search task panicked");
    assert!(
        !results.is_empty(),
        "grep_fallback_search must recover from a reclaim landing between its ensure() call \
         and its read-lock acquisition, not silently return an empty fallback lane"
    );
}
