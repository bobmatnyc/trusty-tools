//! Tests for issue #4289: the create-index-time root-containment guard.
//!
//! Why: `POST /indexes` refused only an EXACT root collision (#2336/#2519/
//! #3993). A root INSIDE an existing index's root, or one ENCLOSING it, was
//! registered happily, and overlapping roots are what let the #402 / #2178
//! reindex hijack and prune another index's corpus.
//! What: drives `create_index_handler` directly (`MockEmbedder`, no daemon)
//! for the four directory relations — descendant, ancestor, sibling, symlinked
//! alias of an ancestor — and calls `find_root_overlap` directly for the
//! fail-closed case, where the candidate cannot be canonicalized at all.
//! Test: run with `cargo test -p trusty-search tests_4289`.

use super::root_overlap::{find_root_overlap, overlap_check_failed_response};
use super::*;
use crate::core::embed::Embedder;
use crate::core::registry::IndexRegistry;
use axum::body::to_bytes;
use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// A `CreateIndexRequest` with every optional field defaulted.
fn create_req(id: &str, root_path: PathBuf) -> super::router::CreateIndexRequest {
    super::router::CreateIndexRequest {
        id: id.to_string(),
        root_path,
        include_paths: None,
        exclude_globs: None,
        extensions: None,
        domain_terms: None,
        path_filter: None,
        include_docs: None,
        respect_gitignore: None,
        follow_links: None,
        lexical_only: None,
        skip_kg: None,
        skip_vector: None,
        defer_embed: None,
        extra_skip_dirs: None,
        data_file_max_bytes: None,
        allow_sensitive_path: false,
    }
}

/// A registry with a mock embedder installed — enough to run the handler.
async fn mock_state() -> Arc<SearchAppState> {
    let state = SearchAppState::new(IndexRegistry::new());
    let embedder: Arc<dyn Embedder> = Arc::new(crate::core::embed::MockEmbedder::new(8));
    state.install_embedder(embedder).await;
    Arc::new(state)
}

/// Create an allowlist-approved subdirectory of `parent`.
fn approved_child(parent: &Path, name: &str) -> PathBuf {
    let child = parent.join(name);
    std::fs::create_dir_all(&child).expect("create child dir");
    let child = std::fs::canonicalize(&child).expect("canonicalize child dir");
    crate::allowlist::test_fixtures::approve(&child);
    child
}

/// Register `id` at `root`, asserting the registration succeeded.
async fn register_ok(state: &Arc<SearchAppState>, id: &str, root: PathBuf) {
    let response =
        super::indexes::create_index_handler(State(Arc::clone(state)), Json(create_req(id, root)))
            .await;
    assert_eq!(response.status(), StatusCode::OK, "first create must succeed");
}

/// Read a refusal's JSON body.
async fn body_json(response: axum::response::Response) -> serde_json::Value {
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    serde_json::from_slice(&bytes).expect("body is JSON")
}

/// A root BELOW an existing index's root is refused with a `409` naming the
/// index already covering those files.
#[tokio::test]
async fn create_index_refuses_a_root_inside_an_existing_index_root() {
    let state = mock_state().await;
    let (_dir, outer) = super::test_support::allowlisted_index_root("ts-4289-inside-");
    let inner = approved_child(&outer, "nested");

    register_ok(&state, "outer-index", outer.clone()).await;
    let response = super::indexes::create_index_handler(
        State(Arc::clone(&state)),
        Json(create_req("inner-index", inner.clone())),
    )
    .await;

    assert_eq!(response.status(), StatusCode::CONFLICT);
    let body = body_json(response).await;
    assert_eq!(body["overlap"], "inside_existing_root");
    assert_eq!(body["existing_index_id"], "outer-index");
    assert_eq!(
        body["existing_root_path"],
        outer.display().to_string(),
        "the refusal must name the conflicting root: {body}"
    );
}

/// A root ABOVE an existing index's root is refused too — it would swallow the
/// registration underneath it, which is the case a user picking a folder is
/// least likely to notice.
#[tokio::test]
async fn create_index_refuses_a_root_that_encloses_an_existing_index_root() {
    let state = mock_state().await;
    let (_dir, outer) = super::test_support::allowlisted_index_root("ts-4289-encloses-");
    let inner = approved_child(&outer, "nested");

    register_ok(&state, "inner-index", inner).await;
    let response = super::indexes::create_index_handler(
        State(Arc::clone(&state)),
        Json(create_req("outer-index", outer.clone())),
    )
    .await;

    assert_eq!(response.status(), StatusCode::CONFLICT);
    let body = body_json(response).await;
    assert_eq!(body["overlap"], "encloses_existing_root");
    assert_eq!(body["existing_index_id"], "inner-index");
}

/// Two siblings under one parent do not overlap — including a sibling whose
/// name merely extends the other's, which a string-prefix check would refuse.
#[tokio::test]
async fn create_index_accepts_a_sibling_of_an_existing_index_root() {
    let state = mock_state().await;
    let (_dir, parent) = super::test_support::allowlisted_index_root("ts-4289-sibling-");
    let first = approved_child(&parent, "app");
    let second = approved_child(&parent, "app2");

    register_ok(&state, "first-index", first).await;
    let response = super::indexes::create_index_handler(
        State(Arc::clone(&state)),
        Json(create_req("second-index", second)),
    )
    .await;

    assert_eq!(
        response.status(),
        StatusCode::OK,
        "a sibling root overlaps nothing and must be accepted"
    );
}

/// A symlink pointing at the PARENT of an existing index root is refused: the
/// containment check resolves aliases rather than comparing path strings.
#[tokio::test]
async fn create_index_refuses_a_symlinked_ancestor_of_an_existing_index_root() {
    let state = mock_state().await;
    let (_dir, real) = super::test_support::allowlisted_index_root("ts-4289-symlink-");
    let inner = approved_child(&real, "nested");
    let link = real.with_file_name(format!("ts-4289-alias-{}", std::process::id()));
    let _ = std::fs::remove_file(&link);
    #[cfg(unix)]
    std::os::unix::fs::symlink(&real, &link).expect("create symlink alias");
    crate::allowlist::test_fixtures::approve(&link);

    register_ok(&state, "inner-index", inner).await;
    let response = super::indexes::create_index_handler(
        State(Arc::clone(&state)),
        Json(create_req("alias-index", link.clone())),
    )
    .await;
    let status = response.status();
    let body = body_json(response).await;
    let _ = std::fs::remove_file(&link);

    assert_eq!(status, StatusCode::CONFLICT, "symlink alias: {body}");
    assert_eq!(body["overlap"], "encloses_existing_root");
    assert_eq!(body["existing_index_id"], "inner-index");
}

/// A candidate that cannot be canonicalized is an ERROR, never an implicit
/// "no overlap, create it" — the fail-open arm is the hazard this guard exists
/// to close.
#[test]
fn overlap_check_fails_closed_when_the_candidate_cannot_be_canonicalized() {
    let missing = PathBuf::from("/nonexistent-4289/never-created");

    let outcome = find_root_overlap(&[], &[], &missing, None);

    let failure = match outcome {
        Err(failure) => failure,
        Ok(other) => panic!("an unresolvable root must not report {other:?}"),
    };
    assert_eq!(failure.path, missing);
    let (status, body) = overlap_check_failed_response(&failure);
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(body["root_path"], missing.display().to_string());
}
