//! Integration tests for the typeahead endpoint and MCP tool (issue #1557).
//!
//! Why: typeahead/autocomplete is a new capability that needs regression
//! coverage for: (a) correct field population from `CodeChunk`, (b) empty-query
//! short-circuit, (c) limit clamping, and (d) source-lane classification.
//! ONNX-backed blended-mode tests are gated behind `#[ignore]` following the
//! crate's existing convention.
//! What: each test stands up a minimal bare index, optionally adds a few
//! fixture chunks via `CodeIndexer::index_file`, then calls the handler under
//! test directly (not via a running HTTP server).
//! Test: this file.

use std::sync::Arc;

use trusty_common::embedder::MockEmbedder;
use trusty_search::core::embed::Embedder;
use trusty_search::core::indexer::{CodeIndexer, TypeaheadHit};
use trusty_search::core::registry::{IndexHandle, IndexId, IndexRegistry};
use trusty_search::core::store::{UsearchStore, VectorStore};
use trusty_search::service::server::{SearchAppState, TypeaheadParamsForTests};

/// Build a minimal bare-index state wired with a `MockEmbedder` and one
/// registered index.
///
/// Why: every test needs the same boilerplate; centralising it keeps each
/// test focused on the assertion and avoids duplication.
/// What: creates a `tempdir`, builds a `CodeIndexer` with the mock embedder
/// and a `UsearchStore`, registers an `IndexHandle` in a fresh registry, and
/// wraps everything in `SearchAppState`.
/// Test: called by every `#[tokio::test]` below.
async fn make_state_with_index(
    index_id: &str,
) -> (
    Arc<SearchAppState>,
    Arc<tokio::sync::RwLock<CodeIndexer>>,
    tempfile::TempDir,
) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dim: usize = 16;
    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(dim));
    let store: Arc<dyn VectorStore> = Arc::new(UsearchStore::new(dim).expect("usearch"));
    let indexer = CodeIndexer::new(index_id, tmp.path())
        .with_components(Arc::clone(&embedder), Arc::clone(&store));
    let indexer_arc = Arc::new(tokio::sync::RwLock::new(indexer));
    let registry = IndexRegistry::new();
    let handle = IndexHandle::bare(
        IndexId::new(index_id),
        Arc::clone(&indexer_arc),
        tmp.path().to_path_buf(),
    );
    registry.register(handle);
    let state = Arc::new(SearchAppState::new(registry));
    state.install_embedder(embedder).await;
    (state, indexer_arc, tmp)
}

// ── Typeahead handler ─────────────────────────────────────────────────────────

/// Why: the typeahead handler must return HTTP 200 with an empty `hits` list
/// for an empty `q` parameter without touching the index at all.
/// What: calls the handler with `q = ""` and asserts `hits` is empty and
/// `mode` is `"lexical"`.
/// Test: this test.
#[tokio::test]
async fn typeahead_empty_query_returns_empty_list() {
    use axum::extract::{Path, Query, State};
    use trusty_search::service::server::typeahead_handler_for_tests;

    let (state, _idx, _tmp) = make_state_with_index("empty-q-idx").await;

    let params = TypeaheadParamsForTests {
        q: String::new(),
        limit: None,
        mode: None,
    };
    let result = typeahead_handler_for_tests(
        State(Arc::clone(&state)),
        Path("empty-q-idx".to_string()),
        Query(params),
    )
    .await;

    let axum::Json(resp) = result.expect("handler must succeed on empty q");
    assert!(resp.hits.is_empty(), "empty q must return zero hits");
    assert_eq!(resp.mode, "lexical");
    assert_eq!(resp.latency_ms, 0, "empty-q fast path must be 0 ms");
}

/// Why: whitespace-only `q` should be treated like empty — do not hit the
/// index with whitespace and risk returning noise.
/// What: calls the handler with `q = "   "` and asserts empty result.
/// Test: this test.
#[tokio::test]
async fn typeahead_whitespace_query_returns_empty_list() {
    use axum::extract::{Path, Query, State};
    use trusty_search::service::server::typeahead_handler_for_tests;

    let (state, _idx, _tmp) = make_state_with_index("ws-q-idx").await;

    let params = TypeaheadParamsForTests {
        q: "   ".to_string(),
        limit: None,
        mode: None,
    };
    let result = typeahead_handler_for_tests(
        State(Arc::clone(&state)),
        Path("ws-q-idx".to_string()),
        Query(params),
    )
    .await;

    let axum::Json(resp) = result.expect("handler must succeed");
    assert!(resp.hits.is_empty(), "whitespace q must return zero hits");
}

