//! Tests for `file_is_within_root` and the search handler.
use super::helpers::file_is_within_root;
use super::*;
use axum::{http::StatusCode, Json};

// ── Issue #882: empty / whitespace-only query validation ──────────────────────

/// Why: an empty query must be rejected before touching the index so callers
/// get an actionable error instead of arbitrary top-k results from a pure
/// k-NN fallback.
/// What: builds a minimal bare index and asserts search_handler returns HTTP
/// 400 with `{"error": "query must not be empty"}` for both `""` and `"   "`.
/// Test: this test.
#[tokio::test]
async fn search_handler_rejects_empty_query() {
    use crate::core::embed::{Embedder, MockEmbedder};
    use crate::core::indexer::{CodeIndexer, SearchQuery, SearchStage};
    use crate::core::registry::{IndexHandle, IndexId, IndexRegistry};
    use crate::core::store::{UsearchStore, VectorStore};
    use tempfile::tempdir;

    let tmp = tempdir().unwrap();
    let dim = 16;
    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(dim));
    let store: Arc<dyn VectorStore> = Arc::new(UsearchStore::new(dim).expect("usearch"));
    let indexer = CodeIndexer::new("empty-q-test", tmp.path())
        .with_components(Arc::clone(&embedder), Arc::clone(&store));
    let registry = IndexRegistry::new();
    let handle = IndexHandle::bare(
        IndexId::new("empty-q-idx"),
        Arc::new(tokio::sync::RwLock::new(indexer)),
        tmp.path().to_path_buf(),
    );
    registry.register(handle);
    let state = Arc::new(SearchAppState::new(registry));
    state.install_embedder(embedder).await;

    for text in ["", "   ", "\t\n"] {
        let resp = search_handler(
            axum::extract::State(Arc::clone(&state)),
            axum::extract::Path("empty-q-idx".to_string()),
            axum::extract::Json(SearchQuery {
                text: text.to_string(),
                top_k: 5,
                expand_graph: false,
                compact: false,
                branch_files: None,
                branch_boost: 1.5,
                branch: None,
                stage: Some(SearchStage::Lexical),
                mode: crate::core::indexer::SearchMode::Code,
                exclude_archived: false,
                refine_query: None,
            }),
        )
        .await;

        let (status, Json(body)) = resp.expect_err("empty query must return Err");
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "expected 400 for query={text:?}, got {status}"
        );
        assert_eq!(
            body.get("error").and_then(|v| v.as_str()),
            Some("query must not be empty"),
            "wrong error body for query={text:?}: {body:?}"
        );
    }
}

#[test]
fn file_is_within_root_relative_ok() {
    let root = std::path::Path::new("/Users/me/proj");
    assert!(file_is_within_root("src/auth.rs", root));
    assert!(file_is_within_root("./src/auth.rs", root));
    assert!(file_is_within_root("Cargo.toml", root));
}

/// Issue #64: relative paths that climb out via `..` must be rejected,
/// even though they may resolve inside `root` for some `root` values.
#[test]
fn file_is_within_root_rejects_dotdot() {
    let root = std::path::Path::new("/Users/me/proj");
    assert!(!file_is_within_root("../other/file.rs", root));
    assert!(!file_is_within_root("src/../../leak.rs", root));
}

/// Issue #64: absolute paths must literally start with the index root.
/// This is the load-bearing guard against cross-index bleed when the
/// daemon ever stores absolute file paths (e.g. legacy chunks from a
/// misregistered index — see #63).
#[test]
fn file_is_within_root_absolute_must_start_with_root() {
    let root = std::path::Path::new("/Users/me/proj");
    assert!(file_is_within_root("/Users/me/proj/src/auth.rs", root));
    assert!(!file_is_within_root(
        "/Users/me/other-proj/src/auth.rs",
        root
    ));
    assert!(!file_is_within_root("/etc/passwd", root));
}

/// Issue #64: empty file strings are defensively rejected — they should
/// never occur in a valid chunk and we don't want them sneaking past
/// the filter as a benign-looking relative path.
#[test]
fn file_is_within_root_rejects_empty() {
    let root = std::path::Path::new("/Users/me/proj");
    assert!(!file_is_within_root("", root));
}

/// Issue #541: when the index root is a symlink alias pointing at a real
/// directory, an absolute file path stored under the real (canonical) root
/// must NOT be dropped — `file_is_within_root` must fall back to
/// canonicalized comparison and return `true`.
///
/// This exercises the slow-path fallback added for #541: the lexical check
/// `/real/dir/src/auth.rs`.starts_with(`/link`) fails, so the predicate
/// canonicalizes both sides and retries.
#[cfg(unix)]
#[test]
fn file_is_within_root_symlinked_root_does_not_drop_valid_result() {
    use std::os::unix::fs::symlink;
    use tempfile::tempdir;

    // Create a real directory that will be the "canonical" root.
    let real_dir = tempdir().unwrap();
    let canonical_root = std::fs::canonicalize(real_dir.path()).unwrap();

    // Symlink → real_dir (the handle holds the symlink path as its root_path).
    let link = canonical_root
        .parent()
        .unwrap()
        .join(format!("trusty-541-root-link-{}", std::process::id()));
    let _ = std::fs::remove_file(&link);
    symlink(&canonical_root, &link).expect("create symlink");

    // A file stored with its canonical (non-symlink) absolute path — this
    // is exactly what the indexer produces after walking the real directory.
    let file_path = canonical_root.join("src/auth.rs");
    let file_str = file_path.to_str().unwrap();

    // With the link as `root`, the lexical check fails but the canonical
    // fallback must pass — the file IS within the root.
    let result = file_is_within_root(file_str, &link);
    let _ = std::fs::remove_file(&link);

    assert!(
        result,
        "file under canonical root must pass even when index root is a symlink alias; \
             file={file_str}, root={link}",
        link = link.display(),
    );
}

