//! The configurable lexical lane through the search route (#9258).
//!
//! Why: a caller measuring BM25 alone needs to switch off the content-scan
//! rows (`fallback:ripgrep`) and bound the BM25 depth, per query or by daemon
//! config, with the per-query value winning and a bad limit refused.
//! What: each test drives `search_report` with a JSON body, so the per-query
//! tests compile against the pre-#9258 commit and fail there on the body
//! being refused as an unknown field.
//! Test: this module.

use std::sync::Arc;

use axum::http::StatusCode;
use serde_json::{json, Value};

use super::search::search_report;
use super::search_global::{global_search_report, GlobalSearchRequest};
use super::SearchAppState;
use crate::core::indexer::{LexicalLaneDefaults, SearchQuery};

const INDEX: &str = "lex-9258";

/// One `=>` chunk only a content scan finds, eight `widget` chunks BM25 finds,
/// and an `HNSW` / `HNSWINDEX` pair: BM25 finds the first, only a scan the second.
async fn state_with(defaults: LexicalLaneDefaults) -> (Arc<SearchAppState>, tempfile::TempDir) {
    use crate::core::embed::{Embedder, MockEmbedder};
    use crate::core::indexer::CodeIndexer;
    use crate::core::registry::{IndexHandle, IndexId, IndexRegistry, StageStatus};
    use crate::core::store::{UsearchStore, VectorStore};

    let tmp = tempfile::tempdir().expect("tempdir");
    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(16));
    let store: Arc<dyn VectorStore> = Arc::new(UsearchStore::new(16).expect("usearch"));
    let indexer = CodeIndexer::new(INDEX, tmp.path())
        .with_components(Arc::clone(&embedder), Arc::clone(&store));
    let mut chunks = vec![(
        "src/arrow.rs:1:1".to_string(),
        "src/arrow.rs".to_string(),
        "match x { A => b }".to_string(),
    )];
    for i in 0..8 {
        chunks.push((
            format!("src/w{i}.rs:1:1"),
            format!("src/w{i}.rs"),
            format!("fn part_{i}() {{ widget }}"),
        ));
    }
    chunks.push((
        "src/hnsw.rs:1:1".to_string(),
        "src/hnsw.rs".to_string(),
        "let HNSW = 1;".to_string(),
    ));
    chunks.push((
        HNSW_SCAN_ONLY.to_string(),
        "src/hnsw_index.rs".to_string(),
        "let HNSWINDEX = 2;".to_string(),
    ));
    for (id, file, content) in &chunks {
        indexer
            .add_chunk(super::tests_dropped_results::drop_test_chunk(
                id,
                file,
                content,
                crate::core::chunker::ChunkType::Code,
            ))
            .await
            .expect("add chunk");
    }
    let registry = IndexRegistry::new();
    let handle = IndexHandle::bare(
        IndexId::new(INDEX),
        Arc::new(tokio::sync::RwLock::new(indexer)),
        tmp.path().to_path_buf(),
    );
    let stages = Arc::clone(&handle.stages);
    registry.register(handle);
    {
        let mut s = stages.write().await;
        s.lexical.status = StageStatus::Ready;
        s.semantic.status = StageStatus::Ready;
    }
    let state = Arc::new(SearchAppState::new(registry).with_lexical_defaults(defaults));
    state.install_embedder(embedder).await;
    (state, tmp)
}

/// Run a lexical-stage query built from `extra` merged into `{text, stage}`.
async fn search(
    state: &Arc<SearchAppState>,
    text: &str,
    extra: Value,
) -> Result<Value, (StatusCode, Value)> {
    let mut body = json!({ "text": text, "stage": "lexical", "top_k": 50 });
    for (k, v) in extra.as_object().expect("object") {
        body[k] = v.clone();
    }
    let query: SearchQuery = serde_json::from_value(body).expect("a SearchQuery body");
    search_report(state, INDEX, query).await
}

fn reasons(body: &Value) -> Vec<String> {
    body["results"]
        .as_array()
        .expect("results")
        .iter()
        .map(|r| r["match_reason"].as_str().unwrap_or_default().to_string())
        .collect()
}

const FALLBACK: &str = "fallback:ripgrep";

#[tokio::test]
#[serial_test::parallel]
async fn ripgrep_lane_off_drops_fallback_hits() {
    let (state, _tmp) = state_with(LexicalLaneDefaults::default()).await;
    let on = search(&state, "=>", json!({}))
        .await
        .expect("default query");
    assert!(
        reasons(&on).iter().any(|r| r == FALLBACK),
        "the default lane must find the scan-only chunk; got {on}"
    );
    let off = search(&state, "=>", json!({ "ripgrep_fallback": false }))
        .await
        .expect("lane-off query");
    assert!(
        !reasons(&off).iter().any(|r| r == FALLBACK),
        "lane off must return no fallback:ripgrep row; got {off}"
    );
}

#[tokio::test]
#[serial_test::parallel]
async fn lexical_limit_caps_the_lexical_lane() {
    let (state, _tmp) = state_with(LexicalLaneDefaults::default()).await;
    let all = search(&state, "widget", json!({})).await.expect("default");
    assert_eq!(
        reasons(&all).len(),
        8,
        "default depth keeps every hit; got {all}"
    );
    let capped = search(&state, "widget", json!({ "lexical_limit": 3 }))
        .await
        .expect("limited");
    assert_eq!(
        reasons(&capped).len(),
        3,
        "limit 3 caps the lane; got {capped}"
    );
}

