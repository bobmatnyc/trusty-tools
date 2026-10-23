//! #10194: a query embed waits at most `TRUSTY_QUERY_EMBED_DEADLINE_MS` for a
//! cold embedder, then answers from the lexical lane.
//!
//! Why: a query that needed the sidecar spawned waited out the whole spawn
//! (3-21 s measured, up to the 30 s startup timeout) and shared the 30 s query
//! deadline with it, so a slow spawn answered `408 query_timeout`.
//! What: handler-level tests against an index whose embedder takes
//! [`COLD_START`] to come up, with the deadline set far below it.
//! Test: this file.

use super::*;
use crate::core::chunker::{ChunkType, RawChunk};
use crate::core::embed::Embedder;
use crate::core::indexer::{CodeIndexer, ParsedBatch};
use crate::core::registry::{IndexHandle, IndexId, IndexRegistry, StageStatus};
use crate::core::store::{UsearchStore, VectorStore};
use axum::extract::State;
use axum::http::StatusCode;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

/// How long the stub embedder takes to "spawn".
const COLD_START: Duration = Duration::from_secs(3);
/// The deadline these tests set, far below [`COLD_START`].
const DEADLINE_MS: &str = "300";
/// Allowed overshoot past the deadline. Pre-fix a query waits out
/// [`COLD_START`], which this bound still excludes.
const SLACK: Duration = Duration::from_millis(1700);

/// An embedder whose first call "spawns" for [`COLD_START`], single-flight,
/// like `LazyEmbedderHandle` holding its state lock across `do_spawn`.
#[derive(Default)]
struct ColdStartEmbedder {
    ready: tokio::sync::Mutex<bool>,
    spawns: AtomicUsize,
}

#[async_trait::async_trait]
impl Embedder for ColdStartEmbedder {
    async fn embed(&self, _text: &str) -> anyhow::Result<Vec<f32>> {
        let mut ready = self.ready.lock().await;
        if !*ready {
            self.spawns.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(COLD_START).await;
            *ready = true;
        }
        Ok(vec![0.5; 8])
    }
    async fn embed_batch(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        let mut out = Vec::with_capacity(texts.len());
        for t in texts {
            out.push(self.embed(t).await?);
        }
        Ok(out)
    }
    fn dimension(&self) -> usize {
        8
    }
}

/// Restores an env var on drop. Only used under `#[serial]`.
struct EnvGuard(&'static str, Option<String>);

impl EnvGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let old = std::env::var(key).ok();
        // SAFETY: `#[serial]` — no other test reads or writes env meanwhile.
        unsafe { std::env::set_var(key, value) }
        Self(key, old)
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        // SAFETY: same invariant as `set`.
        unsafe {
            match &self.1 {
                Some(v) => std::env::set_var(self.0, v),
                None => std::env::remove_var(self.0),
            }
        }
    }
}

/// One index with a lexically-findable chunk, a cold-start embedder, and every
/// stage `Ready`, so the vector lane is really attempted.
async fn state_with_cold_embedder(
    id: &str,
) -> (
    Arc<SearchAppState>,
    Arc<ColdStartEmbedder>,
    tempfile::TempDir,
) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store: Arc<dyn VectorStore> = Arc::new(UsearchStore::new(8).expect("usearch"));
    let cold = Arc::new(ColdStartEmbedder::default());
    let embedder: Arc<dyn Embedder> = Arc::clone(&cold) as Arc<dyn Embedder>;
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
    (state, cold, tmp)
}

type SearchReply = Result<serde_json::Value, (StatusCode, serde_json::Value)>;

async fn search(state: &Arc<SearchAppState>, id: &str, body: serde_json::Value) -> SearchReply {
    let query = serde_json::from_value(body).expect("SearchQuery");
    super::search::search_handler(
        State(Arc::clone(state)),
        axum::extract::Path(id.to_string()),
        axum::Json(query),
    )
    .await
    .map(|axum::Json(b)| b)
    .map_err(|(s, axum::Json(b))| (s, b))
}

