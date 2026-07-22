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

use super::{CodeIndexer, ParsedBatch, SearchQuery};
use crate::core::chunker::{ChunkType, RawChunk};
use crate::core::corpus::CorpusStore;
use crate::core::embed::{Embedder, MockEmbedder};
use crate::core::entity::{EdgeKind, EntityType, RawEntity};
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

/// `RawChunk` builder with a `function_name`/`calls`/`chunk_type`, for
/// symbol-graph-relevant tests (mirrors `symbol_graph::tests::chunk_full`).
fn raw_named(id: &str, file: &str, name: &str, calls: &[&str], chunk_type: ChunkType) -> RawChunk {
    let mut c = raw(id, file, &format!("fn {name}() {{}}"));
    c.function_name = Some(name.to_string());
    c.calls = calls.iter().map(|s| s.to_string()).collect();
    c.chunk_type = chunk_type;
    c
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

/// Regression test (QA follow-up on #2162): `rebuild_symbol_graph` must
/// rehydrate an idle-evicted entity map BEFORE snapshotting it, or a rebuild
/// triggered by an unrelated mutation (`remove_file`, `remove_chunk`, a
/// prune-only reindex, contrib-graph ingest — none of which rehydrate first)
/// would silently persist a graph missing every entity-derived KG edge
/// (`Documents`, `ReferencesConcept`, ...) for the WHOLE corpus, not just the
/// touched file — and that impoverished graph survives a restart because it
/// gets written to redb.
///
/// Reproduces via the exact vulnerable path: build a graph with a
/// `Documents` edge (owner symbol "prose_owner" -> "target", wired from a
/// `DocConcept` entity on "src/owner.rs"), force-evict BM25/entities, then
/// call `remove_file` on a *third, unrelated* file — the same call path
/// `service/reconcile.rs` and the delete HTTP handler use. Without the
/// `ensure_bm25_entities_loaded()` guard at the top of `rebuild_symbol_graph`,
/// this removal would rebuild the graph from an empty entity snapshot and the
/// `Documents` edge would vanish even though `src/owner.rs` was never touched.
#[tokio::test]
async fn rebuild_symbol_graph_rehydrates_entities_after_idle_eviction() {
    let dir = tempfile::tempdir().unwrap();
    let redb_path = dir.path().join("index.redb");
    let idx = make_indexer_with_corpus(&redb_path);

    let chunks = vec![
        raw_named(
            "owner",
            "src/owner.rs",
            "prose_owner",
            &[],
            ChunkType::Function,
        ),
        raw_named(
            "target",
            "src/target.rs",
            "target",
            &[],
            ChunkType::Function,
        ),
        raw_named(
            "unrelated",
            "src/unrelated.rs",
            "unrelated_fn",
            &[],
            ChunkType::Function,
        ),
    ];
    let entities_by_file = vec![(
        "src/owner.rs".to_string(),
        vec![RawEntity::new(
            EntityType::DocConcept,
            "target".to_string(),
            (0, 6),
            "src/owner.rs",
            1,
        )],
    )];
    let parsed = ParsedBatch {
        embeddings: vec![None; chunks.len()],
        chunks,
        entities_by_file,
        parse_ms: 0,
        embed_ms: 0,
        vector_count: 0,
    };
    // defer_graph_rebuild=false: builds the graph immediately, exercising the
    // same commit path `index_files_batch` uses.
    idx.commit_parsed_batch(parsed, false)
        .await
        .expect("commit batch");

    // Sanity: the Documents edge exists before any eviction.
    let g_before = idx.snapshot_symbol_graph().await;
    let docs_before = g_before.neighbors_by_edge("prose_owner", &[EdgeKind::Documents], 1);
    assert!(
        docs_before.iter().any(|(n, _, _)| n == "target"),
        "expected a Documents edge prose_owner -> target before eviction, got {docs_before:?}"
    );

    // Force BM25/entities eviction (sleep past ms resolution — see the other
    // tests in this file for why).
    tokio::time::sleep(Duration::from_millis(5)).await;
    let evicted = idx
        .evict_bm25_entities_if_idle(Duration::from_nanos(1))
        .await;
    assert!(evicted > 0, "expected eviction to actually clear BM25 docs");
    assert_eq!(
        idx.entities.read().await.len(),
        0,
        "entities must be empty immediately after eviction"
    );
    assert!(idx.bm25_entities_evicted.load(Ordering::Relaxed));

    // Trigger a graph rebuild via `remove_file` on an *unrelated* file — the
    // exact production path (`service/reconcile.rs`, the delete HTTP
    // handler, and the FSEvents watcher's `remove_chunk`) that does NOT
    // rehydrate entities before reaching `rebuild_symbol_graph`.
    idx.remove_file("src/unrelated.rs")
        .await
        .expect("remove unrelated file");

    // The guard inside `rebuild_symbol_graph` must have rehydrated entities
    // before snapshotting, so the untouched owner/target Documents edge must
    // still be present.
    let g_after = idx.snapshot_symbol_graph().await;
    let docs_after = g_after.neighbors_by_edge("prose_owner", &[EdgeKind::Documents], 1);
    assert!(
        docs_after.iter().any(|(n, _, _)| n == "target"),
        "Documents edge prose_owner -> target was DROPPED after remove_file on an \
         unrelated file post-eviction — rebuild_symbol_graph rebuilt from an empty \
         entity snapshot instead of rehydrating first; got {docs_after:?}"
    );
    assert!(
        !idx.bm25_entities_evicted.load(Ordering::Relaxed),
        "rebuild_symbol_graph must rehydrate (and clear the flag) via \
         ensure_bm25_entities_loaded"
    );
}

/// HNSW idle re-view (issue #2164): a clean, promoted (write-touched)
/// `UsearchStore` wired into a `CodeIndexer` gets demoted back to mmap-view
/// mode by the same idle sweep that drives chunk/BM25/entity eviction, and
/// search returns identical results before and after.
#[tokio::test]
async fn hnsw_idle_demotion_reviews_clean_promoted_store() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hnsw.usearch");
    let dim = 32;

    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(dim));
    // Keep a concrete handle alongside the `dyn VectorStore` the indexer
    // holds so the test can assert `in_view_mode()` directly, mirroring how
    // production code's `store: Option<Arc<dyn VectorStore>>` and a
    // `UsearchStore`-typed warm-boot handle are the same underlying object.
    let usearch = Arc::new(UsearchStore::new(dim).expect("usearch new"));
    let store: Arc<dyn VectorStore> = usearch.clone();
    let idx = CodeIndexer::new("hnsw-demote-test", "/tmp/hnsw-demote-test")
        .with_components(embedder, store);

    idx.add_chunk(raw("a", "src/a.rs", "fn a() {}"))
        .await
        .expect("add chunk a");
    idx.add_chunk(raw("b", "src/b.rs", "fn b() {}"))
        .await
        .expect("add chunk b");
    assert!(
        !usearch.in_view_mode(),
        "a freshly built, never-loaded store starts mutable"
    );

    // Flush to disk — this is what `spawn_incremental_persist` /
    // `force_incremental_persist` do in production, and (issue #2164) is
    // also what makes a `new()`-built store demotion-eligible by recording
    // `hnsw_path` and clearing `dirty`.
    idx.save_vector_store(&path).await.expect("save hnsw");

    let q = SearchQuery {
        text: "fn a".to_string(),
        top_k: 5,
        expand_graph: false,
        compact: false,
        ..Default::default()
    };
    let results_before = idx.search(&q).await.expect("search before demotion");
    assert!(!results_before.is_empty(), "expected at least one hit");

    // `search()` touches activity — sleep past ms resolution (see the BM25
    // idle tests above) so the near-zero threshold below observes genuine
    // idleness rather than skipping as "just active".
    tokio::time::sleep(Duration::from_millis(5)).await;

    // Zero threshold disables the sweep — no-op.
    assert!(!idx.demote_vector_store_if_idle(Duration::ZERO).await);
    assert!(!usearch.in_view_mode());

    // A long threshold means "not idle yet" — no-op.
    assert!(
        !idx.demote_vector_store_if_idle(Duration::from_secs(3600))
            .await
    );
    assert!(!usearch.in_view_mode());

    // Near-zero idle window: demotion fires.
    assert!(
        idx.demote_vector_store_if_idle(Duration::from_nanos(1))
            .await,
        "expected the idle sweep to demote the clean, promoted store"
    );
    assert!(
        usearch.in_view_mode(),
        "store must be back in mmap-view mode after the idle sweep"
    );

    // Search after demotion returns identical results — no vectors lost.
    let results_after = idx.search(&q).await.expect("search after demotion");
    assert_eq!(
        results_after.iter().map(|c| &c.id).collect::<Vec<_>>(),
        results_before.iter().map(|c| &c.id).collect::<Vec<_>>(),
        "search results must be identical before and after HNSW re-view"
    );

    // A write after demotion re-promotes (view -> mutable -> view -> mutable
    // is sound); search still finds the new chunk.
    idx.add_chunk(raw("c", "src/c.rs", "fn c() {}"))
        .await
        .expect("add chunk c after demotion");
    assert!(
        !usearch.in_view_mode(),
        "write after demotion must re-promote to mutable"
    );
}

