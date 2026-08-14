//! #5069 — a declared semantic lane resolves to the facet of the same repo that
//! holds its vectors.
//!
//! Why: these live in their own `_tests.rs` file rather than beside the #5068
//! refusal tests in `tests_index_routing.rs`, whose basename classifies it as a
//! PRODUCTION file under the 500-SLOC cap.
//! What: handler-level tests against a hand-built `SearchAppState` holding two
//! facets — no daemon, no network. One positive route plus the two negatives
//! that bound it: a different repo is never substituted, and an undeclared
//! query is never routed.
//! Test: this file.

use super::*;
use crate::core::indexer::CodeIndexer;
use crate::core::registry::{
    IndexHandle, IndexId, IndexRegistry, IndexStages, StageState, StageStatus,
};
use crate::service::persistence::PersistedIndex;
use axum::extract::State;
use axum::http::StatusCode;
use std::sync::Arc;
use tokio::sync::RwLock;

/// A `SearchQuery` pinned to the semantic lane.
fn semantic_query() -> crate::core::indexer::SearchQuery {
    serde_json::from_value(serde_json::json!({
        "text": "how does authentication work",
        "stage": "semantic",
    }))
    .expect("semantic SearchQuery")
}

// ---------------------------------------------------------------------------
// #5069 — a pinned semantic lane routes to the facet that holds this repo's
//         vectors
// ---------------------------------------------------------------------------

/// Build a two-facet daemon: a `skip_vector` worktree index and a base index
/// whose vector lane is ready, joined by one `repo_identity` in a seeded
/// `indexes.toml`.
///
/// `base_identity` is what the base entry records — pass a value different from
/// `wt_identity` to build the cross-repo negative control. Keep the returned
/// tempdirs alive for the test's duration.
fn two_facet_state(
    wt_identity: &str,
    base_identity: &str,
) -> (Vec<tempfile::TempDir>, Arc<SearchAppState>) {
    let wt_root = tempfile::tempdir().expect("worktree root");
    let base_root = tempfile::tempdir().expect("base root");
    let data_dir = tempfile::tempdir().expect("data dir");
    let toml_path = data_dir.path().join("indexes.toml");
    crate::service::persistence::save_index_registry_at(
        &toml_path,
        &[
            PersistedIndex {
                id: "wt".into(),
                root_path: wt_root.path().to_path_buf(),
                repo_identity: Some(wt_identity.into()),
                skip_vector: true,
                ..Default::default()
            },
            PersistedIndex {
                id: "base".into(),
                root_path: base_root.path().to_path_buf(),
                repo_identity: Some(base_identity.into()),
                skip_vector: false,
                ..Default::default()
            },
        ],
    )
    .expect("seed indexes.toml");

    let registry = IndexRegistry::new();
    let mut wt = IndexHandle::bare(
        IndexId::new("wt".to_string()),
        Arc::new(RwLock::new(CodeIndexer::new("wt", wt_root.path()))),
        wt_root.path().to_path_buf(),
    );
    wt.skip_vector = true;
    registry.register(wt);

    let ready = StageState {
        status: StageStatus::Ready,
        ..Default::default()
    };
    let mut base = IndexHandle::bare(
        IndexId::new("base".to_string()),
        Arc::new(RwLock::new(CodeIndexer::new("base", base_root.path()))),
        base_root.path().to_path_buf(),
    );
    base.skip_vector = false;
    base.stages = Arc::new(RwLock::new(IndexStages {
        lexical: ready.clone(),
        semantic: ready,
        ..Default::default()
    }));
    registry.register(base);

    (
        vec![wt_root, base_root, data_dir],
        Arc::new(SearchAppState::new(registry).with_registry_path(toml_path)),
    )
}

/// A pinned `stage: semantic` against a `skip_vector` worktree index is served
/// by the sibling facet of the SAME repo that carries the vectors.
///
/// Why (#5069): #5060 registers worktrees `skip_vector`, so every `tm` session
/// asks its semantic questions of an index that has no vector lane, and #5068
/// made that a permanent `503`. #5579 registered the base checkout with the
/// vector lane on, but nothing sent the query there — pre-fix this call returns
/// `Err((503, vector_unavailable))`, so the `expect` below is what fails
/// against the pre-fix commit. `served_by_index` and `served_root_path` are
/// load-bearing: the base checkout sits on a different commit than the
/// worktree, so a caller that reads a returned path must know which tree it is
/// relative to.
/// Test: this test.
#[tokio::test]
async fn pinned_semantic_on_a_worktree_routes_to_the_base_facet() {
    let (_dirs, state) = two_facet_state("acme/widget", "acme/widget");
    let axum::Json(body) = super::search::search_handler(
        State(Arc::clone(&state)),
        axum::extract::Path("wt".to_string()),
        axum::Json(semantic_query()),
    )
    .await
    .expect("the repo's vector-carrying facet must serve the declared semantic lane");

    assert_eq!(
        body["meta"]["served_by_index"], "base",
        "the base facet holds this repo's vectors, so it is what answered"
    );
    assert_eq!(
        body["meta"]["routed_from_index"], "wt",
        "the caller must be able to see its request was resolved elsewhere"
    );
    assert!(
        body["meta"]["served_root_path"].is_string(),
        "results are relative to the SERVING facet's root, which must be named"
    );
}

/// Routing never crosses repos — a vector-carrying index of a DIFFERENT repo is
/// not a substitute.
///
/// Why (#5069): `repo_identity` is the only thing that makes the base checkout
/// a facet of the caller's repo rather than an unrelated index. Without that
/// gate the fix would answer a question about repo A with chunks from repo B —
/// a worse outcome than the `503` it replaces.
/// Test: this test.
#[tokio::test]
async fn semantic_routing_does_not_cross_repo_identities() {
    let (_dirs, state) = two_facet_state("acme/widget", "acme/unrelated");
    let (code, axum::Json(body)) = super::search::search_handler(
        State(Arc::clone(&state)),
        axum::extract::Path("wt".to_string()),
        axum::Json(semantic_query()),
    )
    .await
    .expect_err("another repo's vectors cannot answer this repo's question");

    assert_eq!(code, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["error"], "vector_unavailable");
    assert_eq!(body["index_id"], "wt");
}

/// An UNPINNED query is not routed — it degrades to lexical exactly as before.
///
/// Why (#5069): routing is triggered by the caller's own declaration of the
/// semantic lane, never by the daemon deciding a hybrid query would have been
/// better served by vectors. That distinction is what keeps this a resolution
/// of WHERE the declared lane runs rather than an inference about WHAT the
/// caller wanted (ADR-0046).
/// Test: this test.
#[tokio::test]
async fn an_unpinned_query_is_not_routed_to_the_base_facet() {
    let (_dirs, state) = two_facet_state("acme/widget", "acme/widget");
    let unpinned: crate::core::indexer::SearchQuery =
        serde_json::from_value(serde_json::json!({ "text": "how does authentication work" }))
            .expect("unpinned SearchQuery");
    let axum::Json(body) = super::search::search_handler(
        State(Arc::clone(&state)),
        axum::extract::Path("wt".to_string()),
        axum::Json(unpinned),
    )
    .await
    .expect("an unpinned query still degrades to the lexical lane");

    assert!(
        body["meta"]["served_by_index"].is_null(),
        "no declaration was made, so nothing was resolved elsewhere"
    );
    assert_eq!(
        body["meta"]["vector_disabled_by_config"], true,
        "the worktree index answered, and it is still the vector-less one"
    );
}
