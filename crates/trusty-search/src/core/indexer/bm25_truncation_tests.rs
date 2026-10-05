//! #9235: `CodeIndexer::bm25_truncation` on the ingest and rehydrate paths.
//!
//! Why: the BM25 corpus cap silently dropped chunks the chunk cap admitted.
//! These tests pin that the report names the drop on both paths that fill
//! BM25, and reports nothing when the corpus fits.
//! What: each test runs in an isolated child process with
//! `TRUSTY_BM25_CORPUS_CAP` set at spawn (#6369 — an in-process `set_var`
//! truncates sibling tests' corpora), indexes ten one-function files, and
//! checks the three report fields.
//! Test: this module.

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use super::CodeIndexer;
use crate::core::corpus::CorpusStore;

const FILES: usize = 10;

fn make_indexer_with_corpus(redb_path: &std::path::Path) -> CodeIndexer {
    let mut idx = CodeIndexer::new("bm25-truncation-test", "/tmp/bm25-truncation-test");
    let store = CorpusStore::open(redb_path).expect("open corpus store");
    idx.set_corpus_store(Arc::new(store));
    idx
}

/// Index [`FILES`] one-function files and return the resident chunk count.
async fn ingest(idx: &CodeIndexer) -> usize {
    let files: Vec<(String, String)> = (0..FILES)
        .map(|i| {
            (
                format!("src/f{i:02}.rs"),
                format!("fn f{i:02}() {{ probe(); }}"),
            )
        })
        .collect();
    idx.index_files_batch(&files).await.expect("index batch");
    let chunks = idx.chunk_count();
    assert!(chunks >= FILES, "expected one chunk per file, got {chunks}");
    chunks
}

/// The report, which every test here expects to be computable.
async fn truncation(idx: &CodeIndexer) -> crate::core::bm25::Bm25Truncation {
    idx.bm25_truncation()
        .await
        .expect("the durable count is readable")
}

