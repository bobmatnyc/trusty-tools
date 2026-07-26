//! Tests for issue #3993: two index-registration paths with no `root_path`
//! collision guard — the reindex root-override path (Gap E) and lazy
//! cold-store restore (Gap F).
//!
//! Why: #2336 added `find_root_path_collision` to `create_index_handler` and
//! `relocate_index_handler` only. An audit prompted by #3929 ("a regression
//! of #2305/#2336") found the guard is unreached from two more registration
//! paths that can produce the exact same hazard — two index ids claiming one
//! physical `<root>/.trusty-search/index.redb` corpus:
//!
//!   - **Gap E**: `POST /indexes/:id/reindex` with a `root_path` override
//!     re-registers the handle at the new root with no collision check at
//!     all (`reindex_handlers.rs`).
//!   - **Gap F**: `find_root_path_collision` scans only LIVE handles
//!     (`state.registry.list_handles()`); a cold (unloaded) entry parked in
//!     `state.cold_store` is invisible to it. A `create_index` whose root
//!     matches an unloaded cold entry's root passes the guard, and the cold
//!     entry's later on-demand restore (`restore_index_on_demand`,
//!     `lazy_restore.rs`) then opens the SAME on-disk redb with no guard at
//!     all — reproducing the #2305 `DatabaseAlreadyOpen` shape from a third
//!     source.
//!
//! What: both fixes reuse the existing `find_root_path_collision` primitive
//! (no fourth collision mechanism). Gap E calls it in `reindex_handler`
//! before re-registering. Gap F calls it in `restore_index_on_demand`
//! against `state.registry.list_handles()` — the live handle that stole the
//! cold entry's root is, by construction, already registered by the time the
//! cold entry's first query triggers the on-demand restore, so checking
//! against live handles at that point (rather than teaching the create-time
//! guard about cold entries) closes the actual crash site.
//!
//! Test: `reindex_root_override_rejects_collision_with_live_sibling` (Gap E),
//! `lazy_restore_rejects_cold_entry_colliding_with_live_root_path` (Gap F).

use super::*;
use crate::core::embed::{Embedder, MockEmbedder};
use crate::core::indexer::CodeIndexer;
use crate::core::registry::{IndexHandle, IndexId, IndexRegistry};
use crate::service::persistence::PersistedIndex;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use std::sync::Arc;
use tokio::sync::RwLock;

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

// ── Gap E: reindex root-override collision guard ─────────────────────────

/// `POST /indexes/:id/reindex` with a `root_path` override pointed at a root
/// already owned by a DIFFERENT live index must be rejected with `409
/// Conflict`, mirroring `create_index_handler`'s guard, instead of silently
/// re-registering the target index onto the colliding root.
///
/// Without the Gap E fix this assertion fails: the handler returns `200
/// {"queued": true}` and `index-b`'s registry entry is silently overwritten
/// to share `index-a`'s root_path — two live ids now claim one on-disk
/// corpus, the exact #2305/#2336 hazard.
#[tokio::test]
async fn reindex_root_override_rejects_collision_with_live_sibling() {
    let (_dir_a, root_a) = super::test_support::allowlisted_index_root("ts-3993-gapE-a-");
    let (_dir_b, root_b) = super::test_support::allowlisted_index_root("ts-3993-gapE-b-");

    let registry = IndexRegistry::new();
    registry.register(IndexHandle::bare(
        IndexId::new("index-a"),
        Arc::new(RwLock::new(CodeIndexer::new("index-a", &root_a))),
        root_a.clone(),
    ));
    registry.register(IndexHandle::bare(
        IndexId::new("index-b"),
        Arc::new(RwLock::new(CodeIndexer::new("index-b", &root_b))),
        root_b.clone(),
    ));
    let state = Arc::new(SearchAppState::new(registry));

    // Attempt to re-point index-b's root_path at index-a's root via the
    // reindex override — the same root-theft `create_index_handler` already
    // guards against, but through the reindex path instead.
    let result = reindex_handler(
        State(Arc::clone(&state)),
        Path("index-b".to_string()),
        Some(Json(ReindexRequest {
            root_path: Some(root_a.clone()),
            force: None,
            background: None,
        })),
    )
    .await;

    let err = result.expect_err(
        "reindex root_path override colliding with a live sibling's root must be rejected",
    );
    assert_eq!(err.0, StatusCode::CONFLICT);
    assert_eq!(
        err.1 .0.get("existing_id").and_then(|v| v.as_str()),
        Some("index-a"),
        "the 409 body must name the existing index that owns the root_path"
    );

    // Neither handle's root_path may have changed — the rejected override
    // must not have re-registered index-b onto index-a's root.
    let a_root_after = state
        .registry
        .get(&IndexId::new("index-a"))
        .expect("index-a still registered")
        .root_path
        .clone();
    let b_root_after = state
        .registry
        .get(&IndexId::new("index-b"))
        .expect("index-b still registered")
        .root_path
        .clone();
    assert_eq!(a_root_after, root_a);
    assert_eq!(
        b_root_after, root_b,
        "index-b's root_path must remain unchanged after a rejected collision override"
    );
}

