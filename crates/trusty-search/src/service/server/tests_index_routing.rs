//! Error-arm regression tests for the index-routing / status-reporting
//! cluster: #5068, #5061, #4787, #4839.
//!
//! Why: all four defects share one shape — the daemon answered as if it had
//! succeeded when it had not. A semantic search served lexical rows under a
//! `200`; a cold-parked index produced a bodyless `503` a caller could not
//! branch on or clear; a working semantic lane reported `embedded: 0`; a fleet
//! of empty indexes reported `indexes_loaded: 222/222`. Each test here drives
//! the failure branch directly and asserts the caller can now TELL, because a
//! test that only exercises the success path would have passed before the fix
//! as well.
//!
//! What: handler-level tests against a hand-built `SearchAppState` — no daemon,
//! no network. Each names the issue it pins in its own doc comment.
//! Test: this file.

use super::*;
use crate::core::indexer::CodeIndexer;
use crate::core::registry::{
    IndexHandle, IndexId, IndexRegistry, IndexStages, StageState, StageStatus,
};
use crate::service::persistence::PersistedIndex;
use axum::extract::State;
use axum::http::StatusCode;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Register one bare handle and return the state holding it.
fn state_with_handle(id: &str, configure: impl FnOnce(&mut IndexHandle)) -> Arc<SearchAppState> {
    let registry = IndexRegistry::new();
    let root = format!("/tmp/trusty-5068-{id}");
    let mut handle = IndexHandle::bare(
        IndexId::new(id.to_string()),
        Arc::new(RwLock::new(CodeIndexer::new(id, &root))),
        PathBuf::from(&root),
    );
    configure(&mut handle);
    registry.register(handle);
    Arc::new(SearchAppState::new(registry))
}

/// A `SearchQuery` pinned to the semantic lane.
fn semantic_query() -> crate::core::indexer::SearchQuery {
    serde_json::from_value(serde_json::json!({
        "text": "how does authentication work",
        "stage": "semantic",
    }))
    .expect("semantic SearchQuery")
}

// ---------------------------------------------------------------------------
// #5068 — semantic against a vector-less index
// ---------------------------------------------------------------------------

/// `stage: semantic` on a `skip_vector` index is a 503, not a degraded 200.
///
/// Why (#5068): this is the fail-open. The index answers — with BM25 rows —
/// and the caller has no way to know it asked for meaning-based retrieval and
/// got literal-text retrieval instead. Pre-fix this call returned `Ok` with a
/// results array, so the `expect_err` below is what fails against the pre-fix
/// commit. `reason: skipped_by_config` (not `stage_not_ready`) is load-bearing:
/// the state is permanent for this index's configuration, so a caller that
/// retries on it would loop forever.
/// Test: this test.
#[tokio::test]
async fn semantic_stage_on_skip_vector_index_is_503_not_degraded_200() {
    let state = state_with_handle("vec-off", |h| {
        h.skip_vector = true;
    });
    let (code, axum::Json(body)) = super::search::search_handler(
        State(Arc::clone(&state)),
        axum::extract::Path("vec-off".to_string()),
        axum::Json(semantic_query()),
    )
    .await
    .expect_err("a semantic search against a vector-less index must not succeed");

    assert_eq!(code, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["error"], "vector_unavailable");
    assert_eq!(body["reason"], "skipped_by_config");
    assert_eq!(
        body["retryable"], false,
        "a config-disabled lane never becomes ready on its own"
    );
    assert_eq!(body["index_id"], "vec-off");
    assert!(
        body["stages"].is_object(),
        "the stages snapshot must ride along so the caller need not re-probe"
    );
}

/// The same refusal, but retryable, when the lane is enabled and merely unbuilt.
///
/// Why (#5068): a caller must be able to tell "never" from "not yet" — the two
/// demand opposite actions (reconfigure vs. wait). Both returned an identical
/// degraded 200 pre-fix.
/// Test: this test.
#[tokio::test]
async fn semantic_stage_before_embed_completes_is_503_retryable() {
    let state = state_with_handle("vec-pending", |h| {
        h.skip_vector = false;
    });
    let (code, axum::Json(body)) = super::search::search_handler(
        State(Arc::clone(&state)),
        axum::extract::Path("vec-pending".to_string()),
        axum::Json(semantic_query()),
    )
    .await
    .expect_err("an unbuilt vector lane cannot serve a semantic search");

    assert_eq!(code, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["error"], "vector_unavailable");
    assert_eq!(body["reason"], "stage_not_ready");
    assert_eq!(
        body["retryable"], true,
        "an embed pass in flight WILL finish — the caller should wait, not reconfigure"
    );
}

/// A ready vector lane is not refused — the guard must not break the happy path.
///
/// Why (#5068): the refusal is worthless if it also fires on a working index.
/// Test: this test.
#[tokio::test]
async fn semantic_stage_on_a_ready_vector_lane_is_not_refused() {
    let ready_stage = StageState {
        status: StageStatus::Ready,
        ..Default::default()
    };
    let ready = IndexStages {
        lexical: ready_stage.clone(),
        semantic: ready_stage,
        ..Default::default()
    };
    assert!(
        super::degraded::vector_lane_unavailable("ok-idx", false, &ready).is_none(),
        "a Ready, enabled vector lane must serve the query"
    );
    assert!(
        super::degraded::vector_lane_unavailable("off-idx", true, &ready).is_some(),
        "skip_vector wins even when the stage says Ready — the lane is off by config"
    );
}