/// Issue #541: a file genuinely outside the root must still be rejected
/// even after the canonicalize fallback runs.
#[test]
fn file_is_within_root_outside_root_still_rejected_after_canonicalize() {
    use tempfile::tempdir;

    let root_dir = tempdir().unwrap();
    let canonical_root = std::fs::canonicalize(root_dir.path()).unwrap();

    // A path that is definitely outside the root.
    let outside = "/etc/passwd";
    assert!(
        !file_is_within_root(outside, &canonical_root),
        "path genuinely outside root must still be rejected"
    );
}

/// PR #1103: `search_handler` must consult `last_queried_write_cache` instead
/// of reading indexes.toml on the hot path, and must update the cache after
/// spawning the background write so subsequent queries within the rate-limit
/// window do NOT spawn another write task.
///
/// Why: the previous code called `persistence::read_last_queried_unix` (opens +
/// parses indexes.toml) synchronously on every warm query. The in-memory cache
/// eliminates that disk I/O.
/// What: call `search_handler` twice in rapid succession and assert that
/// `last_queried_write_cache` is populated after the first call and that the
/// cached timestamp is the same after the second call (no second write within
/// the interval).
/// Test: this test.
#[tokio::test]
async fn last_queried_cache_rate_limits_disk_writes() {
    use crate::core::embed::{Embedder, MockEmbedder};
    use crate::core::indexer::{CodeIndexer, SearchStage};
    use crate::core::registry::{IndexHandle, IndexId, IndexRegistry};
    use crate::core::store::{UsearchStore, VectorStore};
    use tempfile::tempdir;

    let tmp = tempdir().unwrap();
    let dim = 16;
    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(dim));
    let store: Arc<dyn VectorStore> = Arc::new(UsearchStore::new(dim).expect("usearch"));
    let indexer = CodeIndexer::new("cache-rate-test", tmp.path())
        .with_components(Arc::clone(&embedder), Arc::clone(&store));
    let registry = IndexRegistry::new();
    let id = IndexId::new("cache-rate-idx");
    let handle = IndexHandle::bare(
        id.clone(),
        Arc::new(tokio::sync::RwLock::new(indexer)),
        tmp.path().to_path_buf(),
    );
    registry.register(handle);
    let state = Arc::new(SearchAppState::new(registry));
    state.install_embedder(embedder).await;

    // Cache should be empty before any search.
    assert!(
        state.last_queried_write_cache.get(&id).is_none(),
        "cache must be empty before first search"
    );

    // First call — should populate the cache.
    let query = crate::core::indexer::SearchQuery {
        text: "hello cache".to_string(),
        top_k: 1,
        expand_graph: false,
        compact: false,
        branch_files: None,
        branch_boost: 1.5,
        branch: None,
        stage: Some(SearchStage::Lexical),
        mode: crate::core::indexer::SearchMode::Code,
        exclude_archived: false,
        refine_query: None,
    };
    let _ = search_handler(
        axum::extract::State(Arc::clone(&state)),
        axum::extract::Path("cache-rate-idx".to_string()),
        axum::extract::Json(query.clone()),
    )
    .await;

    let ts_after_first = *state
        .last_queried_write_cache
        .get(&id)
        .expect("cache must be populated after first search");

    // Second call immediately — cache timestamp must stay the same (rate-limited).
    let _ = search_handler(
        axum::extract::State(Arc::clone(&state)),
        axum::extract::Path("cache-rate-idx".to_string()),
        axum::extract::Json(query),
    )
    .await;

    let ts_after_second = *state
        .last_queried_write_cache
        .get(&id)
        .expect("cache must still be present after second search");

    assert_eq!(
        ts_after_first, ts_after_second,
        "cache timestamp must not change on second call within rate-limit window"
    );
}

