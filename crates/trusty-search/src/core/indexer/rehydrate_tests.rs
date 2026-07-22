//! Detached, deduplicated corpus-rehydrate tests (issue #3683 slice 1,
//! #3684).
//!
//! Why: split out of `tests_idle_evict.rs` to keep that file under the
//! 500-SLOC production cap (`tests_idle_evict.rs`'s basename doesn't match
//! the line-cap script's test-file naming patterns, so it is — somewhat
//! surprisingly — classified as a production file; this file's name ends in
//! `_tests.rs`, which does match, giving it the 1500-SLOC test-file budget
//! instead). Same rationale as `search/lanes_tests.rs`'s own split.
//! What: `detached_rehydrate_survives_caller_cancellation` (the core
//! regression test — a query-timeout-style cancellation of the caller's
//! future must never discard the rehydrate's completed work),
//! `rehydrate_dedupes_concurrent_callers_onto_one_scan`, and
//! `rehydrate_is_deterministic_across_repeated_cycles_over_cap` (issue
//! #3684's determinism guarantee).
//! Test: this module.

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use super::{CodeIndexer, SearchQuery};
use crate::core::corpus::CorpusStore;

/// Indexer with a durable redb corpus but no embedder/store (BM25-only;
/// mirrors `tests_idle_evict::make_indexer_with_corpus` /
/// `search::lanes_tests::make_indexer_with_corpus` — duplicated per that
/// same sibling-test-file convention rather than exposed cross-file).
fn make_indexer_with_corpus(redb_path: &std::path::Path) -> CodeIndexer {
    let mut idx = CodeIndexer::new("rehydrate-test", "/tmp/rehydrate-test");
    let store = CorpusStore::open(redb_path).expect("open corpus store");
    idx.set_corpus_store(Arc::new(store));
    idx
}

// ── Issue #3683 slice 1: detached, deduplicated rehydrate ─────────────────

/// The CORE regression test for issue #3683: a query-timeout-style
/// cancellation of the CALLER's future must never discard the rehydrate's
/// completed work.
///
/// Reproduces the exact production livelock shape: wrap the query in an
/// OUTER `tokio::time::timeout` — mirroring
/// `service::query_timeout::apply_query_timeout`'s middleware, which cancels
/// (drops) the whole handler future on expiry — shorter than an artificially
/// slowed rehydrate scan (standing in for the production 27-40s NFS scan on
/// a 315K-chunk index). Asserts the query itself times out (simulating the
/// 408), then proves the detached rehydrate task survived that cancellation:
/// both `*_evicted` flags clear and the chunk map + BM25 populate strictly
/// AFTER the caller's timeout fired, and a second, immediate query hits
/// already-warm data with no re-scan.
#[tokio::test]
#[serial_test::serial]
async fn detached_rehydrate_survives_caller_cancellation() {
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

    tokio::time::sleep(Duration::from_millis(5)).await;
    idx.evict_bm25_entities_if_idle(Duration::from_nanos(1))
        .await;
    idx.evict_chunks_if_idle(Duration::from_nanos(1)).await;
    assert!(idx.bm25_entities_evicted.load(Ordering::Relaxed));
    assert!(idx.chunks_evicted.load(Ordering::Relaxed));
    assert_eq!(idx.bm25.read().await.len(), 0);

    // Simulate the production NFS scan with a short, test-scale delay — long
    // enough to still be running past the caller's own much shorter timeout.
    super::idle_evict::TEST_REHYDRATE_DELAY_MS.store(300, Ordering::Relaxed);

    // Mirror `apply_query_timeout`: wrap the WHOLE query in an outer timeout
    // that cancels (drops) the handler future on expiry — this is exactly
    // what discarded the rehydrate's work before this fix. Goes through the
    // fully public `search()` entry point (not the private `bm25_search`
    // lane directly) so this reproduces the real HTTP query path; BM25 has
    // non-empty hits here so `grep_fallback_search` never fires and doesn't
    // muddy the BM25-specific assertions below.
    let q = SearchQuery {
        text: "authenticate".to_string(),
        top_k: 5,
        expand_graph: false,
        compact: false,
        ..Default::default()
    };
    let idx_bg = Arc::clone(&idx);
    let q_bg = q.clone();
    let outer = tokio::time::timeout(Duration::from_millis(50), async move {
        idx_bg.search(&q_bg).await
    })
    .await;
    assert!(
        outer.is_err(),
        "the caller must time out (simulates the query-timeout middleware's 408), \
         not wait out the full artificial rehydrate delay"
    );

    // Give the detached task enough real wall-clock time to actually finish
    // (it sleeps ~300ms plus real scan overhead; the caller gave up at 50ms).
    tokio::time::sleep(Duration::from_millis(700)).await;

    assert!(
        !idx.bm25_entities_evicted.load(Ordering::Relaxed),
        "the detached rehydrate must clear bm25_entities_evicted AFTER the timed-out \
         caller returned — proving it survived cancellation (issue #3683 core fix)"
    );
    assert!(
        !idx.chunks_evicted.load(Ordering::Relaxed),
        "the detached rehydrate must also clear chunks_evicted (consolidated scan)"
    );
    assert!(
        idx.bm25.read().await.len() >= 2,
        "BM25 must be repopulated by the detached task despite the caller's cancellation"
    );
    assert!(
        idx.in_memory_chunk_count().await >= 2,
        "the chunk map must also be repopulated by the SAME consolidated scan"
    );

    // A second, immediate query must hit already-warm data with NO re-scan.
    // Reset the artificial delay first so a genuine re-scan (a regression)
    // would be slow and provably caught, not accidentally fast.
    super::idle_evict::TEST_REHYDRATE_DELAY_MS.store(0, Ordering::Relaxed);
    let start = std::time::Instant::now();
    let results = idx
        .search(&q)
        .await
        .expect("search must succeed on warm data");
    assert!(!results.is_empty(), "expected a warm-data hit");
    assert!(
        start.elapsed() < Duration::from_millis(100),
        "a query against already-rehydrated data must not re-scan; took {:?}",
        start.elapsed()
    );
}

