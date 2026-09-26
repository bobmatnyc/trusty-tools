//! Regression tests for #8147: `POST /indexes` must honour `colocated: false`.
//!
//! Why: registration hardcoded `colocated: true`, so it opened — and created —
//! `<root>/.trusty-search/` on every call. On a read-only or root-owned root
//! that corpus open fails and the handler answers `500 corpus open failed for
//! root_path …; refusing to register a broken index handle`, which is what
//! made an index deliberately built into the data-dir store deletable but
//! un-re-registerable without a daemon restart. The persistence layer has
//! always routed every path on `PersistedIndex::colocated`; only this door
//! ignored it, and the request struct had no field to ignore.
//! What: drives the real `create_index_handler` with `colocated: Some(false)`
//! and asserts NOTHING was created under the root while the data-dir corpus
//! was, plus the guard that refuses the flag when the root already carries
//! colocated storage, the read-only-root refusal and retry, and the omitted
//! field's unchanged default.
//! Test: this module. Run with `cargo test -p trusty-search tests_8147`.

use super::*;
use crate::core::embed::Embedder;
use crate::core::registry::{IndexId, IndexRegistry};
use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use std::sync::Arc;

/// A `CreateIndexRequest` with every optional field defaulted except
/// `colocated` — the single input this suite varies.
fn create_req_with_colocated(
    id: &str,
    root_path: std::path::PathBuf,
    colocated: Option<bool>,
) -> super::router::CreateIndexRequest {
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
        colocated,
        extra_skip_dirs: None,
        data_file_max_bytes: None,
        allow_sensitive_path: false,
    }
}

/// Fresh registry with a mock embedder — enough for `create_index_handler`.
async fn mock_state() -> Arc<SearchAppState> {
    let state = SearchAppState::new(IndexRegistry::new());
    let embedder: Arc<dyn Embedder> = Arc::new(crate::core::embed::MockEmbedder::new(8));
    state.install_embedder(embedder).await;
    Arc::new(state)
}

/// #8147: `colocated: false` puts the corpus in the data-dir store and creates
/// nothing under `root_path`.
///
/// Why: this is the reported defect. With the flag ignored, registration
/// created `<root>/.trusty-search/` unconditionally — impossible on a
/// root-owned root, which is why the delivery attempt got a 500.
/// What: registers with `colocated: Some(false)` into an isolated
/// `TRUSTY_DATA_DIR`, then asserts the root is untouched, the data-dir corpus
/// exists, and `indexes.toml` recorded the layout so warm boot agrees.
/// Test: this test.
///
/// `#[serial]` because it sets `TRUSTY_DATA_DIR` process-wide.
#[tokio::test]
#[serial_test::serial]
async fn create_index_honours_colocated_false() {
    let _data_dir = super::tests_components::IsolatedDataDir::new();
    let state = mock_state().await;
    let (_dir, root) = super::test_support::allowlisted_index_root("ts-8147-nocolo-");
    let id = IndexId::new("ts-8147-nocolo");

    let created = super::indexes::create_index_handler(
        State(Arc::clone(&state)),
        Json(create_req_with_colocated(&id.0, root.clone(), Some(false))),
    )
    .await;
    assert_eq!(
        created.status(),
        StatusCode::OK,
        "#8147: colocated=false must register, not 500"
    );

    assert!(
        !root.join(".trusty-search").exists(),
        "#8147: colocated=false must create NOTHING under the root — found {}",
        root.join(".trusty-search").display(),
    );

    let entry = crate::service::persistence::find_index_registry_entry(&id.0)
        .expect("registry readable")
        .expect("the registration must be persisted");
    assert!(
        !entry.colocated,
        "#8147: indexes.toml must record colocated=false so warm boot resolves \
         the same corpus path"
    );

    let data_dir_corpus =
        crate::service::persistence::corpus_redb_path(&id.0).expect("data-dir corpus path");
    assert!(
        data_dir_corpus.exists(),
        "#8147: the corpus must have been opened in the data-dir store at {}",
        data_dir_corpus.display(),
    );

    state.watcher_manager.stop_for_index(&id).await;
}

/// #8147 guard: `colocated: false` over a root that ALREADY has
/// `.trusty-search/` is refused, not split across two layouts.
///
/// Why: the write paths (`reindex::runner`, `corpus_swap`, `hnsw_swap`,
/// `shutdown_flush`) route on `has_colocated_storage(root)`, not on the
/// persisted flag. Honouring the flag there would have the writer commit to
/// the colocated corpus while the loader reads the data-dir one — the #483 /
/// #485 zero-chunk failure with the layouts swapped.
/// What: creates `<root>/.trusty-search/` first, then asserts the request is
/// refused with `400` and nothing is registered.
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn create_index_refuses_colocated_false_over_existing_colocated_storage() {
    let _data_dir = super::tests_components::IsolatedDataDir::new();
    let state = mock_state().await;
    let (_dir, root) = super::test_support::allowlisted_index_root("ts-8147-split-");
    let id = IndexId::new("ts-8147-split");
    std::fs::create_dir_all(root.join(".trusty-search")).expect("pre-create colocated dir");

    let created = super::indexes::create_index_handler(
        State(Arc::clone(&state)),
        Json(create_req_with_colocated(&id.0, root.clone(), Some(false))),
    )
    .await;

    assert_eq!(
        created.status(),
        StatusCode::BAD_REQUEST,
        "#8147: a root that already carries colocated storage must refuse \
         colocated=false rather than register a split-layout index"
    );
    assert!(
        state.registry.get(&id).is_none(),
        "a refused registration must leave nothing registered"
    );
}

