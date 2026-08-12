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
                path_prefix: None,
                repos: Vec::new(),
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
        path_prefix: None,
        repos: Vec::new(),
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
            path_prefix: None,
            repos: Vec::new(),
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
            max_fanout_concurrency: None,
            serial: false,
            path_prefix: None,
            repos: Vec::new(),
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

// ── #4951: the two root_path copies must never diverge ───────────────────────

/// #4951: a `reindex` `root_path` override must move the SHARED indexer's own
/// `root_path` too, or the search post-filter discards every result.
///
/// Why: `IndexHandle::root_path` and `CodeIndexer::root_path` are two copies of
/// one fact. The indexer's copy is the base every stored root-relative chunk
/// path is joined against to build the absolute `CodeChunk::file`
/// (`raw_to_code_chunk`); the handle's copy is what `search_handler` post-filters
/// those absolute paths against (`file_is_within_root`, added for #64/#541). The
/// override rebuilds the handle around the SAME indexer `Arc`, so leaving the
/// indexer on the old root made every materialized `file` fall outside the new
/// root: 100% of candidates were dropped and search returned `results: []` with
/// `stale_index_root: true` — on an index whose `/status` read `ready` with
/// 85,642 chunks and a populated `search_capabilities`. JIRA context was absent
/// from every PR review for 40+ days behind this.
/// What: reproduces the exact production shape — a root re-pointed one level
/// down (`/knowledge` → `/knowledge/Jira`) with a chunk stored relative to it —
/// and asserts the resolved absolute path passes the post-filter. Pre-fix,
/// `set_root_path` does not exist and the stale join produces
/// `/knowledge/ACP/ACP-1.md`, which fails `file_is_within_root` against
/// `/knowledge/Jira`.
/// Test: this test.
#[test]
fn stale_indexer_root_makes_every_chunk_fail_the_search_post_filter() {
    use crate::core::indexer::helpers::resolve_chunk_file;

    let old_root = std::path::Path::new("/knowledge");
    let new_root = std::path::Path::new("/knowledge/Jira");
    // What the walker stores: a path relative to the CURRENT root.
    let stored_relative = "ACP/ACP-1.md";

    // The defect, stated as an assertion: materializing against the stale root
    // yields an absolute path the post-filter rejects.
    let stale_file = resolve_chunk_file(stored_relative, old_root);
    assert_eq!(stale_file, "/knowledge/ACP/ACP-1.md");
    assert!(
        !file_is_within_root(&stale_file, new_root),
        "#4951: this is the drop — a chunk built against the old root cannot \
         pass the new root's containment check, so search returns nothing"
    );

    // The fix: once the indexer's root moves with the handle's, the same stored
    // chunk resolves inside the new root and survives the filter.
    let fixed_file = resolve_chunk_file(stored_relative, new_root);
    assert_eq!(fixed_file, "/knowledge/Jira/ACP/ACP-1.md");
    assert!(
        file_is_within_root(&fixed_file, new_root),
        "#4951: with the roots in lockstep the chunk must survive the post-filter"
    );

    // `path` (the portable root-relative form) stays correct in BOTH cases —
    // which is why the mismatch was invisible on `/status` and surfaced only as
    // an empty result set. `raw_to_code_chunk_populates_path_for_relative_file`
    // pins that half.
}

/// #4951: `CodeIndexer::set_root_path` is the single way the reindex root
/// override keeps the indexer in lockstep with its rebuilt handle.
///
/// Why: without a mutator the override had no way to move the shared indexer at
/// all — the divergence was unfixable at the call site, which is why it shipped.
/// What: builds an indexer at one root, moves it, and asserts chunk resolution
/// follows.
/// Test: this test.
#[test]
fn reindex_root_override_syncs_indexer_root_path() {
    use crate::core::indexer::CodeIndexer;

    let mut indexer = CodeIndexer::new("atlassian", "/knowledge");
    assert_eq!(indexer.root_path, std::path::PathBuf::from("/knowledge"));

    indexer.set_root_path("/knowledge/Jira");

    assert_eq!(
        indexer.root_path,
        std::path::PathBuf::from("/knowledge/Jira"),
        "#4951: the override must re-point the indexer, not just the handle"
    );
    assert!(
        file_is_within_root(
            &crate::core::indexer::helpers::resolve_chunk_file("ACP/ACP-1.md", &indexer.root_path),
            std::path::Path::new("/knowledge/Jira"),
        ),
        "#4951: chunks must resolve inside the new root after the sync"
    );
}

// ── Reporting the bound: rows dropped after fusion ────────────────────────────

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