/// An UNPINNED query still succeeds, and says which lanes answered it.
///
/// Why (#5068): the issue names the missing `vector_unavailable` flag beside
/// the existing `bm25_lane_degraded`. An unpinned hybrid query legitimately
/// degrades to the ready lanes — that is not an error — but the caller must be
/// able to see it happened without diffing `search_capabilities` against a
/// schema it does not have. Pre-fix `meta` carried no such field, so the
/// `meta["vector_unavailable"]` assertion below fails against the pre-fix
/// commit.
/// Test: this test.
#[tokio::test]
async fn unpinned_search_reports_vector_unavailable_in_meta() {
    let state = state_with_handle("vec-meta", |h| {
        h.skip_vector = true;
    });
    let query: crate::core::indexer::SearchQuery =
        serde_json::from_value(serde_json::json!({ "text": "authentication" }))
            .expect("plain SearchQuery");
    let axum::Json(body) = super::search::search_handler(
        State(Arc::clone(&state)),
        axum::extract::Path("vec-meta".to_string()),
        axum::Json(query),
    )
    .await
    .expect("an unpinned query degrades rather than failing");

    assert_eq!(
        body["meta"]["vector_unavailable"], true,
        "the caller must see that no vector lane contributed"
    );
    assert_eq!(
        body["meta"]["vector_disabled_by_config"], true,
        "and whether that is permanent for this index"
    );
}

// ---------------------------------------------------------------------------
// #5061 — the cold-parked 503 needs a body that names the way out
// ---------------------------------------------------------------------------

fn cold_entry(id: &str) -> PersistedIndex {
    PersistedIndex {
        id: id.to_string(),
        root_path: PathBuf::from(format!("/tmp/trusty-5061-{id}")),
        ..Default::default()
    }
}

fn state_with_cold_index(id: &str) -> Arc<SearchAppState> {
    let state = Arc::new(SearchAppState::new(IndexRegistry::new()));
    state.cold_store.register_cold_entries(vec![cold_entry(id)]);
    state
}

/// The cold-parked 503 carries a body naming `search` as what restores it.
///
/// Why (#5061): `index_status_handler` returned a bare `StatusCode`, so an MCP
/// caller saw `returned 503 Service Unavailable: ` — empty text, nothing to
/// branch on, and no hint that a plain `search` against the same id reloads it
/// while this endpoint stays 503 forever. Pre-fix the handler's error type was
/// `StatusCode`, which carries no body at all, so every field assertion below
/// fails against the pre-fix commit.
/// Test: this test.
#[tokio::test]
async fn cold_parked_status_503_body_names_search_as_the_restore_path() {
    let state = state_with_cold_index("cold-body");
    let (code, axum::Json(body)) = super::status::index_status_handler(
        State(Arc::clone(&state)),
        axum::extract::Path("cold-body".to_string()),
    )
    .await
    .expect_err("a non-resident index cannot be reported");

    assert_eq!(code, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["error"], "index_not_resident");
    assert_eq!(body["retryable"], true);
    assert_eq!(
        body["restore_via"], "POST /indexes/cold-body/search",
        "the 503 must name the endpoint that actually clears it"
    );
}

/// A permanently-failed restore is NOT advertised as retryable.
///
/// Why (#5061): the failed set never clears on its own, so telling a caller to
/// retry is the same lie in a different direction. `chunks` and `status` both
/// collapsed the two states into one bodyless 503 pre-fix.
/// Test: this test.
#[tokio::test]
async fn restore_failed_status_503_body_is_not_retryable() {
    let state = state_with_cold_index("failed-body");
    let id = IndexId::new("failed-body".to_string());
    state.cold_store.mark_failed(&id);

    let (code, axum::Json(body)) = super::status::index_status_handler(
        State(Arc::clone(&state)),
        axum::extract::Path("failed-body".to_string()),
    )
    .await
    .expect_err("a permanently-failed index cannot be reported");

    assert_eq!(code, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["error"], "index_restore_failed");
    assert_eq!(
        body["retryable"], false,
        "nothing clears the failed set — retry advice here would loop forever"
    );
    assert!(
        body.get("restore_via").is_none(),
        "no endpoint restores a permanently-failed index; naming one would mislead"
    );
}