/// Issue #3683 slice 1: concurrent callers must dedupe onto ONE detached
/// scan rather than each independently re-triggering their own rehydrate.
///
/// Why deterministic via timing (not a scan counter): `Bm25Index`/
/// `CorpusStore` don't expose a scan-count hook, so this pins the
/// OBSERVABLE contract instead — 5 concurrent callers racing a slow rehydrate
/// all complete close to the duration of ONE scan, not N serialized scans.
#[tokio::test]
#[serial_test::serial]
async fn rehydrate_dedupes_concurrent_callers_onto_one_scan() {
    let dir = tempfile::tempdir().unwrap();
    let redb_path = dir.path().join("index.redb");
    let idx = Arc::new(make_indexer_with_corpus(&redb_path));

    idx.index_files_batch(&[("src/auth.rs".into(), "fn authenticate() {}".into())])
        .await
        .expect("index batch");

    tokio::time::sleep(Duration::from_millis(5)).await;
    idx.evict_bm25_entities_if_idle(Duration::from_nanos(1))
        .await;
    idx.evict_chunks_if_idle(Duration::from_nanos(1)).await;

    super::idle_evict::TEST_REHYDRATE_DELAY_MS.store(200, Ordering::Relaxed);

    let q = SearchQuery {
        text: "authenticate".to_string(),
        top_k: 5,
        expand_graph: false,
        compact: false,
        ..Default::default()
    };
    let start = std::time::Instant::now();
    let mut tasks = Vec::new();
    for _ in 0..5 {
        let idx_bg = Arc::clone(&idx);
        let q_bg = q.clone();
        tasks.push(tokio::spawn(async move { idx_bg.search(&q_bg).await }));
    }
    for t in tasks {
        let r = t.await.expect("task panicked").expect("search errored");
        assert!(!r.is_empty(), "every caller must see the rehydrated hit");
    }
    super::idle_evict::TEST_REHYDRATE_DELAY_MS.store(0, Ordering::Relaxed);

    // One ~200ms scan shared by all 5 callers finishes well under 700ms even
    // with scheduling overhead; 5 INDEPENDENT (non-deduped) scans serialized
    // behind the same locks would take roughly 5x that.
    assert!(
        start.elapsed() < Duration::from_millis(700),
        "5 concurrent callers must dedupe onto one scan, not each re-trigger their own; \
         took {:?}",
        start.elapsed()
    );
}

