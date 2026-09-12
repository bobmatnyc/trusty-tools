//! The per-root file watcher at the HTTP surface — #7434 slice 3.
//!
//! Why: slice 2 made `POST /indexes/:id/roots` append to the root table and
//! queue a walk, which closes the coverage gap ONCE. Nothing then watched the
//! new tree, so every edit made in it afterwards was invisible until someone
//! reindexed — the index reporting `watcher.active: true` throughout, because
//! its primary root's watch was fine. These tests pin the two halves of the
//! answer: the add starts the watch in the same request, and
//! `GET /indexes/:id/status` names what every root's watch is doing.
//!
//! What: drives `indexes_roots::add_index_roots_report` and
//! `status::index_status_report` directly against a `MockEmbedder` state on
//! real temp roots, then reads the manager.
//!
//! Test: run with `cargo test -p trusty-search tests_7434_watch`.

use super::*;
use crate::core::embed::{Embedder, MockEmbedder};
use crate::core::registry::{IndexId, IndexRegistry};
use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// A fresh, empty registry with a mock embedder installed.
async fn mock_state_async() -> Arc<SearchAppState> {
    let state = SearchAppState::new(IndexRegistry::new());
    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(8));
    state.install_embedder(embedder).await;
    Arc::new(state)
}

/// `POST /indexes` for `id` at `root`, asserting it succeeded.
async fn create_index(state: &Arc<SearchAppState>, id: &str, root: &Path) {
    let req = super::router::CreateIndexRequest {
        id: id.to_string(),
        root_path: root.to_path_buf(),
        roots: None,
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
    };
    let resp = super::indexes::create_index_handler(State(Arc::clone(state)), Json(req)).await;
    assert_eq!(resp.status(), StatusCode::OK, "create '{id}' must succeed");
}

/// `POST /indexes/:id/roots` with `roots`, asserting it succeeded.
async fn add_roots(state: &Arc<SearchAppState>, id: &str, roots: Vec<PathBuf>) {
    super::indexes_roots::add_index_roots_report(
        state,
        id,
        super::indexes_roots::AddRootsRequest { roots },
    )
    .await
    .expect("adding a fresh, uncovered root must succeed");
}

/// Why: this is the behaviour the add endpoint was missing. The walk it queues
/// closes the coverage gap once; without a watch, every subsequent edit in the
/// added tree is invisible until the next reindex — and the index keeps
/// reporting a live watcher on the strength of its primary root.
/// Test: this test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn add_root_spawns_a_watch_for_the_new_root() {
    let (_dp, primary) = super::test_support::allowlisted_index_root("ts-7434-aw-p-");
    let (_dx, extra) = super::test_support::allowlisted_index_root("ts-7434-aw-x-");
    let state = mock_state_async().await;
    let id = "add-root-watch";

    create_index(&state, id, &primary).await;
    assert_eq!(
        state.watcher_manager.watched_root_count().await,
        1,
        "the index starts with its primary root watched"
    );

    add_roots(&state, id, vec![extra.clone()]).await;

    assert_eq!(
        state.watcher_manager.watched_root_count().await,
        2,
        "the added root must be watched in the same request that added it"
    );
    let rows = state
        .watcher_manager
        .root_watch_states(&IndexId::new(id))
        .await;
    let added = rows
        .iter()
        .find(|r| r.slot == Some(0))
        .unwrap_or_else(|| panic!("the added root must have a watch record, got {rows:?}"));
    assert_eq!(added.state, "watching", "and it must be live: {rows:?}");
    assert_eq!(added.root, extra.display().to_string());

    state.watcher_manager.stop_all().await;
}

/// Why: `GET /indexes/:id/status` answered `watcher.active` plus one reason
/// string, which cannot express "two of three trees are watched" — an index
/// with a dead root looks identical to a healthy one. The pre-existing fields
/// keep their meaning so no consumer breaks; the `roots` array is what makes
/// the condition diagnosable.
/// Test: this test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn status_reports_every_root_watch_state() {
    let (_dp, primary) = super::test_support::allowlisted_index_root("ts-7434-sw-p-");
    let (_dx, extra) = super::test_support::allowlisted_index_root("ts-7434-sw-x-");
    let state = mock_state_async().await;
    let id = "status-root-watch";

    create_index(&state, id, &primary).await;
    add_roots(&state, id, vec![extra.clone()]).await;

    let body = super::status::index_status_report(&state, id)
        .await
        .expect("status must succeed");
    let watcher = &body["watcher"];

    // Back-compat: the three pre-#7434 fields are untouched.
    assert_eq!(watcher["active"], serde_json::json!(true));
    assert_eq!(watcher["network_mount_degraded"], serde_json::json!(false));
    assert!(watcher["degraded_reason"].is_null());

    let roots = watcher["roots"]
        .as_array()
        .unwrap_or_else(|| panic!("watcher.roots must be an array, got {watcher}"));
    assert_eq!(roots.len(), 2, "one row per index root: {roots:?}");
    assert_eq!(roots[0]["primary"], serde_json::json!(true));
    assert!(roots[0]["slot"].is_null(), "the primary root has no slot");
    assert_eq!(roots[0]["state"], serde_json::json!("watching"));
    assert_eq!(roots[1]["slot"], serde_json::json!(0));
    assert_eq!(roots[1]["primary"], serde_json::json!(false));
    assert_eq!(roots[1]["state"], serde_json::json!("watching"));
    assert_eq!(
        roots[1]["root"],
        serde_json::json!(extra.display().to_string())
    );
    assert!(
        roots[0].get("reason").is_none() && roots[1].get("reason").is_none(),
        "a healthy watch carries no reason: {roots:?}"
    );

    state.watcher_manager.stop_all().await;
}
