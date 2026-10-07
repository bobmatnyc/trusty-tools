//! #8348: a failed query embed degrades to lexical search instead of a 500.
//!
//! Why: an embedder sidecar that failed to spawn turned every hybrid query into
//! `500 internal search error` while BM25 and the corpus were intact, and
//! `/health` still reported the embedder `ready`.
//! What: handler-level tests against an index whose embedder always fails and
//! whose semantic stage is `Ready`, so the vector lane is really attempted.
//! Test: this file.

use super::*;
use crate::core::chunker::{ChunkType, RawChunk};
use crate::core::embed::Embedder;
use crate::core::indexer::{CodeIndexer, ParsedBatch};
use crate::core::registry::{IndexHandle, IndexId, IndexRegistry, StageStatus};
use crate::core::store::{UsearchStore, VectorStore};
use axum::extract::State;
use axum::http::StatusCode;
use std::sync::Arc;
use tokio::sync::RwLock;

/// An embedder whose every call fails, like a sidecar that cannot spawn.
struct SpawnFailingEmbedder;

#[async_trait::async_trait]
impl Embedder for SpawnFailingEmbedder {
    async fn embed(&self, _text: &str) -> anyhow::Result<Vec<f32>> {
        anyhow::bail!("lazy embedderd spawn failed: sidecar startup probe failed")
    }
    async fn embed_batch(&self, _texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        anyhow::bail!("lazy embedderd spawn failed: sidecar startup probe failed")
    }
    fn dimension(&self) -> usize {
        8
    }
}

/// Build a state holding one index with a lexically-findable chunk, a failing
/// embedder, and every stage `Ready`.
async fn state_with_failing_embedder(id: &str) -> (Arc<SearchAppState>, tempfile::TempDir) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store: Arc<dyn VectorStore> = Arc::new(UsearchStore::new(8).expect("usearch"));
    let embedder: Arc<dyn Embedder> = Arc::new(SpawnFailingEmbedder);
    let indexer = CodeIndexer::new(id, tmp.path()).with_components(Arc::clone(&embedder), store);
    let chunk = RawChunk {
        id: "auth.rs:1:3".into(),
        file: "auth.rs".into(),
        start_line: 1,
        end_line: 3,
        content: "fn authenticate_session_token() { verify_session_token(); }".into(),
        function_name: Some("authenticate_session_token".into()),
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
    };
    let parsed = ParsedBatch {
        chunks: vec![chunk],
        embeddings: vec![None],
        entities_by_file: vec![],
        parse_ms: 0,
        embed_ms: 0,
        vector_count: 0,
    };
    indexer
        .commit_parsed_batch(parsed, false)
        .await
        .expect("commit");
    let handle = IndexHandle::bare(
        IndexId::new(id),
        Arc::new(RwLock::new(indexer)),
        tmp.path().to_path_buf(),
    );
    {
        let mut stages = handle.stages.write().await;
        stages.lexical.status = StageStatus::Ready;
        stages.semantic.status = StageStatus::Ready;
        stages.graph.status = StageStatus::Ready;
    }
    let registry = IndexRegistry::new();
    registry.register(handle);
    let state = Arc::new(SearchAppState::new(registry).with_embedder(embedder));
    (state, tmp)
}

fn query(body: serde_json::Value) -> crate::core::indexer::SearchQuery {
    serde_json::from_value(body).expect("SearchQuery")
}

/// An unpinned query answers 200 with the lexical hit, flags the vector lane
/// unavailable with the embedder's error, and `/health` stops saying `ready`.
///
/// Pre-fix the `search_handler` call returned `Err((500, …))`.
/// Test: this test.
#[tokio::test]
async fn a_failed_query_embed_degrades_to_lexical_hits_flagged_vector_unavailable() {
    let (state, _tmp) = state_with_failing_embedder("fail-embed-8348").await;
    let axum::Json(body) = super::search::search_handler(
        State(Arc::clone(&state)),
        axum::extract::Path("fail-embed-8348".to_string()),
        axum::Json(query(
            serde_json::json!({ "text": "authenticate session token" }),
        )),
    )
    .await
    .unwrap_or_else(|(status, axum::Json(body))| panic!("expected 200, got {status}: {body}"));

    let results = body["results"].as_array().expect("results array");
    assert!(
        results.iter().any(|r| r["id"] == "auth.rs:1:3"),
        "the lexical lane must still answer: {body}"
    );
    assert_eq!(body["meta"]["vector_unavailable"], true, "{body}");
    assert!(
        body["meta"]["embedder_error"]
            .as_str()
            .is_some_and(|e| e.contains("spawn failed")),
        "the degraded response names the embedder failure: {body}"
    );

    let axum::Json(health) = super::health::health_handler(State(Arc::clone(&state))).await;
    assert_eq!(
        health.embedder, "stalled",
        "a failing embedder must not read as ready on /health"
    );
}

/// A query that PINNED the semantic lane gets `503 vector_unavailable`, not a
/// 500 and not lexical rows.
///
/// Test: this test.
#[tokio::test]
async fn a_pinned_semantic_query_with_a_failed_embed_is_503_not_500() {
    let (state, _tmp) = state_with_failing_embedder("fail-embed-pinned-8348").await;
    let (status, axum::Json(body)) = super::search::search_handler(
        State(Arc::clone(&state)),
        axum::extract::Path("fail-embed-pinned-8348".to_string()),
        axum::Json(query(serde_json::json!({
            "text": "authenticate session token",
            "stage": "semantic",
        }))),
    )
    .await
    .expect_err("a pinned semantic query cannot be answered lexically");
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(body["error"], "vector_unavailable");
    assert_eq!(body["reason"], "embedder_unavailable");
    assert_eq!(body["retryable"], true);
}

/// One failed query embed through the daemon's pool counts as exactly one
/// embedder failure on `/health`.
///
/// Pre-fix the pool recorded the failure and the search handler recorded it
/// again, so the count read 2.
/// Test: this test.
#[tokio::test]
#[serial_test::parallel]
async fn one_failed_query_embed_counts_once_against_the_embedder() {
    let id = "fail-embed-count-8348";
    let (state, _tmp) = state_with_failing_embedder(id).await;
    let pool = Arc::new(
        crate::service::embed_pool::EmbedPool::new(1, Arc::new(SpawnFailingEmbedder))
            .with_stall_tracker(Arc::clone(&state.embedder_stall_tracker)),
    );
    let handle = state.registry.get(&IndexId::new(id)).expect("registered");
    handle.indexer.write().await.set_embed_pool(Some(pool));
    let before = state.embedder_stall_tracker.recent_timeout_count();

    let axum::Json(body) = super::search::search_handler(
        State(Arc::clone(&state)),
        axum::extract::Path(id.to_string()),
        axum::Json(query(
            serde_json::json!({ "text": "authenticate session token" }),
        )),
    )
    .await
    .unwrap_or_else(|(status, axum::Json(body))| panic!("expected 200, got {status}: {body}"));
    assert_eq!(body["meta"]["vector_unavailable"], true, "{body}");

    assert_eq!(
        state.embedder_stall_tracker.recent_timeout_count() - before,
        1,
        "one failed query embed must count exactly once"
    );
}
