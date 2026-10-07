//! `POST /indexes {roots}` — create-time additional roots (#7434).
//!
//! Why: a root supplied at creation must pass exactly the gate a root added
//! later passes, and `roots` on an already-registered id must never be
//! silently dropped.
//! What: drives `create_index_handler` directly with a mock embedder.
//! Test: this file.

use super::*;
use crate::core::embed::Embedder;
use crate::core::registry::{IndexId, IndexRegistry};
use axum::body::to_bytes;
use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use std::path::PathBuf;
use std::sync::Arc;

fn create_req(
    id: &str,
    root_path: PathBuf,
    roots: Vec<PathBuf>,
) -> super::router::CreateIndexRequest {
    super::router::CreateIndexRequest {
        roots: Some(roots),
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
        colocated: None,
        extra_skip_dirs: None,
        data_file_max_bytes: None,
        allow_sensitive_path: false,
    }
}

async fn mock_state() -> Arc<SearchAppState> {
    let state = SearchAppState::new(IndexRegistry::new());
    let embedder: Arc<dyn Embedder> = Arc::new(crate::core::embed::MockEmbedder::new(8));
    state.install_embedder(embedder).await;
    Arc::new(state)
}

async fn create(
    state: &Arc<SearchAppState>,
    req: super::router::CreateIndexRequest,
) -> (StatusCode, serde_json::Value) {
    let response = super::indexes::create_index_handler(State(Arc::clone(state)), Json(req)).await;
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    (status, serde_json::from_slice(&bytes).expect("json body"))
}

/// Why: the first reindex must walk the whole table, so the table must be
/// on the handle, the indexer and the `indexes.toml` row from creation on.
/// Test: this test.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial]
async fn create_index_accepts_additional_roots() {
    let _isolated = super::tests_components::IsolatedDataDir::new();
    let state = mock_state().await;
    let (_p, primary) = super::test_support::allowlisted_index_root("ts-7434-create-a-");
    let (_e, extra) = super::test_support::allowlisted_index_root("ts-7434-create-b-");

    let (status, body) = create(
        &state,
        create_req(
            "mr-create",
            primary.clone(),
            vec![extra.clone(), primary.clone()],
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let handle = state
        .registry
        .get(&IndexId::new("mr-create"))
        .expect("registered");
    assert_eq!(
        handle.additional_roots,
        vec![extra.clone()],
        "the primary named again is a no-op, not a second slot"
    );
    assert_eq!(
        handle.indexer.read().await.additional_roots,
        vec![extra.clone()]
    );
    let row = crate::service::persistence::load_index_registry()
        .expect("load")
        .into_iter()
        .find(|e| e.id == "mr-create")
        .expect("row");
    assert_eq!(row.additional_roots, vec![extra]);
}

/// Why: error arm — a root another index owns is refused `409` and nothing
/// is registered (fail-open check: the id must not appear).
/// Test: this test.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial]
async fn create_index_refuses_a_root_another_index_owns() {
    let _isolated = super::tests_components::IsolatedDataDir::new();
    let state = mock_state().await;
    let (_a, owned) = super::test_support::allowlisted_index_root("ts-7434-owned-");
    let (_b, primary) = super::test_support::allowlisted_index_root("ts-7434-second-");
    let (status, body) = create(&state, create_req("owner", owned.clone(), vec![])).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, body) = create(&state, create_req("taker", primary, vec![owned])).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(state.registry.get(&IndexId::new("taker")).is_none());
}

/// Why: error arm — `roots` on an id that already exists used to be the
/// silent `created: false` arm; a root the index does not hold is refused.
/// Test: this test.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial]
async fn create_with_new_roots_on_an_existing_id_is_refused() {
    let _isolated = super::tests_components::IsolatedDataDir::new();
    let state = mock_state().await;
    let (_p, primary) = super::test_support::allowlisted_index_root("ts-7434-again-a-");
    let (_e, extra) = super::test_support::allowlisted_index_root("ts-7434-again-b-");
    let (status, _) = create(&state, create_req("again", primary.clone(), vec![])).await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = create(&state, create_req("again", primary, vec![extra])).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"], "roots_not_applied", "{body}");
    let handle = state.registry.get(&IndexId::new("again")).expect("again");
    assert!(handle.additional_roots.is_empty());
}

/// Why (#7434 review, fail-open check): the add-root route's own test cannot
/// fail on the old `.ok()` fallback, because the upsert re-reads the corrupt
/// file and refuses it too. The fallback is reachable only when the first
/// read fails and the second succeeds, so the row decision is tested alone.
/// What: an `Err` load is an error, never a row rebuilt with defaults; a
/// readable registry keeps the on-disk row's fields and swaps in the table.
/// Test: this test.
#[test]
fn an_unreadable_registry_never_yields_a_rebuilt_row() {
    use crate::service::persistence::PersistedIndex;
    let indexer = crate::core::indexer::CodeIndexer::new("row", PathBuf::from("/w/primary"));
    let handle = crate::core::registry::IndexHandle::bare(
        IndexId::new("row"),
        Arc::new(tokio::sync::RwLock::new(indexer)),
        PathBuf::from("/w/primary"),
    );
    let table = vec![PathBuf::from("/w/extra")];

    let unreadable = super::indexes_roots::roots_row(
        Err(anyhow::anyhow!("indexes.toml could not be read")),
        &handle,
        table.clone(),
    );
    assert!(
        unreadable.is_err(),
        "an unreadable registry must refuse, not rebuild a default row: {unreadable:?}"
    );

    let on_disk = PersistedIndex {
        id: "row".into(),
        root_path: PathBuf::from("/w/primary"),
        colocated: true,
        ..Default::default()
    };
    let kept = super::indexes_roots::roots_row(Ok(vec![on_disk]), &handle, table.clone())
        .expect("a readable registry yields a row");
    assert!(kept.colocated, "the on-disk row's fields survive");
    assert_eq!(kept.additional_roots, table);
}