/// The `TRUSTY_HNSW_REVIEW_IDLE=0` escape hatch disables demotion while
/// leaving the store otherwise untouched (issue #2164).
#[tokio::test]
async fn hnsw_idle_demotion_skips_when_disabled_via_env() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hnsw.usearch");
    let dim = 32;

    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(dim));
    let usearch = Arc::new(UsearchStore::new(dim).expect("usearch new"));
    let store: Arc<dyn VectorStore> = usearch.clone();
    let idx = CodeIndexer::new(
        "hnsw-demote-disabled-test",
        "/tmp/hnsw-demote-disabled-test",
    )
    .with_components(embedder, store);

    idx.add_chunk(raw("a", "src/a.rs", "fn a() {}"))
        .await
        .expect("add chunk a");
    idx.save_vector_store(&path).await.expect("save hnsw");
    tokio::time::sleep(Duration::from_millis(5)).await;

    let prior = std::env::var("TRUSTY_HNSW_REVIEW_IDLE").ok();
    // SAFETY: this test is the only reader/writer of this env var while it runs.
    unsafe { std::env::set_var("TRUSTY_HNSW_REVIEW_IDLE", "0") };

    assert!(
        !idx.demote_vector_store_if_idle(Duration::from_nanos(1))
            .await,
        "TRUSTY_HNSW_REVIEW_IDLE=0 must disable demotion"
    );
    assert!(
        !usearch.in_view_mode(),
        "store must remain mutable while the gate is disabled"
    );

    // SAFETY: see above.
    unsafe {
        match prior {
            Some(v) => std::env::set_var("TRUSTY_HNSW_REVIEW_IDLE", v),
            None => std::env::remove_var("TRUSTY_HNSW_REVIEW_IDLE"),
        }
    }

    // Re-enabled (default): demotion now proceeds.
    assert!(
        idx.demote_vector_store_if_idle(Duration::from_nanos(1))
            .await,
        "demotion must proceed once the gate is re-enabled"
    );
    assert!(usearch.in_view_mode());
}

