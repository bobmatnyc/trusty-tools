//! Regression tests for the #8134 fail-open on `POST /indexes`.
//!
//! Why: a registration that restored vectors but zero corpus rows reported
//! `status: "ready"` with `search_capabilities: ["vector"]`, and every search
//! returned nothing because no row answered a vector hit's id.
//! What: drives the real `create_index_handler` over a colocated root whose
//! HNSW snapshot survived and whose `index.redb` did not, then reads the
//! status body `GET /indexes/{id}/status` serves.
//! Test: `cargo test -p trusty-search tests_8134`.

use super::*;
use crate::core::embed::Embedder;
use crate::core::registry::{IndexId, IndexRegistry, StageStatus};
use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use std::sync::Arc;

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
        colocated: None,
        extra_skip_dirs: None,
        data_file_max_bytes: None,
        allow_sensitive_path: false,
    }
}

/// #8134: vectors over an empty corpus must not register as a ready index.
///
/// Why: this is the reported fail-open — `restored chunks=0
/// hnsw_snapshot=true`, then `status: ready` and zero results forever.
/// What: builds a real corpus + HNSW snapshot, deregisters, deletes only
/// `index.redb`, re-registers, and asserts the semantic stage is `Failed`
/// with a reason naming the empty corpus, `vector` is not advertised, and the
/// status body reports `degraded` rather than `ready`.
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn create_index_vectors_over_an_empty_corpus_is_not_ready() {
    let state = SearchAppState::new(IndexRegistry::new());
    let embedder: Arc<dyn Embedder> = Arc::new(crate::core::embed::MockEmbedder::new(8));
    state.install_embedder(embedder).await;
    let state = Arc::new(state);
    let (_dir, root) = super::test_support::allowlisted_index_root("ts-8134-orphan-");
    let id = IndexId::new("ts-8134-orphan");

    let first = super::indexes::create_index_handler(
        State(Arc::clone(&state)),
        Json(create_req(&id.0, root.clone())),
    )
    .await;
    assert_eq!(first.status(), StatusCode::OK, "first create must succeed");
    {
        let handle = state.registry.get(&id).expect("registered");
        let indexer = handle.indexer.read().await;
        indexer
            .index_files_batch(&[
                ("src/caller.rs".into(), "fn caller() { callee(); }".into()),
                ("src/callee.rs".into(), "fn callee() {}".into()),
            ])
            .await
            .expect("index batch");
        let hnsw_path = crate::service::colocated_storage::colocated_hnsw_path(&root)
            .expect("colocated hnsw path");
        assert!(
            indexer.save_vector_store(&hnsw_path).await.expect("save"),
            "precondition: an HNSW snapshot must be written"
        );
    }
    state.watcher_manager.stop_for_index(&id).await;
    assert!(state.registry.unregister(&id), "unregister");

    // The artifact shape the reporter registered: vectors, no corpus rows.
    let redb = crate::service::colocated_storage::colocated_redb_path(&root).expect("redb path");
    std::fs::remove_file(&redb).expect("remove index.redb");

    let second = super::indexes::create_index_handler(
        State(Arc::clone(&state)),
        Json(create_req(&id.0, root.clone())),
    )
    .await;
    assert_eq!(second.status(), StatusCode::OK, "re-register");

    let handle = state.registry.get(&id).expect("re-registered");
    assert!(
        handle
            .indexer
            .read()
            .await
            .vector_count()
            .await
            .unwrap_or(0)
            > 0,
        "precondition: the HNSW snapshot must have restored vectors"
    );
    let stages = handle.stages.read().await.clone();
    assert_eq!(
        stages.semantic.status,
        StageStatus::Failed,
        "#8134: vectors over an empty corpus must fail the semantic stage, got {:?}",
        stages.semantic
    );
    let reason = stages.semantic.failure.clone().unwrap_or_default();
    assert!(
        reason.contains("0 chunks"),
        "the reason must name the cause: {reason}"
    );
    assert!(
        !stages.search_capabilities().contains(&"vector"),
        "#8134: the vector lane must not be advertised: {:?}",
        stages.search_capabilities()
    );

    let body = super::status::index_status_report(&state, &id.0)
        .await
        .expect("status body");
    assert_eq!(
        body["status"], "degraded",
        "#8134: status must not report ready over a lost corpus: {body}"
    );
    state.watcher_manager.stop_for_index(&id).await;
}