/// `chunks` reports the identical verdict for the identical state.
///
/// Why (#5061): the drift between these endpoints IS the defect. One builder,
/// one answer.
/// Test: this test.
#[tokio::test]
async fn chunks_and_status_agree_on_a_cold_parked_index() {
    let state = state_with_cold_index("cold-agree");
    let params: super::files::ChunksParams =
        serde_json::from_value(serde_json::json!({})).expect("empty ChunksParams");

    let (status_code, axum::Json(status_body)) = super::status::index_status_handler(
        State(Arc::clone(&state)),
        axum::extract::Path("cold-agree".to_string()),
    )
    .await
    .expect_err("cold");
    let (chunks_code, axum::Json(chunks_body)) = super::files::get_index_chunks_handler(
        State(Arc::clone(&state)),
        axum::extract::Path("cold-agree".to_string()),
        axum::extract::Query(params),
    )
    .await
    .expect_err("cold");

    assert_eq!(status_code, chunks_code);
    assert_eq!(status_body["error"], chunks_body["error"]);
    assert_eq!(status_body["restore_via"], chunks_body["restore_via"]);
}

/// A genuinely unknown id is still a 404, and now says so in the body.
///
/// Why (#5061): #4715 depends on this 404 meaning "absent from every store".
/// Widening the body must not widen the status code.
/// Test: this test.
#[tokio::test]
async fn residency_miss_is_404_only_when_absent_everywhere() {
    let state = Arc::new(SearchAppState::new(IndexRegistry::new()));
    let (code, axum::Json(body)) = super::status::index_status_handler(
        State(Arc::clone(&state)),
        axum::extract::Path("never-existed".to_string()),
    )
    .await
    .expect_err("genuinely unknown");

    assert_eq!(code, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "unknown index: never-existed");
}

// ---------------------------------------------------------------------------
// #4787 — cumulative semantic coverage beside the per-boot delta
// ---------------------------------------------------------------------------

/// `index_status` reports cumulative vector coverage, not only this boot's delta.
///
/// Why (#4787): `stages.semantic.embedded` reads `0` on a fully-working index
/// whose HNSW snapshot was already current at boot — the exact signature of the
/// #2178 dead-lane degradation, which is how three healthy indexes were flagged
/// during an estate audit. Pre-fix the response carried no `semantic_coverage`
/// key at all, so the assertions below fail against the pre-fix commit. A
/// BM25-only handle has no vector store wired, so `vectors_present` is `null` —
/// an absent measurement, deliberately not reported as `0`, which would be the
/// same lie this issue is about.
/// Test: this test.
#[tokio::test]
async fn status_semantic_coverage_reports_live_vector_count() {
    let state = state_with_handle("coverage", |h| {
        h.skip_vector = false;
    });
    {
        let handle = state
            .registry
            .get(&IndexId::new("coverage".to_string()))
            .expect("registered");
        handle.stages.write().await.semantic = StageState {
            status: StageStatus::Ready,
            // The misleading per-boot zero the issue reports.
            embedded: Some(0),
            ..Default::default()
        };
    }

    let axum::Json(body) = super::status::index_status_handler(
        State(Arc::clone(&state)),
        axum::extract::Path("coverage".to_string()),
    )
    .await
    .expect("a resident index reports status");

    assert_eq!(
        body["stages"]["semantic"]["embedded"], 0,
        "the per-boot delta keeps its name and value — this is wire-compatible"
    );
    let coverage = &body["semantic_coverage"];
    assert!(
        coverage.is_object(),
        "cumulative coverage must be reported beside the delta"
    );
    assert_eq!(
        coverage["embedded_this_boot"], 0,
        "the delta is restated under a name that says what it measures"
    );
    assert!(
        coverage["vectors_present"].is_null(),
        "no vector store wired ⇒ unknown, never a fabricated 0"
    );
}

// ---------------------------------------------------------------------------
// #4839 — registered is not populated
// ---------------------------------------------------------------------------

/// `/health` distinguishes registered indexes from populated ones.
///
/// Why (#4839): a production deployment reported `indexes_loaded: 222/222`,
/// `indexes_failed: 0`, `status: ok` while 220 of those held zero chunks, and a
/// consuming application returned empty context on 913 consecutive operations
/// across 44 days with no signal. Every counter on the response was
/// structurally blind to it. Pre-fix `HealthResponse` had no
/// `indexes_populated` / `indexes_empty` / `total_chunks` fields, so this test
/// does not compile against the pre-fix commit — which is the strongest form of
/// the assertion.
/// Test: this test.
#[tokio::test]
async fn health_reports_populated_and_empty_index_counts() {
    let registry = IndexRegistry::new();
    for id in ["empty-a", "empty-b"] {
        registry.register(IndexHandle::bare(
            IndexId::new(id.to_string()),
            Arc::new(RwLock::new(CodeIndexer::new(id, "/tmp/health-4839"))),
            PathBuf::from("/tmp/health-4839"),
        ));
    }
    let state = Arc::new(SearchAppState::new(registry));
    let axum::Json(resp) = super::health::health_handler(State(state)).await;

    assert_eq!(
        resp.indexes, 2,
        "registration count is unchanged — it still counts slots"
    );
    assert_eq!(
        resp.indexes_populated, 0,
        "no chunks were ever committed to either index"
    );
    assert_eq!(
        resp.indexes_empty, 2,
        "both are registered but hold nothing"
    );
    assert_eq!(
        resp.total_chunks, 0,
        "the fleet-wide corpus size makes a near-zero deployment visible at a glance"
    );
}