/// A cold query answers 200 with the lexical hit inside the deadline, flagged
/// `vector_unavailable`; the spawn keeps running, and a later query is hybrid
/// on the same single spawn.
///
/// Pre-fix the first query waited out [`COLD_START`] and came back hybrid.
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn a_cold_embedder_answers_lexically_within_the_query_embed_deadline() {
    let _env = EnvGuard::set("TRUSTY_QUERY_EMBED_DEADLINE_MS", DEADLINE_MS);
    let id = "cold-embed-10194";
    let (state, cold, _tmp) = state_with_cold_embedder(id).await;

    let started = Instant::now();
    let body = search(
        &state,
        id,
        serde_json::json!({ "text": "authenticate session token" }),
    )
    .await
    .unwrap_or_else(|(s, b)| panic!("expected 200, got {s}: {b}"));
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_millis(300) + SLACK,
        "the query must not wait out the cold start: {elapsed:?}"
    );
    let results = body["results"].as_array().expect("results array");
    assert!(
        results.iter().any(|r| r["id"] == "auth.rs:1:3"),
        "the lexical lane must answer: {body}"
    );
    assert_eq!(body["meta"]["vector_unavailable"], true, "{body}");
    assert!(
        body["meta"]["embedder_error"]
            .as_str()
            .is_some_and(|e| e.contains("TRUSTY_QUERY_EMBED_DEADLINE_MS")),
        "the degraded reply names the deadline: {body}"
    );

    // The spawn keeps running after the query degraded: a later query is
    // hybrid, without a second spawn.
    let mut hybrid = false;
    let until = Instant::now() + COLD_START * 4;
    let mut n = 0;
    while Instant::now() < until {
        n += 1;
        let text = format!("verify session token {n}");
        let body = search(&state, id, serde_json::json!({ "text": text }))
            .await
            .unwrap_or_else(|(s, b)| panic!("expected 200, got {s}: {b}"));
        if body["meta"]["vector_unavailable"] == false {
            hybrid = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(hybrid, "a query after the spawn finished must be hybrid");
    assert_eq!(cold.spawns.load(Ordering::SeqCst), 1, "one spawn only");
}

/// Concurrent cold queries each answer inside the deadline and share one
/// spawn.
///
/// Pre-fix every query waited out [`COLD_START`].
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn concurrent_cold_queries_share_one_spawn_and_all_answer_in_time() {
    let _env = EnvGuard::set("TRUSTY_QUERY_EMBED_DEADLINE_MS", DEADLINE_MS);
    let id = "cold-embed-many-10194";
    let (state, cold, _tmp) = state_with_cold_embedder(id).await;

    let started = Instant::now();
    let mut tasks = Vec::new();
    for n in 0..4 {
        let state = Arc::clone(&state);
        tasks.push(tokio::spawn(async move {
            let text = format!("authenticate session token {n}");
            search(&state, id, serde_json::json!({ "text": text })).await
        }));
    }
    for t in tasks {
        let body = t
            .await
            .expect("join")
            .unwrap_or_else(|(s, b)| panic!("expected 200, got {s}: {b}"));
        assert_eq!(body["meta"]["vector_unavailable"], true, "{body}");
    }
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_millis(300) + SLACK,
        "no query may wait out the cold start: {elapsed:?}"
    );

    let until = Instant::now() + COLD_START * 4;
    while !*cold.ready.lock().await && Instant::now() < until {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(cold.spawns.load(Ordering::SeqCst), 1, "one spawn only");
}

/// A query that pinned the semantic lane keeps the #8348 contract on expiry:
/// `503 vector_unavailable`, retryable, inside the deadline.
///
/// Pre-fix it waited out [`COLD_START`] and answered 200.
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn a_pinned_semantic_query_on_a_cold_embedder_is_503_inside_the_deadline() {
    let _env = EnvGuard::set("TRUSTY_QUERY_EMBED_DEADLINE_MS", DEADLINE_MS);
    let id = "cold-embed-pinned-10194";
    let (state, _cold, _tmp) = state_with_cold_embedder(id).await;

    let started = Instant::now();
    let (status, body) = search(
        &state,
        id,
        serde_json::json!({ "text": "authenticate session token", "stage": "semantic" }),
    )
    .await
    .expect_err("a pinned semantic query cannot be answered lexically");
    assert!(started.elapsed() < Duration::from_millis(300) + SLACK);
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(body["error"], "vector_unavailable");
    assert_eq!(body["retryable"], true);
}
