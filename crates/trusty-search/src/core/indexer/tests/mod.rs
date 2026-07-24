//! Unit and integration tests for [`CodeIndexer`].
//!
//! Why: this module lives as a directory (`tests/`) split into concern-focused
//! submodules to respect the 1500-SLOC test-file cap (issue #1195). The former
//! single `tests.rs` had grown past the cap. Shared fixtures (`abs`, `raw`,
//! `raw_with_kind`, `make_indexer`) stay here in `mod.rs` so every submodule
//! reaches them via `use super::*`; indexer internals are reached via
//! `use super::super::*`.
//! What: declares the concern submodules and defines the corpus-agnostic
//! fixtures used across them.
//! Test: the submodules (`persistence_and_search`, `ranking_and_modes`,
//! `branch_and_corpus`, `eviction_kg_paths`) contain the actual assertions.

use super::*;
use crate::core::corpus::CorpusStore;
use crate::core::embed::MockEmbedder;
use crate::core::store::UsearchStore;

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
/// branch in `commit_parsed_batch` / `load_chunks_from_redb` / removal. Shared
/// by `branch_and_corpus` and `eviction_kg_paths`.
fn make_indexer_with_corpus(redb_path: &std::path::Path) -> CodeIndexer {
    let mut idx = CodeIndexer::new("corpus-test", "/tmp/corpus-test");
    let store = CorpusStore::open(redb_path).expect("open corpus store");
    idx.set_corpus_store(Arc::new(store));
    idx
}

mod branch_and_corpus;
mod embed_pool_routing;
mod eviction_kg_paths;
mod path_filter_search;
mod persistence_and_search;
mod ranking_and_modes;