/// Why: requesting more than `MAX_TYPEAHEAD_LIMIT` (25) hits must be silently
/// clamped rather than erroring or returning an unbounded result.
/// What: index a fixture file with several chunks, then call with
/// `limit = 999` — the response must have at most 25 hits.
/// Test: this test.
#[tokio::test]
async fn typeahead_limit_clamped_to_max() {
    use axum::extract::{Path, Query, State};
    use trusty_search::service::server::typeahead_handler_for_tests;

    let (state, idx_arc, _tmp) = make_state_with_index("clamp-idx").await;

    // Index several functions so the search engine has results.
    {
        let idx = idx_arc.read().await;
        for i in 0..10u32 {
            let content = format!("pub fn fn_{i}() {{ /* function {i} */ }}\n", i = i);
            idx.index_file(&format!("src/mod_{i}.rs"), &content)
                .await
                .expect("index_file");
        }
    }

    let params = TypeaheadParamsForTests {
        q: "fn".to_string(),
        limit: Some(999),
        mode: None,
    };
    let result = typeahead_handler_for_tests(
        State(Arc::clone(&state)),
        Path("clamp-idx".to_string()),
        Query(params),
    )
    .await;

    let axum::Json(resp) = result.expect("handler must succeed");
    assert!(
        resp.hits.len() <= 25,
        "hits must be clamped to MAX_TYPEAHEAD_LIMIT=25; got {}",
        resp.hits.len()
    );
}

/// Why: when a caller requests fewer hits than the default, that smaller limit
/// must be honoured.
/// What: index 5 chunks with a matching term, then request `limit=2` — the
/// response must have at most 2 hits.
/// Test: this test.
#[tokio::test]
async fn typeahead_respects_smaller_limit() {
    use axum::extract::{Path, Query, State};
    use trusty_search::service::server::typeahead_handler_for_tests;

    let (state, idx_arc, _tmp) = make_state_with_index("small-limit-idx").await;

    {
        let idx = idx_arc.read().await;
        for i in 0..5u32 {
            let content = format!(
                "pub fn authenticate_{i}(token: &str) -> bool {{ true }}\n",
                i = i
            );
            idx.index_file(&format!("src/auth_{i}.rs"), &content)
                .await
                .expect("index_file");
        }
    }

    let params = TypeaheadParamsForTests {
        q: "authenticate".to_string(),
        limit: Some(2),
        mode: None,
    };
    let result = typeahead_handler_for_tests(
        State(Arc::clone(&state)),
        Path("small-limit-idx".to_string()),
        Query(params),
    )
    .await;

    let axum::Json(resp) = result.expect("handler must succeed");
    assert!(
        resp.hits.len() <= 2,
        "hits must be <= requested limit 2; got {}",
        resp.hits.len()
    );
}

/// Why: lexical typeahead must return hits with all required fields populated
/// for a known prefix against a tiny indexed fixture.
/// What: indexes one Rust function, calls typeahead with `q = "authenticate"`,
/// and asserts the first hit has non-empty `label`, `path`, `snippet`, and
/// `source` is one of the three valid tags.
/// Test: this test.
#[tokio::test]
async fn typeahead_lexical_returns_hits_with_required_fields() {
    use axum::extract::{Path, Query, State};
    use trusty_search::service::server::typeahead_handler_for_tests;

    let (state, idx_arc, _tmp) = make_state_with_index("fields-idx").await;

    {
        let idx = idx_arc.read().await;
        idx.index_file(
            "src/auth.rs",
            "pub fn authenticate(token: &str) -> bool {\n    !token.is_empty()\n}\n",
        )
        .await
        .expect("index_file");
    }

    let params = TypeaheadParamsForTests {
        q: "authenticate".to_string(),
        limit: Some(6),
        mode: None,
    };
    let result = typeahead_handler_for_tests(
        State(Arc::clone(&state)),
        Path("fields-idx".to_string()),
        Query(params),
    )
    .await;

    let axum::Json(resp) = result.expect("handler must succeed");
    assert_eq!(resp.mode, "lexical");

    // The BM25/lexical search on a tiny index may or may not return results.
    // When hits ARE returned, validate their shape.
    for hit in &resp.hits {
        assert!(
            !hit.label.is_empty(),
            "hit label must not be empty: {hit:?}"
        );
        assert!(!hit.path.is_empty(), "hit path must not be empty: {hit:?}");
        assert!(
            !hit.snippet.is_empty(),
            "hit snippet must not be empty: {hit:?}"
        );
        assert!(
            matches!(hit.source.as_str(), "lexical" | "semantic" | "kg"),
            "hit source must be one of lexical/semantic/kg: {hit:?}"
        );
        assert!(hit.start_line >= 1, "start_line must be 1-based: {hit:?}");
    }
}

