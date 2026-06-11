/// Issue #402 and #674: relative path storage, query-time resolution,
/// and the portable `path` field on CodeChunk.
use super::*;

/// Why: `resolve_chunk_file` must convert a stored relative path to an
/// absolute path by joining with `root_path`.
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

/// Why: `index_file` must store chunk `file` fields relative to `root_path`
/// (issue #402). The raw chunk in the in-memory corpus is relative; the
/// materialized `CodeChunk.file` returned by `search` is absolute.
/// What: index a file with a relative path, then assert raw storage is relative
/// and search results carry the absolute path.
/// Test: this test.
#[tokio::test]
async fn relative_storage_resolved_to_absolute_in_search_results() {
    let idx = make_indexer(); // root_path = "/tmp/test"
    idx.index_file("src/lib.rs", "pub fn hello() {}\n")
        .await
        .unwrap();

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

/// Why: two `CodeIndexer` instances with different `root_path` values but the
/// same relative chunk data must resolve to different absolute paths — this
/// validates relocation resilience.
/// What: index `"src/main.rs"` in two indexers with different roots; assert
/// each resolves its `file` to its own root prefix.
/// Test: this test.
#[tokio::test]
async fn relative_chunk_resolves_correctly_for_different_roots() {
    let dim = 32;
    let embedder_a: Arc<dyn Embedder> = Arc::new(crate::core::embed::MockEmbedder::new(dim));
    let store_a: Arc<dyn VectorStore> =
        Arc::new(crate::core::store::UsearchStore::new(dim).expect("usearch"));
    let idx_a = CodeIndexer::new("proj-a", "/home/alice/proj").with_components(embedder_a, store_a);

    let embedder_b: Arc<dyn Embedder> = Arc::new(crate::core::embed::MockEmbedder::new(dim));
    let store_b: Arc<dyn VectorStore> =
        Arc::new(crate::core::store::UsearchStore::new(dim).expect("usearch"));
    let idx_b =
        CodeIndexer::new("proj-b", "/home/bob/relocated").with_components(embedder_b, store_b);

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

/// Why: `enumerate_chunks` (`GET /indexes/:id/chunks`) must also return
/// resolved absolute file paths, not raw relative ones.
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
/// What: a `RawChunk` with a relative `file` → `CodeChunk.path == Some("src/lib.rs")`.
/// Test: this test.
#[test]
fn raw_to_code_chunk_populates_path_for_relative_file() {
    use crate::core::indexer::raw_to_code_chunk;

    let raw_c = make_raw_chunk("src/lib.rs", "pub fn hello() {}\n");
    let root = std::path::Path::new("/home/alice/proj");
    let chunk = raw_to_code_chunk(&raw_c, 0.9, "bm25", None, root);

    assert!(
        std::path::Path::new(&chunk.file).is_absolute(),
        "file must be absolute; got {:?}",
        chunk.file
    );
    assert_eq!(chunk.file, "/home/alice/proj/src/lib.rs");

    assert_eq!(
        chunk.path.as_deref(),
        Some("src/lib.rs"),
        "path must be the stored relative value; got {:?}",
        chunk.path
    );
}

/// Why: a pre-#402 legacy chunk whose stored `file` is absolute must not
/// surface a wrong `path` value. `path` must be `None` so consumers that use
/// `path` as a portable key do not pick up a stale absolute path.
/// What: a `RawChunk` with an absolute `file` → `CodeChunk.path == None`.
/// Test: this test.
#[test]
fn raw_to_code_chunk_path_is_none_for_absolute_file() {
    use crate::core::indexer::raw_to_code_chunk;

    let raw_c = make_raw_chunk("/mnt/efs/data/repos/proj/src/lib.rs", "pub fn hello() {}\n");
    let root = std::path::Path::new("/mnt/efs/data/repos/proj");
    let chunk = raw_to_code_chunk(&raw_c, 0.9, "bm25", None, root);

    assert_eq!(chunk.file, "/mnt/efs/data/repos/proj/src/lib.rs");

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
        assert!(
            std::path::Path::new(&chunk.file).is_absolute(),
            "CodeChunk.file must be absolute; got {:?}",
            chunk.file
        );
    }
}
