//! Regression tests for the silent-failure surfaces in issues #4087 and #4333.
//!
//! Why: every behaviour here is invisible without a test. A corpus-failed
//! index answering `200 []` looks exactly like a healthy index with no
//! matches; a timeout-dropped index looks exactly like an index that was never
//! registered; a "corrupted format" string for a transient timeout reads
//! exactly like a real corruption. A regression in any of them would ship
//! green.
//! What: three groups — the single-index 503 guard, the fan-out exclusion +
//! count, and the cold-store parking of a timed-out restore.
//! Test: these tests.
use super::*;
use axum::http::StatusCode;
use axum::Json;

use crate::core::corpus::CorpusOpenFailure;
use crate::core::embed::{Embedder, MockEmbedder};
use crate::core::indexer::{CodeIndexer, SearchMode, SearchQuery, SearchStage};
use crate::core::registry::{IndexHandle, IndexId, IndexRegistry};
use crate::core::store::{UsearchStore, VectorStore};

/// Build a registered index handle, optionally flagged corpus-open-failed.
fn build_state_with(indexes: &[(&str, Option<CorpusOpenFailure>)]) -> (Arc<SearchAppState>, Arc<dyn Embedder>) {
    let dim = 16;
    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(dim));
    let registry = IndexRegistry::new();
    for (id, failure) in indexes {
        let store: Arc<dyn VectorStore> = Arc::new(UsearchStore::new(dim).expect("usearch"));
        let mut indexer =
            CodeIndexer::new(*id, "/tmp/4087").with_components(Arc::clone(&embedder), store);
        if let Some(kind) = failure {
            // Mirrors exactly what `persistence_loader::build_indexer_from_entry`
            // does on a failed open — flag set, kind recorded, no corpus wired.
            indexer.corpus_open_failed = true;
            indexer.corpus_open_failure = Some(*kind);
        }
        registry.register(IndexHandle::bare(
            IndexId::new((*id).to_string()),
            Arc::new(tokio::sync::RwLock::new(indexer)),
            "/tmp/4087".into(),
        ));
    }
    (Arc::new(SearchAppState::new(registry)), embedder)
}

fn probe_query() -> SearchQuery {
    SearchQuery {
        text: "anything".to_string(),
        top_k: 5,
        expand_graph: false,
        compact: false,
        branch_files: None,
        branch_boost: 1.5,
        branch: None,
        stage: Some(SearchStage::Lexical),
        mode: SearchMode::Code,
        exclude_archived: false,
        refine_query: None,
        path_prefix: None,
        repos: Vec::new(),
    }
}

/// Why (issue #4087, the headline defect): a registered index whose durable
/// corpus failed to open holds zero chunks, so every query returned HTTP 200
/// with `results: []`. The caller could not distinguish "nothing matched" from
/// "this index is entirely broken" — a total search outage presented as a
/// successful answer. Three live indexes were serving this way.
/// What: registers a corpus-failed index, issues a normal search, and asserts
/// the handler returns 503 with the `index_corpus_unavailable` code rather
/// than an empty 200. Against pre-fix code this fails with `Ok(200)`.
/// Test: this test.
#[tokio::test]
async fn search_against_corpus_failed_index_returns_503_not_empty_200() {
    let (state, embedder) = build_state_with(&[("broken", Some(CorpusOpenFailure::OpenTimeout))]);
    state.install_embedder(embedder).await;

    let resp = search_handler(
        axum::extract::State(Arc::clone(&state)),
        axum::extract::Path("broken".to_string()),
        axum::extract::Json(probe_query()),
    )
    .await;

    let (status, Json(body)) = resp.expect_err(
        "a corpus-failed index must NOT answer 200-with-empty-results (issue #4087)",
    );
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["error"], "index_corpus_unavailable");
    assert_eq!(body["index_id"], "broken");
    // #4333: the classification must ride along so the caller knows whether to
    // retry or escalate.
    assert_eq!(body["failure_kind"], "open_timeout");
    assert_eq!(body["transient"], true);
    let message = body["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("DO NOT reindex"),
        "a transient failure must not invite a destructive rebuild: {message}"
    );
}