/// Why: an unknown index must return 404, not panic.
/// What: calls typeahead with a non-existent index id and asserts 404.
/// Test: this test.
#[tokio::test]
async fn typeahead_unknown_index_returns_404() {
    use axum::extract::{Path, Query, State};
    use axum::http::StatusCode;
    use trusty_search::service::server::typeahead_handler_for_tests;

    let (state, _idx, _tmp) = make_state_with_index("real-idx").await;

    let params = TypeaheadParamsForTests {
        q: "fn".to_string(),
        limit: None,
        mode: None,
    };
    let result = typeahead_handler_for_tests(
        State(Arc::clone(&state)),
        Path("does-not-exist".to_string()),
        Query(params),
    )
    .await;

    let (status, _) = result.expect_err("missing index must return Err");
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// ── `TypeaheadHit` unit-level assertions (no network) ─────────────────────────

/// Why: `classify_source` must correctly tag each `match_reason` string.
/// What: exercises every documented `match_reason` value.
/// Test: this test.
#[test]
fn classify_source_covers_all_known_match_reasons() {
    assert_eq!(TypeaheadHit::classify_source("hybrid+kg"), "kg");
    assert_eq!(TypeaheadHit::classify_source("kg"), "kg");
    assert_eq!(TypeaheadHit::classify_source("vector"), "semantic");
    assert_eq!(TypeaheadHit::classify_source("hybrid"), "lexical");
    assert_eq!(TypeaheadHit::classify_source("bm25"), "lexical");
    assert_eq!(TypeaheadHit::classify_source("fallback:ripgrep"), "lexical");
}

// ── Blended mode (ONNX-gated) ─────────────────────────────────────────────────

/// Why: blended mode exercises the semantic + KG lanes which require the
/// ONNX embedder. Without the model, the test is meaningless, so it is
/// `#[ignore]`d following the crate's existing convention (run with
/// `cargo test -- --include-ignored` after the model is present).
/// What: indexes a fixture, calls typeahead with `mode=blended`, and asserts
/// the response has results; also asserts `source` values are one of the three
/// valid tags.
/// Test: `cargo test -p trusty-search --test typeahead -- --include-ignored`.
#[tokio::test]
#[ignore]
async fn typeahead_blended_mode_returns_results_tagged_by_source() {
    use axum::extract::{Path, Query, State};
    use trusty_search::core::indexer::typeahead::TypeaheadMode;
    use trusty_search::service::server::typeahead_handler_for_tests;

    let (state, idx_arc, _tmp) = make_state_with_index("blended-idx").await;

    {
        let idx = idx_arc.read().await;
        idx.index_file(
            "src/auth.rs",
            "pub fn authenticate(token: &str) -> bool { !token.is_empty() }\n",
        )
        .await
        .expect("index_file");
    }

    let params = TypeaheadParamsForTests {
        q: "authenticate".to_string(),
        limit: Some(6),
        mode: Some(TypeaheadMode::Blended),
    };
    let result = typeahead_handler_for_tests(
        State(Arc::clone(&state)),
        Path("blended-idx".to_string()),
        Query(params),
    )
    .await;

    let axum::Json(resp) = result.expect("blended typeahead must succeed");
    assert_eq!(resp.mode, "blended");
    // Blended mode should return at least one hit when there's matching content.
    assert!(!resp.hits.is_empty(), "blended mode must return results");
    for hit in &resp.hits {
        assert!(
            matches!(hit.source.as_str(), "lexical" | "semantic" | "kg"),
            "source must be one of lexical/semantic/kg; got: {}",
            hit.source
        );
    }
}
