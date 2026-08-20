//! `POST /indexes/:id/search` must refuse an index whose corpus cannot be read
//! (#5917), not answer it with an empty result set.
//!
//! Why: the daemon returned `HTTP 200` with `results: []` for indexes holding
//! tens of thousands of chunks, and the only hint was `meta.bm25_lane_degraded`
//! — a flag that means "still warming up". A single-shot caller had no field
//! that would reveal the answer was wrong, which is how a green audit reports
//! nothing and still looks thorough.
//! What: builds a corpus-backed index over a real redb file, breaks its reads
//! the way redb's "Previous I/O error occurred" state does, and asserts the
//! handler's status and body. Asserts through the response, not the Rust API,
//! so the shape a caller branches on is what is pinned.
//! Test: this module.

use super::*;
use axum::http::StatusCode;
use axum::Json;

/// Same definition redb keyed the corpus rows under — redb identifies a table
/// by name, so dropping this one breaks the real reads.
const CHUNKS_TABLE: redb::TableDefinition<'static, &str, &[u8]> =
    redb::TableDefinition::new("chunks");

/// #5917: an unreadable corpus is a `503 index_corpus_unavailable` naming the
/// index and the fault, never a `200` with an empty result set.
///
/// Why: #4087's guard covers a corpus that fails to OPEN. This one opened, was
/// populated, and then went bad — so the guard passes, the rehydrate that would
/// refill the in-memory caches fails silently, and every lane answers from
/// nothing.
/// What: indexes a file, proves the query works, drops the chunks table, evicts
/// the caches, and re-runs the identical request. Before the fix the handler
/// returned `200` with `results: []`.
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn search_over_an_unreadable_corpus_returns_503_naming_the_index() {
    use crate::core::corpus::CorpusStore;
    use crate::core::indexer::CodeIndexer;
    use crate::core::registry::{IndexHandle, IndexId, IndexRegistry, StageStatus};
    use tempfile::tempdir;

    let tmp = tempdir().unwrap();
    let corpus = Arc::new(CorpusStore::open(&tmp.path().join("index.redb")).expect("open corpus"));
    let mut indexer = CodeIndexer::new("corpus-read-5917", tmp.path());
    indexer.set_corpus_store(Arc::clone(&corpus));
    indexer
        .index_files_batch(&[("src/a.rs".into(), "fn authenticate_user() {}".into())])
        .await
        .expect("index batch");

    let registry = IndexRegistry::new();
    let handle = IndexHandle::bare(
        IndexId::new("corpus-read-5917"),
        Arc::new(tokio::sync::RwLock::new(indexer)),
        tmp.path().to_path_buf(),
    );
    let stages = Arc::clone(&handle.stages);
    registry.register(handle);
    {
        let mut s = stages.write().await;
        s.lexical.status = StageStatus::Ready;
    }
    let state = Arc::new(SearchAppState::new(registry));

    let query = |text: &str| crate::core::indexer::SearchQuery {
        text: text.to_string(),
        top_k: 10,
        expand_graph: false,
        compact: false,
        branch_files: None,
        branch_boost: 1.0,
        branch: None,
        stage: None,
        mode: crate::core::indexer::SearchMode::All,
        exclude_archived: false,
        refine_query: None,
        path_prefix: None,
        repos: Vec::new(),
    };
    let call = |state: Arc<SearchAppState>, q| {
        search_handler(
            axum::extract::State(state),
            axum::extract::Path("corpus-read-5917".to_string()),
            axum::extract::Json(q),
        )
    };

    let Json(warm) = call(Arc::clone(&state), query("authenticate_user"))
        .await
        .expect("the query answers while the corpus reads");
    assert!(
        !warm["results"]
            .as_array()
            .expect("results array")
            .is_empty(),
        "the query matches before the corpus breaks; body={warm:?}"
    );

    {
        let txn = corpus.db().begin_write().expect("begin write txn");
        assert!(
            txn.delete_table(CHUNKS_TABLE).expect("delete chunks table"),
            "the chunks table existed before the test broke it"
        );
        txn.commit().expect("commit table drop");
    }
    // SAFETY: serialized against every other reader/writer of this var by
    // `#[serial_test::serial]` on this test.
    unsafe { std::env::set_var("TRUSTY_REHYDRATE_WAIT_MS", "25") };
    {
        let handle = state
            .registry
            .get(&IndexId::new("corpus-read-5917"))
            .expect("registered");
        let evicted = handle.indexer.read().await.reclaim_memory_now().await;
        assert!(evicted > 0, "eviction dropped the in-memory caches");
    }

    let result = call(Arc::clone(&state), query("authenticate_user")).await;
    // SAFETY: same serialization as the `set_var` above.
    unsafe { std::env::remove_var("TRUSTY_REHYDRATE_WAIT_MS") };

    let (status, Json(body)) = result.expect_err(
        "an unreadable corpus must be refused — answering it with an empty \
         result set is #5917",
    );
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "body={body:?}");
    assert_eq!(body["error"], "index_corpus_unavailable", "body={body:?}");
    assert_eq!(body["index_id"], "corpus-read-5917", "body={body:?}");
    assert_eq!(body["retryable"], serde_json::json!(true), "body={body:?}");
    assert_eq!(body["failure_kind"], "read_failed", "body={body:?}");
    let message = body["message"].as_str().expect("message is a string");
    assert!(
        message.contains("corpus-read-5917") && message.contains("#5917"),
        "the message names the index and the ticket; message={message}"
    );
}
