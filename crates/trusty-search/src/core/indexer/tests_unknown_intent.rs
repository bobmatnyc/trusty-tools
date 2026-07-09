//! Regression tests for issue #2203 (P0): `Unknown`-intent (short 1-3 word
//! natural-language) queries must never silently return empty/irrelevant
//! results.
//!
//! Why: these tests live in their own file (rather than growing `tests.rs`
//! further) to respect the 1500-SLOC test cap — `tests.rs` is already
//! grandfathered near its frozen budget (same rationale as `tests_cursor.rs`
//! / `tests_idle_evict.rs`).
//! What: pins the fix for the reported bug — a short NL query classifies as
//! `Unknown` intent (`multi_noun_re` requires >= 4 tokens to reach
//! `Conceptual`, see `classifier::classify`); `SearchQuery::mode` defaults to
//! `SearchMode::Code`; before this fix `Unknown` was not in the Code→All
//! upgrade set in `CodeIndexer::search`, so `effective_mode` stayed `Code`
//! and `apply_archive_downrank`'s hard file-type `retain` deleted every
//! non-code chunk post-hoc, silently dropping BM25's correct `.md` hit. Also
//! pins the companion no-regression contract: intents that are NOT `Unknown`
//! (e.g. `BugDebt`) still hard-filter to source-only in `Code` mode.
//! Test: this module.

use std::sync::Arc;

use super::{CodeIndexer, SearchMode, SearchQuery};
use crate::core::chunker::{ChunkType, RawChunk};
use crate::core::classifier::{QueryClassifier, QueryIntent};
use crate::core::embed::{Embedder, MockEmbedder};
use crate::core::store::{UsearchStore, VectorStore};

const TEST_ROOT: &str = "/tmp/test";

/// Build an absolute file path for a relative path under [`TEST_ROOT`]
/// (mirrors `tests::abs` — `CodeChunk.file` is resolved to an absolute path
/// at materialisation time, issue #402).
fn abs(rel: &str) -> String {
    std::path::Path::new(TEST_ROOT)
        .join(rel)
        .to_string_lossy()
        .into_owned()
}

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
    CodeIndexer::new("test", TEST_ROOT).with_components(embedder, store)
}

/// Issue #2203 (P0) acceptance test: a short NL query classifies as
/// `Unknown`, `mode` is left at its default (`Code`) — mirroring a real
/// caller that never sets `mode` explicitly — and the matching `.md` doc hit
/// must survive in a non-empty result set instead of being hard-dropped.
#[tokio::test]
async fn test_unknown_intent_downranks_docs_instead_of_dropping_them() {
    let idx = make_indexer();
    idx.add_chunk(raw(
        "doc:pool",
        "docs/pooling.md",
        "# connection pooling\nHow the database connection pooling subsystem batches reuse.",
    ))
    .await
    .unwrap();
    idx.add_chunk(raw(
        "src:pool",
        "src/pool.rs",
        "fn acquire() -> Conn { unimplemented!() }",
    ))
    .await
    .unwrap();
    idx.add_chunk(raw(
        "src:other",
        "src/other.rs",
        "fn unrelated() -> bool { true }",
    ))
    .await
    .unwrap();

    let query_text = "connection pooling";
    assert_eq!(
        QueryClassifier::classify(query_text),
        QueryIntent::Unknown,
        "fixture query must classify as Unknown for this to be a valid regression test"
    );

    let q = SearchQuery {
        text: query_text.to_string(),
        top_k: 20,
        expand_graph: false,
        compact: false,
        // Deliberately left at the default (`Code`) — the real-world shape
        // of the bug: callers of the NL/semantic search tool never set
        // `mode` explicitly.
        ..Default::default()
    };
    let results = idx.search(&q).await.unwrap();
    assert!(
        !results.is_empty(),
        "Unknown-intent NL query must not return empty results"
    );
    let files: Vec<&str> = results.iter().map(|c| c.file.as_str()).collect();
    assert!(
        files.iter().any(|f| f.ends_with(".md")),
        "Unknown-intent NL query must surface the matching doc hit: {files:?}"
    );
}

/// Companion no-regression test: an intent that is NOT `Unknown` (here
/// `BugDebt`) must still hard-filter to source-only in `Code` mode.
/// `indexer::tests::test_mode_filter_code_returns_only_source` already
/// covers a `Usage`-intent query end-to-end; this adds an independent
/// `BugDebt`-intent example.
#[tokio::test]
async fn test_bugdebt_intent_still_hard_filters_code_mode() {
    let idx = make_indexer();
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

    let query_text = "bug in alpha_qwerty";
    assert_eq!(
        QueryClassifier::classify(query_text),
        QueryIntent::BugDebt,
        "fixture query must classify as BugDebt for this to be a valid regression test"
    );

    let q = SearchQuery {
        text: query_text.to_string(),
        top_k: 20,
        expand_graph: false,
        compact: false,
        mode: SearchMode::Code,
        ..Default::default()
    };
    let results = idx.search(&q).await.unwrap();
    let files: Vec<&str> = results.iter().map(|c| c.file.as_str()).collect();
    assert!(
        files.contains(&abs("src/lib.rs").as_str()),
        "BugDebt intent + code mode must still include source: {files:?}"
    );
    assert!(
        !files.iter().any(|f| f.ends_with(".md")),
        "BugDebt intent + code mode must still exclude .md: {files:?}"
    );
}