/// `reclaim_memory_now` force-clears chunks + BM25 + entities **regardless of
/// idle state**, and every reader lazily rehydrates from the durable corpus.
///
/// Why: this is the steady-state memory-limit enforcement primitive (issue
/// #2846). Unlike the idle-evict path, it must reclaim an index that was
/// active microseconds ago — a daemon under real memory pressure cannot wait
/// for the idle window. The reclaim must nonetheless be non-destructive:
/// search results after reclaim must match results before.
/// What: index two files, snapshot BM25/entity/chunk state, call
/// `reclaim_memory_now()` with the index freshly active (no idle wait), assert
/// every in-memory structure is emptied and the reclaim count covers the BM25
/// docs, then confirm a search rehydrates and returns the same hit.
/// Test: this test.
#[tokio::test]
async fn memory_pressure_reclaim_now_clears_caches() {
    let dir = tempfile::tempdir().unwrap();
    let redb_path = dir.path().join("index.redb");
    let idx = make_indexer_with_corpus(&redb_path);

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
    assert!(
        !idx.entities.read().await.is_empty(),
        "expected entity-map entries before reclaim"
    );

    // The index is active RIGHT NOW (ingest just called touch_activity), so an
    // idle-evict would be a no-op — but pressure reclaim must fire regardless.
    assert_eq!(
        idx.evict_bm25_entities_if_idle(Duration::from_secs(3600))
            .await,
        0,
        "idle-evict must NOT fire on an active index"
    );

    let reclaimed = idx.reclaim_memory_now().await;
    assert!(
        reclaimed >= bm25_docs_before,
        "reclaim count ({reclaimed}) should cover the {bm25_docs_before} BM25 documents"
    );
    assert_eq!(
        idx.bm25.read().await.len(),
        0,
        "BM25 must be empty after reclaim"
    );
    assert_eq!(
        idx.entities.read().await.len(),
        0,
        "entities map must be empty after reclaim"
    );
    assert_eq!(
        idx.in_memory_chunk_count().await,
        0,
        "in-memory chunk map must be empty after reclaim"
    );
    assert!(
        idx.bm25_entities_evicted.load(Ordering::Relaxed),
        "bm25_entities_evicted flag must be set after reclaim"
    );

    // Non-destructive: a search after reclaim rehydrates from redb and still
    // returns the hit.
    let q = SearchQuery {
        text: "authenticate".to_string(),
        top_k: 5,
        expand_graph: false,
        compact: false,
        ..Default::default()
    };
    let results_after = idx.search(&q).await.expect("search after reclaim");
    assert!(
        !results_after.is_empty(),
        "expected a BM25 hit after reclaim + lazy rehydration"
    );
    assert!(
        idx.bm25.read().await.len() >= 2,
        "BM25 must be rehydrated after the post-reclaim search"
    );
}

