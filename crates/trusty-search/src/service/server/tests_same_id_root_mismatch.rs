//! Registration was asymmetric: same tree under a new id was refused, same id
//! over a new tree was accepted and the OLD tree kept answering.
//!
//! Why: `find_root_path_collision` (#2336, #2519, #3993) closed one direction
//! three times over — a second id claiming a registered tree is rejected with
//! `409`, and the `(dev, ino)` comparison even catches a case-variant spelling
//! on APFS. The mirror was never checked. `create_index_handler` canonicalized
//! the supplied `root_path` and then compared only the ID, so a request naming
//! a DIFFERENT tree under an already-registered id returned
//! `200 {created: false, reason: "already exists"}` while the previously
//! registered tree went on serving every query. `best_effort_create_index` read
//! only the status, so the caller could not tell that apart from a real create:
//! it pinned the id and reported success. A session opening in one checkout was
//! answered from another, with no error and no warning.
//!
//! An index identifies one directory tree, so a request naming a tree the
//! daemon does not hold under that id has not been satisfied. These tests pin
//! that: mismatch is a `409`, a genuine re-registration of the SAME tree still
//! reports `already exists`, and the `already exists` body now names the tree it
//! joined so the caller can verify rather than infer.
//!
//! Basename keying is a SEPARATE defect these tests do not address: ids are path
//! basenames, so two distinct checkouts `.../bob-duetto/api` and
//! `.../bobmatnyc/api` both derive `api`. After this change the second is
//! refused rather than silently bound to the first — loud instead of wrong, but
//! still not registerable. Richer id derivation is its own change.
//!
//! Test: run with `cargo test -p trusty-search tests_same_id_root_mismatch`.

use super::*;
use crate::core::embed::{Embedder, MockEmbedder};
use crate::core::registry::{IndexId, IndexRegistry};
use crate::service::persistence::PersistedIndex;
use axum::body::to_bytes;
use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use std::sync::Arc;

/// Build a `CreateIndexRequest` with every optional field defaulted, varying
/// only `id` and `root_path` (mirrors `tests_2336::create_req`).
fn create_req(id: &str, root_path: std::path::PathBuf) -> super::router::CreateIndexRequest {
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

/// A fresh, empty registry with a mock embedder installed.
async fn mock_state_async() -> Arc<SearchAppState> {
    let state = SearchAppState::new(IndexRegistry::new());
    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(8));
    state.install_embedder(embedder).await;
    Arc::new(state)
}

/// Read a handler response body as JSON.
async fn json_body(resp: axum::response::Response) -> serde_json::Value {
    let bytes = to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("read response body");
    serde_json::from_slice(&bytes).expect("response body is JSON")
}

/// THE regression. Registering an id at tree A and then re-registering the same
/// id at tree B must be refused.
///
/// Before the fix this test FAILS at the status assertion: the handler returns
/// `200 {created: false, reason: "already exists"}`, tree A stays registered,
/// and the caller — which reads only the status — treats that as a successful
/// registration of tree B. Every query it then issues is answered from tree A.
#[tokio::test]
async fn create_index_same_id_different_root_is_refused() {
    let state = mock_state_async().await;
    let (_dir_a, root_a) = super::test_support::allowlisted_index_root("ts-mismatch-a-");
    let (_dir_b, root_b) = super::test_support::allowlisted_index_root("ts-mismatch-b-");

    let first = super::indexes::create_index_handler(
        State(Arc::clone(&state)),
        Json(create_req("api", root_a.clone())),
    )
    .await;
    assert_eq!(first.status(), StatusCode::OK, "first create must succeed");

    let second = super::indexes::create_index_handler(
        State(Arc::clone(&state)),
        Json(create_req("api", root_b.clone())),
    )
    .await;
    assert_eq!(
        second.status(),
        StatusCode::CONFLICT,
        "re-registering an id onto a different tree must be refused, not \
         answered 200 while the old tree keeps serving"
    );

    let body = json_body(second).await;
    assert_eq!(body["index_id"], "api");
    assert_eq!(
        body["registered_root_path"],
        serde_json::json!(root_a),
        "the refusal must name the tree that actually owns the id"
    );
    assert_eq!(
        body["requested_root_path"],
        serde_json::json!(root_b),
        "and the tree that was asked for, so the caller can tell them apart"
    );

    // The registered tree is untouched — a refused request changes nothing.
    let handle = state
        .registry
        .get(&IndexId::new("api".to_string()))
        .expect("index 'api' is still registered");
    assert_eq!(
        handle.root_path, root_a,
        "the refusal must not have relocated the index"
    );
    assert_eq!(state.registry.list().len(), 1, "no second entry was minted");
}

