//! Handler-level tests for `DELETE /indexes/:id?delete_data=true` under the
//! registry-named storage layout (#8438).
//!
//! Why: `unregister_index` picks the layout three ways — a live handle's
//! indexer, a cold-store entry, or the DataDir default. Unit tests on
//! `StorageLayout::remove_storage` do not prove the handler feeds it the right
//! layout for each source.
//! What: drives the real router with `oneshot` against a live colocated index
//! and a cold `colocated=false` entry. `multi_thread` is required:
//! `unregister_index` reaches `watcher_manager.stop_for_index`, whose teardown
//! uses `block_in_place`.
//! Test: this module. Run with `cargo test -p trusty-search tests_8438`.

use super::build_router;
use super::tests_components::IsolatedDataDir;
use crate::core::indexer::CodeIndexer;
use crate::core::registry::{IndexHandle, IndexId, IndexRegistry};
use crate::service::colocated_storage::COLOCATED_DIR_NAME;
use crate::service::persistence::PersistedIndex;
use crate::service::server::SearchAppState;
use crate::service::storage_layout::{StorageLayout, REDB_FILE};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use std::sync::Arc;
use tower::ServiceExt;

/// Issue the DELETE with `delete_data=true` and return `(status, body)`.
async fn delete_with_data(state: SearchAppState, id: &str) -> (StatusCode, serde_json::Value) {
    let resp = build_router(state)
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/indexes/{id}?delete_data=true"))
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("body");
    (status, serde_json::from_slice(&bytes).expect("json body"))
}

/// A root whose `.trusty-search/` already holds an `index.redb`.
fn root_with_repo_corpus() -> tempfile::TempDir {
    let root = tempfile::tempdir().expect("root tempdir");
    let repo = root.path().join(COLOCATED_DIR_NAME);
    std::fs::create_dir_all(&repo).expect("repo dir");
    std::fs::write(repo.join(REDB_FILE), b"repo corpus").expect("repo corpus");
    root
}

/// Why (#8438): a live colocated index keeps its corpus in
/// `<root>/.trusty-search/`; `delete_data` must remove it, which requires the
/// handler to read the layout from the live handle's indexer.
/// Test: this test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
async fn delete_data_on_a_live_colocated_index_removes_its_in_repo_dir() {
    let _isolated = IsolatedDataDir::new();
    const ID: &str = "live-coloc-8438";
    let root = root_with_repo_corpus();
    let registry = IndexRegistry::new();
    let indexer = CodeIndexer::new(ID, root.path()).with_storage_layout(StorageLayout::Colocated);
    registry.register(IndexHandle::bare(
        IndexId::new(ID),
        Arc::new(tokio::sync::RwLock::new(indexer)),
        root.path().to_path_buf(),
    ));

    let (status, body) = delete_with_data(SearchAppState::new(registry), ID).await;

    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["data_deleted"], true, "body: {body}");
    assert!(
        !root.path().join(COLOCATED_DIR_NAME).exists(),
        "#8438: delete_data on a live colocated index must remove <root>/.trusty-search/"
    );
    assert!(root.path().exists(), "the index root itself must survive");
}

/// Why (#8438): a cold `colocated=false` entry owns only its data dir. An
/// in-repo `.trusty-search/` beside it may be another instance's live corpus
/// and must survive `delete_data` byte for byte.
/// Test: this test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
async fn delete_data_on_a_cold_non_colocated_entry_leaves_the_repo_dir() {
    let isolated = IsolatedDataDir::new();
    const ID: &str = "cold-datadir-8438";
    let root = root_with_repo_corpus();
    let data_index_dir = isolated.path().join("indexes").join(ID);
    std::fs::create_dir_all(&data_index_dir).expect("data dir");
    std::fs::write(data_index_dir.join(REDB_FILE), b"own corpus").expect("own corpus");
    let state = SearchAppState::new(IndexRegistry::new());
    state.cold_store.register_cold_entries(vec![PersistedIndex {
        id: ID.to_string(),
        root_path: root.path().to_path_buf(),
        colocated: false,
        ..Default::default()
    }]);

    let (status, body) = delete_with_data(state, ID).await;

    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["data_deleted"], true, "body: {body}");
    assert!(
        !data_index_dir.exists(),
        "the entry's own data dir must be removed"
    );
    assert_eq!(
        std::fs::read(root.path().join(COLOCATED_DIR_NAME).join(REDB_FILE)).expect("repo corpus"),
        b"repo corpus",
        "#8438: a colocated=false delete must leave the in-repo .trusty-search/ untouched"
    );
}
