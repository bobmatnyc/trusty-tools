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

/// Build a BM25-only indexer (no embedder/store needed) with a durable redb
/// `CorpusStore` wired at `redb_path`.
///
/// Why: the corpus-integration tests exercise the commit → redb → warm-boot
/// rehydration path, which is orthogonal to the HNSW lane. A BM25-only indexer
/// keeps the tests hermetic (no ONNX) while still hitting every `corpus`
/// branch in `commit_parsed_batch` / `load_chunks_from_redb` / removal.
fn make_indexer_with_corpus(redb_path: &std::path::Path) -> CodeIndexer {
    use crate::core::corpus::CorpusStore;
    let mut idx = CodeIndexer::new("corpus-test", "/tmp/corpus-test");
    let store = CorpusStore::open(redb_path).expect("open corpus store");
    idx.set_corpus_store(Arc::new(store));
    idx
}

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

mod batch;
mod entity;
mod grep;
mod paths;
mod persistence;
mod search_basic;
mod search_kg;
mod search_mode;
mod search_ranking;