/// Issue #3684: rehydrating the same over-cap BM25 corpus twice — via two
/// separate evict/rehydrate cycles — must yield the IDENTICAL searchable doc
/// set both times, and that set must be the sorted-by-id prefix, not
/// whatever redb's B-tree iteration order happens to produce.
#[tokio::test]
#[serial_test::serial]
async fn rehydrate_is_deterministic_across_repeated_cycles_over_cap() {
    let prev_cap = std::env::var("TRUSTY_BM25_CORPUS_CAP").ok();
    unsafe { std::env::set_var("TRUSTY_BM25_CORPUS_CAP", "5") };

    let dir = tempfile::tempdir().unwrap();
    let redb_path = dir.path().join("index.redb");
    let idx = make_indexer_with_corpus(&redb_path);

    // 10 tiny one-function files sharing a common lexical probe term so a
    // single BM25 query can enumerate every SURVIVING doc id directly,
    // rather than needing a doc-id-enumeration API on `Bm25Index`.
    let files: Vec<(String, String)> = (0..10)
        .map(|i| {
            (
                format!("src/f{i:02}.rs"),
                format!("fn f{i:02}() {{ /* shared_probe_term */ }}"),
            )
        })
        .collect();
    idx.index_files_batch(&files).await.expect("index batch");
    let total_chunks = idx.corpus_store().unwrap().chunk_count().unwrap();
    assert!(
        total_chunks >= 8,
        "expected close to one chunk per file, got {total_chunks}"
    );

    async fn wait_for_rehydrate(idx: &CodeIndexer) {
        idx.ensure_bm25_entities_loaded().await;
        for _ in 0..100 {
            if !idx.bm25_entities_evicted.load(Ordering::Relaxed) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("rehydrate did not complete in time");
    }

    // Calls the BM25 lane directly (`bm25_search`, `pub(crate)`) rather than
    // the full `search()` pipeline — `search()` also runs
    // `grep_fallback_search` as an unconditional third RRF lane for
    // `Definition`-classified queries (issue #75), which scans the
    // (uncapped) in-memory chunk map and would leak every chunk id into the
    // fused result regardless of the BM25 cap, defeating this test's whole
    // premise.
    async fn surviving_bm25_ids(idx: &CodeIndexer) -> std::collections::BTreeSet<String> {
        idx.bm25_search("shared_probe_term", 50, None)
            .await
            .expect("bm25_search")
            .into_iter()
            .map(|(id, _)| id)
            .collect()
    }

    // Cycle 1: evict, rehydrate, record the surviving set.
    tokio::time::sleep(Duration::from_millis(5)).await;
    idx.evict_bm25_entities_if_idle(Duration::from_nanos(1))
        .await;
    idx.evict_chunks_if_idle(Duration::from_nanos(1)).await;
    wait_for_rehydrate(&idx).await;
    let ids_first = surviving_bm25_ids(&idx).await;
    assert_eq!(
        ids_first.len(),
        5,
        "the cap must still apply to a rehydrated corpus; got {ids_first:?}"
    );

    // Cycle 2: evict + rehydrate again.
    tokio::time::sleep(Duration::from_millis(5)).await;
    idx.evict_bm25_entities_if_idle(Duration::from_nanos(1))
        .await;
    idx.evict_chunks_if_idle(Duration::from_nanos(1)).await;
    wait_for_rehydrate(&idx).await;
    let ids_second = surviving_bm25_ids(&idx).await;

    assert_eq!(
        ids_first, ids_second,
        "the surviving BM25 subset must be IDENTICAL across repeated evict/rehydrate \
         cycles over the cap (issue #3684)"
    );

    // And it must be the sorted-by-id PREFIX of the full corpus, not an
    // arbitrary redb-iteration-order subset.
    let mut all_ids: Vec<String> = idx
        .corpus_store()
        .unwrap()
        .load_all_chunks()
        .unwrap()
        .into_iter()
        .map(|c| c.id)
        .collect();
    all_ids.sort();
    let expected_prefix: std::collections::BTreeSet<String> = all_ids.into_iter().take(5).collect();
    assert_eq!(
        ids_first, expected_prefix,
        "the surviving subset must be the sorted-by-id prefix, not an arbitrary \
         redb-iteration-order subset (issue #3684)"
    );

    match prev_cap {
        Some(v) => unsafe { std::env::set_var("TRUSTY_BM25_CORPUS_CAP", v) },
        None => unsafe { std::env::remove_var("TRUSTY_BM25_CORPUS_CAP") },
    }
}
