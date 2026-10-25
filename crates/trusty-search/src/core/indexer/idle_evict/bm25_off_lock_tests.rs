//! BM25 rebuilt off the query path after idle eviction (#10193).
//!
//! Why: a rehydrate held the BM25 write guard for its whole build, so a cold
//! query blocked behind it for as long as the build ran.
//! What: a query during a slowed BM25 build answers within the query wait,
//! flagged degraded, and the flag clears at commit; the rebuilt index holds
//! the pre-eviction doc set; a write landing during the build survives the
//! swap.
//! Test: this module.

use std::collections::BTreeSet;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::super::{CodeIndexer, SearchQuery};
use super::Bm25BuildDelayArmed;
use crate::core::corpus::CorpusStore;
use crate::core::embed::{Embedder, MockEmbedder};
use crate::core::store::{UsearchStore, VectorStore};

/// Hybrid indexer: mock embedder, HNSW store and a durable redb corpus.
fn hybrid_indexer(redb_path: &std::path::Path) -> CodeIndexer {
    let dim = 32;
    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(dim));
    let store: Arc<dyn VectorStore> = Arc::new(UsearchStore::new(dim).expect("usearch new"));
    let mut idx =
        CodeIndexer::new("bm25-off-lock", "/tmp/bm25-off-lock").with_components(embedder, store);
    idx.set_corpus_store(Arc::new(CorpusStore::open(redb_path).expect("open corpus")));
    idx
}

/// BM25-only indexer with a durable redb corpus.
fn corpus_indexer(redb_path: &std::path::Path) -> CodeIndexer {
    let mut idx = CodeIndexer::new("bm25-off-lock-lex", "/tmp/bm25-off-lock-lex");
    idx.set_corpus_store(Arc::new(CorpusStore::open(redb_path).expect("open corpus")));
    idx
}

async fn evict_all(idx: &CodeIndexer) {
    tokio::time::sleep(Duration::from_millis(5)).await;
    idx.evict_bm25_entities_if_idle(Duration::from_nanos(1))
        .await;
    idx.evict_chunks_if_idle(Duration::from_nanos(1)).await;
    assert!(idx.corpus_evicted(), "precondition: the corpus is evicted");
}

/// Poll until the detached rehydrate has committed.
async fn wait_for_commit(idx: &CodeIndexer) {
    for _ in 0..500 {
        if !idx.corpus_evicted() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("the rehydrate did not commit within 10 s");
}

/// Every chunk id BM25 returns for `term`.
async fn bm25_ids(idx: &CodeIndexer, term: &str) -> BTreeSet<String> {
    idx.bm25_search(term, 1_000, None)
        .await
        .expect("bm25_search")
        .into_iter()
        .map(|(id, _)| id)
        .collect()
}

fn hybrid_query(text: &str) -> SearchQuery {
    SearchQuery {
        text: text.to_string(),
        top_k: 5,
        expand_graph: false,
        compact: false,
        ..Default::default()
    }
}

/// #10193: a hybrid query that lands while the BM25 build runs answers within
/// the query wait from the vector lane, flagged degraded; the flag clears at
/// commit and the next query is lexical again.
#[tokio::test]
#[serial_test::serial]
async fn a_query_during_a_slow_bm25_build_answers_degraded_within_the_wait() {
    let dir = tempfile::tempdir().unwrap();
    let idx = Arc::new(hybrid_indexer(&dir.path().join("index.redb")));
    idx.index_files_batch(&[
        ("src/auth.rs".into(), "fn authenticate() {}".into()),
        ("src/token.rs".into(), "fn verify_token() {}".into()),
    ])
    .await
    .expect("index batch");
    evict_all(&idx).await;
    let _slow = Bm25BuildDelayArmed::arm(&idx, Duration::from_millis(4_000));

    let started = Instant::now();
    let idx_q = Arc::clone(&idx);
    let answered = tokio::time::timeout(Duration::from_millis(3_500), async move {
        idx_q.search(&hybrid_query("authenticate")).await
    })
    .await;
    let elapsed = started.elapsed();
    let results = answered
        .expect("a query during the BM25 build must not wait out the build")
        .expect("search");
    assert!(
        elapsed < Duration::from_millis(2_000),
        "the query must answer within the short rehydrate wait, took {elapsed:?}"
    );
    assert!(
        !results.is_empty(),
        "the vector lane answers while BM25 rebuilds"
    );
    assert!(idx.lane_degraded(), "the answer must be flagged degraded");
    assert!(idx.corpus_evicted(), "the build is still running");

    wait_for_commit(&idx).await;
    assert!(!idx.lane_degraded(), "the commit clears the degraded flag");
    let warm = idx
        .search(&hybrid_query("authenticate"))
        .await
        .expect("search");
    assert!(!warm.is_empty());
    assert!(!idx.lane_degraded(), "a warm query is not degraded");
    assert!(
        !bm25_ids(&idx, "authenticate").await.is_empty(),
        "BM25 answers after the commit"
    );
}

/// #10193: the off-lock build publishes exactly the doc set BM25 held before
/// eviction — no document dropped or added by the swap.
#[tokio::test]
#[serial_test::serial]
async fn rehydrated_bm25_holds_the_pre_eviction_doc_set() {
    let dir = tempfile::tempdir().unwrap();
    let idx = corpus_indexer(&dir.path().join("index.redb"));
    let files: Vec<(String, String)> = (0..12)
        .map(|i| {
            (
                format!("src/m{i:02}.rs"),
                format!("fn m{i:02}() {{ /* doc_set_probe */ }}"),
            )
        })
        .collect();
    idx.index_files_batch(&files).await.expect("index batch");
    let before = bm25_ids(&idx, "doc_set_probe").await;
    let durable = idx.corpus_store().unwrap().chunk_count().unwrap();
    assert_eq!(
        before.len(),
        durable,
        "precondition: every chunk is in BM25"
    );
    let len_before = idx.bm25.read().await.len();

    evict_all(&idx).await;
    assert_eq!(idx.bm25.read().await.len(), 0);
    drop(idx.begin_corpus_rehydrate());
    wait_for_commit(&idx).await;

    assert_eq!(
        bm25_ids(&idx, "doc_set_probe").await,
        before,
        "the rebuilt BM25 must hold the pre-eviction doc set"
    );
    assert_eq!(idx.bm25.read().await.len(), len_before);
    assert!(!idx.bm25_entities_evicted.load(Ordering::Relaxed));
}

/// #10193: a write that reaches the live BM25 index while the off-lock build
/// runs is kept — the commit merges instead of swapping it away.
#[tokio::test]
#[serial_test::serial]
async fn a_write_landing_during_the_bm25_build_survives_the_swap() {
    let dir = tempfile::tempdir().unwrap();
    let idx = corpus_indexer(&dir.path().join("index.redb"));
    idx.index_files_batch(&[("src/auth.rs".into(), "fn authenticate() {}".into())])
        .await
        .expect("index batch");
    evict_all(&idx).await;
    let slow = Bm25BuildDelayArmed::arm(&idx, Duration::from_millis(600));
    drop(idx.begin_corpus_rehydrate());
    tokio::time::sleep(Duration::from_millis(150)).await;
    idx.bm25
        .write()
        .await
        .upsert_document("src/late.rs:1:1", "late_writer_term");
    wait_for_commit(&idx).await;
    drop(slow);

    assert!(
        bm25_ids(&idx, "late_writer_term")
            .await
            .contains("src/late.rs:1:1"),
        "a write landing during the build must survive the commit"
    );
    assert!(
        !bm25_ids(&idx, "authenticate").await.is_empty(),
        "the rehydrated documents are present too"
    );
}