#[tokio::test]
#[serial_test::parallel]
async fn config_defaults_apply_when_the_query_is_silent() {
    let (state, _tmp) = state_with(LexicalLaneDefaults {
        ripgrep_fallback: false,
        lexical_limit: Some(2),
    })
    .await;
    let arrow = search(&state, "=>", json!({})).await.expect("arrow");
    assert!(!reasons(&arrow).iter().any(|r| r == FALLBACK), "{arrow}");
    let widget = search(&state, "widget", json!({})).await.expect("widget");
    assert_eq!(
        reasons(&widget).len(),
        2,
        "config limit applies; got {widget}"
    );
}

#[tokio::test]
#[serial_test::parallel]
async fn explicit_query_values_override_config_defaults() {
    let (state, _tmp) = state_with(LexicalLaneDefaults {
        ripgrep_fallback: false,
        lexical_limit: Some(2),
    })
    .await;
    let arrow = search(&state, "=>", json!({ "ripgrep_fallback": true }))
        .await
        .expect("arrow");
    assert!(reasons(&arrow).iter().any(|r| r == FALLBACK), "{arrow}");
    let widget = search(&state, "widget", json!({ "lexical_limit": 5 }))
        .await
        .expect("widget");
    assert_eq!(reasons(&widget).len(), 5, "query limit wins; got {widget}");
}

#[tokio::test]
#[serial_test::parallel]
async fn an_invalid_lexical_limit_is_rejected_not_clamped() {
    let (state, _tmp) = state_with(LexicalLaneDefaults::default()).await;
    for bad in [0usize, crate::core::indexer::MAX_LEXICAL_LIMIT + 1] {
        let (status, body) = search(&state, "widget", json!({ "lexical_limit": bad }))
            .await
            .expect_err("an out-of-range limit must be refused");
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(body["error"], "invalid_lexical_limit", "{body}");
        assert!(
            body["message"]
                .as_str()
                .unwrap_or_default()
                .contains("the request"),
            "{body}"
        );
    }
    let req: GlobalSearchRequest =
        serde_json::from_value(json!({ "query": "widget", "lexical_limit": 0 })).expect("body");
    let (status, _) = global_search_report(&state, req)
        .await
        .expect_err("the fan-out refuses it too");
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (state, _tmp) = state_with(LexicalLaneDefaults {
        ripgrep_fallback: true,
        lexical_limit: Some(0),
    })
    .await;
    let (status, body) = search(&state, "widget", json!({}))
        .await
        .expect_err("a bad config limit must be refused, not ignored");
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["message"]
            .as_str()
            .unwrap_or_default()
            .contains("daemon config"),
        "{body}"
    );
}

/// The chunk only a substring scan for `HNSW` finds: BM25 indexes the whole
/// token `hnswindex`, never `hnsw`.
const HNSW_SCAN_ONLY: &str = "src/hnsw_index.rs:1:1";
/// The chunk only the exact-match lane finds for the quoted literal `=>`.
const ARROW: &str = "src/arrow.rs:1:1";

fn ids(body: &Value) -> Vec<String> {
    body["results"]
        .as_array()
        .expect("results")
        .iter()
        .map(|r| r["id"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// #9258: the Definition-intent grep lane obeys the switch. `HNSW` is a
/// Definition query, BM25 answers it (so the empty-result fallback never
/// runs), and only the grep lane adds `HNSWINDEX`.
#[tokio::test]
#[serial_test::parallel]
async fn ripgrep_lane_off_drops_the_definition_grep_lane() {
    let (state, _tmp) = state_with(LexicalLaneDefaults::default()).await;
    let on = search(&state, "HNSW", json!({})).await.expect("lane on");
    assert_eq!(on["intent"], "Definition", "{on}");
    assert!(ids(&on).iter().any(|i| i == HNSW_SCAN_ONLY), "{on}");
    let off = search(&state, "HNSW", json!({ "ripgrep_fallback": false }))
        .await
        .expect("lane off");
    assert!(!ids(&off).is_empty(), "BM25 still answers; got {off}");
    assert!(
        !ids(&off).iter().any(|i| i == HNSW_SCAN_ONLY),
        "lane off must drop the grep-lane hit; got {off}"
    );
}

/// #9258: the exact-match lane obeys the switch. The quoted literal `=>` is
/// a token BM25 never indexes, `widget` keeps BM25's page non-empty, and only
/// the exact-match lane reaches `arrow.rs`.
#[tokio::test]
#[serial_test::parallel]
async fn ripgrep_lane_off_drops_the_exact_match_lane() {
    let (state, _tmp) = state_with(LexicalLaneDefaults::default()).await;
    let q = "widget \"=>\"";
    let on = search(&state, q, json!({})).await.expect("lane on");
    assert_ne!(on["intent"], "Definition", "{on}");
    assert!(ids(&on).iter().any(|i| i == ARROW), "{on}");
    let off = search(&state, q, json!({ "ripgrep_fallback": false }))
        .await
        .expect("lane off");
    assert!(!ids(&off).is_empty(), "BM25 still answers; got {off}");
    assert!(
        !ids(&off).iter().any(|i| i == ARROW),
        "lane off must drop the exact-match hit; got {off}"
    );
}
