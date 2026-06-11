use super::*;

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
    let content = idx.chunk_content_by_id("a:1:1").await;
    assert_eq!(
        content.as_deref(),
        Some("fn alpha() {}"),
        "chunk_content_by_id must return the raw content for a known id"
    );
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
async fn test_enumerate_chunks_paginates_stable_order() {
    // Why: pagination over an underlying HashMap must produce a stable
    // total order so successive pages don't overlap or skip rows.
    let idx = make_indexer();
    fn raw_lines(id: &str, file: &str, start: usize, end: usize, content: &str) -> RawChunk {
        let mut r = raw(id, file, content);
        r.start_line = start;
        r.end_line = end;
        r
    }
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

    let (total_all, all) = idx.enumerate_chunks(0, 100).await;
    assert_eq!(total_all, 4);
    let ids: Vec<_> = all.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(
        ids,
        vec!["a.rs:1:5", "a.rs:30:40", "b.rs:1:5", "b.rs:10:20"]
    );

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

    let (total_end, end) = idx.enumerate_chunks(10, 5).await;
    assert_eq!(total_end, 4);
    assert!(end.is_empty());

    let (total_z, z) = idx.enumerate_chunks(0, 0).await;
    assert_eq!(total_z, 4);
    assert!(z.is_empty());
}

#[test]
fn test_intent_routing_definitions() {
    use crate::core::classifier::QueryIntent;
    let (a, b, kg) = QueryIntent::Definition.weights();
    assert!((a - 0.3).abs() < 1e-6 && (b - 0.7).abs() < 1e-6 && !kg);
    let (a, b, kg) = QueryIntent::Usage.weights();
    assert!((a - 0.5).abs() < 1e-6 && (b - 0.5).abs() < 1e-6 && kg);
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