/// Evict both in-memory maps, then rehydrate them from redb.
async fn evict_and_rehydrate(idx: &CodeIndexer) {
    tokio::time::sleep(Duration::from_millis(5)).await;
    idx.evict_bm25_entities_if_idle(Duration::from_nanos(1))
        .await;
    idx.evict_chunks_if_idle(Duration::from_nanos(1)).await;
    assert!(idx.bm25_entities_evicted.load(Ordering::Relaxed));
    idx.ensure_bm25_entities_loaded().await;
    idx.ensure_chunks_loaded().await;
    for _ in 0..100 {
        if !idx.bm25_entities_evicted.load(Ordering::Relaxed)
            && !idx.chunks_evicted.load(Ordering::Relaxed)
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("rehydrate did not complete in time");
}

/// Over the cap, the ingest path reports every chunk BM25 refused.
#[tokio::test]
async fn ingest_over_the_bm25_cap_reports_truncation() {
    if !crate::service::test_isolation::run_isolated(
        "core::indexer::bm25_truncation_tests::ingest_over_the_bm25_cap_reports_truncation",
        &[("TRUSTY_BM25_CORPUS_CAP", "5")],
    ) {
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let idx = make_indexer_with_corpus(&dir.path().join("index.redb"));
    let chunks = ingest(&idx).await;

    let report = truncation(&idx).await;
    assert!(report.truncated, "{report:?}");
    assert_eq!(report.docs_dropped, (chunks - 5) as u64, "{report:?}");
    assert_eq!(report.corpus_cap, 5);
}

/// Under the cap, the ingest path reports no truncation.
#[tokio::test]
async fn ingest_under_the_bm25_cap_reports_no_truncation() {
    if !crate::service::test_isolation::run_isolated(
        "core::indexer::bm25_truncation_tests::ingest_under_the_bm25_cap_reports_no_truncation",
        &[("TRUSTY_BM25_CORPUS_CAP", "1000")],
    ) {
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let idx = make_indexer_with_corpus(&dir.path().join("index.redb"));
    ingest(&idx).await;

    let report = truncation(&idx).await;
    assert!(!report.truncated, "{report:?}");
    assert_eq!(report.docs_dropped, 0);
    assert_eq!(report.corpus_cap, 1000);
}

/// Over the cap, the report is right while evicted (predicted from redb) and
/// after the rehydrate rebuilds BM25 (measured).
#[tokio::test]
async fn rehydrate_over_the_bm25_cap_reports_truncation() {
    if !crate::service::test_isolation::run_isolated(
        "core::indexer::bm25_truncation_tests::rehydrate_over_the_bm25_cap_reports_truncation",
        &[("TRUSTY_BM25_CORPUS_CAP", "5")],
    ) {
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let idx = make_indexer_with_corpus(&dir.path().join("index.redb"));
    let chunks = ingest(&idx).await;

    tokio::time::sleep(Duration::from_millis(5)).await;
    idx.evict_bm25_entities_if_idle(Duration::from_nanos(1))
        .await;
    let evicted = truncation(&idx).await;
    assert!(
        evicted.truncated,
        "evicted BM25 must not read as complete: {evicted:?}"
    );
    assert_eq!(evicted.docs_dropped, (chunks - 5) as u64, "{evicted:?}");

    evict_and_rehydrate(&idx).await;
    let report = truncation(&idx).await;
    assert!(report.truncated, "{report:?}");
    assert_eq!(report.docs_dropped, (chunks - 5) as u64, "{report:?}");
    assert_eq!(report.corpus_cap, 5);
}

/// Under the cap, eviction and rehydrate report no truncation — an empty
/// evicted BM25 beside resident chunks is not a drop.
#[tokio::test]
async fn rehydrate_under_the_bm25_cap_reports_no_truncation() {
    if !crate::service::test_isolation::run_isolated(
        "core::indexer::bm25_truncation_tests::rehydrate_under_the_bm25_cap_reports_no_truncation",
        &[("TRUSTY_BM25_CORPUS_CAP", "1000")],
    ) {
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let idx = make_indexer_with_corpus(&dir.path().join("index.redb"));
    ingest(&idx).await;

    tokio::time::sleep(Duration::from_millis(5)).await;
    idx.evict_bm25_entities_if_idle(Duration::from_nanos(1))
        .await;
    let evicted = truncation(&idx).await;
    assert!(!evicted.truncated, "{evicted:?}");
    assert_eq!(evicted.docs_dropped, 0);

    evict_and_rehydrate(&idx).await;
    let report = truncation(&idx).await;
    assert!(!report.truncated, "{report:?}");
    assert_eq!(report.docs_dropped, 0);
    assert_eq!(report.corpus_cap, 1000);
}

/// A reclaim that has emptied BM25 but not yet flagged it — the state a reader
/// sees when the reclaim lands between its chunk-map and BM25 reads — must not
/// read as truncation on an under-cap index. Holding the rehydrate-generation
/// lock parks the eviction exactly there: it empties BM25, then waits on that
/// lock to set `bm25_entities_evicted`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reclaim_between_the_reads_does_not_report_truncation_under_the_cap() {
    if !crate::service::test_isolation::run_isolated(
        "core::indexer::bm25_truncation_tests::a_reclaim_between_the_reads_does_not_report_truncation_under_the_cap",
        &[("TRUSTY_BM25_CORPUS_CAP", "1000")],
    ) {
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let idx = Arc::new(make_indexer_with_corpus(&dir.path().join("index.redb")));
    ingest(&idx).await;
    tokio::time::sleep(Duration::from_millis(5)).await;

    let generation = Arc::clone(&idx.rehydrate_generation);
    let (locked_tx, locked_rx) = tokio::sync::oneshot::channel();
    let (release, release_rx) = std::sync::mpsc::channel::<()>();
    let holder = std::thread::spawn(move || {
        let _held = generation.lock().unwrap_or_else(|e| e.into_inner());
        let _ = locked_tx.send(());
        let _ = release_rx.recv();
    });
    locked_rx.await.expect("generation lock taken");

    let evicting = Arc::clone(&idx);
    let evict = tokio::spawn(async move {
        evicting
            .evict_bm25_entities_if_idle(Duration::from_nanos(1))
            .await
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        while !idx.bm25.try_read().is_ok_and(|b| b.is_empty()) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the eviction emptied BM25");
    assert!(
        !idx.bm25_entities_evicted.load(Ordering::Relaxed),
        "the eviction must still be parked before its flag"
    );
    assert!(idx.chunk_count() > 0, "the chunk map stays resident");

    let report = truncation(&idx).await;
    let _ = release.send(());
    holder.join().expect("generation holder");
    assert!(evict.await.expect("evict") > 0);
    assert!(
        !report.truncated,
        "a reclaim in flight read as a drop: {report:?}"
    );
    assert_eq!(report.docs_dropped, 0, "{report:?}");
}
