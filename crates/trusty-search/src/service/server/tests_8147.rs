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
//! colocated storage.
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
