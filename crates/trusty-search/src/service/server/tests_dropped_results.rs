//! Regression tests for #2203's reporting gap: a search that drops candidates
//! after fusion must tell the caller how many and why.
//!
//! Why: split out of `tests_search.rs`, which these two tests pushed to 528
//! SLOC against the 500 production cap (the file's name matches none of the
//! test-file patterns, so it is capped as production).
//! What: asserts `meta.dropped` on the search handler's JSON for the
//! unresolvable-corpus drop and for the two post-fusion filters. Both assert
//! through the response body rather than the Rust API, so they compile against
//! the pre-fix commit and fail on the assertion — `meta` carried no `dropped`
//! key at all.
//! Test: this module.

use super::*;
use axum::Json;

/// `RawChunk` builder for the drop-reporting tests below.
fn drop_test_chunk(
    id: &str,
    file: &str,
    content: &str,
    chunk_type: crate::core::chunker::ChunkType,
) -> crate::core::chunker::RawChunk {
    crate::core::chunker::RawChunk {
        id: id.to_string(),
        file: file.to_string(),
        start_line: 1,
        end_line: 1 + content.lines().count(),
        content: content.to_string(),
        function_name: None,
        language: Some("rust".to_string()),
        chunk_type,
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

/// Query shape shared by the two drop-reporting tests.
fn drop_test_query(
    text: &str,
    mode: crate::core::indexer::SearchMode,
    stage: Option<crate::core::indexer::SearchStage>,
) -> crate::core::indexer::SearchQuery {
    crate::core::indexer::SearchQuery {
        text: text.to_string(),
        top_k: 20,
        expand_graph: false,
        compact: false,
        branch_files: None,
        branch_boost: 1.0,
        branch: None,
        stage,
        mode,
        exclude_archived: false,
        refine_query: None,
        path_prefix: None,
        repos: Vec::new(),
    }
}

/// A fused id with no row in the corpus is dropped at materialisation
/// (`core::indexer::search::materialize`), and the caller must be able to see
/// that it happened.
///
/// Why: the drop is real and sometimes correct, but it was invisible —
/// `results` came back one row shorter with no count and no `meta` field, so
/// `top_k: 20` returning 1 row read exactly like "1 chunk matched". When
/// `fetch_chunks_for_ids`' durable read fails and falls back to an
/// idle-evicted in-memory map, every id misses and a healthy index returns an
/// empty result set (#2203).
/// What: pre-seeds the vector store with an id no chunk row backs, so the HNSW
/// lane returns a candidate materialisation cannot resolve, then asserts the
/// count reaches the caller in `meta.dropped.unresolved_corpus`.
/// Test: this test.
#[tokio::test]
async fn search_handler_meta_reports_rows_dropped_when_the_corpus_has_no_matching_row() {
    use crate::core::embed::{Embedder, MockEmbedder};
    use crate::core::indexer::CodeIndexer;
    use crate::core::registry::{IndexHandle, IndexId, IndexRegistry, StageStatus};
    use crate::core::store::{UsearchStore, VectorStore};
    use tempfile::tempdir;

    let tmp = tempdir().unwrap();
    let dim = 16;
    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(dim));
    let store: Arc<dyn VectorStore> = Arc::new(UsearchStore::new(dim).expect("usearch"));

    // An id the vector lane returns but no chunk row backs — the same shape
    // `fetch_chunks_for_ids` produces when its durable read fails.
    store
        .upsert("src/ghost.rs:1:9", vec![0.5_f32; dim])
        .await
        .expect("seed orphan vector");

    let indexer = CodeIndexer::new("drop-corpus-test", tmp.path())
        .with_components(Arc::clone(&embedder), Arc::clone(&store));
    indexer
        .add_chunk(drop_test_chunk(
            "src/real.rs:1:2",
            "src/real.rs",
            "fn alpha_qwerty() -> bool { true }",
            crate::core::chunker::ChunkType::Code,
        ))
        .await
        .expect("add real chunk");

    let registry = IndexRegistry::new();
    let handle = IndexHandle::bare(
        IndexId::new("drop-corpus-idx"),
        Arc::new(tokio::sync::RwLock::new(indexer)),
        tmp.path().to_path_buf(),
    );
    let stages = Arc::clone(&handle.stages);
    registry.register(handle);
    {
        // The handler down-shifts to the lexical lane unless semantic is ready,
        // and the lexical lane never sees the orphan vector.
        let mut s = stages.write().await;
        s.lexical.status = StageStatus::Ready;
        s.semantic.status = StageStatus::Ready;
    }

    let state = Arc::new(SearchAppState::new(registry));
    state.install_embedder(embedder).await;

    let resp = search_handler(
        axum::extract::State(Arc::clone(&state)),
        axum::extract::Path("drop-corpus-idx".to_string()),
        axum::extract::Json(drop_test_query(
            "alpha_qwerty",
            crate::core::indexer::SearchMode::All,
            None,
        )),
    )
    .await;

    let Json(body) = resp.expect("handler must succeed");
    let meta = body.get("meta").expect("meta block present");
    assert_eq!(
        meta["dropped"]["unresolved_corpus"],
        serde_json::json!(1),
        "the unresolvable fused id must be counted and reported; meta={meta:?}"
    );
    assert_eq!(
        body["results"].as_array().map(Vec::len),
        Some(1),
        "the resolvable row still comes back; body={body:?}"
    );
}

/// The mode filter and the docstring filter both delete post-fusion rows, and
/// the caller must be able to see how many and why.
///
/// Why: `apply_archive_downrank`'s two `retain`s delete rows the lanes did
/// retrieve. That is often the right call, but it was uncounted and absent from
/// `meta` — the mechanism users read as "search is broken" in #2203.
/// What: indexes a source chunk, a `.md` chunk, and a docstring chunk, runs a
/// `BugDebt`-intent query (the intent that keeps `Code` mode's hard filter,
/// since #2203 upgrades `Unknown` to `All`), and asserts both counts reach the
/// caller.
/// Test: this test.
#[tokio::test]
async fn search_handler_meta_reports_rows_dropped_by_the_mode_and_docstring_filters() {
    use crate::core::embed::{Embedder, MockEmbedder};
    use crate::core::indexer::CodeIndexer;
    use crate::core::registry::{IndexHandle, IndexId, IndexRegistry};
    use crate::core::store::{UsearchStore, VectorStore};
    use tempfile::tempdir;

    let tmp = tempdir().unwrap();
    let dim = 16;
    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(dim));
    let store: Arc<dyn VectorStore> = Arc::new(UsearchStore::new(dim).expect("usearch"));
    let indexer = CodeIndexer::new("drop-mode-test", tmp.path())
        .with_components(Arc::clone(&embedder), Arc::clone(&store));

    for (id, file, content, chunk_type) in [
        (
            "src/lib.rs:1:2",
            "src/lib.rs",
            "fn alpha_qwerty() -> bool { true }",
            crate::core::chunker::ChunkType::Code,
        ),
        (
            "docs/intro.md:1:3",
            "docs/intro.md",
            "# alpha_qwerty\nDocumentation about alpha_qwerty.",
            crate::core::chunker::ChunkType::Code,
        ),
        (
            "src/doc.rs:1:2",
            "src/doc.rs",
            "/// alpha_qwerty is documented here.",
            crate::core::chunker::ChunkType::Docstring,
        ),
    ] {
        indexer
            .add_chunk(drop_test_chunk(id, file, content, chunk_type))
            .await
            .expect("add chunk");
    }

    let registry = IndexRegistry::new();
    let handle = IndexHandle::bare(
        IndexId::new("drop-mode-idx"),
        Arc::new(tokio::sync::RwLock::new(indexer)),
        tmp.path().to_path_buf(),
    );
    registry.register(handle);
    let state = Arc::new(SearchAppState::new(registry));
    state.install_embedder(embedder).await;

    let resp = search_handler(
        axum::extract::State(Arc::clone(&state)),
        axum::extract::Path("drop-mode-idx".to_string()),
        axum::extract::Json(drop_test_query(
            // Classifies as `BugDebt` — the intent that keeps `Code` mode's
            // hard file-type filter.
            "bug in alpha_qwerty",
            crate::core::indexer::SearchMode::Code,
            Some(crate::core::indexer::SearchStage::Lexical),
        )),
    )
    .await;

    let Json(body) = resp.expect("handler must succeed");
    let meta = body.get("meta").expect("meta block present");
    assert_eq!(
        meta["dropped"]["mode_filtered"],
        serde_json::json!(1),
        "the .md row the lexical lane retrieved and the mode filter deleted must \
         be counted; meta={meta:?}"
    );
    assert_eq!(
        meta["dropped"]["docstring_filtered"],
        serde_json::json!(1),
        "the docstring row deleted by the Code-mode chunk-type retain must be \
         counted; meta={meta:?}"
    );
}