/// `reclaim_memory_now` is a safe no-op on an index with no durable corpus —
/// it has nothing to rehydrate from, so it must not drop unrecoverable data.
///
/// Why: BM25-only / test indexes hold their only copy in memory. Reclaiming
/// them would be data loss, not a cache eviction. Both `clear_*` helpers guard
/// on `corpus.is_none()`, so reclaim must return 0 and leave state intact.
/// What: build a corpus-less indexer, populate BM25 directly, call
/// `reclaim_memory_now()`, assert it returns 0 and BM25 is untouched.
/// Test: this test.
#[tokio::test]
async fn memory_pressure_reclaim_now_is_noop_without_corpus() {
    let idx = make_indexer(); // no CorpusStore wired
    {
        let mut bm25 = idx.bm25.write().await;
        bm25.upsert_document("a:1:1", "fn authenticate");
        bm25.upsert_document("b:1:1", "fn verify_token");
    }
    let before = idx.bm25.read().await.len();
    assert!(before >= 2);

    let reclaimed = idx.reclaim_memory_now().await;
    assert_eq!(
        reclaimed, 0,
        "reclaim must be a no-op without a durable corpus (would be data loss)"
    );
    assert_eq!(
        idx.bm25.read().await.len(),
        before,
        "BM25 must be untouched when there is no corpus to rehydrate from"
    );
}

/// Idle eviction must not just empty the in-memory `chunks` map's entries but
/// actually release its backing table allocation — proving the eviction path
/// genuinely frees memory rather than merely `clear()`-ing while retaining a
/// large reserved buffer for reuse (issue #3657).
///
/// Why: the production incident (RSS climbing 20.3 → 26.4 GiB while the
/// daemon repeatedly logged `evicted N in-memory chunks after 60s idle`)
/// raised two hypotheses: (a) a lingering secondary holder keeps the freed
/// chunks' data alive (a true Rust-level leak), or (b) the allocations are
/// genuinely dropped but the OS allocator never returns the pages. `RawChunk`
/// (`core::chunker::types::RawChunk`) owns every field directly (`String`s
/// and `Vec`s, never `Arc`), so no secondary holder exists — ruling out (a)
/// at the Rust level. This test pins that half of the story deterministically:
/// `clear_in_memory_chunks` must call `HashMap::shrink_to_fit()` (not just
/// `clear()`), so the table's own backing allocation drops to zero capacity
/// — i.e. the eviction path holds up its end before any OS-level allocator
/// behaviour (hypothesis (b), fixed by `core::memguard::trim_heap()` and
/// covered by `memguard::tests::test_trim_heap_does_not_panic`) even enters
/// the picture.
/// What: index 64 files (well past any small-map inline capacity), assert
/// the `chunks` map has nonzero reserved capacity, force an eviction, then
/// assert capacity has dropped to exactly 0 — a deterministic, timing-free
/// check (no RSS sampling, no allocator introspection needed here).
/// Test: this test.
#[tokio::test]
async fn idle_eviction_releases_chunk_map_backing_allocation() {
    let dir = tempfile::tempdir().unwrap();
    let redb_path = dir.path().join("index.redb");
    let idx = make_indexer_with_corpus(&redb_path);

    let files: Vec<(String, String)> = (0..64)
        .map(|i| (format!("src/file_{i}.rs"), format!("fn f_{i}() {{}}")))
        .collect();
    idx.index_files_batch(&files).await.expect("index batch");

    let resident_before = idx.in_memory_chunk_count().await;
    assert!(resident_before >= 64, "expected >= 64 resident chunks");
    assert!(
        idx.chunks.read().await.capacity() > 0,
        "map must have reserved backing capacity before eviction"
    );

    let evicted = idx
        .evict_chunks_if_idle(std::time::Duration::from_nanos(1))
        .await;
    assert_eq!(evicted, resident_before, "eviction should drop every chunk");

    assert_eq!(
        idx.chunks.read().await.capacity(),
        0,
        "eviction must shrink_to_fit — release the map's backing allocation \
         entirely, not just clear() its entries while keeping the reserved \
         capacity resident (the root cause the #3657 incident traced past \
         this point and into OS-level allocator retention)"
    );
}