/// A reindex root_path override onto a genuinely distinct, unclaimed root
/// must still succeed — the new guard must not over-trigger.
#[tokio::test]
async fn reindex_root_override_distinct_root_succeeds() {
    let (_dir_old, root_old) = super::test_support::allowlisted_index_root("ts-3993-gapE-old-");
    let (_dir_new, root_new) = super::test_support::allowlisted_index_root("ts-3993-gapE-new-");

    let registry = IndexRegistry::new();
    registry.register(IndexHandle::bare(
        IndexId::new("solo"),
        Arc::new(RwLock::new(CodeIndexer::new("solo", &root_old))),
        root_old.clone(),
    ));
    let state = Arc::new(SearchAppState::new(registry));

    let result = reindex_handler(
        State(Arc::clone(&state)),
        Path("solo".to_string()),
        Some(Json(ReindexRequest {
            root_path: Some(root_new.clone()),
            force: None,
            background: None,
        })),
    )
    .await
    .expect("override onto an unclaimed root must succeed");
    assert_eq!(result.0["queued"], serde_json::Value::Bool(true));

    let root_after = state
        .registry
        .get(&IndexId::new("solo"))
        .expect("solo still registered")
        .root_path
        .clone();
    assert_eq!(root_after, root_new);
}

// ── Gap F: lazy cold-store restore vs. a live sibling's root_path ────────

/// A cold (unloaded) index entry whose `root_path` collides with a LIVE
/// sibling index must never be restored into the hot registry — restoring it
/// would call `build_indexer_from_entry`, which opens the same on-disk redb
/// the live sibling already holds open, hitting `DatabaseAlreadyOpen`
/// (issue #3993 Gap F: the third source of the #2305 hazard, distinct from
/// both #2305's warm-boot path and #2336's create/relocate path).
///
/// Without the Gap F fix this assertion fails: `restore_index_on_demand`
/// builds the indexer anyway (its corpus open fails and sets
/// `corpus_open_failed = true`, caught nowhere) and unconditionally
/// registers the broken handle into the hot registry — a live duplicate
/// silently claiming a root_path another live index already owns.
///
/// The entry is genuinely COLD here: it is registered directly into
/// `state.cold_store` and never touches `state.registry` before
/// `restore_index_on_demand` is invoked — this does not merely simulate the
/// gap, it drives the exact function the search handler calls on a cold-index
/// query miss.
#[tokio::test]
async fn lazy_restore_rejects_cold_entry_colliding_with_live_root_path() {
    let state = mock_state_async().await;
    let (_dir, root) = super::test_support::allowlisted_index_root("ts-3993-gapF-");

    // Step 1: register a REAL live index at `root` via create_index_handler
    // (colocated storage — its redb corpus is opened and held for the
    // lifetime of this registered handle, exactly as in production).
    let created = create_index_handler(
        State(Arc::clone(&state)),
        Json(create_req("index-a", root.clone())),
    )
    .await;
    assert_eq!(created.status(), StatusCode::OK, "index-a must register");

    // Step 2: park a cold entry for a DIFFERENT id at the SAME canonical
    // root, colocated=true — matching create_index_handler's own init_entry
    // shape, so it resolves to the identical `<root>/.trusty-search/index.redb`
    // path index-a already has open.
    let cold_entry = PersistedIndex {
        id: "index-b".to_string(),
        root_path: root.clone(),
        colocated: true,
        ..Default::default()
    };
    state
        .cold_store
        .register_cold_entries(vec![cold_entry.clone()]);
    assert!(
        state.cold_store.contains(&IndexId::new("index-b")),
        "cold entry must be parked before the restore attempt"
    );

    // Step 3: drive the exact on-demand restore path a cold-index query miss
    // triggers (mirrors `get_or_load_index`'s call in `search.rs`).
    let embedder = state
        .current_embedder()
        .await
        .expect("mock embedder installed");
    crate::service::lazy_restore::restore_index_on_demand(&state, &embedder, cold_entry).await;

    // The colliding cold entry must never have been registered live.
    assert!(
        state.registry.get(&IndexId::new("index-b")).is_none(),
        "a cold entry colliding with a live sibling's root_path must never be \
         registered into the hot registry"
    );
    // It must be marked permanently failed (existing #1106 semantics) so
    // subsequent queries return a fast 503 instead of retrying the doomed
    // restore on every request.
    assert!(
        state.cold_store.is_failed(&IndexId::new("index-b")),
        "the colliding cold entry must be marked permanently failed"
    );

    // index-a must be completely unaffected: still live, still healthy.
    let handle_a = state
        .registry
        .get(&IndexId::new("index-a"))
        .expect("index-a must remain registered");
    assert!(
        !handle_a.indexer.read().await.corpus_open_failed,
        "index-a's corpus must not have been disturbed by the rejected restore"
    );
}

/// A cold entry whose root does NOT collide with any live handle must still
/// restore successfully — the new guard must not over-trigger.
#[tokio::test]
async fn lazy_restore_succeeds_for_non_colliding_cold_entry() {
    let state = mock_state_async().await;
    let (_dir_a, root_a) = super::test_support::allowlisted_index_root("ts-3993-gapF-live-");
    let (_dir_b, root_b) = super::test_support::allowlisted_index_root("ts-3993-gapF-cold-");

    let created = create_index_handler(
        State(Arc::clone(&state)),
        Json(create_req("index-a", root_a.clone())),
    )
    .await;
    assert_eq!(created.status(), StatusCode::OK);

    let cold_entry = PersistedIndex {
        id: "index-b".to_string(),
        root_path: root_b.clone(),
        colocated: true,
        ..Default::default()
    };
    state
        .cold_store
        .register_cold_entries(vec![cold_entry.clone()]);

    let embedder = state
        .current_embedder()
        .await
        .expect("mock embedder installed");
    crate::service::lazy_restore::restore_index_on_demand(&state, &embedder, cold_entry).await;

    assert!(
        state.registry.get(&IndexId::new("index-b")).is_some(),
        "a non-colliding cold entry must restore normally"
    );
    assert!(
        !state.cold_store.is_failed(&IndexId::new("index-b")),
        "a non-colliding restore must not be marked failed"
    );
}