/// Restores a read-only directory's write bit on drop, so a failed assertion
/// never leaves `TempDir::drop` a directory it cannot remove.
struct ReadOnlyDir(std::path::PathBuf);

impl ReadOnlyDir {
    fn new(path: &std::path::Path) -> Self {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o555))
            .expect("make root read-only");
        Self(path.to_path_buf())
    }
}

impl Drop for ReadOnlyDir {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
    }
}

/// #8147: a colocated request on a read-only root is refused with an error
/// that names the permission problem, leaves nothing behind, and a retry with
/// `colocated: false` then registers the index in the data-dir store.
///
/// Why: this is the reported defect. The operator got `500 corpus open
/// failed`, and `colocated: false` did not help because it was ignored.
/// What: makes the root `0o555`, registers with `colocated` omitted and
/// asserts `403` + "permission denied", no registry row (memory or
/// `indexes.toml`), no `.trusty-search/` and no data-dir directory; then
/// retries with `Some(false)` and asserts `200`, `colocated = false` in
/// `indexes.toml`, and the corpus in the data dir.
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn create_index_colocated_on_read_only_root_names_the_permission_problem() {
    let _data_dir = super::tests_components::IsolatedDataDir::new();
    let state = mock_state().await;
    let (_dir, root) = super::test_support::allowlisted_index_root("ts-8147-ro-");
    // Declared after `_dir`, so it drops first and the tempdir is writable again.
    let _read_only = ReadOnlyDir::new(&root);
    let id = IndexId::new("ts-8147-ro");

    let refused = super::indexes::create_index_handler(
        State(Arc::clone(&state)),
        Json(create_req_with_colocated(&id.0, root.clone(), None)),
    )
    .await;
    assert_eq!(
        refused.status(),
        StatusCode::FORBIDDEN,
        "#8147: a colocated registration the daemon cannot write must be refused \
         as a permission problem, not a generic 500"
    );
    let body = axum::body::to_bytes(refused.into_body(), usize::MAX)
        .await
        .expect("read body");
    let body: serde_json::Value = serde_json::from_slice(&body).expect("json body");
    let error = body["error"].as_str().unwrap_or_default();
    assert!(
        error.contains("permission denied") && error.contains("colocated=false"),
        "the error must name the permission problem and the way out. Body: {body}"
    );

    assert!(
        state.registry.get(&id).is_none(),
        "no half-registered handle"
    );
    assert!(
        crate::service::persistence::find_index_registry_entry(&id.0)
            .expect("registry readable")
            .is_none(),
        "no half-registered indexes.toml row"
    );
    assert!(
        !root.join(".trusty-search").exists(),
        "nothing created under the root"
    );
    let data_dir_index = crate::service::persistence::data_dir()
        .expect("data dir")
        .join("indexes")
        .join(crate::service::persistence::sanitize_id_for_path(&id.0));
    assert!(
        !data_dir_index.exists(),
        "no partial data-dir directory at {}",
        data_dir_index.display()
    );

    let retried = super::indexes::create_index_handler(
        State(Arc::clone(&state)),
        Json(create_req_with_colocated(&id.0, root.clone(), Some(false))),
    )
    .await;
    assert_eq!(
        retried.status(),
        StatusCode::OK,
        "#8147: colocated=false on a read-only root must register"
    );
    let entry = crate::service::persistence::find_index_registry_entry(&id.0)
        .expect("registry readable")
        .expect("the retry must be persisted");
    assert!(!entry.colocated, "indexes.toml must record colocated=false");
    assert!(
        data_dir_index.join("index.redb").exists(),
        "the corpus must live in the data dir at {}",
        data_dir_index.display()
    );

    state.watcher_manager.stop_for_index(&id).await;
}

/// #8147: a request body without `colocated` behaves exactly as before — the
/// corpus is colocated under the root and the registry records it so.
///
/// Why: the field is additive; every existing caller omits it.
/// What: deserializes a JSON body that has no `colocated` key, registers it,
/// and asserts `colocated = true` in `indexes.toml` and
/// `<root>/.trusty-search/index.redb` on disk.
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn create_index_omitted_colocated_keeps_the_colocated_default() {
    let _data_dir = super::tests_components::IsolatedDataDir::new();
    let state = mock_state().await;
    let (_dir, root) = super::test_support::allowlisted_index_root("ts-8147-default-");
    let id = IndexId::new("ts-8147-default");

    let req: super::router::CreateIndexRequest =
        serde_json::from_value(serde_json::json!({ "id": id.0, "root_path": root }))
            .expect("a body without `colocated` must deserialize");
    assert_eq!(req.colocated, None);

    let created = super::indexes::create_index_handler(State(Arc::clone(&state)), Json(req)).await;
    assert_eq!(created.status(), StatusCode::OK);

    let entry = crate::service::persistence::find_index_registry_entry(&id.0)
        .expect("registry readable")
        .expect("the registration must be persisted");
    assert!(entry.colocated, "an omitted field keeps the #403 default");
    assert!(
        root.join(".trusty-search").join("index.redb").exists(),
        "the corpus must be colocated under the root"
    );

    state.watcher_manager.stop_for_index(&id).await;
}
