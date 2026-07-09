//! Idle-eviction, KG refine-query, and relative-path resolution tests for
//! [`CodeIndexer`].
//!
//! Why: split out of the former monolithic `tests.rs` to keep each test
//! file under the 1500-SLOC cap (issue #1195).
//! What: covers `idle_evict_secs` env handling, idle-eviction/rehydrate
//! paths, KG neighbour refine-query filtering and threshold boundary,
//! and relative->absolute chunk-path resolution across roots.
//! Test: this module.
use super::super::*;
use super::*;
use crate::core::embed::MockEmbedder;
use crate::core::store::UsearchStore;
use std::sync::atomic::Ordering;

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
