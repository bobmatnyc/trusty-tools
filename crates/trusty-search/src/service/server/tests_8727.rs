//! A parked registration that blocks `POST /indexes` is listed, and the `409`
//! names it (#8727).
//!
//! Why: the overlap check scans cold entries, while `GET /indexes` listed only
//! resident handles. A cold registration whose worktree had been deleted
//! refused a rebuild of the enclosing repo from an index `list`, `status`, and
//! MCP `list_indexes` never showed.
//! What: a cold entry rooted at a deleted worktree inside an allowlisted repo;
//! the test creates an index over the repo, then lists.
//! Test: this module.

use std::sync::Arc;

use axum::body::to_bytes;
use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;

use super::indexes::{create_index_handler, list_indexes_report, ListIndexesParams};
use super::router::CreateIndexRequest;
use super::SearchAppState;
use crate::core::embed::Embedder;
use crate::core::registry::IndexRegistry;
use crate::service::persistence::PersistedIndex;

const STALE_ID: &str = "agent-gone-8727";

/// #8727: the blocking registration is visible in both list shapes, and the
/// refusal names its id, its root, and the command that clears it.
#[tokio::test]
async fn a_parked_registration_that_blocks_create_is_listed_and_named() {
    let state = SearchAppState::new(IndexRegistry::new());
    let embedder: Arc<dyn Embedder> = Arc::new(crate::core::embed::MockEmbedder::new(8));
    state.install_embedder(embedder).await;
    let state = Arc::new(state);
    let (_dir, repo) = super::test_support::allowlisted_index_root("ts-8727-");
    let worktrees = repo.join(".claude").join("worktrees");
    std::fs::create_dir_all(&worktrees).expect("worktrees dir");
    let stale_root = worktrees.join(STALE_ID);
    state
        .cold_store
        .register_cold_entries(vec![PersistedIndex::new(
            STALE_ID.to_string(),
            stale_root.clone(),
        )]);

    let req: CreateIndexRequest = serde_json::from_value(serde_json::json!({
        "id": "repo-8727",
        "root_path": repo,
    }))
    .expect("request");
    let response = create_index_handler(State(Arc::clone(&state)), Json(req)).await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let body: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
    let stale = stale_root.display().to_string();
    assert_eq!(body["existing_index_id"], STALE_ID, "{body}");
    assert_eq!(body["existing_root_path"], stale, "{body}");
    assert_eq!(body["existing_root_state"], "orphaned", "{body}");
    assert!(
        body["error"]
            .as_str()
            .is_some_and(|e| e.contains(&format!("index remove -i {STALE_ID}"))),
        "the refusal must name the command that clears it: {body}"
    );

    for params in [
        ListIndexesParams::default(),
        ListIndexesParams {
            details: true,
            ..Default::default()
        },
    ] {
        let listed = list_indexes_report(&state, &params).await;
        let parked = listed["parked"].as_array().cloned().unwrap_or_default();
        assert!(
            parked.iter().any(|row| row["id"] == STALE_ID
                && row["root_path"] == stale.as_str()
                && row["root_state"] == "orphaned"),
            "a registration the overlap check consults must be listed: {listed}"
        );
    }
}