/// The ordinary case must keep working: every session relaunch in the SAME
/// checkout re-issues this call, and it has to stay a cheap idempotent success.
///
/// The response now also NAMES the tree it joined. That field is what lets
/// `best_effort_create_index` verify rather than infer — without it, a client
/// talking to a daemon still has only a status code to go on.
#[tokio::test]
async fn create_index_same_id_same_root_still_reports_already_exists() {
    let state = mock_state_async().await;
    let (_dir, root) = super::test_support::allowlisted_index_root("ts-mismatch-same-");

    let first = super::indexes::create_index_handler(
        State(Arc::clone(&state)),
        Json(create_req("api", root.clone())),
    )
    .await;
    assert_eq!(first.status(), StatusCode::OK);

    let second = super::indexes::create_index_handler(
        State(Arc::clone(&state)),
        Json(create_req("api", root.clone())),
    )
    .await;
    assert_eq!(
        second.status(),
        StatusCode::OK,
        "re-registering the same tree under the same id is still idempotent"
    );

    let body = json_body(second).await;
    assert_eq!(body["created"], serde_json::json!(false));
    assert_eq!(body["reason"], "already exists");
    assert_eq!(
        body["root_path"],
        serde_json::json!(root),
        "the already-exists answer must name the tree it joined"
    );
}

/// APFS is case-insensitive and case-preserving, and `canonicalize` returns the
/// spelling it was given rather than normalising it. So a re-registration that
/// differs only in case is the SAME tree and must stay idempotent — the guard
/// has to compare `(dev, ino)`, not strings, or it would refuse a request that
/// is entirely legitimate.
#[cfg(target_os = "macos")]
#[tokio::test]
async fn create_index_same_id_case_variant_root_is_not_a_mismatch() {
    let state = mock_state_async().await;
    let (_dir, root) = super::test_support::allowlisted_index_root("TS-Mismatch-Case-");

    let first = super::indexes::create_index_handler(
        State(Arc::clone(&state)),
        Json(create_req("api", root.clone())),
    )
    .await;
    assert_eq!(first.status(), StatusCode::OK);

    let name = root
        .file_name()
        .expect("temp root has a final component")
        .to_str()
        .expect("temp root component is UTF-8");
    let flipped: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_uppercase() {
                c.to_ascii_lowercase()
            } else {
                c.to_ascii_uppercase()
            }
        })
        .collect();
    let variant = root.with_file_name(flipped);
    assert_ne!(variant, root, "the two spellings must differ as strings");

    let second = super::indexes::create_index_handler(
        State(Arc::clone(&state)),
        Json(create_req("api", variant)),
    )
    .await;
    assert_eq!(
        second.status(),
        StatusCode::OK,
        "a differently-cased spelling of one inode is the SAME tree and must \
         not be refused as a mismatch"
    );
}

/// A cold-parked entry's tree is just as claimed as a resident one's — the same
/// conclusion #3993's second round reached for the mirror guard.
///
/// Without the cold-store arm, an id parked in `state.cold_store` is invisible
/// to `state.registry.get`, so a create at a different tree passes the check,
/// registers a live handle, and the subsequent registry write overwrites the
/// cold entry's row with the new tree. The parked index loses its tree with no
/// race required at all.
#[tokio::test]
async fn create_index_same_id_different_root_is_refused_against_a_cold_entry() {
    let state = mock_state_async().await;
    let (_dir_a, root_a) = super::test_support::allowlisted_index_root("ts-mismatch-cold-a-");
    let (_dir_b, root_b) = super::test_support::allowlisted_index_root("ts-mismatch-cold-b-");

    // Park an entry for `api` at tree A without ever making it resident.
    let mut cold = PersistedIndex::new("api", root_a.clone());
    cold.colocated = true;
    state.cold_store.register_cold_entries(vec![cold]);
    assert!(
        state
            .registry
            .get(&IndexId::new("api".to_string()))
            .is_none(),
        "the entry must be cold, not resident, for this test to mean anything"
    );

    let resp = super::indexes::create_index_handler(
        State(Arc::clone(&state)),
        Json(create_req("api", root_b.clone())),
    )
    .await;
    assert_eq!(
        resp.status(),
        StatusCode::CONFLICT,
        "a cold entry's tree must be defended exactly like a live handle's"
    );

    let body = json_body(resp).await;
    assert_eq!(body["registered_root_path"], serde_json::json!(root_a));
}
