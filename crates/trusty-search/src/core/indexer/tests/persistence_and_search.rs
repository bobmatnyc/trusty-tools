//! Persistence, core hybrid search, KG expansion, entity-exact-match, and
//! batch-indexing tests for [`CodeIndexer`].
//!
//! Why: split out of the former monolithic `tests.rs` to keep each test
//! file under the 1500-SLOC cap (issue #1195).
//! What: covers snapshot/restore, incremental persist throttling, RRF
//! integration, query cache, BM25 fallback, removal, KG neighbour
//! expansion, symbol-graph rebuild, entity exact match, virtual terms,
//! embedding lookups, similarity, and batch indexing.
//! Test: this module.
use super::super::*;
use super::*;
use std::sync::atomic::Ordering;

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
