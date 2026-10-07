//! #9235: `index_status` and the search `meta` block report BM25 truncation.
//!
//! Why: the BM25 corpus cap dropped chunks with no field naming it; a caller
//! saw a converged index whose lexical lane could not find some chunks.
//! What: a BM25-only index of four chunks, committed through the ingest path
//! under `TRUSTY_BM25_CORPUS_CAP=2` (set at spawn in an isolated child, #6369),
//! read back through both bodies; plus a corpus-backed index whose durable
//! count errors while evicted.
//! Test: this file.

use super::*;
use crate::core::chunker::{ChunkType, RawChunk};
use crate::core::indexer::{CodeIndexer, ParsedBatch};
use crate::core::registry::{IndexHandle, IndexId, IndexRegistry, StageStatus};
use axum::extract::State;
use std::sync::Arc;
use tokio::sync::RwLock;

fn chunk(i: usize) -> RawChunk {
    RawChunk {
        id: format!("f{i}.rs:1:1"),
        file: format!("f{i}.rs"),
        start_line: 1,
        end_line: 1,
        content: format!("fn shared_probe_{i}() {{ shared_probe(); }}"),
        function_name: Some(format!("shared_probe_{i}")),
        language: Some("rust".into()),
        chunk_type: ChunkType::Code,
        calls: vec![],
        inherits_from: vec![],
        chunk_depth: 0,
        parent_chunk_id: None,
        child_chunk_ids: vec![],
        nlp_keywords: vec![],
        nlp_code_refs: vec![],
        virtual_terms: vec![],
    }
}

async fn state_with_chunks(id: &str, n: usize) -> (Arc<SearchAppState>, tempfile::TempDir) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let indexer = CodeIndexer::new(id, tmp.path());
    commit_chunks(&indexer, n).await;
    (register(id, indexer, tmp.path()).await, tmp)
}

async fn commit_chunks(indexer: &CodeIndexer, n: usize) {
    let parsed = ParsedBatch {
        chunks: (0..n).map(chunk).collect(),
        embeddings: vec![None; n],
        entities_by_file: vec![],
        parse_ms: 0,
        embed_ms: 0,
        vector_count: 0,
    };
    indexer
        .commit_parsed_batch(parsed, false)
        .await
        .expect("commit");
}

async fn register(id: &str, indexer: CodeIndexer, root: &std::path::Path) -> Arc<SearchAppState> {
    let handle = IndexHandle::bare(
        IndexId::new(id),
        Arc::new(RwLock::new(indexer)),
        root.to_path_buf(),
    );
    handle.stages.write().await.lexical.status = StageStatus::Ready;
    let registry = IndexRegistry::new();
    registry.register(handle);
    Arc::new(SearchAppState::new(registry))
}

async fn status_and_search_meta(
    state: &Arc<SearchAppState>,
    id: &str,
) -> (serde_json::Value, serde_json::Value) {
    let status = super::status::index_status_report(state, id)
        .await
        .unwrap_or_else(|(code, body)| panic!("status {code}: {body}"));
    let query =
        serde_json::from_value(serde_json::json!({ "text": "shared_probe" })).expect("SearchQuery");
    let axum::Json(body) = super::search::search_handler(
        State(Arc::clone(state)),
        axum::extract::Path(id.to_string()),
        axum::Json(query),
    )
    .await
    .unwrap_or_else(|(code, axum::Json(body))| panic!("search {code}: {body}"));
    (status, body["meta"].clone())
}

/// Four chunks over a cap of two: both bodies report two dropped.
#[tokio::test]
#[serial_test::parallel]
async fn status_and_search_meta_report_bm25_truncation_over_the_cap() {
    if !crate::service::test_isolation::run_isolated(
        "service::server::tests_9235::status_and_search_meta_report_bm25_truncation_over_the_cap",
        &[("TRUSTY_BM25_CORPUS_CAP", "2")],
    ) {
        return;
    }
    let (state, _tmp) = state_with_chunks("bm25-over-9235", 4).await;
    let (status, meta) = status_and_search_meta(&state, "bm25-over-9235").await;
    for body in [&status, &meta] {
        assert_eq!(body["bm25_truncated"], true, "{body}");
        assert_eq!(body["bm25_docs_dropped"], 2, "{body}");
        assert_eq!(body["bm25_corpus_cap"], 2, "{body}");
        assert!(
            body["bm25_truncation_unavailable_reason"].is_null(),
            "{body}"
        );
    }
    // #9235: truncation is not the not-converged signal.
    assert_eq!(meta["bm25_lane_degraded"], false, "{meta}");
}

/// Four chunks under a cap of 1000: both bodies report no truncation.
#[tokio::test]
#[serial_test::parallel]
async fn status_and_search_meta_report_no_bm25_truncation_under_the_cap() {
    if !crate::service::test_isolation::run_isolated(
        "service::server::tests_9235::status_and_search_meta_report_no_bm25_truncation_under_the_cap",
        &[("TRUSTY_BM25_CORPUS_CAP", "1000")],
    ) {
        return;
    }
    let (state, _tmp) = state_with_chunks("bm25-under-9235", 4).await;
    let (status, meta) = status_and_search_meta(&state, "bm25-under-9235").await;
    for body in [&status, &meta] {
        assert_eq!(body["bm25_truncated"], false, "{body}");
        assert_eq!(body["bm25_docs_dropped"], 0, "{body}");
        assert_eq!(body["bm25_corpus_cap"], 1000, "{body}");
        assert!(
            body["bm25_truncation_unavailable_reason"].is_null(),
            "{body}"
        );
    }
}

/// Evicted, with a durable count that errors: the three fields are unknown,
/// not "nothing dropped" (the Fail-Open Check). Before the fix an unreadable
/// count read as 0 and the status reported `bm25_truncated: false`.
#[tokio::test]
#[serial_test::parallel]
async fn status_reports_bm25_truncation_unavailable_when_the_durable_count_errors() {
    let id = "bm25-unreadable-9235";
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut indexer = CodeIndexer::new(id, tmp.path());
    let corpus = Arc::new(
        crate::core::corpus::CorpusStore::open(&tmp.path().join("index.redb"))
            .expect("open corpus"),
    );
    indexer.set_corpus_store(Arc::clone(&corpus));
    commit_chunks(&indexer, 4).await;
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    let evicted = indexer
        .evict_bm25_entities_if_idle(std::time::Duration::from_nanos(1))
        .await;
    assert!(
        evicted > 0 && indexer.corpus_evicted(),
        "BM25 must be evicted"
    );
    // Drop the chunks table so `CorpusStore::chunk_count` errors.
    let txn = corpus.db().begin_write().expect("write txn");
    txn.delete_table(redb::TableDefinition::<&str, &[u8]>::new("chunks"))
        .expect("delete chunks table");
    txn.commit().expect("commit");
    assert!(
        corpus.chunk_count().is_err(),
        "the durable count must error"
    );

    let state = register(id, indexer, tmp.path()).await;
    let status = super::status::index_status_report(&state, id)
        .await
        .unwrap_or_else(|(code, body)| panic!("status {code}: {body}"));
    for field in ["bm25_truncated", "bm25_docs_dropped", "bm25_corpus_cap"] {
        assert!(status[field].is_null(), "{field} must be unknown: {status}");
    }
    assert_eq!(
        status["bm25_truncation_unavailable_reason"], "corpus_count_unreadable",
        "{status}"
    );
}
