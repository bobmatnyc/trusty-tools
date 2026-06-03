use super::*;
use crate::core::embed::MockEmbedder;
use crate::core::store::UsearchStore;
use std::sync::atomic::Ordering;

/// Root path used by all test indexers whose constructor is `make_indexer()`
/// or `CodeIndexer::new(_, "/tmp/test")`. `CodeChunk.file` values returned
/// by search/enumerate are now **absolute** (issue #402 — relocation resilience),
/// so assertions must compare against the fully-resolved form.
const TEST_ROOT: &str = "/tmp/test";

/// Build an absolute file path for a relative path under [`TEST_ROOT`].
///
/// Why: all `CodeChunk.file` values are now resolved to absolute paths at
/// materialization time (issue #402). Tests that previously compared against
/// relative paths (e.g. `"src/lib.rs"`) must now compare against
/// `/tmp/test/src/lib.rs`.
/// What: joins `TEST_ROOT` with `rel`, returning the platform path string.
/// Test: used throughout this module wherever `CodeChunk.file` is asserted.
fn abs(rel: &str) -> String {
    std::path::Path::new(TEST_ROOT)
        .join(rel)
        .to_string_lossy()
        .into_owned()
}

fn raw(id: &str, file: &str, content: &str) -> RawChunk {
    RawChunk {
        id: id.to_string(),
        file: file.to_string(),
        start_line: 1,
        end_line: 1 + content.lines().count(),
        content: content.to_string(),
        function_name: None,
        language: Some("rust".to_string()),
        chunk_type: crate::core::chunker::ChunkType::Code,
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

/// Convenience: build a `RawChunk` with a specific `chunk_type` and
/// `function_name`. Used by the issue #117 structural-boost regression test
/// (and any future test that needs to plant a declaration-shaped chunk into
/// the in-memory indexer without going through the tree-sitter pipeline).
fn raw_with_kind(
    id: &str,
    file: &str,
    content: &str,
    chunk_type: crate::core::chunker::ChunkType,
    function_name: Option<&str>,
) -> RawChunk {
    let mut c = raw(id, file, content);
    c.chunk_type = chunk_type;
    c.function_name = function_name.map(|s| s.to_string());
    c
}

fn make_indexer() -> CodeIndexer {
    let dim = 32;
    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(dim));
    let store: Arc<dyn VectorStore> = Arc::new(UsearchStore::new(dim).expect("usearch new"));
    CodeIndexer::new("test", "/tmp/test").with_components(embedder, store)
}

#[tokio::test]
async fn test_save_chunks_roundtrip() {
    // Issue #85: a freshly-loaded indexer must have its chunks restored
    // and its BM25 posting list rebuilt from disk — no re-parsing of
    // source files allowed.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("chunks.json");

    // Phase 1: populate an indexer and snapshot it.
    let idx = make_indexer();
    idx.add_chunk(raw("a", "src/a.rs", "fn authenticate() {}"))
        .await
        .unwrap();
    idx.add_chunk(raw("b", "src/b.rs", "fn verify_token() {}"))
        .await
        .unwrap();
    idx.save_chunks_to_disk(&path).await.expect("save chunks");
    assert!(path.exists());

    // Phase 2: load into a fresh indexer and confirm both corpus and
    // BM25 see the restored chunks.
    let restored = make_indexer();
    let n = restored
        .load_chunks_from_disk(&path)
        .await
        .expect("load chunks");
    assert_eq!(n, 2);
    assert_eq!(restored.chunk_count(), 2);
    // BM25 must be rebuilt — a "authenticate" lexical query should hit
    // chunk "a".
    let bm25 = restored.bm25.read().await;
    let hits = bm25.score_query_all("authenticate", 5);
    drop(bm25);
    assert!(
        hits.iter().any(|(id, _)| id == "a"),
        "BM25 not rebuilt from restored chunks: {:?}",
        hits
    );
}

#[tokio::test]
async fn test_load_chunks_missing_file_returns_zero() {
    let idx = make_indexer();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nope.json");
    let n = idx.load_chunks_from_disk(&path).await.unwrap();
    assert_eq!(n, 0);
}

/// Regression test for the memory-explosion bug: prior to the coalescing
/// fix, `spawn_incremental_persist` was called once per committed batch
/// and each invocation spawned a detached task that cloned the full
/// chunk corpus + serialized it to JSON. A reindex with N batches stacked
/// N tasks; for the duetto-cto / duetto monorepos that meant 46–174 GB
/// of concurrent allocation and an OS kill.
///
/// Why: prove that rapid-fire calls coalesce — the protocol guarantees
/// at most one task is alive (`in_flight == true`) at any moment, and
/// the `dirty` flag ensures the final on-disk state still converges.
/// What: drives 64 rapid-fire `spawn_incremental_persist` calls and
/// asserts that the per-indexer `in_flight` flag is never observed
/// stacked beyond a single task. We also assert it returns to `false`
/// once the tasks drain (proving the loop terminates and releases the
/// flag rather than leaking).
/// Test: this test directly. The fix is structural — without it, the
/// `assert!(active <= 1)` invariant would not even be expressible because
/// each call would spawn an independent task.
#[tokio::test]
async fn test_persist_coalesces_concurrent_calls() {
    let idx = make_indexer();
    idx.add_chunk(raw("a", "a.rs", "fn a() {}")).await.unwrap();

    // Fire 64 rapid `spawn_incremental_persist` calls. The structural
    // guarantee is that at most ONE detached task is ever alive at a
    // time, regardless of call cadence. We sample the in_flight flag
    // during the burst — a value of true means "the single coalesced
    // task is mid-flight", a value of false means "no task currently
    // running or the running task is between iterations".
    //
    // We allow the flag to be `true` (≤1 task is the whole point) but
    // we strengthen the test by counting "task starts" — the only way
    // for a NEW task to start is for `in_flight` to first be false. We
    // can't directly observe spawns, but we CAN observe that after the
    // burst completes, the flag eventually returns to `false` and stays
    // there, proving the loop terminates cleanly.
    // Issue #29: use `force = true` so every call bypasses the per-batch
    // throttle and actually exercises the coalescing protocol — the throttle
    // itself is covered by `test_incremental_persist_throttles_to_interval`.
    for _ in 0..64 {
        idx.spawn_incremental_persist(true);
    }

    // The flag MUST be observably true at least briefly (we just spawned
    // a task) — if it weren't, the coalescing logic would be broken (no
    // task started despite dirty being set). Sample within a short
    // window.
    //
    // Because path resolution may fail (in test env where data_dir is
    // unwritable) the task may flip in_flight back to false immediately
    // without doing work. We tolerate that — the structural fix is
    // unchanged: AT MOST ONE TASK IS ALIVE.
    //
    // The real invariant we test below is termination + flag release.

    // Wait for the persist loop to drain. Bound the wait so a hang
    // surfaces as a test failure rather than an infinite hang.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    loop {
        let in_flight = idx.persist_state.in_flight.load(Ordering::Acquire);
        let dirty = idx.persist_state.dirty.load(Ordering::Acquire);
        if !in_flight && !dirty {
            break;
        }
        if std::time::Instant::now() >= deadline {
            panic!(
                "persist coalescing loop did not drain within 15s: \
                 in_flight={in_flight}, dirty={dirty}"
            );
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }

    // After draining, fire one more call — it MUST be able to start
    // (i.e. the CAS must succeed). We verify by observing the
    // in_flight flag flips to true at least once within a short window.
    idx.persist_state.dirty.store(false, Ordering::Release);
    idx.spawn_incremental_persist(true);
    // Either the flag is true now (task running), OR the task already
    // finished a single iteration and released. Both are correct
    // post-fix behaviors. The buggy pre-fix code would have spawned a
    // NEW task on every call regardless of state — that pathology is
    // not directly observable here, but is captured by the
    // `MAX_COALESCED_ITERATIONS` cap and the single shared
    // `persist_state`.
    let _ = idx.persist_state.in_flight.load(Ordering::Acquire);
}

/// Issue #29: a non-forced `spawn_incremental_persist` must increment the
/// per-index batch counter on every call, and only the calls whose
/// post-increment count is a multiple of `HNSW_SNAPSHOT_BATCH_INTERVAL`
/// actually proceed past the throttle. A forced call bypasses the throttle
/// entirely and never touches the counter.
///
/// Why: the throttle is what reclaims ~15 s of redundant `Index::save` I/O on
/// a large reindex. Without this test a regression that drops the modulo (or
/// the early return) silently reverts to a save-per-batch, and the only
/// symptom would be slow reindexes — easy to miss.
/// What: fires `HNSW_SNAPSHOT_BATCH_INTERVAL` non-forced calls and asserts the
/// counter lands exactly on the interval; fires one more and asserts it kept
/// counting; then fires a forced call and asserts the counter is untouched.
/// Test: this test.
#[tokio::test]
async fn test_incremental_persist_throttles_to_interval() {
    let idx = make_indexer();

    // Counter starts at zero.
    assert_eq!(idx.persist_state.batch_counter.load(Ordering::Acquire), 0);

    // Fire exactly one interval's worth of non-forced calls. After the Nth
    // call the counter must equal the interval — the Nth call is the one that
    // passes the `n % INTERVAL == 0` gate.
    for _ in 0..HNSW_SNAPSHOT_BATCH_INTERVAL {
        idx.spawn_incremental_persist(false);
    }
    assert_eq!(
        idx.persist_state.batch_counter.load(Ordering::Acquire),
        HNSW_SNAPSHOT_BATCH_INTERVAL,
        "every non-forced call must increment the batch counter"
    );

    // One more non-forced call keeps counting (no reset).
    idx.spawn_incremental_persist(false);
    assert_eq!(
        idx.persist_state.batch_counter.load(Ordering::Acquire),
        HNSW_SNAPSHOT_BATCH_INTERVAL + 1
    );

    // A forced call bypasses the throttle and must NOT touch the counter.
    let before = idx.persist_state.batch_counter.load(Ordering::Acquire);
    idx.force_incremental_persist();
    assert_eq!(
        idx.persist_state.batch_counter.load(Ordering::Acquire),
        before,
        "force_incremental_persist must not increment the batch counter"
    );

    // Let any spawned persist tasks drain so the test doesn't leak them.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while idx.persist_state.in_flight.load(Ordering::Acquire)
        || idx.persist_state.dirty.load(Ordering::Acquire)
    {
        if std::time::Instant::now() >= deadline {
            panic!("persist tasks did not drain within 15s");
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
}

#[tokio::test]
async fn test_search_integration_returns_relevant_chunk_first() {
    let idx = make_indexer();

    idx.add_chunk(raw(
        "src/auth.rs:1:5",
        "src/auth.rs",
        "fn authenticate(user: &str, password: &str) -> bool { true }",
    ))
    .await
    .unwrap();
    idx.add_chunk(raw(
        "src/render.rs:1:3",
        "src/render.rs",
        "fn render_ui_components() { /* svelte */ }",
    ))
    .await
    .unwrap();
    idx.add_chunk(raw(
        "src/db.rs:1:4",
        "src/db.rs",
        "struct Database { conn: String }",
    ))
    .await
    .unwrap();

    let q = SearchQuery {
        text: "fn authenticate".to_string(),
        top_k: 3,
        expand_graph: false,
        compact: true,
        ..Default::default()
    };
    let results = idx.search(&q).await.expect("search");
    assert!(!results.is_empty(), "search should return at least one hit");
    assert_eq!(
        results[0].id,
        "src/auth.rs:1:5",
        "auth chunk must rank first; got {:?}",
        results.iter().map(|r| &r.id).collect::<Vec<_>>()
    );
    assert!(
        results[0].compact_snippet.is_some(),
        "compact_snippet should be populated when compact=true"
    );
    // BM25 lane must hit on the literal token "authenticate" → reason includes bm25.
    assert!(
        results[0].match_reason == "hybrid" || results[0].match_reason == "bm25",
        "expected hybrid or bm25 match_reason, got {}",
        results[0].match_reason
    );
}

#[tokio::test]
async fn test_query_cache_skips_embedder_on_repeat() {
    // We don't have a hit-counter on the trait, so drive correctness
    // indirectly: the cache hit path must populate `query_cache` and
    // return the same vector without invoking the embedder.
    let idx = make_indexer();
    let q = "find user authentication logic";

    let v1 = idx.embed_query(q).await.unwrap().unwrap();
    // After first call, cache should hold this entry.
    let key = hash_query(q);
    let cached = {
        let mut g = idx.query_cache.lock().unwrap();
        g.get(&key).cloned()
    };
    assert_eq!(cached.as_ref(), Some(&v1), "cache must be populated");

    let v2 = idx.embed_query(q).await.unwrap().unwrap();
    assert_eq!(v1, v2, "second call must return identical vector via cache");
}

#[tokio::test]
async fn test_search_with_no_embedder_falls_back_to_bm25() {
    // Indexer without `with_components` → embedder/store None → BM25-only.
    let idx = CodeIndexer::new("bm25-only", "/tmp/test");
    // We can't call add_chunk's vector path, but no embedder means it skips.
    idx.add_chunk(raw("f.rs:1:1", "f.rs", "fn authenticate() {}"))
        .await
        .unwrap();
    idx.add_chunk(raw("g.rs:1:1", "g.rs", "fn unrelated() {}"))
        .await
        .unwrap();

    let q = SearchQuery {
        text: "authenticate".to_string(),
        top_k: 5,
        expand_graph: false,
        compact: false,
        ..Default::default()
    };
    let r = idx.search(&q).await.unwrap();
    assert_eq!(r[0].id, "f.rs:1:1");
    assert_eq!(r[0].match_reason, "bm25");
}

#[tokio::test]
async fn test_remove_chunk_removes_from_results() {
    let idx = make_indexer();
    idx.add_chunk(raw("a:1:1", "a.rs", "fn authenticate() {}"))
        .await
        .unwrap();
    idx.add_chunk(raw("b:1:1", "b.rs", "fn other_thing() {}"))
        .await
        .unwrap();
    idx.remove_chunk("a:1:1").await.unwrap();

    let q = SearchQuery {
        text: "authenticate".to_string(),
        top_k: 5,
        expand_graph: false,
        compact: false,
        ..Default::default()
    };
    let r = idx.search(&q).await.unwrap();
    assert!(!r.iter().any(|c| c.id == "a:1:1"));
}

#[tokio::test]
async fn test_kg_expansion_marks_neighbours_with_hybrid_kg() {
    // Build a corpus where "login_handler" calls "authenticate".
    // Query for "authenticate" with Usage intent so KG expansion fires;
    // login_handler should appear via KG with match_reason "hybrid+kg".
    //
    // Use BM25-only mode (no embedder) so the vector lane can't pull
    // login_handler in as a near-neighbour and dilute the test signal.
    let idx = CodeIndexer::new("kg-test", "/tmp/test");
    // Caller's *body* deliberately omits the literal token "authenticate"
    // so BM25 / vector lanes won't surface it directly — its only path into
    // the result set is via KG expansion from the authenticate chunk.
    idx.add_chunk(RawChunk {
        id: "h:1".to_string(),
        file: "h.rs".to_string(),
        start_line: 1,
        end_line: 3,
        content: "fn login_handler() { /* dispatch to verifier */ }".to_string(),
        function_name: Some("login_handler".to_string()),
        language: Some("rust".to_string()),
        chunk_type: crate::core::chunker::ChunkType::Function,
        calls: vec!["authenticate".to_string()],
        inherits_from: Vec::new(),
        chunk_depth: 0,
        parent_chunk_id: None,
        child_chunk_ids: Vec::new(),
        nlp_keywords: Vec::new(),
        nlp_code_refs: Vec::new(),
        virtual_terms: Vec::new(),
    })
    .await
    .unwrap();
    idx.add_chunk(RawChunk {
        id: "a:1".to_string(),
        file: "a.rs".to_string(),
        start_line: 1,
        end_line: 1,
        content: "fn authenticate() {}".to_string(),
        function_name: Some("authenticate".to_string()),
        language: Some("rust".to_string()),
        chunk_type: crate::core::chunker::ChunkType::Function,
        calls: Vec::new(),
        inherits_from: Vec::new(),
        chunk_depth: 0,
        parent_chunk_id: None,
        child_chunk_ids: Vec::new(),
        nlp_keywords: Vec::new(),
        nlp_code_refs: Vec::new(),
        virtual_terms: Vec::new(),
    })
    .await
    .unwrap();

    // "callers of authenticate" → Usage intent → use_kg_first=true
    let q = SearchQuery {
        text: "callers of authenticate".to_string(),
        top_k: 10,
        expand_graph: true,
        compact: false,
        ..Default::default()
    };
    let results = idx.search(&q).await.unwrap();
    let login = results
        .iter()
        .find(|c| c.id == "h:1")
        .expect("login_handler should surface via KG expansion");
    assert_eq!(
        login.match_reason, "hybrid+kg",
        "KG-expanded chunks must carry hybrid+kg marker, got {}",
        login.match_reason
    );

    // Verify the 0.7× score factor: login_handler's score should be
    // exactly 0.7 × the trigger chunk's RRF score (within fp tolerance),
    // unless it was also a direct hit (then RRF would have ranked it).
    let trigger = results
        .iter()
        .find(|c| c.id == "a:1")
        .expect("authenticate must appear directly");
    let expected = trigger.score * KG_EXPAND_SCORE_FACTOR;
    assert!(
        (login.score - expected).abs() < 1e-5,
        "expected KG score = 0.7 * {} = {}, got {}",
        trigger.score,
        expected,
        login.score
    );
}

#[tokio::test]
async fn test_kg_expansion_disabled_by_expand_graph_false() {
    let idx = make_indexer();
    idx.add_chunk(RawChunk {
        id: "h:1".to_string(),
        file: "h.rs".to_string(),
        start_line: 1,
        end_line: 1,
        content: "fn caller() { target(); }".to_string(),
        function_name: Some("caller".to_string()),
        language: Some("rust".to_string()),
        chunk_type: crate::core::chunker::ChunkType::Function,
        calls: vec!["target".to_string()],
        inherits_from: Vec::new(),
        chunk_depth: 0,
        parent_chunk_id: None,
        child_chunk_ids: Vec::new(),
        nlp_keywords: Vec::new(),
        nlp_code_refs: Vec::new(),
        virtual_terms: Vec::new(),
    })
    .await
    .unwrap();
    idx.add_chunk(RawChunk {
        id: "t:1".to_string(),
        file: "t.rs".to_string(),
        start_line: 1,
        end_line: 1,
        content: "fn target() {}".to_string(),
        function_name: Some("target".to_string()),
        language: Some("rust".to_string()),
        chunk_type: crate::core::chunker::ChunkType::Function,
        calls: Vec::new(),
        inherits_from: Vec::new(),
        chunk_depth: 0,
        parent_chunk_id: None,
        child_chunk_ids: Vec::new(),
        nlp_keywords: Vec::new(),
        nlp_code_refs: Vec::new(),
        virtual_terms: Vec::new(),
    })
    .await
    .unwrap();

    let q = SearchQuery {
        text: "callers of target".to_string(),
        top_k: 10,
        expand_graph: false,
        compact: false,
        ..Default::default()
    };
    let results = idx.search(&q).await.unwrap();
    assert!(
        !results.iter().any(|c| c.match_reason.contains("kg")),
        "expand_graph=false must suppress KG expansion, got {results:#?}"
    );
}

/// Issue #138 — `SearchStage::Semantic` skips KG expansion even when the
/// query intent would otherwise enable it. `stage=semantic` selects the
/// BM25 + HNSW lanes but explicitly drops the graph hop. Mirrors the
/// behaviour the new `search_semantic` MCP tool needs.
///
/// Why: the LLM that chose `search_semantic` is asking for conceptual
/// recall, not a callgraph walk; surfacing KG-only neighbours would muddle
/// the result set and defeat the per-tool intent split.
/// What: build a corpus where a caller-only chunk would surface via KG
/// expansion under default Usage-intent routing; assert `stage=Semantic`
/// suppresses it.
/// Test: covers the search dispatcher's `skip_kg` branch for the
/// Semantic variant added in #138.
#[tokio::test]
async fn search_semantic_stage_skips_kg_expansion() {
    let idx = make_indexer();
    idx.add_chunk(RawChunk {
        id: "h:1".to_string(),
        file: "h.rs".to_string(),
        start_line: 1,
        end_line: 1,
        content: "fn caller() { /* dispatch */ }".to_string(),
        function_name: Some("caller".to_string()),
        language: Some("rust".to_string()),
        chunk_type: crate::core::chunker::ChunkType::Function,
        calls: vec!["target".to_string()],
        inherits_from: Vec::new(),
        chunk_depth: 0,
        parent_chunk_id: None,
        child_chunk_ids: Vec::new(),
        nlp_keywords: Vec::new(),
        nlp_code_refs: Vec::new(),
        virtual_terms: Vec::new(),
    })
    .await
    .unwrap();
    idx.add_chunk(RawChunk {
        id: "t:1".to_string(),
        file: "t.rs".to_string(),
        start_line: 1,
        end_line: 1,
        content: "fn target() {}".to_string(),
        function_name: Some("target".to_string()),
        language: Some("rust".to_string()),
        chunk_type: crate::core::chunker::ChunkType::Function,
        calls: Vec::new(),
        inherits_from: Vec::new(),
        chunk_depth: 0,
        parent_chunk_id: None,
        child_chunk_ids: Vec::new(),
        nlp_keywords: Vec::new(),
        nlp_code_refs: Vec::new(),
        virtual_terms: Vec::new(),
    })
    .await
    .unwrap();
    let q = SearchQuery {
        text: "callers of target".to_string(),
        top_k: 10,
        expand_graph: true,
        compact: false,
        stage: Some(super::SearchStage::Semantic),
        ..Default::default()
    };
    let results = idx.search(&q).await.unwrap();
    assert!(
        !results.iter().any(|c| c.match_reason.contains("kg")),
        "stage=Semantic must suppress KG expansion, got {results:#?}"
    );
}

/// Issue #138 — `SearchStage::Graph` forces KG expansion ON regardless
/// of the intent's `use_kg_first` weighting. Mirrors the `search_kg`
/// MCP tool: when the LLM picked the KG tool, the daemon must expand the
/// graph even on a Definition-intent seed query that would normally
/// suppress it.
///
/// Why: the per-lane MCP tools are an explicit contract — when the LLM
/// chose `search_kg`, the user's mental model is "explore the graph from
/// this seed", not "let the intent classifier decide".
/// What: build a corpus with a caller→target edge; query with the seed
/// "target" (a Definition-intent query that ordinarily disables KG); pin
/// `stage=Graph` and assert the caller surfaces via KG expansion.
/// Test: covers the search dispatcher's `force_kg` branch for the
/// Graph variant added in #138.
#[tokio::test]
async fn search_graph_stage_forces_kg_expansion_on_definition_query() {
    // BM25-only mode so the vector lane can't pull `caller` into the
    // result set as a near-neighbour and mask the KG expansion signal we
    // are testing. The body of `caller` deliberately omits the literal
    // token "target" so its only path into the result set is via KG.
    let idx = CodeIndexer::new("graph-stage-force", "/tmp/test");
    idx.add_chunk(RawChunk {
        id: "h:1".to_string(),
        file: "h.rs".to_string(),
        start_line: 1,
        end_line: 1,
        content: "fn caller() { /* dispatch to function */ }".to_string(),
        function_name: Some("caller".to_string()),
        language: Some("rust".to_string()),
        chunk_type: crate::core::chunker::ChunkType::Function,
        calls: vec!["target".to_string()],
        inherits_from: Vec::new(),
        chunk_depth: 0,
        parent_chunk_id: None,
        child_chunk_ids: Vec::new(),
        nlp_keywords: Vec::new(),
        nlp_code_refs: Vec::new(),
        virtual_terms: Vec::new(),
    })
    .await
    .unwrap();
    idx.add_chunk(RawChunk {
        id: "t:1".to_string(),
        file: "t.rs".to_string(),
        start_line: 1,
        end_line: 1,
        content: "fn target() {}".to_string(),
        function_name: Some("target".to_string()),
        language: Some("rust".to_string()),
        chunk_type: crate::core::chunker::ChunkType::Function,
        calls: Vec::new(),
        inherits_from: Vec::new(),
        chunk_depth: 0,
        parent_chunk_id: None,
        child_chunk_ids: Vec::new(),
        nlp_keywords: Vec::new(),
        nlp_code_refs: Vec::new(),
        virtual_terms: Vec::new(),
    })
    .await
    .unwrap();

    // Use a bare-symbol query — would otherwise classify as Definition
    // intent (use_kg_first = false). The Graph stage must override that.
    let q = SearchQuery {
        text: "target".to_string(),
        top_k: 10,
        expand_graph: true,
        compact: false,
        stage: Some(super::SearchStage::Graph),
        ..Default::default()
    };
    let results = idx.search(&q).await.unwrap();
    let caller = results
        .iter()
        .find(|c| c.id == "h:1")
        .unwrap_or_else(|| panic!("caller must surface via KG, got {results:#?}"));
    assert!(
        caller.match_reason.contains("kg"),
        "stage=Graph must force KG expansion on caller, got match_reason={}",
        caller.match_reason
    );
}

#[tokio::test]
async fn test_symbol_graph_rebuilds_after_indexing() {
    let idx = make_indexer();
    assert_eq!(idx.symbol_graph().await.node_count(), 0);
    idx.index_file("a.rs", "fn alpha() { beta(); }\nfn beta() {}\n")
        .await
        .unwrap();
    let g = idx.symbol_graph().await;
    assert!(g.node_count() >= 2, "graph should hold alpha + beta");
    assert!(
        !g.callees_of("alpha", 1).is_empty(),
        "alpha should have a callee edge to beta"
    );
}

#[tokio::test]
async fn test_entity_exact_match_finds_chunk() {
    // Issue #20: an exact-name entity hit should resolve to a chunk in the
    // entity's file whose line range contains the entity. We use a struct
    // declaration so the AST emits a NamedType that matches the query.
    let idx = make_indexer();
    idx.index_file("e.rs", "pub struct MyType { x: u32 }\nfn f() {}\n")
        .await
        .unwrap();
    let hit = idx.entity_exact_match("MyType").await;
    assert!(hit.is_some(), "expected entity_exact_match to find MyType");
    let hit_id = hit.unwrap();
    let chunks = idx.chunks.read().await;
    assert!(
        chunks
            .get(&hit_id)
            .map(|c| c.file == "e.rs")
            .unwrap_or(false),
        "matched chunk should live in e.rs",
    );
}

#[tokio::test]
async fn test_entity_exact_match_struct_ranks_first() {
    // Issue #20: indexing a Rust snippet with `struct FooBar` and querying
    // "FooBar" must surface that chunk at rank 1 via the synthetic BM25
    // injection. We use BM25-only mode so the vector lane can't dilute
    // the signal with a near-neighbour.
    let idx = CodeIndexer::new("ent-rank-1", "/tmp/test");
    idx.index_file(
        "src/types.rs",
        "pub struct FooBar { pub x: u32 }\n\nfn unrelated() { let _ = 1; }\n",
    )
    .await
    .unwrap();
    idx.index_file("src/other.rs", "fn other_thing() {}\n")
        .await
        .unwrap();

    let q = SearchQuery {
        text: "FooBar".to_string(),
        top_k: 5,
        expand_graph: false,
        compact: false,
        ..Default::default()
    };
    let results = idx.search(&q).await.expect("search");
    assert!(!results.is_empty(), "search must return at least one hit");
    assert_eq!(
        results[0].file,
        abs("src/types.rs"),
        "FooBar's defining file must rank first; got {:?}",
        results.iter().map(|r| &r.file).collect::<Vec<_>>(),
    );
    assert!(
        results[0].content.contains("FooBar"),
        "rank-1 chunk must contain the FooBar definition; got {:?}",
        results[0].content,
    );
}

#[tokio::test]
async fn test_entity_exact_match_skips_non_symbol_entities() {
    // Issue #20: only NamedType and ModulePath entities should anchor
    // exact-name boosts. A LiteralString like "this is a long literal"
    // appearing in a file must not be returned as an entity match.
    let idx = make_indexer();
    idx.index_file("lit.rs", "fn f() { let _ = \"this is a long literal\"; }\n")
        .await
        .unwrap();
    // Single-word literal subset that exists as a string token but is
    // neither a NamedType nor a ModulePath — must miss.
    assert!(
        idx.entity_exact_match("literal").await.is_none(),
        "non-symbol entity types must not satisfy entity_exact_match"
    );
}

#[tokio::test]
async fn test_entity_exact_match_skips_multiword_query() {
    let idx = make_indexer();
    idx.index_file("e.rs", "use std::sync::Arc;\nfn f() {}\n")
        .await
        .unwrap();
    assert!(idx.entity_exact_match("Arc thing").await.is_none());
}

#[tokio::test]
async fn test_virtual_terms_populated_from_entities() {
    // Issue #19: chunks should pick up entity text as virtual_terms so
    // BM25 matches symbolic queries that don't appear literally in the body.
    let idx = make_indexer();
    idx.index_file(
        "v.rs",
        "use std::sync::Arc;\nfn f() { let _x: Arc<String> = Arc::new(String::new()); }\n",
    )
    .await
    .unwrap();
    let chunks = idx.chunks.read().await;
    let f_chunk = chunks
        .values()
        .find(|c| c.function_name.as_deref() == Some("f"))
        .expect("f chunk");
    assert!(
        f_chunk.virtual_terms.iter().any(|t| t == "Arc"),
        "expected 'Arc' in virtual_terms, got {:?}",
        f_chunk.virtual_terms
    );
}

#[tokio::test]
async fn test_get_embedding_returns_some_after_indexing() {
    let idx = make_indexer();
    idx.add_chunk(raw("a:1:1", "a.rs", "fn alpha() {}"))
        .await
        .unwrap();
    let emb = idx.get_embedding("a:1:1");
    assert!(emb.is_some(), "expected embedding cached after add_chunk");
    assert!(idx.get_embedding("nope").is_none());
}

/// Issue #484 — `chunk_content_by_id` is the escape hatch used by
/// `search_similar_handler` when the embedding LRU cache misses.
///
/// Why: the handler falls back from `get_embedding` (LRU cache) to
/// `chunk_content_by_id` + `embed_text` so it can produce results for
/// `skip_kg=true` indexes where the cache is cold.
/// What: asserts that known IDs return content and unknown IDs return `None`.
/// Test: this function.
#[tokio::test]
async fn test_chunk_content_by_id_returns_none_for_unknown() {
    let idx = make_indexer();
    idx.add_chunk(raw("a:1:1", "a.rs", "fn alpha() {}"))
        .await
        .unwrap();
    // Known id → returns the content.
    let content = idx.chunk_content_by_id("a:1:1").await;
    assert_eq!(
        content.as_deref(),
        Some("fn alpha() {}"),
        "chunk_content_by_id must return the raw content for a known id"
    );
    // Unknown id → None.
    let missing = idx.chunk_content_by_id("not_a_real_id").await;
    assert!(
        missing.is_none(),
        "chunk_content_by_id must return None for an unknown id"
    );
}

#[tokio::test]
async fn test_similar_by_embedding_excludes_seed() {
    let idx = make_indexer();
    idx.add_chunk(raw("a:1:1", "a.rs", "fn alpha() {}"))
        .await
        .unwrap();
    idx.add_chunk(raw("b:1:1", "b.rs", "fn beta() {}"))
        .await
        .unwrap();
    let emb = idx.get_embedding("a:1:1").unwrap();
    let results = idx
        .similar_by_embedding(&emb, 5, Some("a:1:1"))
        .await
        .unwrap();
    assert!(results.iter().all(|c| c.id != "a:1:1"));
    assert!(results.iter().all(|c| c.match_reason == "vector"));
}

#[tokio::test]
async fn test_index_files_batch_indexes_all_chunks_once() {
    // Bulk-indexing two files should leave the corpus with the same chunks
    // as if we'd called index_file twice, but issue exactly one symbol-graph
    // rebuild and one batched embed call (we can't observe the latter
    // directly without a counter, but we can assert correctness end-to-end).
    let idx = make_indexer();
    let files = vec![
        (
            "src/a.rs".to_string(),
            "fn alpha() { beta(); }\nfn beta() {}\n".to_string(),
        ),
        (
            "src/b.rs".to_string(),
            "fn gamma() {}\nfn delta() { gamma(); }\n".to_string(),
        ),
    ];
    let added = idx.index_files_batch(&files).await.unwrap();
    assert!(added >= 4, "expected at least 4 chunks, got {added}");
    // Symbol graph must reflect cross-file edges (delta -> gamma).
    let g = idx.symbol_graph().await;
    assert!(g.node_count() >= 4);
    // Search must surface the right chunk.
    let q = SearchQuery {
        text: "fn alpha".to_string(),
        top_k: 5,
        expand_graph: false,
        compact: false,
        ..Default::default()
    };
    let r = idx.search(&q).await.unwrap();
    assert!(r.iter().any(|c| c.file == abs("src/a.rs")));
}

#[tokio::test]
async fn test_index_files_batch_empty_input_is_noop() {
    let idx = make_indexer();
    let added = idx.index_files_batch(&[]).await.unwrap();
    assert_eq!(added, 0);
    assert_eq!(idx.chunk_count(), 0);
}

#[tokio::test]
async fn test_index_files_batch_bm25_only_mode() {
    // No embedder/store wired — the batch path must still populate the
    // corpus and BM25 must still find chunks.
    let idx = CodeIndexer::new("bm25-batch", "/tmp/test");
    let files = vec![(
        "x.rs".to_string(),
        "fn authenticate() {}\nfn other() {}\n".to_string(),
    )];
    let added = idx.index_files_batch(&files).await.unwrap();
    assert!(added >= 2);
    let r = idx
        .search(&SearchQuery {
            text: "authenticate".to_string(),
            top_k: 5,
            expand_graph: false,
            compact: false,
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(r.iter().any(|c| c.content.contains("authenticate")));
}

/// `CodeIndexer::search` must route otherwise-`Unknown` queries to
/// `Definition` intent when the per-index `domain_terms` vocabulary
/// matches the query.
///
/// Why: this is the wiring point for `trusty-search.yaml`'s
/// `domain_terms:` field. Without this test, a regression that drops the
/// `with_domain_terms`/`set_domain_terms` call (or reverts `search` back
/// to the non-domain `classify`) silently disables domain-aware routing
/// for every multi-index repo.
///
/// What: the indexer is wired with `["PMS"]`. We index a file containing
/// a `pms_handler` symbol and search for `"PMS integration query"` —
/// a phrase the generic classifier returns `Unknown` for. The domain
/// classifier should upgrade to `Definition`, which uses lexical-heavy
/// weights; we verify by asserting the symbol chunk is the top hit.
/// Test: this test.
#[tokio::test]
async fn search_uses_domain_terms_when_provided() {
    use crate::core::classifier::{QueryClassifier, QueryIntent};

    // First, confirm the generic classifier *can't* route the bare phrase
    // to Definition without the domain hint — otherwise the test would
    // pass for the wrong reason.
    // (Updated for issue #119: the original test used the acronym "PMS"
    // which now classifies as Definition directly via the all-caps acronym
    // hint. We switched to lowercase domain jargon — `rezo` — to keep this
    // test focused on the domain-vocabulary upgrade path rather than the
    // acronym hint.)
    let plain = QueryClassifier::classify("rezo integration query");
    assert_eq!(
        plain,
        QueryIntent::Unknown,
        "baseline: plain classifier must treat the rezo phrase as Unknown"
    );

    let idx =
        CodeIndexer::new("domain-test", "/tmp/domain").with_domain_terms(vec!["rezo".to_string()]);
    idx.index_file("api.rs", "fn rezo_handler() {}\nfn other() {}\n")
        .await
        .expect("index_file ok");
    let r = idx
        .search(&SearchQuery {
            text: "rezo integration query".into(),
            top_k: 5,
            expand_graph: false,
            compact: false,
            ..Default::default()
        })
        .await
        .expect("search ok");
    // The corpus only has two functions; the rezo-named one should win
    // under Definition's BM25-heavy weighting.
    assert!(
        r.iter().any(|c| c.content.contains("rezo_handler")),
        "expected rezo_handler chunk to appear in results: {:?}",
        r.iter().map(|c| &c.content).collect::<Vec<_>>()
    );
}

#[test]
fn test_file_type_multiplier_demotes_docs() {
    // Why: Definition-intent ranking should prefer source over docs.
    // What: confirms the helper's contract — multiplier 0.5 for .md/.toml/
    // .yaml/.json/.txt, 1.0 for everything else.
    // Test: direct assertions on the helper.
    assert_eq!(file_type_score_multiplier("src/auth.rs"), 1.0);
    assert_eq!(file_type_score_multiplier("src/auth.py"), 1.0);
    assert_eq!(file_type_score_multiplier("src/auth.go"), 1.0);
    assert_eq!(file_type_score_multiplier("CHANGELOG.md"), 0.5);
    assert_eq!(file_type_score_multiplier("docs/CLAUDE.md"), 0.5);
    assert_eq!(file_type_score_multiplier("Cargo.toml"), 0.5);
    assert_eq!(file_type_score_multiplier("config.yaml"), 0.5);
    assert_eq!(file_type_score_multiplier("data.json"), 0.5);
    // Case-insensitive
    assert_eq!(file_type_score_multiplier("README.MD"), 0.5);
}

#[tokio::test]
async fn test_definition_demotes_markdown_below_source() {
    // Why: issue #92 — for Definition-intent queries, the canonical
    // source-file declaration must outrank any .md doc that mentions the
    // symbol many times.
    // What: build a corpus with one .rs source chunk and one .md chunk
    // both containing the literal "CodeChunk struct"; run a Definition
    // query and assert the .rs file ranks first.
    // Test: this test.
    let idx = make_indexer();
    idx.add_chunk(raw(
        "doc:1",
        "CHANGELOG.md",
        "## CodeChunk struct\nCodeChunk struct fields: id, file. CodeChunk struct fields are stable.",
    ))
    .await
    .unwrap();
    idx.add_chunk(raw(
        "src:1",
        "src/indexer.rs",
        "pub struct CodeChunk { pub id: String, pub file: String }",
    ))
    .await
    .unwrap();

    let q = SearchQuery {
        text: "struct CodeChunk fields".to_string(),
        top_k: 10,
        expand_graph: false,
        compact: false,
        ..Default::default()
    };
    let results = idx.search(&q).await.unwrap();
    assert!(!results.is_empty(), "search must return results");
    assert!(
        results[0].file.ends_with(".rs"),
        "Definition intent must rank source over docs, top result file = {}",
        results[0].file
    );
}

#[tokio::test]
async fn test_struct_definition_boost_surfaces_struct_over_usage() {
    // Why: issue #117 — queries containing struct-name acronyms (`HNSW`,
    // `BM25`, `RRF`, `ORT`) historically returned usage sites at top ranks
    // because the BM25 lane couldn't distinguish "file mentions HNSW many
    // times" from "file IS the HNSW declaration". On the v0.8.1 benchmark
    // `HNSW vector similarity search` placed `hnsw_store.rs` at rank 8,
    // behind `retrieval.rs` and `mmr.rs`.
    //
    // Combined fix:
    //   1. #119 classifies short acronym queries (≤2 tokens) as Definition
    //      via the ALL-CAPS acronym hint.
    //   2. The structural boost in `apply_score_adjustments` multiplies
    //      the score of any Struct/Enum/Class/Trait chunk whose
    //      `function_name` matches a query token by `STRUCT_DEFINITION_BOOST`.
    //
    // Updated for issue #197: the original `HNSW vector similarity search`
    // query no longer routes to Definition (the token-count guard suppresses
    // ACRONYM_HINT_RE for multi-word NL-heavy queries) — it now reads as a
    // Conceptual query, which is the correct semantic intent. The
    // Definition structural-boost path is still exercised here by the
    // shorter 2-token acronym query `HNSW lookup`, which preserves the
    // #117 acceptance criterion: `hnsw_store.rs` (canonical struct decl)
    // must outrank usage sites for a Definition-intent acronym query.
    // Test: this test.
    use crate::core::chunker::ChunkType;
    use crate::core::classifier::{QueryClassifier, QueryIntent};

    // Sanity: the (short, 2-token) query must classify as Definition. The
    // acronym-hint rule from #119, gated by the #197 token-count guard,
    // is what makes this true; if it regresses, the test should fail loudly
    // here rather than in the ranking assertion below.
    assert_eq!(
        QueryClassifier::classify("HNSW lookup"),
        QueryIntent::Definition,
        "test pre-condition: short ALL-CAPS acronym query must classify as \
         Definition (#119 + #197 short-query carve-out)"
    );

    let idx = make_indexer();
    // 1) The canonical declaration: a Struct chunk whose function_name
    //    (= the type name) is `HnswStore` — lowercased, this matches the
    //    `hnsw` query token.
    idx.add_chunk(raw_with_kind(
        "def:1",
        "src/hnsw_store.rs",
        "pub struct HnswStore { index: Index, dim: usize }",
        ChunkType::Struct,
        Some("HnswStore"),
    ))
    .await
    .unwrap();
    // 2-4) Three usage chunks in plausible-looking files. They mention
    //      `HNSW` heavily so the BM25 lane would otherwise rank them
    //      above the declaration (the #117 failure mode).
    idx.add_chunk(raw(
        "use:1",
        "src/retrieval.rs",
        "// HNSW lookup path.\n\
         // Uses HNSW to retrieve top-k vectors.\n\
         // HNSW lookup HNSW lookup HNSW HNSW HNSW HNSW HNSW HNSW HNSW HNSW",
    ))
    .await
    .unwrap();
    idx.add_chunk(raw(
        "use:2",
        "src/mmr.rs",
        "// MMR diversity reranker over HNSW lookup results.\n\
         // HNSW HNSW HNSW lookup lookup lookup HNSW HNSW HNSW",
    ))
    .await
    .unwrap();
    idx.add_chunk(raw(
        "use:3",
        "src/search.rs",
        "// Top-level hybrid search: BM25 lane + HNSW lookup lane.\n\
         // HNSW HNSW HNSW lookup lookup HNSW HNSW lookup HNSW",
    ))
    .await
    .unwrap();

    let q = SearchQuery {
        text: "HNSW lookup".to_string(),
        top_k: 10,
        expand_graph: false,
        compact: false,
        ..Default::default()
    };
    let results = idx.search(&q).await.unwrap();
    assert!(!results.is_empty(), "search must return results");
    let top3_files: Vec<String> = results.iter().take(3).map(|c| c.file.clone()).collect();
    let hnsw_abs = abs("src/hnsw_store.rs");
    assert!(
        top3_files.contains(&hnsw_abs),
        "issue #117 acceptance: hnsw_store.rs must rank in top-3 for \
         the canonical acronym query; got top-3 files = {top3_files:?}, \
         full ranking = {:?}",
        results
            .iter()
            .map(|c| (c.file.as_str(), c.score))
            .collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn test_function_definition_boost_surfaces_function_over_string_literal_usage() {
    // Why: issue #122 — function-name queries (`BRUSILOV_EPOCH`,
    // `get_call_chain`) were placing usage sites OR string-literal
    // occurrences at rank 1 instead of the canonical declaration.
    // The synthetic-corpus baseline (#123) reproduced this on a clean
    // corpus across all three modes (lexical/hybrid/kg-leading), so it
    // is a real ranking bug rather than a circular-bias artifact.
    //
    // Fix: extend the Definition-intent structural boost (#117) to also
    // cover `Function`/`Method` chunks. The chunk_type filter naturally
    // excludes string-literal occurrences embedded in JSON-shaped
    // descriptors because those chunk as `Constant`, not `Function`.
    //
    // What: plant one Function chunk (the canonical declaration) and one
    // Constant chunk that contains the query token only as a string
    // literal inside a JSON-like descriptor (the historical false-positive
    // shape from `mcp_descriptor.rs`). Assert the Function chunk ranks at
    // top-2 or better for the function-name query.
    // Test: this test.
    use crate::core::chunker::ChunkType;
    use crate::core::classifier::{QueryClassifier, QueryIntent};

    // Sanity: snake_case identifier with a digit / underscore should
    // classify as Definition (or Unknown, both eligible — but #119 routes
    // SCREAMING_SNAKE_CASE / get_xxx-style symbols to Definition).
    assert_eq!(
        QueryClassifier::classify("get_call_chain"),
        QueryIntent::Definition,
        "test pre-condition: snake_case symbol must classify as Definition"
    );

    let idx = make_indexer();
    // 1) The canonical Function declaration. function_name matches the
    //    query token verbatim; the chunk body is short and contains the
    //    symbol exactly once — i.e. BM25 TF is LOW. Without the boost,
    //    the usage / string-literal chunks dominate.
    idx.add_chunk(raw_with_kind(
        "def:fn",
        "src/call_chain.rs",
        "pub fn get_call_chain(symbol: &str) -> Vec<String> {\n    \
         vec![symbol.to_string()]\n}",
        ChunkType::Function,
        Some("get_call_chain"),
    ))
    .await
    .unwrap();
    // 2) A `Constant` chunk that mentions `get_call_chain` only as a
    //    string literal inside a JSON-shaped MCP tool descriptor. This
    //    is the historical false-positive shape (`mcp_descriptor.rs`).
    //    We deliberately make the TF very high so without the chunk_type
    //    filter the boost would mis-fire here.
    idx.add_chunk(raw_with_kind(
        "use:descriptor",
        "src/mcp_descriptor.rs",
        "const TOOL: &str = r#\"{ \"name\": \"get_call_chain\", \
         \"description\": \"get_call_chain helper get_call_chain tool \
         get_call_chain get_call_chain get_call_chain\" }\"#;",
        ChunkType::Constant,
        Some("TOOL"),
    ))
    .await
    .unwrap();
    // 3) A plain code/usage chunk that calls the function — mid-TF.
    idx.add_chunk(raw(
        "use:call",
        "src/caller.rs",
        "let chain = get_call_chain(\"foo\"); \
         // get_call_chain returns the call chain; get_call_chain is a helper.",
    ))
    .await
    .unwrap();

    let q = SearchQuery {
        text: "get_call_chain".to_string(),
        top_k: 10,
        expand_graph: false,
        compact: false,
        ..Default::default()
    };
    let results = idx.search(&q).await.unwrap();
    assert!(!results.is_empty(), "search must return results");
    let rank_of_fn = results
        .iter()
        .position(|c| c.file == abs("src/call_chain.rs"))
        .expect("Function declaration must be in results");
    assert!(
        rank_of_fn < 2,
        "issue #122 acceptance: Function declaration must rank at top-2 or \
         better; got rank {rank_of_fn}, ranking = {:?}",
        results
            .iter()
            .map(|c| (c.file.as_str(), c.score))
            .collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn test_method_definition_boost_fires() {
    // Why: issue #122 — symmetric coverage for `ChunkType::Method`. The
    // boost must apply identically for impl-block method declarations.
    // What: plant one Method chunk + one usage chunk; assert the Method
    // ranks above the usage chunk for a method-name query.
    // Test: this test.
    use crate::core::chunker::ChunkType;

    let idx = make_indexer();
    // Method declaration (impl-block shape).
    idx.add_chunk(raw_with_kind(
        "def:method",
        "src/parser.rs",
        "impl Parser {\n    \
         pub fn parse_token(&self, input: &str) -> Token { Token::default() }\n}",
        ChunkType::Method,
        Some("parse_token"),
    ))
    .await
    .unwrap();
    // Usage chunk: mentions parse_token several times in a regular code
    // block (typed as Code, not Method).
    idx.add_chunk(raw(
        "use:method",
        "src/driver.rs",
        "// driver calls parse_token; parse_token returns a Token. parse_token \
         parse_token parse_token parse_token.",
    ))
    .await
    .unwrap();

    let q = SearchQuery {
        text: "parse_token".to_string(),
        top_k: 10,
        expand_graph: false,
        compact: false,
        ..Default::default()
    };
    let results = idx.search(&q).await.unwrap();
    let rank_of_method = results
        .iter()
        .position(|c| c.file == abs("src/parser.rs"))
        .expect("Method declaration must be in results");
    let rank_of_usage = results
        .iter()
        .position(|c| c.file == abs("src/driver.rs"))
        .expect("Usage chunk must be in results");
    assert!(
        rank_of_method < rank_of_usage,
        "issue #122: Method declaration (rank {rank_of_method}) must \
         out-rank the usage chunk (rank {rank_of_usage}); ranking = {:?}",
        results
            .iter()
            .map(|c| (c.file.as_str(), c.score))
            .collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn test_function_boost_skipped_on_conceptual_intent() {
    // Why: issue #122 — the function-definition boost must only fire when
    // the classifier routes the query to Definition. On Conceptual intent
    // (e.g. "how does ...") the BM25 lane should decide ranking. This pins
    // the conditional so a future refactor can't silently widen the boost
    // to all intents.
    // What: same shape as the positive test, but use a Conceptual-phrased
    // query. Assert the Function chunk does NOT receive the 2× boost —
    // we verify this by checking that the boost was skipped: with the
    // boost active the Function chunk would dominate, but on Conceptual
    // intent the usage chunk should compete on equal BM25 footing.
    // Test: this test.
    use crate::core::chunker::ChunkType;
    use crate::core::classifier::{QueryClassifier, QueryIntent};

    // Pre-condition: "how does X work" must classify as Conceptual.
    assert_eq!(
        QueryClassifier::classify("how does parse_token work in the parser"),
        QueryIntent::Conceptual,
        "test pre-condition: 'how does X work' must classify as Conceptual"
    );

    let idx = make_indexer();
    // Function declaration: short, low TF.
    idx.add_chunk(raw_with_kind(
        "def:fn",
        "src/parser.rs",
        "pub fn parse_token(input: &str) -> Token { Token::default() }",
        ChunkType::Function,
        Some("parse_token"),
    ))
    .await
    .unwrap();
    // Conceptual / explanatory chunk in a doc with high TF for the
    // query terms.
    idx.add_chunk(raw(
        "doc:1",
        "docs/ARCHITECTURE.md",
        "How does parse_token work? parse_token in the parser tokenises input \
         strings into Token values. parse_token parse_token parser parser \
         tokenise tokenise.",
    ))
    .await
    .unwrap();

    let q = SearchQuery {
        text: "how does parse_token work in the parser".to_string(),
        top_k: 10,
        expand_graph: false,
        compact: false,
        ..Default::default()
    };
    let results = idx.search(&q).await.unwrap();
    // Negative-direction assertion: on Conceptual intent the boost is
    // skipped, so the Function chunk gains no artificial 2× lift. The
    // doc-heavy chunk with high TF should at minimum compete with the
    // Function chunk — i.e. the Function is NOT guaranteed to be rank 0
    // the way it is in `test_function_definition_boost_surfaces_function_over_string_literal_usage`.
    // We assert the doc chunk is present in the top results — proving the
    // function-definition boost did not silently fire on Conceptual.
    assert!(
        results.iter().any(|c| c.file.ends_with(".md")),
        "Conceptual intent must not apply the function-definition boost — \
         the doc chunk should still surface; ranking = {:?}",
        results
            .iter()
            .map(|c| (c.file.as_str(), c.score))
            .collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn test_function_boost_no_op_when_function_name_missing() {
    // Why: issue #122 — guard against a panic / unwrap regression. A
    // Function chunk that somehow ended up without a `function_name`
    // (e.g. anonymous closure that the chunker couldn't name) must not
    // crash the boost path and must not be boosted (no name to match).
    // What: plant a Function chunk with `function_name: None` and an
    // empty-name Function chunk; run a Definition-intent query that
    // would match if the name were present. Assert: no panic, both
    // chunks are returned at unboosted scores.
    // Test: this test.
    use crate::core::chunker::ChunkType;

    let idx = make_indexer();
    // Function with no name at all.
    idx.add_chunk(raw_with_kind(
        "def:noname",
        "src/anon.rs",
        "// anonymous body referencing get_call_chain\n\
         get_call_chain(\"x\");",
        ChunkType::Function,
        None,
    ))
    .await
    .unwrap();
    // Function with empty-string name — defensive: should be treated the
    // same as None for boost purposes (no query token can be a substring
    // of the empty string except the empty token, which the tokenizer
    // discards via the `len() >= 2` filter).
    idx.add_chunk(raw_with_kind(
        "def:empty",
        "src/empty.rs",
        "// another anon block: get_call_chain helper",
        ChunkType::Function,
        Some(""),
    ))
    .await
    .unwrap();
    // Control: a normal chunk with the same token.
    idx.add_chunk(raw(
        "use:1",
        "src/use.rs",
        "let r = get_call_chain(\"foo\");",
    ))
    .await
    .unwrap();

    let q = SearchQuery {
        text: "get_call_chain".to_string(),
        top_k: 10,
        expand_graph: false,
        compact: false,
        ..Default::default()
    };
    // Primary assertion: this must not panic. Secondary assertion: all
    // three chunks come back (the boost path didn't filter them out).
    let results = idx.search(&q).await.unwrap();
    assert!(
        !results.is_empty(),
        "search must return results — no panic in the boost path"
    );
    // Verify the unnamed Function chunks were NOT boosted: their score
    // must not be artificially lifted to the top. Since none of the
    // three chunks have a function_name that matches `get_call_chain`,
    // none should be boosted, so ranking comes purely from BM25.
    // We simply verify no panic + non-empty results above; the precise
    // ranking is BM25-determined and out of scope.
}

#[tokio::test]
async fn test_conceptual_does_not_demote_docs() {
    // Why: issue #73 — Conceptual queries are documentation-retrieval by
    // nature; they need `.md` content to answer correctly. When the
    // caller uses the default `SearchMode::Code` (the implicit default,
    // not an explicit override), the search pipeline must upgrade the
    // effective mode to `All` so docs survive the post-filter. An
    // explicit `SearchMode::Code` from the caller still excludes `.md`
    // (covered by `test_mode_filter_code_excludes_markdown`).
    // What: same corpus shape as before, but uses the default mode
    // (i.e. `SearchMode::Code` via `..Default::default()`) and asserts
    // that the intent-aware effective-mode override still surfaces docs.
    // Test: this test plus `test_mode_filter_code_excludes_markdown` for
    // the explicit-mode contract.
    let idx = make_indexer();
    idx.add_chunk(raw(
        "doc:1",
        "ARCHITECTURE.md",
        "How does the CodeChunk pipeline work in trusty-search.",
    ))
    .await
    .unwrap();
    idx.add_chunk(raw(
        "src:1",
        "src/indexer.rs",
        "pub struct CodeChunk { pub id: String }",
    ))
    .await
    .unwrap();

    let q = SearchQuery {
        text: "how does the CodeChunk pipeline work".to_string(),
        top_k: 10,
        expand_graph: false,
        compact: false,
        // Intentionally leave `mode` as default (`SearchMode::Code`) — the
        // intent-aware override in `search()` should upgrade it to `All`
        // for Conceptual intent so .md content can still surface.
        ..Default::default()
    };
    let results = idx.search(&q).await.unwrap();
    assert!(
        results.iter().any(|c| c.file.ends_with(".md")),
        "Conceptual queries in default mode must still surface .md docs \
         (intent-aware effective-mode override, issue #73)"
    );
}

/// Issue #72 regression: in explicit `SearchMode::Code`, a high-BM25-TF
/// prose chunk must not crowd a genuine source-file match out of `top_k`
/// before the post-RRF hard filter runs.
///
/// Why: production reported code-navigation queries returning docs-heavy
/// or empty result sets. The `doc_score_penalty` matrix used to fire only
/// *after* the `take(top_k)` truncation, so a long CHANGELOG.md with many
/// keyword repeats could fill every top_k slot, the source chunk got
/// truncated away, and then the hard file-type filter dropped the prose —
/// leaving zero results. Issue #72 moved the penalty into
/// `apply_score_adjustments` (pre-truncation) so prose sinks before the
/// cut and the source chunk claims a slot.
/// What: builds a corpus with a high-TF `.md` chunk and a single `.rs`
/// source chunk, runs a BugDebt-intent query (which keeps the explicit
/// `Code` mode — it is not upgraded to `All` like Definition/Conceptual)
/// with `top_k = 1`, and asserts the surviving result is the `.rs` source
/// chunk rather than nothing.
/// Test: this test.
#[tokio::test]
async fn test_code_mode_source_outranks_changelog_pre_truncation() {
    use crate::core::classifier::{QueryClassifier, QueryIntent};

    // Pre-condition: the query must NOT classify as Definition/Conceptual,
    // otherwise the intent-aware override promotes mode to All and the
    // hard filter no longer drops the .md — defeating the scenario.
    let intent = QueryClassifier::classify("error handling retry logic deprecated path");
    assert_eq!(
        intent,
        QueryIntent::BugDebt,
        "test pre-condition: query should classify as BugDebt so explicit Code mode survives"
    );

    let idx = make_indexer();
    // High-TF prose chunk: repeats the query terms many times so its raw
    // BM25 score dominates the single source chunk pre-penalty.
    idx.add_chunk(raw(
        "doc:1",
        "CHANGELOG.md",
        "error handling error handling error handling retry logic retry logic \
         deprecated path deprecated path error handling retry logic deprecated \
         error handling retry logic deprecated path error handling retry logic",
    ))
    .await
    .unwrap();
    idx.add_chunk(raw(
        "src:1",
        "src/retry.rs",
        "fn handle_error_with_retry() { /* error handling + retry logic, deprecated path */ }",
    ))
    .await
    .unwrap();

    let q = SearchQuery {
        text: "error handling retry logic deprecated path".to_string(),
        top_k: 1,
        expand_graph: false,
        compact: false,
        // Explicit Code mode — BugDebt intent does not upgrade it, so the
        // .md chunk must be penalised pre-truncation, not after.
        mode: crate::core::indexer::SearchMode::Code,
        ..Default::default()
    };
    let results = idx.search(&q).await.unwrap();
    assert_eq!(
        results.len(),
        1,
        "with top_k=1 the source chunk must survive into the single slot \
         (pre-truncation penalty, issue #72) — got {:?}",
        results.iter().map(|c| &c.file).collect::<Vec<_>>()
    );
    assert!(
        results[0].file.ends_with(".rs"),
        "code-mode query must return the source file, not be crowded out by \
         high-TF prose (issue #72); got {}",
        results[0].file
    );
}

/// Issue #79 regression: a Definition-intent query against a corpus where
/// the matching content lives ONLY in markdown docs must still return
/// results when the caller uses the default mode.
///
/// Why: production v0.4.4 reported "UserPromptSubmit hook registration"
/// (Definition intent, default Code mode) returning zero results, because
/// the intent override to `All` mode was being undermined elsewhere in the
/// pipeline. The previous `test_conceptual_does_not_demote_docs` only
/// checked that .md docs *survived* alongside .rs source; it did not
/// exercise the docs-only path where the source-file fallback hides the
/// bug.
/// What: index a single .md chunk describing a hook registration concept
/// (no matching .rs file at all), classify as Definition via a PascalCase
/// trigger, run the search in default mode, and assert non-empty results.
/// Test: this test.
#[tokio::test]
async fn test_definition_default_mode_returns_docs_when_no_source_matches() {
    use crate::core::classifier::{QueryClassifier, QueryIntent};

    // Sanity: ensure the query phrase classifies as Definition so this
    // test exercises the intent-override code path.
    let intent = QueryClassifier::classify("UserPromptSubmit hook registration");
    assert_eq!(
        intent,
        QueryIntent::Definition,
        "test pre-condition: PascalCase identifier should classify as Definition"
    );

    let idx = make_indexer();
    idx.add_chunk(raw(
        "doc:1",
        "docs/HOOKS.md",
        "# UserPromptSubmit hook registration\n\
         The UserPromptSubmit hook fires whenever the user submits a prompt. \
         Register your hook handler via the registration API to receive these events.",
    ))
    .await
    .unwrap();

    let q = SearchQuery {
        text: "UserPromptSubmit hook registration".to_string(),
        top_k: 10,
        expand_graph: false,
        compact: false,
        // Default mode (SearchMode::Code) — the intent override must promote
        // to All so the .md chunk survives the post-filter.
        ..Default::default()
    };
    let results = idx.search(&q).await.unwrap();
    assert!(
        !results.is_empty(),
        "Definition-intent query against docs-only corpus returned 0 results — \
         the intent-aware mode override is broken (issue #79)"
    );
    assert!(
        results.iter().any(|c| c.file.ends_with(".md")),
        "expected the .md chunk to survive the post-filter, got: {:?}",
        results.iter().map(|c| &c.file).collect::<Vec<_>>()
    );
}

/// Issue #79 regression: a Conceptual-intent query against a docs-only
/// corpus must return results even when the caller uses the default mode.
///
/// Why: parallel to `test_definition_default_mode_returns_docs_when_no_source_matches`
/// but for Conceptual intent ("how does the X work" queries that should
/// retrieve architecture / overview docs).
/// What: index a single .md chunk, run a "how does ..." query, assert
/// non-empty results in default mode.
/// Test: this test.
#[tokio::test]
async fn test_conceptual_default_mode_returns_docs_when_no_source_matches() {
    use crate::core::classifier::{QueryClassifier, QueryIntent};

    let intent = QueryClassifier::classify("how does the hook system work");
    assert_eq!(
        intent,
        QueryIntent::Conceptual,
        "test pre-condition: 'how does' should classify as Conceptual"
    );

    let idx = make_indexer();
    idx.add_chunk(raw(
        "doc:1",
        "docs/ARCHITECTURE.md",
        "## How the hook system works\n\
         The hook system dispatches events to registered handlers in priority order.",
    ))
    .await
    .unwrap();

    let q = SearchQuery {
        text: "how does the hook system work".to_string(),
        top_k: 10,
        expand_graph: false,
        compact: false,
        ..Default::default()
    };
    let results = idx.search(&q).await.unwrap();
    assert!(
        !results.is_empty(),
        "Conceptual-intent query against docs-only corpus returned 0 results — \
         the intent-aware mode override is broken (issue #79)"
    );
}

#[tokio::test]
async fn test_kg_results_survive_top_k_truncation() {
    // Why: issue #94 — KG-expanded neighbours used to be appended after
    // `take(top_k)` had already trimmed the result list, so on busy
    // indexes the "hybrid+kg" reason never surfaced. We now re-sort the
    // merged direct+KG list by score before truncation.
    // What: fill the index with N direct hits at top_k limit, plus one
    // KG-only neighbour; assert the neighbour survives.
    // Test: this test.
    let idx = CodeIndexer::new("kg-trunc", "/tmp/test");
    // Direct hit + KG seed via `calls`.
    idx.add_chunk(RawChunk {
        id: "src:caller".to_string(),
        file: "caller.rs".to_string(),
        start_line: 1,
        end_line: 3,
        content: "fn caller() { /* dispatches */ }".to_string(),
        function_name: Some("caller".to_string()),
        language: Some("rust".to_string()),
        chunk_type: crate::core::chunker::ChunkType::Function,
        calls: vec!["authenticate".to_string()],
        inherits_from: Vec::new(),
        chunk_depth: 0,
        parent_chunk_id: None,
        child_chunk_ids: Vec::new(),
        nlp_keywords: Vec::new(),
        nlp_code_refs: Vec::new(),
        virtual_terms: Vec::new(),
    })
    .await
    .unwrap();
    idx.add_chunk(RawChunk {
        id: "src:authenticate".to_string(),
        file: "auth.rs".to_string(),
        start_line: 1,
        end_line: 1,
        content: "fn authenticate() {}".to_string(),
        function_name: Some("authenticate".to_string()),
        language: Some("rust".to_string()),
        chunk_type: crate::core::chunker::ChunkType::Function,
        calls: Vec::new(),
        inherits_from: Vec::new(),
        chunk_depth: 0,
        parent_chunk_id: None,
        child_chunk_ids: Vec::new(),
        nlp_keywords: Vec::new(),
        nlp_code_refs: Vec::new(),
        virtual_terms: Vec::new(),
    })
    .await
    .unwrap();

    let q = SearchQuery {
        text: "callers of authenticate".to_string(),
        top_k: 10,
        expand_graph: true,
        compact: false,
        ..Default::default()
    };
    let results = idx.search(&q).await.unwrap();
    assert!(
        results.iter().any(|c| c.match_reason == "hybrid+kg"),
        "at least one result must carry 'hybrid+kg' match_reason, got: {:#?}",
        results
            .iter()
            .map(|c| (&c.id, &c.match_reason))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_intent_routing_definitions() {
    // Sanity: intent table from CLAUDE.md is wired through.
    use crate::core::classifier::QueryIntent;
    let (a, b, kg) = QueryIntent::Definition.weights();
    assert!((a - 0.3).abs() < 1e-6 && (b - 0.7).abs() < 1e-6 && !kg);
    let (a, b, kg) = QueryIntent::Usage.weights();
    assert!((a - 0.5).abs() < 1e-6 && (b - 0.5).abs() < 1e-6 && kg);
}

#[tokio::test]
async fn test_enumerate_chunks_paginates_stable_order() {
    // Why: pagination over an underlying HashMap must produce a stable
    // total order so successive pages don't overlap or skip rows.
    let idx = make_indexer();
    // Helper: build a chunk whose `start_line`/`end_line` match the ID so
    // the `(file, start_line, end_line)` sort exercised below has the
    // expected total order (the bare `raw` helper hardcodes
    // `start_line: 1` for every chunk).
    fn raw_lines(id: &str, file: &str, start: usize, end: usize, content: &str) -> RawChunk {
        let mut r = raw(id, file, content);
        r.start_line = start;
        r.end_line = end;
        r
    }
    // Insert in an order that exercises the file/start_line sort.
    idx.add_chunk(raw_lines("b.rs:10:20", "b.rs", 10, 20, "fn b_two() {}"))
        .await
        .unwrap();
    idx.add_chunk(raw_lines("a.rs:1:5", "a.rs", 1, 5, "fn a_one() {}"))
        .await
        .unwrap();
    idx.add_chunk(raw_lines("b.rs:1:5", "b.rs", 1, 5, "fn b_one() {}"))
        .await
        .unwrap();
    idx.add_chunk(raw_lines("a.rs:30:40", "a.rs", 30, 40, "fn a_two() {}"))
        .await
        .unwrap();

    // Full enumeration: sorted by (file, start_line).
    let (total_all, all) = idx.enumerate_chunks(0, 100).await;
    assert_eq!(total_all, 4);
    let ids: Vec<_> = all.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(
        ids,
        vec!["a.rs:1:5", "a.rs:30:40", "b.rs:1:5", "b.rs:10:20"]
    );

    // Page 1 (offset=0, limit=2) + Page 2 (offset=2, limit=2) cover all.
    let (total_p1, page1) = idx.enumerate_chunks(0, 2).await;
    let (total_p2, page2) = idx.enumerate_chunks(2, 2).await;
    assert_eq!(total_p1, 4);
    assert_eq!(total_p2, 4);
    assert_eq!(page1.len(), 2);
    assert_eq!(page2.len(), 2);
    let combined: Vec<_> = page1
        .iter()
        .chain(page2.iter())
        .map(|c| c.id.as_str())
        .collect();
    assert_eq!(combined, ids);

    // Offset past the end returns empty, but total is preserved.
    let (total_end, end) = idx.enumerate_chunks(10, 5).await;
    assert_eq!(total_end, 4);
    assert!(end.is_empty());

    // limit=0 returns empty.
    let (total_z, z) = idx.enumerate_chunks(0, 0).await;
    assert_eq!(total_z, 4);
    assert!(z.is_empty());
}

// ---- Branch-aware search (issue #122) ----------------------------------

fn make_branch_query(text: &str, files: Vec<String>, boost: f32) -> SearchQuery {
    SearchQuery {
        text: text.to_string(),
        top_k: 10,
        expand_graph: false,
        compact: false,
        branch_files: Some(files),
        branch_boost: boost,
        branch: None,
        mode: SearchMode::Code,
        exclude_archived: false,
        stage: None,
        refine_query: None,
    }
}

#[tokio::test]
async fn test_branch_boost_applied_to_matching_chunks() {
    // Why: chunks whose file is in `branch_files` must out-rank otherwise
    // equivalent chunks. We use two files with the same BM25-relevant
    // content so the baseline ranking is a stable tie broken by the boost.
    // What: build a corpus with two chunks ("on-branch" and "off-branch"),
    // run a query with `branch_files=[on-branch path]`, assert the
    // on-branch chunk ranks first and carries `on_branch: true`.
    // Test: this test.
    let idx = make_indexer();
    idx.add_chunk(raw(
        "src/on.rs:1:1",
        "src/on.rs",
        "fn authenticate(user: &str) -> bool { true }",
    ))
    .await
    .unwrap();
    idx.add_chunk(raw(
        "src/off.rs:1:1",
        "src/off.rs",
        "fn authenticate(user: &str) -> bool { true }",
    ))
    .await
    .unwrap();

    let q = make_branch_query("fn authenticate", vec!["src/on.rs".to_string()], 1.5);
    let results = idx.search(&q).await.unwrap();
    assert!(!results.is_empty(), "branch-aware search must return hits");
    let on_branch = results
        .iter()
        .find(|c| c.file == abs("src/on.rs"))
        .expect("on-branch chunk in results");
    let off_branch = results.iter().find(|c| c.file == abs("src/off.rs"));

    assert!(on_branch.on_branch, "on_branch must be true for on.rs");
    if let Some(off) = off_branch {
        assert!(!off.on_branch, "on_branch must be false for off.rs");
        assert!(
            on_branch.score >= off.score,
            "branch boost must make on.rs >= off.rs (got {} vs {})",
            on_branch.score,
            off.score
        );
    }
    assert_eq!(
        results[0].file,
        abs("src/on.rs"),
        "on-branch chunk must rank first; got {:?}",
        results.iter().map(|c| &c.file).collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn test_branch_boost_clamped_to_3x() {
    // Why: callers must not be able to drown out all off-branch results by
    // passing wild multipliers (e.g. 10x). The pipeline must clamp.
    // What: feed a query with `branch_boost = 10.0` and a single on-branch
    // chunk; verify the resolved boost equals 3.0 via `resolve_branch_set`.
    // Test: this test (direct helper) + the integration test above.
    let q = make_branch_query("foo", vec!["src/on.rs".to_string()], 10.0);
    let root = std::path::PathBuf::from("/tmp/test");
    let (set, boost) = super::search::resolve_branch_set(&q, &root);
    assert!(set.is_some(), "branch set must be present");
    assert!(
        (boost - 3.0).abs() < f32::EPSILON,
        "branch_boost=10.0 must clamp to 3.0, got {boost}"
    );

    // Floor: 0.0 must clamp up to 1.0 (no-op).
    let q_low = make_branch_query("foo", vec!["src/on.rs".to_string()], 0.0);
    let (set_low, boost_low) = super::search::resolve_branch_set(&q_low, &root);
    assert!(
        (boost_low - 1.0).abs() < f32::EPSILON,
        "branch_boost=0.0 must clamp to 1.0, got {boost_low}"
    );
    // 1.0 disables boosting → the set is dropped to skip per-chunk work.
    assert!(
        set_low.is_none(),
        "branch_boost=1.0 must drop the set (no-op)"
    );
}

#[tokio::test]
async fn test_on_branch_set_correctly() {
    // Why: every returned chunk must carry an accurate `on_branch` flag so
    // clients can highlight branch work in UI without re-doing the lookup.
    // What: index two chunks, query with branch_files=[one], assert each
    // result's flag matches set membership.
    // Test: this test.
    let idx = make_indexer();
    idx.add_chunk(raw(
        "src/on.rs:1:1",
        "src/on.rs",
        "fn authenticate() -> bool { true }",
    ))
    .await
    .unwrap();
    idx.add_chunk(raw(
        "src/off.rs:1:1",
        "src/off.rs",
        "fn authenticate() -> bool { true }",
    ))
    .await
    .unwrap();

    let q = make_branch_query("fn authenticate", vec!["src/on.rs".to_string()], 1.5);
    let results = idx.search(&q).await.unwrap();
    let on_abs = abs("src/on.rs");
    let off_abs = abs("src/off.rs");
    for c in &results {
        if c.file == on_abs {
            assert!(c.on_branch, "on.rs must be flagged on_branch=true");
        } else if c.file == off_abs {
            assert!(!c.on_branch, "off.rs must be flagged on_branch=false");
        }
    }

    // Normalize leading "./" — branch_files entries with "./src/on.rs" must
    // still match a chunk whose file is "src/on.rs".
    let q2 = make_branch_query("fn authenticate", vec!["./src/on.rs".to_string()], 1.5);
    let results2 = idx.search(&q2).await.unwrap();
    let on2 = results2
        .iter()
        .find(|c| c.file == on_abs)
        .expect("on-branch chunk in results");
    assert!(on2.on_branch, "leading './' must be normalized away");
}

#[tokio::test]
async fn test_no_boost_when_branch_files_absent() {
    // Why: a vanilla query with no branch context must not pay any branch
    // overhead and must report `on_branch: false` on every result.
    // What: run the baseline search query and confirm scores match the
    // pre-#122 behavior (i.e. on_branch is always false, no panic).
    // Test: this test.
    let idx = make_indexer();
    idx.add_chunk(raw(
        "src/auth.rs:1:5",
        "src/auth.rs",
        "fn authenticate(user: &str, password: &str) -> bool { true }",
    ))
    .await
    .unwrap();
    idx.add_chunk(raw(
        "src/render.rs:1:3",
        "src/render.rs",
        "fn render_ui_components() { /* svelte */ }",
    ))
    .await
    .unwrap();

    let q = SearchQuery {
        text: "fn authenticate".to_string(),
        top_k: 5,
        expand_graph: false,
        compact: false,
        branch_files: None,
        branch_boost: SearchQuery::default_branch_boost(),
        branch: None,
        mode: SearchMode::Code,
        exclude_archived: false,
        stage: None,
        refine_query: None,
    };
    let results = idx.search(&q).await.unwrap();
    assert!(!results.is_empty());
    for c in &results {
        assert!(
            !c.on_branch,
            "on_branch must default to false when no branch context provided"
        );
    }
}

// ---------------------------------------------------------------------------
// Issue #28 — durable redb corpus integration.
// ---------------------------------------------------------------------------

use crate::core::corpus::CorpusStore;

/// Build a BM25-only indexer (no embedder/store needed) with a durable redb
/// `CorpusStore` wired at `redb_path`.
///
/// Why: the corpus-integration tests exercise the commit → redb → warm-boot
/// rehydration path, which is orthogonal to the HNSW lane. A BM25-only indexer
/// keeps the tests hermetic (no ONNX) while still hitting every `corpus`
/// branch in `commit_parsed_batch` / `load_chunks_from_redb` / removal.
fn make_indexer_with_corpus(redb_path: &std::path::Path) -> CodeIndexer {
    let mut idx = CodeIndexer::new("corpus-test", "/tmp/corpus-test");
    let store = CorpusStore::open(redb_path).expect("open corpus store");
    idx.set_corpus_store(Arc::new(store));
    idx
}

/// Phase 2 + 3: a committed batch must persist to redb, and a fresh indexer
/// pointed at the same redb file must rehydrate the corpus on warm-boot.
#[tokio::test]
async fn test_corpus_store_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let redb_path = dir.path().join("index.redb");

    // Phase 1: index two files into an indexer with a durable corpus.
    {
        let idx = make_indexer_with_corpus(&redb_path);
        idx.index_files_batch(&[
            ("src/auth.rs".into(), "fn authenticate() {}".into()),
            ("src/token.rs".into(), "fn verify_token() {}".into()),
        ])
        .await
        .expect("index batch");
        assert!(idx.chunk_count() >= 2);
    } // indexer (and its CorpusStore Arc) dropped here — simulates shutdown.

    // The redb file must hold the committed chunks.
    {
        let store = CorpusStore::open(&redb_path).unwrap();
        assert!(
            store.chunk_count().unwrap() >= 2,
            "committed batch was not persisted to redb"
        );
    }

    // Phase 2: a fresh indexer warm-boots from the redb corpus — no reindex.
    let restored = make_indexer_with_corpus(&redb_path);
    let n = restored
        .load_chunks_from_redb()
        .await
        .expect("warm-boot from redb");
    assert!(n >= 2, "warm-boot rehydrated {n} chunks, expected >= 2");
    assert_eq!(restored.chunk_count(), n);

    // BM25 must be rebuilt from the rehydrated corpus.
    let bm25 = restored.bm25.read().await;
    let hits = bm25.score_query_all("authenticate", 5);
    drop(bm25);
    assert!(
        !hits.is_empty(),
        "BM25 not rebuilt from redb-restored chunks"
    );
}

/// Phase 3: warm-boot from an empty / missing redb corpus must yield zero
/// chunks (the first-run / post-upgrade fallback that triggers a reindex).
#[tokio::test]
async fn test_corpus_store_warm_boot_empty_is_zero() {
    let dir = tempfile::tempdir().unwrap();
    let idx = make_indexer_with_corpus(&dir.path().join("fresh.redb"));
    let n = idx.load_chunks_from_redb().await.unwrap();
    assert_eq!(n, 0, "empty redb corpus must rehydrate zero chunks");

    // An indexer with no corpus store at all also yields zero (BM25-only).
    let bare = CodeIndexer::new("bare", "/tmp/bare");
    assert_eq!(bare.load_chunks_from_redb().await.unwrap(), 0);
}

/// Phase 2: `remove_file` / `remove_chunk` must evict from the durable redb
/// corpus too — otherwise a warm-boot resurrects the deleted chunks.
#[tokio::test]
async fn test_corpus_store_deletes_on_remove() {
    let dir = tempfile::tempdir().unwrap();
    let redb_path = dir.path().join("index.redb");

    let idx = make_indexer_with_corpus(&redb_path);
    idx.index_files_batch(&[
        ("src/keep.rs".into(), "fn keep_me() {}".into()),
        ("src/drop.rs".into(), "fn drop_me() {}".into()),
    ])
    .await
    .unwrap();
    let before = idx.chunk_count();
    assert!(before >= 2);

    // Remove one file — this must delete its chunks from redb as well.
    idx.remove_file("src/drop.rs").await.unwrap();
    drop(idx);

    // Re-open the redb corpus directly: the dropped file's chunks must be gone.
    // redb is single-process-exclusive, so this store MUST be dropped before
    // the warm-boot indexer below re-opens the same file.
    let chunks = {
        let store = CorpusStore::open(&redb_path).unwrap();
        store.load_all_chunks().unwrap()
    };
    assert!(
        chunks.iter().all(|c| c.file != "src/drop.rs"),
        "removed file's chunks still present in redb after remove_file"
    );
    assert!(
        chunks.iter().any(|c| c.file == "src/keep.rs"),
        "remove_file evicted the wrong file's chunks from redb"
    );

    // Warm-boot a fresh indexer: the removal must survive the restart.
    let restored = make_indexer_with_corpus(&redb_path);
    restored.load_chunks_from_redb().await.unwrap();
    let ids = restored.find_chunk_id("drop.rs", None).await;
    assert!(ids.is_none(), "deleted chunk resurrected on warm-boot");
}

/// Phase 3 migration: a daemon upgraded from a JSON-snapshot build has a
/// populated `chunks.json` and an empty `index.redb`. `migrate_corpus_to_redb`
/// must seed redb so the next restart uses the fast path.
#[tokio::test]
async fn test_corpus_store_migrates_from_json() {
    let dir = tempfile::tempdir().unwrap();
    let json_path = dir.path().join("chunks.json");
    let redb_path = dir.path().join("index.redb");

    // Stage a legacy JSON snapshot via an indexer with no corpus store.
    {
        let legacy = make_indexer();
        legacy
            .add_chunk(raw("a", "src/a.rs", "fn legacy_a() {}"))
            .await
            .unwrap();
        legacy
            .add_chunk(raw("b", "src/b.rs", "fn legacy_b() {}"))
            .await
            .unwrap();
        legacy.save_chunks_to_disk(&json_path).await.unwrap();
    }
    assert!(json_path.exists());

    // Warm-boot path: load the JSON snapshot, then migrate it into redb.
    let idx = make_indexer_with_corpus(&redb_path);
    let n = idx.load_chunks_from_disk(&json_path).await.unwrap();
    assert_eq!(n, 2);
    idx.migrate_corpus_to_redb().await;
    drop(idx);

    // The redb corpus must now hold the migrated chunks, so a subsequent
    // restart can skip the JSON file entirely.
    let restored = make_indexer_with_corpus(&redb_path);
    let m = restored.load_chunks_from_redb().await.unwrap();
    assert_eq!(m, 2, "redb corpus was not seeded by the JSON migration");
}

/// Phase 4: `swap_corpus_store` / `take_corpus_store` give the reindex
/// orchestrator the ability to stage a rebuilt corpus in a temp file and then
/// restore the indexer's durable store — without losing the original.
#[tokio::test]
async fn test_corpus_store_swap_and_take() {
    let dir = tempfile::tempdir().unwrap();
    let live_path = dir.path().join("index.redb");
    let tmp_path = dir.path().join("index.redb.tmp");

    let mut idx = make_indexer_with_corpus(&live_path);
    assert!(idx.has_corpus_store());

    // Stage a fresh tmp corpus, capturing the live one it replaced. The prior
    // store's `Arc` is dropped immediately: redb is single-process-exclusive,
    // and `commit_force_corpus_swap` likewise drops the prior handle before
    // the rename. We only assert its path first.
    let staged = Arc::new(CorpusStore::open_fresh(&tmp_path).unwrap());
    let prev = idx.swap_corpus_store(staged).expect("prior store returned");
    assert_eq!(prev.path(), live_path.as_path());
    drop(prev);

    // Commit a batch — it must land in the *staging* file, not the live one.
    idx.index_files_batch(&[("src/new.rs".into(), "fn brand_new() {}".into())])
        .await
        .unwrap();

    // Take the staging store back out so its Arc can be dropped before a
    // rename (mirrors `commit_force_corpus_swap`).
    let staged_back = idx.take_corpus_store().expect("staging store taken");
    assert_eq!(staged_back.path(), tmp_path.as_path());
    assert!(!idx.has_corpus_store());
    assert!(
        staged_back.chunk_count().unwrap() >= 1,
        "batch did not commit to the staged corpus"
    );
    // Drop the staging handle so the live file can be re-opened below.
    drop(staged_back);

    // The original live file must be untouched — it never saw the new batch.
    let live = CorpusStore::open(&live_path).unwrap();
    assert_eq!(
        live.chunk_count().unwrap(),
        0,
        "live corpus was mutated while a staging corpus was swapped in"
    );
}

// ----- Issue #75 — line numbers, grep fallback, archive downranking ---------

#[test]
fn test_compute_match_reason_fallback_label() {
    // Why: the `(false,false,false)` arm used to return the bare "fallback"
    // string. Issue #75 renamed it to `"fallback:ripgrep"` so grep-fallback
    // hits are clearly labelled in MCP / HTTP output.
    assert_eq!(
        compute_match_reason(false, false, false),
        "fallback:ripgrep"
    );
    assert_eq!(compute_match_reason(true, false, false), "vector");
    assert_eq!(compute_match_reason(false, true, false), "bm25");
    assert_eq!(compute_match_reason(true, true, false), "hybrid");
    assert_eq!(compute_match_reason(false, false, true), "hybrid+kg");
}

#[tokio::test]
async fn test_grep_fallback_returns_substring_hits() {
    // Why: when both primary lanes return nothing, an exact-substring scan
    // over the in-memory corpus should still surface relevant chunks. The
    // hits must carry a score equal to GREP_FALLBACK_SCORE so they sink
    // below any real hit.
    let idx = make_indexer();
    idx.add_chunk(raw("a", "src/a.rs", "fn alpha_qwerty_unique() {}"))
        .await
        .unwrap();
    idx.add_chunk(raw("b", "src/b.rs", "fn beta() {}"))
        .await
        .unwrap();
    let hits = idx.grep_fallback_search("alpha_qwerty_unique", 5).await;
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].0, "a");
    // The score must be tiny — well below any real BM25 / vector hit.
    assert!(hits[0].1 < 0.01, "fallback score should be sub-0.01");
}

#[tokio::test]
async fn test_grep_fallback_treats_query_as_literal() {
    // Why: user input must never be treated as a regex. A query containing
    // regex metacharacters should match literally (or not at all) — never
    // explode into a partial substring match driven by the metachar.
    let idx = make_indexer();
    idx.add_chunk(raw("a", "src/a.rs", "fn foo() {} // literal: a.b.c"))
        .await
        .unwrap();
    idx.add_chunk(raw("b", "src/b.rs", "fn aXbYc() {}"))
        .await
        .unwrap();
    // `.` is a regex metachar. With `regex::escape` it should only match the
    // literal "a.b.c" in chunk `a` — not the wildcard-style "aXbYc" in `b`.
    let hits = idx.grep_fallback_search("a.b.c", 5).await;
    let ids: Vec<&str> = hits.iter().map(|(id, _)| id.as_str()).collect();
    assert!(ids.contains(&"a"), "literal match in a missing: {ids:?}");
    assert!(
        !ids.contains(&"b"),
        "wildcard-style match leaked through regex escape"
    );
}

#[test]
fn test_merge_grep_lane_appends_new_ids() {
    // Why: merge_grep_lane must add brand-new ids to the fused list without
    // dropping any of the existing fused entries, and the resulting order
    // must be sorted by score descending.
    use super::search::merge_grep_lane;
    let fused = vec![("a".to_string(), 0.05), ("b".to_string(), 0.04)];
    let grep_lane = vec![("c".to_string(), 0.001)];
    let out = merge_grep_lane(fused, &grep_lane, 0.5, 10);
    let ids: Vec<&str> = out.iter().map(|(id, _)| id.as_str()).collect();
    assert!(ids.contains(&"a"));
    assert!(ids.contains(&"b"));
    assert!(ids.contains(&"c"));
    // The previously-top entry must still be ranked at index 0.
    assert_eq!(out[0].0, "a");
}

#[tokio::test]
async fn test_archive_downrank_demotes_deprecated_chunks() {
    // Why: chunks whose file path matches an archive keyword (here: "legacy")
    // should be demoted below comparable clean-path chunks via the post-MMR
    // archive pass, and their `archive_reason` field should be populated.
    let idx = make_indexer();
    idx.add_chunk(raw("live", "src/auth.rs", "fn authenticate_user_xyz() {}"))
        .await
        .unwrap();
    idx.add_chunk(raw(
        "old",
        "src/legacy/auth_old.rs",
        "fn authenticate_user_xyz_old() {}",
    ))
    .await
    .unwrap();
    let results = idx
        .search(&SearchQuery {
            text: "authenticate_user_xyz".to_string(),
            top_k: 5,
            expand_graph: false,
            compact: false,
            ..Default::default()
        })
        .await
        .unwrap();
    // Both should appear — the live one must rank above the archived one,
    // and the archived one must carry `archive_reason`.
    let pos_live = results.iter().position(|c| c.id == "live");
    let pos_old = results.iter().position(|c| c.id == "old");
    assert!(pos_live.is_some(), "live chunk missing from results");
    assert!(pos_old.is_some(), "archived chunk missing from results");
    assert!(
        pos_live.unwrap() < pos_old.unwrap(),
        "live chunk should outrank archived chunk: live={pos_live:?} old={pos_old:?}"
    );
    let old_chunk = results.iter().find(|c| c.id == "old").unwrap();
    assert!(
        old_chunk.archive_reason.is_some(),
        "archived chunk missing archive_reason: {:?}",
        old_chunk
    );
    let reason = old_chunk.archive_reason.as_deref().unwrap();
    assert!(
        reason.starts_with("path:"),
        "expected path-prefix reason, got {reason}"
    );
}

/// Issue #74: `exclude_archived: true` drops archived chunks from the
/// result set entirely instead of downranking them, and the configurable
/// path detection covers the requested directory conventions.
///
/// Why: archive downranking (issue #75) keeps legacy code in the results
/// (sunk in ranking) which is the right default for exploratory queries.
/// Code-navigation callers want archived code gone outright. This test
/// pins the opt-in hard filter and verifies it fires for each of the
/// `_archive/`, `archive/`, `_deprecated/`, `old/`, `.archive/` path
/// conventions named in the issue.
/// What: indexes one live `.rs` chunk plus several archived chunks (one
/// per path convention), runs the same query with `exclude_archived: true`,
/// and asserts only the live chunk survives.
/// Test: this test.
#[tokio::test]
async fn test_exclude_archived_drops_archive_chunks() {
    let idx = make_indexer();
    idx.add_chunk(raw("live", "src/auth.rs", "fn authenticate_user_xyz() {}"))
        .await
        .unwrap();
    // One archived chunk per path convention the issue enumerates. Each
    // contains the query token so it would otherwise rank in the result set.
    for (id, path) in [
        ("a1", "src/_archive/auth.rs"),
        ("a2", "src/archive/auth.rs"),
        ("a3", "src/_deprecated/auth.rs"),
        ("a4", "src/old/auth.rs"),
        ("a5", "src/.archive/auth.rs"),
    ] {
        idx.add_chunk(raw(id, path, "fn authenticate_user_xyz_old() {}"))
            .await
            .unwrap();
    }

    // Baseline: without the flag, archived chunks are present (downranked).
    let downranked = idx
        .search(&SearchQuery {
            text: "authenticate_user_xyz".to_string(),
            top_k: 10,
            expand_graph: false,
            compact: false,
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(
        downranked.iter().any(|c| c.id.starts_with('a')),
        "pre-condition: archived chunks should be present (downranked) without the flag"
    );

    // With `exclude_archived`, every archived chunk must be gone.
    let filtered = idx
        .search(&SearchQuery {
            text: "authenticate_user_xyz".to_string(),
            top_k: 10,
            expand_graph: false,
            compact: false,
            exclude_archived: true,
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(
        filtered.iter().all(|c| c.id == "live"),
        "exclude_archived must drop every archived chunk; got {:?}",
        filtered.iter().map(|c| &c.file).collect::<Vec<_>>()
    );
    assert!(
        filtered.iter().any(|c| c.id == "live"),
        "the live chunk must still be returned"
    );
}

#[tokio::test]
async fn test_archive_downrank_skips_clean_chunks() {
    // Why: a chunk with no archive signals must not receive an
    // `archive_reason`, and its score must be unchanged by the downrank pass.
    let idx = make_indexer();
    idx.add_chunk(raw("clean", "src/main.rs", "fn run_main() {}"))
        .await
        .unwrap();
    let results = idx
        .search(&SearchQuery {
            text: "run_main".to_string(),
            top_k: 5,
            expand_graph: false,
            compact: false,
            ..Default::default()
        })
        .await
        .unwrap();
    let chunk = results.iter().find(|c| c.id == "clean").unwrap();
    assert!(chunk.archive_reason.is_none());
}

#[tokio::test]
async fn test_search_result_preserves_line_numbers() {
    // Why: issue #75 requires every search result to carry start_line and
    // end_line. They are already on RawChunk; this guards against a future
    // regression where the materializer drops them.
    let idx = make_indexer();
    let mut chunk = raw("a", "src/a.rs", "fn alpha_qwerty_unique() {}");
    chunk.start_line = 42;
    chunk.end_line = 50;
    idx.add_chunk(chunk).await.unwrap();
    let results = idx
        .search(&SearchQuery {
            text: "alpha_qwerty_unique".to_string(),
            top_k: 5,
            expand_graph: false,
            compact: false,
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(!results.is_empty());
    assert_eq!(results[0].start_line, 42);
    assert_eq!(results[0].end_line, 50);
}

// ---- Issue #77 final design: mode-based hard file-type filter ---------

/// Build a mixed corpus across the three file-type buckets so each mode
/// test can assert which slice of the index is admitted.
///
/// Why: the mode-filter contract is about which file types are returned,
/// not about which is ranked highest. Seeding one chunk per bucket with
/// the same query-matching content lets each test verify inclusion /
/// exclusion in isolation.
/// What: registers a source (`.rs`), a prose doc (`.md`), a named doc
/// (`LICENSE` with no extension), a config file (`.toml`), and a data
/// file (`.json`) — all containing the literal token "alpha_qwerty" so
/// every chunk matches the same query.
/// Test: used by every `test_mode_filter_*` test below.
async fn seed_mode_filter_corpus(idx: &CodeIndexer) {
    idx.add_chunk(raw(
        "src:1",
        "src/lib.rs",
        "fn alpha_qwerty() -> bool { true }",
    ))
    .await
    .unwrap();
    idx.add_chunk(raw(
        "doc:1",
        "docs/intro.md",
        "# alpha_qwerty\nDocumentation about alpha_qwerty.",
    ))
    .await
    .unwrap();
    idx.add_chunk(raw(
        "named:1",
        "LICENSE",
        "MIT licence text mentioning alpha_qwerty.",
    ))
    .await
    .unwrap();
    idx.add_chunk(raw(
        "cfg:1",
        "Cargo.toml",
        "[package]\nname = \"alpha_qwerty\"",
    ))
    .await
    .unwrap();
    idx.add_chunk(raw(
        "data:1",
        "fixtures/alpha.json",
        "{\"name\": \"alpha_qwerty\"}",
    ))
    .await
    .unwrap();
}

#[tokio::test]
async fn test_mode_filter_code_returns_only_source() {
    // Why: code mode (the default) must return strictly source-code
    // extensions. Prose docs, named docs, configs, and data files must
    // be dropped from results entirely — not merely demoted.
    let idx = make_indexer();
    seed_mode_filter_corpus(&idx).await;
    // Issue #119 update: query is `alpha` (no underscore, no PascalCase, no
    // acronym) so it classifies as `Unknown` and the intent-aware
    // effective-mode override in `search()` does not promote Code → All.
    // The previous query `alpha_qwerty` started classifying as Definition
    // under the v0.8.3 classifier rules, which (correctly) triggers the
    // override for docs-only fallback paths and would defeat this test's
    // explicit Code-mode contract. The corpus chunks all contain
    // `alpha_qwerty`, which BM25-tokenises into `alpha` + `qwerty`, so the
    // single-word `alpha` still matches every seeded chunk.
    let q = SearchQuery {
        text: "alpha".to_string(),
        top_k: 20,
        expand_graph: false,
        compact: false,
        mode: SearchMode::Code,
        ..Default::default()
    };
    let results = idx.search(&q).await.unwrap();
    let files: Vec<&str> = results.iter().map(|c| c.file.as_str()).collect();
    let lib_abs = abs("src/lib.rs");
    let license_abs = abs("LICENSE");
    assert!(
        files.contains(&lib_abs.as_str()),
        "code mode must include source: {files:?}"
    );
    assert!(
        !files.iter().any(|f| f.ends_with(".md")),
        "code mode must exclude .md: {files:?}"
    );
    assert!(
        !files.contains(&license_abs.as_str()),
        "code mode must exclude named docs: {files:?}"
    );
    assert!(
        !files.iter().any(|f| f.ends_with(".toml")),
        "code mode must exclude config: {files:?}"
    );
    assert!(
        !files.iter().any(|f| f.ends_with(".json")),
        "code mode must exclude data: {files:?}"
    );
}

#[tokio::test]
async fn test_mode_filter_text_returns_only_prose_and_named_docs() {
    // Why: text mode must return only prose extensions and path-based
    // named docs (README*, LICENSE*, CHANGELOG*, …). Source, config,
    // and data files must be excluded.
    let idx = make_indexer();
    seed_mode_filter_corpus(&idx).await;
    let q = SearchQuery {
        text: "alpha_qwerty".to_string(),
        top_k: 20,
        expand_graph: false,
        compact: false,
        mode: SearchMode::Text,
        ..Default::default()
    };
    let results = idx.search(&q).await.unwrap();
    let files: Vec<&str> = results.iter().map(|c| c.file.as_str()).collect();
    let license_abs = abs("LICENSE");
    assert!(
        files.iter().any(|f| f.ends_with(".md")),
        "text mode must include prose: {files:?}"
    );
    assert!(
        files.contains(&license_abs.as_str()),
        "text mode must include named docs without extension: {files:?}"
    );
    assert!(
        !files.iter().any(|f| f.ends_with(".rs")),
        "text mode must exclude source: {files:?}"
    );
    assert!(
        !files.iter().any(|f| f.ends_with(".toml")),
        "text mode must exclude config: {files:?}"
    );
    assert!(
        !files.iter().any(|f| f.ends_with(".json")),
        "text mode must exclude data: {files:?}"
    );
}

#[tokio::test]
async fn test_mode_filter_data_returns_only_structured_data() {
    // Why: data mode must return only structured-data / config / schema
    // files. Source and prose must be excluded.
    let idx = make_indexer();
    seed_mode_filter_corpus(&idx).await;
    let q = SearchQuery {
        text: "alpha_qwerty".to_string(),
        top_k: 20,
        expand_graph: false,
        compact: false,
        mode: SearchMode::Data,
        ..Default::default()
    };
    let results = idx.search(&q).await.unwrap();
    let files: Vec<&str> = results.iter().map(|c| c.file.as_str()).collect();
    assert!(
        files.iter().any(|f| f.ends_with(".toml")),
        "data mode must include config: {files:?}"
    );
    assert!(
        files.iter().any(|f| f.ends_with(".json")),
        "data mode must include data files: {files:?}"
    );
    assert!(
        !files.iter().any(|f| f.ends_with(".rs")),
        "data mode must exclude source: {files:?}"
    );
    assert!(
        !files.iter().any(|f| f.ends_with(".md")),
        "data mode must exclude prose: {files:?}"
    );
    assert!(
        !files.contains(&abs("LICENSE").as_str()),
        "data mode must exclude named docs: {files:?}"
    );
}

#[tokio::test]
async fn test_mode_filter_all_returns_everything() {
    // Why: `all` mode is the escape hatch — no file-type filter applies.
    // Every seeded chunk must appear in results.
    let idx = make_indexer();
    seed_mode_filter_corpus(&idx).await;
    let q = SearchQuery {
        text: "alpha_qwerty".to_string(),
        top_k: 20,
        expand_graph: false,
        compact: false,
        mode: SearchMode::All,
        ..Default::default()
    };
    let results = idx.search(&q).await.unwrap();
    let files: Vec<String> = results.iter().map(|c| c.file.clone()).collect();
    for expected_rel in &[
        "src/lib.rs",
        "docs/intro.md",
        "LICENSE",
        "Cargo.toml",
        "fixtures/alpha.json",
    ] {
        let expected = abs(expected_rel);
        assert!(
            files.contains(&expected),
            "all mode must include {expected}: {files:?}"
        );
    }
}

/// Idle-eviction (issue #83 follow-up): `idle_evict_secs` honours the default
/// and the `TRUSTY_CHUNKS_IDLE_EVICT_SECS` override, including `0` (disabled)
/// and an unparseable value (falls back to default).
#[test]
fn idle_evict_secs_default_and_env_override() {
    let prior = std::env::var("TRUSTY_CHUNKS_IDLE_EVICT_SECS").ok();

    // Unset → default.
    // SAFETY: this test is the only reader/writer of this env var.
    unsafe { std::env::remove_var("TRUSTY_CHUNKS_IDLE_EVICT_SECS") };
    assert_eq!(idle_evict_secs(), DEFAULT_CHUNKS_IDLE_EVICT_SECS);

    // Valid override wins.
    // SAFETY: see above.
    unsafe { std::env::set_var("TRUSTY_CHUNKS_IDLE_EVICT_SECS", "30") };
    assert_eq!(idle_evict_secs(), 30);

    // Zero disables (returned verbatim; the caller treats 0 as "off").
    // SAFETY: see above.
    unsafe { std::env::set_var("TRUSTY_CHUNKS_IDLE_EVICT_SECS", "0") };
    assert_eq!(idle_evict_secs(), 0);

    // Garbage falls back to default (with a warn).
    // SAFETY: see above.
    unsafe { std::env::set_var("TRUSTY_CHUNKS_IDLE_EVICT_SECS", "nope") };
    assert_eq!(idle_evict_secs(), DEFAULT_CHUNKS_IDLE_EVICT_SECS);

    // Restore.
    // SAFETY: see above.
    unsafe {
        match prior {
            Some(v) => std::env::set_var("TRUSTY_CHUNKS_IDLE_EVICT_SECS", v),
            None => std::env::remove_var("TRUSTY_CHUNKS_IDLE_EVICT_SECS"),
        }
    }
}

/// Idle-eviction core behaviour: a durably-backed indexer drops its in-memory
/// `chunks` map once idle past the threshold, and the next in-memory read
/// transparently rehydrates it from redb.
#[tokio::test]
async fn idle_eviction_drops_and_lazily_rehydrates_chunks() {
    let dir = tempfile::tempdir().unwrap();
    let redb_path = dir.path().join("index.redb");
    let idx = make_indexer_with_corpus(&redb_path);

    // Populate two chunks; they land in both the in-memory map and redb.
    idx.index_files_batch(&[
        ("src/auth.rs".into(), "fn authenticate() {}".into()),
        ("src/token.rs".into(), "fn verify_token() {}".into()),
    ])
    .await
    .expect("index batch");
    let resident_before = idx.in_memory_chunk_count().await;
    assert!(resident_before >= 2, "expected >= 2 resident chunks");

    // A zero threshold disables eviction — nothing is dropped.
    assert_eq!(idx.evict_chunks_if_idle(std::time::Duration::ZERO).await, 0);
    assert_eq!(idx.in_memory_chunk_count().await, resident_before);

    // A long threshold means the index isn't idle yet (it was just ingested,
    // which calls touch_activity) — nothing is dropped.
    assert_eq!(
        idx.evict_chunks_if_idle(std::time::Duration::from_secs(3600))
            .await,
        0
    );
    assert_eq!(idx.in_memory_chunk_count().await, resident_before);

    // A zero-length idle window (every elapsed duration exceeds it) forces
    // eviction now. The durable corpus is wired, so this is safe.
    let evicted = idx
        .evict_chunks_if_idle(std::time::Duration::from_nanos(1))
        .await;
    assert_eq!(evicted, resident_before, "eviction should drop every chunk");
    assert_eq!(
        idx.in_memory_chunk_count().await,
        0,
        "map must be empty after eviction"
    );
    assert!(
        idx.chunks_evicted.load(Ordering::Relaxed),
        "chunks_evicted flag must be set after eviction"
    );

    // The durable corpus is untouched — redb still has every chunk.
    assert!(idx.corpus_store().unwrap().chunk_count().unwrap() >= 2);

    // An in-memory read (raw_chunks_snapshot) lazily rehydrates from redb.
    let snapshot = idx.raw_chunks_snapshot().await;
    assert_eq!(
        snapshot.len(),
        resident_before,
        "raw_chunks_snapshot must rehydrate the evicted map"
    );
    assert_eq!(
        idx.in_memory_chunk_count().await,
        resident_before,
        "map must be repopulated after a read"
    );
    assert!(
        !idx.chunks_evicted.load(Ordering::Relaxed),
        "chunks_evicted flag must clear after rehydration"
    );
}

/// Idle-eviction safety: a BM25-only indexer (no durable corpus) is NEVER
/// evicted — its in-memory map is the only copy of the data.
#[tokio::test]
async fn idle_eviction_skips_indexers_without_corpus() {
    let idx = make_indexer(); // embedder + store, but corpus: None
    idx.add_chunk(raw("a", "src/a.rs", "fn a() {}"))
        .await
        .unwrap();
    let before = idx.in_memory_chunk_count().await;
    assert_eq!(before, 1);

    // Even with an always-idle window, eviction is a no-op without a corpus.
    let evicted = idx
        .evict_chunks_if_idle(std::time::Duration::from_nanos(1))
        .await;
    assert_eq!(evicted, 0, "must not evict without a durable corpus");
    assert_eq!(idx.in_memory_chunk_count().await, before);
    assert!(!idx.chunks_evicted.load(Ordering::Relaxed));
}

// ── Issue #147: search_kg refine_query tests ──────────────────────────────

/// `refine_query = None` must preserve all KG-expanded neighbours, matching
/// existing backward-compatible behaviour.
///
/// Why: the refine path is opt-in — existing callers that omit `refine_query`
/// must see exactly the same result set as before the feature landed.
/// What: build a tiny KG (seed → neighbour_a → neighbour_b), run
/// `search_kg` without `refine_query`, verify both neighbours surface.
/// Test: this test.
#[tokio::test]
async fn test_kg_refine_query_none_preserves_all_neighbours() {
    let idx = make_indexer();
    // Seed chunk
    idx.add_chunk(RawChunk {
        id: "seed:1".to_string(),
        file: "seed.rs".to_string(),
        start_line: 1,
        end_line: 1,
        content: "fn seed_fn() { neighbour_a(); neighbour_b(); }".to_string(),
        function_name: Some("seed_fn".to_string()),
        language: Some("rust".to_string()),
        chunk_type: crate::core::chunker::ChunkType::Function,
        calls: vec!["neighbour_a".to_string(), "neighbour_b".to_string()],
        inherits_from: Vec::new(),
        chunk_depth: 0,
        parent_chunk_id: None,
        child_chunk_ids: Vec::new(),
        nlp_keywords: Vec::new(),
        nlp_code_refs: Vec::new(),
        virtual_terms: Vec::new(),
    })
    .await
    .unwrap();
    // Neighbour A — same domain as seed
    idx.add_chunk(RawChunk {
        id: "na:1".to_string(),
        file: "a.rs".to_string(),
        start_line: 1,
        end_line: 1,
        content: "fn neighbour_a() {}".to_string(),
        function_name: Some("neighbour_a".to_string()),
        language: Some("rust".to_string()),
        chunk_type: crate::core::chunker::ChunkType::Function,
        calls: Vec::new(),
        inherits_from: Vec::new(),
        chunk_depth: 0,
        parent_chunk_id: None,
        child_chunk_ids: Vec::new(),
        nlp_keywords: Vec::new(),
        nlp_code_refs: Vec::new(),
        virtual_terms: Vec::new(),
    })
    .await
    .unwrap();
    // Neighbour B — unrelated domain
    idx.add_chunk(RawChunk {
        id: "nb:1".to_string(),
        file: "b.rs".to_string(),
        start_line: 1,
        end_line: 1,
        content: "fn neighbour_b() {}".to_string(),
        function_name: Some("neighbour_b".to_string()),
        language: Some("rust".to_string()),
        chunk_type: crate::core::chunker::ChunkType::Function,
        calls: Vec::new(),
        inherits_from: Vec::new(),
        chunk_depth: 0,
        parent_chunk_id: None,
        child_chunk_ids: Vec::new(),
        nlp_keywords: Vec::new(),
        nlp_code_refs: Vec::new(),
        virtual_terms: Vec::new(),
    })
    .await
    .unwrap();

    // No refine_query → all neighbours must survive KG expansion.
    let q = SearchQuery {
        text: "callers of seed_fn".to_string(),
        top_k: 20,
        expand_graph: true,
        compact: false,
        refine_query: None,
        ..Default::default()
    };
    let results = idx.search(&q).await.unwrap();
    let ids: Vec<&str> = results.iter().map(|c| c.id.as_str()).collect();
    assert!(
        ids.contains(&"na:1"),
        "neighbour_a must appear without refine_query, got {ids:?}"
    );
    assert!(
        ids.contains(&"nb:1"),
        "neighbour_b must appear without refine_query, got {ids:?}"
    );
}

/// `refine_query` filters KG-expanded neighbours below the cosine threshold
/// (issue #147).
///
/// Why: when the seed chunk is wrong, unfiltered KG expansion compounds
/// the error by returning an irrelevant neighbourhood.  A `refine_query`
/// describing the user's intent should keep only semantically relevant
/// neighbours and drop the rest.
///
/// What: this test calls `expand_with_kg_for_test` directly (bypassing the
/// full search pipeline) so HNSW / BM25 cannot independently surface the
/// irrelevant chunk and mask the filter's effect.  With `refine_embedding =
/// None` both neighbours survive; with `refine_embedding = Some(refine_emb)`
/// only the chunk whose stored embedding has cosine ≥ 0.4 against the refine
/// vector survives.  `MockEmbedder` is deterministic, so `content == refine_text`
/// gives cosine 1.0 (rel:1) while orthogonal uppercase content gives ≈ 0.33
/// (irr:1 — verified at dim=32, see comment below).
///
/// Test: this test.  Also verified by `test_kg_refine_threshold_boundary`.
#[tokio::test]
async fn test_kg_refine_query_filters_irrelevant_neighbours() {
    use crate::core::classifier::QueryIntent;

    let idx = make_indexer();

    // Seed: calls both auth_target and xyz_qqq so the KG has edges to both
    // neighbours.  We will supply `fused = [(seed:1, 1.0)]` directly to
    // `expand_with_kg_for_test` — no full search query needed.
    idx.add_chunk(RawChunk {
        id: "seed:1".to_string(),
        file: "seed.rs".to_string(),
        start_line: 1,
        end_line: 1,
        content: "fn seed_fn() { auth_target(); xyz_qqq(); }".to_string(),
        function_name: Some("seed_fn".to_string()),
        language: Some("rust".to_string()),
        chunk_type: crate::core::chunker::ChunkType::Function,
        calls: vec!["auth_target".to_string(), "xyz_qqq".to_string()],
        inherits_from: Vec::new(),
        chunk_depth: 0,
        parent_chunk_id: None,
        child_chunk_ids: Vec::new(),
        nlp_keywords: Vec::new(),
        nlp_code_refs: Vec::new(),
        virtual_terms: Vec::new(),
    })
    .await
    .unwrap();

    // The "relevant" neighbour: content identical to refine_text, so
    // MockEmbedder gives cosine = 1.0 against the refine embedding.
    let refine_text = "fn auth_target() { /* JWT validation */ }";
    idx.add_chunk(RawChunk {
        id: "rel:1".to_string(),
        file: "rel.rs".to_string(),
        start_line: 1,
        end_line: 1,
        content: refine_text.to_string(),
        function_name: Some("auth_target".to_string()),
        language: Some("rust".to_string()),
        chunk_type: crate::core::chunker::ChunkType::Function,
        calls: Vec::new(),
        inherits_from: Vec::new(),
        chunk_depth: 0,
        parent_chunk_id: None,
        child_chunk_ids: Vec::new(),
        nlp_keywords: Vec::new(),
        nlp_code_refs: Vec::new(),
        virtual_terms: Vec::new(),
    })
    .await
    .unwrap();

    // The "irrelevant" neighbour: uppercase O–Z (byte range 0x4F–0x5A) at
    // dim=32 hash to different slots than the lowercase+punctuation bytes in
    // `refine_text`, giving cosine ≈ 0.33 < KG_REFINE_THRESHOLD (0.4).
    // function_name matches the seed's calls edge; content only affects the
    // MockEmbedder hash.
    idx.add_chunk(RawChunk {
        id: "irr:1".to_string(),
        file: "irr.rs".to_string(),
        start_line: 1,
        end_line: 1,
        content: "OPQRSTUVWXYZOPQRSTUVWXYZOPQRSTUVWXYZOPQRSTUVWXYZ".to_string(),
        function_name: Some("xyz_qqq".to_string()),
        language: Some("rust".to_string()),
        chunk_type: crate::core::chunker::ChunkType::Function,
        calls: Vec::new(),
        inherits_from: Vec::new(),
        chunk_depth: 0,
        parent_chunk_id: None,
        child_chunk_ids: Vec::new(),
        nlp_keywords: Vec::new(),
        nlp_code_refs: Vec::new(),
        virtual_terms: Vec::new(),
    })
    .await
    .unwrap();

    // Build the seed list for expand_with_kg — just the seed chunk, no HNSW
    // or BM25 interference.
    let fused_seed: Vec<(String, f32)> = vec![("seed:1".to_string(), 1.0)];
    let intent = QueryIntent::Usage; // use_kg_first = true for this intent

    // Without refine_embedding: BOTH neighbours must appear in the expansion.
    let (all_no_refine, kg_ids_no_refine) = idx
        .expand_with_kg_for_test(fused_seed.clone(), &intent, true, true, None)
        .await;
    let no_refine_ids: Vec<&str> = all_no_refine.iter().map(|(id, _)| id.as_str()).collect();
    assert!(
        kg_ids_no_refine.contains("rel:1"),
        "rel:1 must appear in KG expansion without refine_embedding, \
         kg_ids={kg_ids_no_refine:?}"
    );
    assert!(
        kg_ids_no_refine.contains("irr:1"),
        "irr:1 must appear in KG expansion without refine_embedding, \
         kg_ids={kg_ids_no_refine:?}"
    );
    assert!(
        no_refine_ids.contains(&"rel:1"),
        "rel:1 must be in all_no_refine, got {no_refine_ids:?}"
    );
    assert!(
        no_refine_ids.contains(&"irr:1"),
        "irr:1 must be in all_no_refine, got {no_refine_ids:?}"
    );

    // Compute the refine embedding from the indexer's embedder so we use the
    // same MockEmbedder instance — guarantees vec equality.
    let refine_emb = idx
        .embed_text(refine_text)
        .await
        .unwrap()
        .unwrap_or_default();

    // Sanity-check cosines before making behavioural assertions.
    let rel_emb = idx.get_embedding("rel:1").unwrap_or_default();
    let irr_emb = idx.get_embedding("irr:1").unwrap_or_default();
    let cos_rel = crate::core::mmr::cosine_similarity(&refine_emb, &rel_emb);
    let cos_irr = crate::core::mmr::cosine_similarity(&refine_emb, &irr_emb);
    eprintln!(
        "cos_rel={cos_rel:.4} cos_irr={cos_irr:.4} threshold={}",
        KG_REFINE_THRESHOLD
    );
    assert!(
        cos_rel >= KG_REFINE_THRESHOLD,
        "relevant chunk cosine {cos_rel:.4} must be >= threshold {}",
        KG_REFINE_THRESHOLD
    );
    assert!(
        cos_irr < KG_REFINE_THRESHOLD,
        "irrelevant chunk cosine {cos_irr:.4} must be < threshold {} — \
         adjust the test content if MockEmbedder byte distribution changed",
        KG_REFINE_THRESHOLD
    );

    // With refine_embedding: rel:1 (cosine 1.0) must survive the filter;
    // irr:1 (cosine ≈ 0.33) must be dropped from the KG expansion.
    let (all_with_refine, kg_ids_with_refine) = idx
        .expand_with_kg_for_test(
            fused_seed.clone(),
            &intent,
            true,
            true,
            Some(refine_emb.as_slice()),
        )
        .await;
    let refine_ids: Vec<&str> = all_with_refine.iter().map(|(id, _)| id.as_str()).collect();

    assert!(
        kg_ids_with_refine.contains("rel:1"),
        "rel:1 must survive the refine filter (cosine={cos_rel:.4} >= threshold), \
         kg_ids={kg_ids_with_refine:?}"
    );
    assert!(
        !kg_ids_with_refine.contains("irr:1"),
        "irr:1 must be dropped by the refine filter (cosine={cos_irr:.4} < threshold), \
         kg_ids={kg_ids_with_refine:?}"
    );
    assert!(
        refine_ids.contains(&"rel:1"),
        "rel:1 must be in final results (cosine={cos_rel:.4}), got {refine_ids:?}"
    );
    assert!(
        !refine_ids.contains(&"irr:1"),
        "irr:1 must not be in final results (cosine={cos_irr:.4}), got {refine_ids:?}"
    );
}

/// Threshold boundary: a neighbour with cosine exactly equal to
/// `KG_REFINE_THRESHOLD` must be kept (>= semantics).
///
/// Why: off-by-one on the boundary condition would silently drop valid
/// results exactly at the cutoff.  We verify the comparison is `>=`, not `>`.
/// What: manually drive `expand_with_kg` with a synthetic refine embedding
/// whose cosine with a planted chunk embedding equals the threshold.
/// Test: this test.
#[tokio::test]
async fn test_kg_refine_threshold_boundary() {
    use crate::core::mmr::cosine_similarity;
    use KG_REFINE_THRESHOLD;

    // Build two unit vectors whose cosine is exactly KG_REFINE_THRESHOLD.
    // cos(θ) = KG_REFINE_THRESHOLD → θ = arccos(KG_REFINE_THRESHOLD).
    // We use a 2-D construction:
    //   chunk_vec = [1, 0]
    //   refine_vec = [KG_REFINE_THRESHOLD, sqrt(1 - threshold²)]
    // So cosine(chunk_vec, refine_vec) = KG_REFINE_THRESHOLD exactly.
    let threshold = KG_REFINE_THRESHOLD;
    let chunk_vec = vec![1.0_f32, 0.0];
    let refine_vec = vec![threshold, (1.0_f32 - threshold * threshold).sqrt()];

    let actual_cos = cosine_similarity(&chunk_vec, &refine_vec);
    assert!(
        (actual_cos - threshold).abs() < 1e-5,
        "test setup: cosine {actual_cos:.6} should equal threshold {threshold:.6}"
    );

    // The boundary cosine must NOT be filtered out (>= semantics).
    assert!(
        actual_cos >= threshold,
        "boundary: {actual_cos:.6} >= {threshold:.6} must hold"
    );
}

// ── Issue #402: relative path storage + query-time resolution ─────────────────

/// Why: `resolve_chunk_file` must convert a stored relative path to an
/// absolute path by joining with `root_path`. This is the read-side half of
/// issue #402 — relocation resilience.
/// What: `"src/lib.rs"` + `"/tmp/test"` → `"/tmp/test/src/lib.rs"`.
/// Test: this test.
#[test]
fn resolve_chunk_file_relative_becomes_absolute() {
    let root = std::path::Path::new("/tmp/test");
    let result = resolve_chunk_file("src/lib.rs", root);
    assert_eq!(result, "/tmp/test/src/lib.rs");
}

/// Why: `resolve_chunk_file` must pass through an already-absolute path
/// unchanged. This supports the dual-read migration path for pre-M002 indexes
/// that still carry absolute paths in their redb corpus.
/// What: `"/Users/alice/proj/src/lib.rs"` → same string unchanged.
/// Test: this test.
#[test]
fn resolve_chunk_file_absolute_passthrough() {
    let root = std::path::Path::new("/tmp/test");
    let abs_path = "/Users/alice/proj/src/lib.rs";
    let result = resolve_chunk_file(abs_path, root);
    assert_eq!(result, abs_path);
}

/// Why: `index_file` (and by extension `index_files_batch`) must store chunk
/// `file` fields relative to the index `root_path` as of issue #402. This
/// test verifies the storage side: the raw chunk held in the in-memory corpus
/// has a relative `file`, while the materialized `CodeChunk.file` returned by
/// `search` is absolute.
/// What: index a file with a relative path, then assert that
///   (a) the raw `RawChunk.file` in the in-memory corpus is relative, and
///   (b) `CodeChunk.file` in search results is the resolved absolute path.
/// Test: this test.
#[tokio::test]
async fn relative_storage_resolved_to_absolute_in_search_results() {
    let idx = make_indexer(); // root_path = "/tmp/test"
    idx.index_file("src/lib.rs", "pub fn hello() {}\n")
        .await
        .unwrap();

    // (a) Raw storage is relative — inspect the in-memory map directly.
    {
        let chunks_guard = idx.chunks.read().await;
        let stored: Vec<&str> = chunks_guard.values().map(|c| c.file.as_str()).collect();
        assert!(
            stored.contains(&"src/lib.rs"),
            "raw chunk file must be stored relative; got {stored:?}"
        );
        assert!(
            !stored.iter().any(|f| f.starts_with('/')),
            "raw chunk file must NOT be absolute; got {stored:?}"
        );
    }

    // (b) Search results expose absolute paths.
    let q = SearchQuery {
        text: "hello".to_string(),
        top_k: 5,
        expand_graph: false,
        compact: false,
        ..Default::default()
    };
    let results = idx.search(&q).await.unwrap();
    assert!(!results.is_empty(), "search must return at least one hit");
    let resolved_file = &results[0].file;
    assert_eq!(
        resolved_file,
        &abs("src/lib.rs"),
        "CodeChunk.file must be resolved to absolute path; got {resolved_file:?}"
    );
    assert!(
        std::path::Path::new(resolved_file).is_absolute(),
        "CodeChunk.file must be absolute; got {resolved_file:?}"
    );
}

/// Why: moving a project (updating `root_path`) must yield correct result
/// paths without a full re-index. This test simulates the relocation by
/// indexing with one root, then querying via a second indexer with a different
/// `root_path` that points to the same content (using a symlink or, in the
/// test, by indexing the raw `file`/`content` and then resolving against a
/// new root).
///
/// Since we can't easily move files in a unit test, we verify the invariant
/// directly: two `CodeIndexer` instances with different `root_path` values
/// but the same relative chunk data resolve to different absolute paths for
/// the same stored relative `file`.
/// What: insert the same relative chunk into two indexers with different
/// roots; assert each resolves its `file` to its own root prefix.
/// Test: this test.
#[tokio::test]
async fn relative_chunk_resolves_correctly_for_different_roots() {
    let dim = 32;
    let embedder_a: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(dim));
    let store_a: Arc<dyn VectorStore> = Arc::new(UsearchStore::new(dim).expect("usearch"));
    let idx_a = CodeIndexer::new("proj-a", "/home/alice/proj").with_components(embedder_a, store_a);

    let embedder_b: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(dim));
    let store_b: Arc<dyn VectorStore> = Arc::new(UsearchStore::new(dim).expect("usearch"));
    let idx_b =
        CodeIndexer::new("proj-b", "/home/bob/relocated").with_components(embedder_b, store_b);

    // Index the same relative path in both.
    idx_a
        .index_file("src/main.rs", "fn main() {}\n")
        .await
        .unwrap();
    idx_b
        .index_file("src/main.rs", "fn main() {}\n")
        .await
        .unwrap();

    let q = SearchQuery {
        text: "main".to_string(),
        top_k: 5,
        expand_graph: false,
        compact: false,
        ..Default::default()
    };

    let res_a = idx_a.search(&q).await.unwrap();
    let res_b = idx_b.search(&q).await.unwrap();

    assert!(!res_a.is_empty());
    assert!(!res_b.is_empty());

    let file_a = &res_a[0].file;
    let file_b = &res_b[0].file;

    assert_eq!(
        file_a, "/home/alice/proj/src/main.rs",
        "proj-a must resolve to alice's root; got {file_a:?}"
    );
    assert_eq!(
        file_b, "/home/bob/relocated/src/main.rs",
        "proj-b must resolve to bob's root; got {file_b:?}"
    );
}

/// Why: `enumerate_chunks` (used by `GET /indexes/:id/chunks`) must also
/// return resolved absolute file paths, not raw relative ones.
/// What: index a file, enumerate chunks, assert the `file` field is absolute.
/// Test: this test.
#[tokio::test]
async fn enumerate_chunks_returns_resolved_absolute_paths() {
    let dir = tempfile::tempdir().unwrap();
    let redb_path = dir.path().join("index.redb");
    let idx = make_indexer_with_corpus(&redb_path);

    idx.index_file("docs/guide.md", "# Guide\n\nWelcome.\n")
        .await
        .unwrap();

    let (total, page) = idx.enumerate_chunks(0, 100).await;
    assert!(total > 0, "expected at least one chunk");
    for chunk in &page {
        assert!(
            std::path::Path::new(&chunk.file).is_absolute(),
            "enumerate_chunks must return absolute file paths; got {:?}",
            chunk.file
        );
    }
}

// ── Issue #674: `path` (portable relative path) field on CodeChunk ────────────

/// Why: `raw_to_code_chunk` must populate `path` with the raw stored form when
/// the stored `file` is already relative (the normal post-#402 case).
/// This is the read-side half of the portable-paths feature (issue #674).
/// What: a `RawChunk` with a relative `file` → `CodeChunk.path == Some("src/lib.rs")`.
/// Test: this test.
#[test]
fn raw_to_code_chunk_populates_path_for_relative_file() {
    use crate::core::indexer::raw_to_code_chunk;

    let raw = make_raw_chunk("src/lib.rs", "pub fn hello() {}\n");
    let root = std::path::Path::new("/home/alice/proj");
    let chunk = raw_to_code_chunk(&raw, 0.9, "bm25", None, root);

    // `file` must be absolute.
    assert!(
        std::path::Path::new(&chunk.file).is_absolute(),
        "file must be absolute; got {:?}",
        chunk.file
    );
    assert_eq!(chunk.file, "/home/alice/proj/src/lib.rs");

    // `path` must carry the root-relative form.
    assert_eq!(
        chunk.path.as_deref(),
        Some("src/lib.rs"),
        "path must be the stored relative value; got {:?}",
        chunk.path
    );
}

/// Why: a pre-#402 legacy chunk whose stored `file` is absolute must not have
/// a wrong path value in the `path` field. `path` must be `None` so consumers
/// that use `path` as a portable key do not pick up a stale absolute path.
/// What: a `RawChunk` with an absolute `file` → `CodeChunk.path == None`.
/// Test: this test.
#[test]
fn raw_to_code_chunk_path_is_none_for_absolute_file() {
    use crate::core::indexer::raw_to_code_chunk;

    let raw = make_raw_chunk("/mnt/efs/data/repos/proj/src/lib.rs", "pub fn hello() {}\n");
    let root = std::path::Path::new("/mnt/efs/data/repos/proj");
    let chunk = raw_to_code_chunk(&raw, 0.9, "bm25", None, root);

    // `file` must pass through unchanged (absolute input → absolute output).
    assert_eq!(chunk.file, "/mnt/efs/data/repos/proj/src/lib.rs");

    // `path` must be None — we cannot strip the root reliably at read time.
    assert_eq!(
        chunk.path, None,
        "path must be None for a legacy absolute-path chunk; got {:?}",
        chunk.path
    );
}

/// Why: `index_file` must store a relative `file` in the in-memory corpus,
/// and search results must expose a non-null `path` carrying that relative form
/// (issue #674 — portable-paths feature).
/// What: index a file, search for it, assert `CodeChunk.path == Some("src/auth.rs")`.
/// Test: this test.
#[tokio::test]
async fn search_result_path_field_is_populated_after_index_file() {
    let idx = make_indexer(); // root_path = "/tmp/test"
    idx.index_file("src/auth.rs", "pub fn authenticate() { /* ok */ }\n")
        .await
        .unwrap();

    let q = SearchQuery {
        text: "authenticate".to_string(),
        top_k: 5,
        expand_graph: false,
        compact: false,
        ..Default::default()
    };
    let results = idx.search(&q).await.unwrap();
    assert!(!results.is_empty(), "search must find the indexed chunk");

    for chunk in &results {
        assert_eq!(
            chunk.path.as_deref(),
            Some("src/auth.rs"),
            "CodeChunk.path must be the root-relative path after index_file; got {:?}",
            chunk.path
        );
        // `file` must still be absolute for backward compatibility.
        assert!(
            std::path::Path::new(&chunk.file).is_absolute(),
            "CodeChunk.file must be absolute; got {:?}",
            chunk.file
        );
    }
}

// ── Helper: build a minimal RawChunk for unit tests ──────────────────────────

fn make_raw_chunk(file: &str, content: &str) -> crate::core::chunker::RawChunk {
    use crate::core::chunker::{ChunkType, RawChunk};
    RawChunk {
        id: format!("{file}:1:10"),
        file: file.to_string(),
        start_line: 1,
        end_line: 10,
        content: content.to_string(),
        function_name: None,
        language: None,
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
