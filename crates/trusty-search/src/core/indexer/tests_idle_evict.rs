//! Idle-eviction tests for the BM25 corpus and per-file entity map (issue
//! #2162).
//!
//! Why: `evict_bm25_entities_if_idle` / `ensure_bm25_entities_loaded`
//! (`idle_evict.rs`) mirror the chunk-eviction pair in `mod.rs`, whose tests
//! already live in `tests.rs`. These tests live in their own file to keep
//! `tests.rs` under the 1500-SLOC test cap (same rationale as
//! `tests_cursor.rs`).
//! What: pins the eviction/rehydration state machine (zero threshold is a
//! no-op, a fresh index isn't idle, a forced eviction clears both structures
//! and sets the flag, a subsequent reader rehydrates and clears the flag) and
//! confirms `search()` returns identical results before eviction and after
//! idle eviction + lazy rehydration. A second test pins the no-durable-corpus
//! safety guard.
//! Test: this module.

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use super::{CodeIndexer, SearchQuery};
use crate::core::chunker::{ChunkType, RawChunk};
use crate::core::corpus::CorpusStore;
use crate::core::embed::{Embedder, MockEmbedder};
use crate::core::store::{UsearchStore, VectorStore};

/// Minimal in-memory `RawChunk` builder (mirrors `tests::raw`).
fn raw(id: &str, file: &str, content: &str) -> RawChunk {
    RawChunk {
        id: id.to_string(),
        file: file.to_string(),
        start_line: 1,
        end_line: 1 + content.lines().count(),
        content: content.to_string(),
        function_name: None,
        language: Some("rust".to_string()),
        chunk_type: ChunkType::Code,
        calls: Vec::new(),
        inherits_from: Vec::new(),
        chunk_depth: 0,
        parent_chunk_id: None,
        child_chunk_ids: Vec::new(),
        nlp_keywords: Vec::new(),
        nlp_code_refs: Vec::new(),
        virtual_terms: Vec::new(),
    }
}

/// Indexer with an embedder + HNSW store but no durable corpus (mirrors
/// `tests::make_indexer`).
fn make_indexer() -> CodeIndexer {
    let dim = 32;
    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(dim));
    let store: Arc<dyn VectorStore> = Arc::new(UsearchStore::new(dim).expect("usearch new"));
    CodeIndexer::new("test", "/tmp/test").with_components(embedder, store)
}

/// Indexer with a durable redb corpus but no embedder/store (BM25-only;
/// mirrors `tests::make_indexer_with_corpus`).
fn make_indexer_with_corpus(redb_path: &std::path::Path) -> CodeIndexer {
    let mut idx = CodeIndexer::new("bm25-evict-test", "/tmp/bm25-evict-test");
    let store = CorpusStore::open(redb_path).expect("open corpus store");
    idx.set_corpus_store(Arc::new(store));
    idx
}