/// Why (#4333): the same 503 must carry the OPPOSITE guidance for a genuine
/// format incompatibility — the fix must not swing to never recommending a
/// rebuild.
/// What: same flow with `FormatIncompatible`; asserts `transient: false` and
/// that the rebuild instruction survives.
/// Test: this test.
#[tokio::test]
async fn corpus_failure_response_distinguishes_permanent_from_transient() {
    let (state, embedder) =
        build_state_with(&[("rotten", Some(CorpusOpenFailure::FormatIncompatible))]);
    state.install_embedder(embedder).await;

    let (status, Json(body)) = search_handler(
        axum::extract::State(Arc::clone(&state)),
        axum::extract::Path("rotten".to_string()),
        axum::extract::Json(probe_query()),
    )
    .await
    .expect_err("corpus-failed index must fail loudly");

    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["failure_kind"], "format_incompatible");
    assert_eq!(body["transient"], false);
    assert!(body["message"].as_str().unwrap_or_default().contains("--force"));
}

/// Why: the guard must not fire for healthy indexes — a false positive would
/// turn this fix into a worse outage than the bug.
/// What: a healthy index answers 200 with a (possibly empty) result set.
/// Test: this test.
#[tokio::test]
async fn healthy_index_is_unaffected_by_the_corpus_failure_guard() {
    let (state, embedder) = build_state_with(&[("healthy", None)]);
    state.install_embedder(embedder).await;

    let result = search_handler(
        axum::extract::State(Arc::clone(&state)),
        axum::extract::Path("healthy".to_string()),
        axum::extract::Json(probe_query()),
    )
    .await;

    assert!(
        result.is_ok(),
        "a healthy index must still be searchable (issue #4087 must not over-fire)"
    );
}

/// Why (issue #4087, finding 2 applied to fan-out): `search_all` folded a
/// corpus-failed index's empty lane into the fused result set and reported the
/// fan-out as complete. A consumer had no way to learn that one of its corpora
/// was entirely absent from the answer.
/// What: one healthy and one corpus-failed index; asserts the failed index is
/// absent from `indexes_searched` AND counted in
/// `corpus_failed_indexes_skipped`. The count is the load-bearing half — mere
/// exclusion without a count would be the same silence in a different place.
/// Test: this test.
#[tokio::test]
async fn global_search_excludes_and_counts_corpus_failed_indexes() {
    let (state, embedder) = build_state_with(&[
        ("ok-one", None),
        ("broken-one", Some(CorpusOpenFailure::Contention)),
    ]);
    state.install_embedder(embedder).await;

    let Json(body) = global_search_handler(
        axum::extract::State(Arc::clone(&state)),
        axum::extract::Json(
            serde_json::from_value::<super::search_global::GlobalSearchRequest>(
                serde_json::json!({ "query": "anything", "top_k": 5 }),
            )
            .expect("request body"),
        ),
    )
    .await
    .expect("fan-out must succeed over the healthy index");

    assert_eq!(
        body["corpus_failed_indexes_skipped"], 1,
        "the corpus-failed index must be COUNTED, not silently folded in (issue #4087)"
    );
    let searched: Vec<String> = body["indexes_searched"]
        .as_array()
        .expect("indexes_searched")
        .iter()
        .map(|v| v.as_str().unwrap_or_default().to_string())
        .collect();
    assert!(
        !searched.contains(&"broken-one".to_string()),
        "a corpus-failed index must not be reported as searched: {searched:?}"
    );
}

/// Why (issue #4087, finding 1): a warm-boot restore timeout tallied a counter
/// and did nothing else — the entry reached neither the registry nor the cold
/// store, so the index simply ceased to exist for the rest of that boot (11
/// indexes lost this way on a live daemon, recoverable only by a restart). A
/// timeout is the most transient failure available; dropping is the least
/// appropriate response.
/// What: calls `park_timed_out_entry` (the exact call the timeout branch makes)
/// and asserts the entry is now in the cold store, i.e. reachable by the
/// existing lazy-load path and counted by `cold_store.len()` — which is what
/// feeds `search_all`'s `cold_indexes_skipped`.
/// Test: this test.
#[tokio::test]
async fn timed_out_entry_is_parked_in_cold_store() {
    use crate::service::persistence::PersistedIndex;

    let (state, _embedder) = build_state_with(&[]);
    let id = IndexId::new("slow-restore".to_string());
    assert!(
        !state.cold_store.contains(&id),
        "precondition: not parked yet"
    );

    state.cold_store.park_timed_out(PersistedIndex {
        id: "slow-restore".to_string(),
        root_path: "/tmp/4087-slow".into(),
        ..Default::default()
    });

    assert!(
        state.cold_store.contains(&id),
        "a timed-out restore must be PARKED for lazy load, not dropped (issue #4087)"
    );
    assert_eq!(
        state.cold_store.len(),
        1,
        "the parked entry must be counted so `cold_indexes_skipped` surfaces the \
         incomplete fan-out"
    );
}