/// Issue #541: `search_handler` must always include `stale_index_root` in
/// the response `meta` block (as a boolean). When no results are dropped by
/// the out-of-root filter the field is `false`; we verify its presence and
/// type because the BM25 / MockEmbedder may return 0 results on a minimal
/// test index, making it hard to guarantee `true` without complex setup.
/// What: builds a minimal bare index, calls `search_handler`, and asserts the
/// `stale_index_root` field is present and boolean in the `meta` block.
/// Test: this test.
#[tokio::test]
async fn search_handler_meta_includes_stale_index_root_field() {
    use crate::core::embed::{Embedder, MockEmbedder};
    use crate::core::indexer::CodeIndexer;
    use crate::core::registry::{IndexHandle, IndexId, IndexRegistry};
    use crate::core::store::{UsearchStore, VectorStore};
    use tempfile::tempdir;

    let tmp = tempdir().unwrap();
    let dim = 16;
    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(dim));
    let store: Arc<dyn VectorStore> = Arc::new(UsearchStore::new(dim).expect("usearch"));
    let indexer = CodeIndexer::new("stale-meta-test", tmp.path())
        .with_components(Arc::clone(&embedder), Arc::clone(&store));

    let registry = IndexRegistry::new();
    let handle = IndexHandle::bare(
        IndexId::new("stale-meta-idx"),
        Arc::new(tokio::sync::RwLock::new(indexer)),
        tmp.path().to_path_buf(),
    );
    registry.register(handle);

    let state = Arc::new(SearchAppState::new(registry));
    state.install_embedder(embedder).await;

    let resp = search_handler(
        axum::extract::State(Arc::clone(&state)),
        axum::extract::Path("stale-meta-idx".to_string()),
        axum::extract::Json(crate::core::indexer::SearchQuery {
            text: "hello".to_string(),
            top_k: 5,
            expand_graph: false,
            compact: false,
            branch_files: None,
            branch_boost: 1.5,
            branch: None,
            stage: Some(crate::core::indexer::SearchStage::Lexical),
            mode: crate::core::indexer::SearchMode::Code,
            exclude_archived: false,
            refine_query: None,
        }),
    )
    .await;

    let Json(body) = resp.expect("handler must succeed");
    let meta = body.get("meta").expect("meta block present");

    assert!(
        meta.get("stale_index_root").is_some(),
        "meta block must contain stale_index_root field; meta={meta:?}"
    );
    assert!(
        meta["stale_index_root"].is_boolean(),
        "stale_index_root must be a boolean; got={:?}",
        meta["stale_index_root"]
    );
    // For an empty index (no chunks were added), no results can be dropped,
    // so stale_index_root must be false.
    assert_eq!(
        meta["stale_index_root"], false,
        "stale_index_root must be false when no results were dropped"
    );
}

/// PR #1103: `POST /search` (global fan-out) must surface `cold_indexes_skipped`
/// in the response so callers know the fan-out may be incomplete when selective
/// warm-boot has not yet loaded all indexes.
///
/// Why: `registry.list()` returns only hot indexes. Cold indexes in `cold_store`
/// are silently skipped; without `cold_indexes_skipped` callers have no way to
/// distinguish "0 results" from "0 results in hot indexes but there are more".
/// What: registers one hot index and one cold index, calls global search, asserts
/// `cold_indexes_skipped == 1` in the response.
/// Test: this test.
#[tokio::test]
async fn test_global_search_surfaces_cold_indexes_skipped() {
    use crate::core::embed::{Embedder, MockEmbedder};
    use crate::core::indexer::CodeIndexer;
    use crate::core::registry::{IndexHandle, IndexId, IndexRegistry};
    use crate::core::store::{UsearchStore, VectorStore};
    use crate::service::lazy_loader::ColdIndexStore;
    use crate::service::persistence::PersistedIndex;
    use axum::extract::{Json, State};
    use tempfile::tempdir;

    let dim = 16;
    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(dim));

    // Hot index.
    let tmp_hot = tempdir().unwrap();
    let store: Arc<dyn VectorStore> = Arc::new(UsearchStore::new(dim).expect("usearch"));
    let hot_indexer = CodeIndexer::new("hot-global", tmp_hot.path())
        .with_components(Arc::clone(&embedder), Arc::clone(&store));
    let registry = IndexRegistry::new();
    let hot_handle = IndexHandle::bare(
        IndexId::new("hot-global"),
        Arc::new(tokio::sync::RwLock::new(hot_indexer)),
        tmp_hot.path().to_path_buf(),
    );
    registry.register(hot_handle);

    // Cold index: registered in cold_store but NOT in the hot registry.
    let cold_store = Arc::new(ColdIndexStore::new());
    cold_store.register_cold_entries(vec![PersistedIndex {
        id: "cold-global".to_string(),
        root_path: std::path::PathBuf::from("/tmp/cold-global"),
        ..PersistedIndex::default()
    }]);

    let mut state = SearchAppState::new(registry);
    // Swap in the cold store that has the cold entry.
    state.cold_store = cold_store;
    let state = Arc::new(state);
    state.install_embedder(embedder).await;

    let resp = global_search_handler(
        State(Arc::clone(&state)),
        Json(super::search_global::GlobalSearchRequest {
            query: "hello".to_string(),
            top_k: 5,
            full_content: false,
            indexes: None,
            routing: None,
            routing_n: None,
            routing_threshold: None,
        }),
    )
    .await;

    let Json(body) = resp.expect("global search must succeed");
    let cold_skipped = body
        .get("cold_indexes_skipped")
        .and_then(|v| v.as_u64())
        .expect("cold_indexes_skipped must be present in response");
    assert_eq!(
        cold_skipped, 1,
        "global fan-out must report 1 cold index skipped; body={body:?}"
    );
}