/// Idle-eviction core behaviour for BM25 + entities — a durably-backed
/// indexer drops its in-memory BM25 corpus and per-file entity map once idle
/// past the threshold, and the next reader (entity lookup, then a full
/// `search()`) transparently rehydrates both from redb and returns identical
/// results to before eviction.
#[tokio::test]
async fn bm25_entities_idle_eviction_drops_and_lazily_rehydrates() {
    let dir = tempfile::tempdir().unwrap();
    let redb_path = dir.path().join("index.redb");
    let idx = make_indexer_with_corpus(&redb_path);

    // Populate two files; one carries a NamedType entity ("MyType") so we can
    // also exercise entity-exact-match rehydration, not just BM25.
    idx.index_files_batch(&[
        (
            "src/auth.rs".into(),
            "pub struct MyType { x: u32 }\nfn authenticate() {}".into(),
        ),
        ("src/token.rs".into(), "fn verify_token() {}".into()),
    ])
    .await
    .expect("index batch");

    let bm25_docs_before = idx.bm25.read().await.len();
    assert!(bm25_docs_before >= 2, "expected >= 2 BM25 documents");
    let entities_before = idx.entities.read().await.len();
    assert_eq!(entities_before, 2, "expected one entity-map entry per file");
    let entity_hit_before = idx.entity_exact_match("MyType").await;
    assert!(
        entity_hit_before.is_some(),
        "expected MyType entity before eviction"
    );

    let q = SearchQuery {
        text: "authenticate".to_string(),
        top_k: 5,
        expand_graph: false,
        compact: false,
        ..Default::default()
    };
    let results_before = idx.search(&q).await.expect("search before eviction");
    assert!(
        !results_before.is_empty(),
        "expected a BM25 hit before eviction"
    );

    // `search()` just called `touch_activity()`; sleep past millisecond
    // resolution so the "near-zero idle window" eviction below observes a
    // genuinely nonzero `idle_duration()` (it's tracked in milliseconds).
    tokio::time::sleep(Duration::from_millis(5)).await;

    // A zero threshold disables eviction — nothing is dropped.
    assert_eq!(idx.evict_bm25_entities_if_idle(Duration::ZERO).await, 0);
    assert_eq!(idx.bm25.read().await.len(), bm25_docs_before);

    // A long threshold means the index isn't idle yet (ingest calls
    // touch_activity) — nothing is dropped.
    assert_eq!(
        idx.evict_bm25_entities_if_idle(Duration::from_secs(3600))
            .await,
        0
    );
    assert_eq!(idx.bm25.read().await.len(), bm25_docs_before);

    // A near-zero idle window forces eviction now. The durable corpus is
    // wired, so this is safe.
    let evicted = idx
        .evict_bm25_entities_if_idle(Duration::from_nanos(1))
        .await;
    assert_eq!(
        evicted, bm25_docs_before,
        "eviction should drop every BM25 document"
    );
    assert_eq!(
        idx.bm25.read().await.len(),
        0,
        "BM25 must be empty after eviction"
    );
    assert_eq!(
        idx.entities.read().await.len(),
        0,
        "entities map must be empty after eviction"
    );
    assert!(
        idx.bm25_entities_evicted.load(Ordering::Relaxed),
        "bm25_entities_evicted flag must be set after eviction"
    );

    // The durable corpus is untouched — redb still has every chunk.
    assert!(idx.corpus_store().unwrap().chunk_count().unwrap() >= 2);

    // `entity_exact_match` lazily rehydrates BM25 + entities and returns the
    // same hit as before eviction.
    let entity_hit_after = idx.entity_exact_match("MyType").await;
    assert_eq!(
        entity_hit_after, entity_hit_before,
        "entity_exact_match must rehydrate and return the identical hit"
    );
    assert_eq!(
        idx.bm25.read().await.len(),
        bm25_docs_before,
        "BM25 must be repopulated after rehydration"
    );
    assert_eq!(
        idx.entities.read().await.len(),
        entities_before,
        "entities must be repopulated after rehydration"
    );
    assert!(
        !idx.bm25_entities_evicted.load(Ordering::Relaxed),
        "bm25_entities_evicted flag must clear after rehydration"
    );

    // Re-evict, then confirm a full `search()` call (which routes through
    // `bm25_search`) also rehydrates and returns identical results to the
    // pre-eviction search. Sleep past millisecond resolution again (see
    // above) so this forced eviction isn't skipped as "not idle yet".
    tokio::time::sleep(Duration::from_millis(5)).await;
    idx.evict_bm25_entities_if_idle(Duration::from_nanos(1))
        .await;
    assert_eq!(idx.bm25.read().await.len(), 0);
    let results_after = idx.search(&q).await.expect("search after eviction");
    assert_eq!(
        results_after.iter().map(|c| &c.id).collect::<Vec<_>>(),
        results_before.iter().map(|c| &c.id).collect::<Vec<_>>(),
        "search results must be identical before and after idle eviction + rehydration"
    );
    assert!(
        !idx.bm25_entities_evicted.load(Ordering::Relaxed),
        "search() must trigger rehydration via bm25_search"
    );
}

/// Idle-eviction safety for BM25 + entities — a BM25-only indexer (no
/// durable corpus) is NEVER evicted, mirroring
/// `tests::idle_eviction_skips_indexers_without_corpus` for chunks.
#[tokio::test]
async fn bm25_entities_idle_eviction_skips_indexers_without_corpus() {
    let idx = make_indexer(); // embedder + store, but corpus: None
    idx.add_chunk(raw("a", "src/a.rs", "fn a() {}"))
        .await
        .unwrap();
    let before = idx.bm25.read().await.len();
    assert!(before >= 1);

    // Even with an always-idle window, eviction is a no-op without a corpus.
    let evicted = idx
        .evict_bm25_entities_if_idle(Duration::from_nanos(1))
        .await;
    assert_eq!(evicted, 0, "must not evict without a durable corpus");
    assert_eq!(idx.bm25.read().await.len(), before);
    assert!(!idx.bm25_entities_evicted.load(Ordering::Relaxed));
}
