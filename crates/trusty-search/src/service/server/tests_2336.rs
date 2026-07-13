//! Tests for issue #2336: `create_index`/`relocate_index` root_path collision
//! guard.
//!
//! Why: review finding (HIGH) on the #2305 warm-boot dedup fix — the boot path
//! was fixed, but `create_index_handler` and `relocate_index_handler` guarded
//! only *id* collisions, never *root_path* collisions. Two live registrations
//! sharing one colocated root resolve to the SAME on-disk
//! `<root>/.trusty-search/index.redb` (redb is single-open); the second
//! registration's corpus open would silently fail (`corpus_open_failed`)
//! while the handler still returned `200 {"created": true}` for the broken
//! handle — recreating the #2305 hazard at runtime.
//! What: exercises `create_index_handler` and `relocate_index_handler`
//! directly (no running daemon or real embedder needed — `MockEmbedder`
//! stands in), asserting a second registration over the same canonical root
//! is rejected with `409 Conflict` naming the existing index, while a
//! same-id re-create and a same-root PATCH-to-self remain unaffected.
//! Test: run with `cargo test -p trusty-search tests_2336`.

use super::*;
use crate::core::embed::Embedder;
use crate::core::registry::IndexRegistry;
use axum::body::to_bytes;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use std::sync::Arc;

/// Build a `CreateIndexRequest` with every optional field defaulted, varying
/// only `id` and `root_path`.
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
        defer_embed: None,
        extra_skip_dirs: None,
        data_file_max_bytes: None,
    }
}

/// Create a temp directory under `target/` (never in the hard denylist) with
/// RAII cleanup, returning its canonical path.
fn temp_root(prefix: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let cwd = std::env::current_dir().expect("cwd");
    let base = cwd.join("target");
    std::fs::create_dir_all(&base).expect("create target/");
    let dir = tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in(&base)
        .expect("create tempdir");
    let canonical = dir.path().canonicalize().expect("canonicalize tempdir");
    (dir, canonical)
}

/// Build a fresh, empty registry with a mock embedder installed — enough for
/// `create_index_handler` / `relocate_index_handler` to run without a live
/// daemon or network.
async fn mock_state_async() -> Arc<SearchAppState> {
    let state = SearchAppState::new(IndexRegistry::new());
    let embedder: Arc<dyn Embedder> = Arc::new(crate::core::embed::MockEmbedder::new(8));
    state.install_embedder(embedder).await;
    Arc::new(state)
}

// ── create_index_handler root_path collision guard ───────────────────────

/// A second `POST /indexes` over the SAME canonical `root_path` (different
/// id) must be rejected with `409 Conflict` naming the existing index,
/// instead of silently registering a second handle over the same redb file.
#[tokio::test]
async fn create_index_rejects_duplicate_root_path() {
    let state = mock_state_async().await;
    let (_dir, root) = temp_root("ts-2336-dup-root-");

    let first = super::indexes::create_index_handler(
        State(Arc::clone(&state)),
        Json(create_req("first-id", root.clone())),
    )
    .await;
    assert_eq!(first.status(), StatusCode::OK, "first create must succeed");

    let second = super::indexes::create_index_handler(
        State(Arc::clone(&state)),
        Json(create_req("second-id", root.clone())),
    )
    .await;
    assert_eq!(
        second.status(),
        StatusCode::CONFLICT,
        "second create over the same root_path must be rejected with 409"
    );

    let body = to_bytes(second.into_body(), 4096).await.expect("body");
    let v: serde_json::Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(
        v.get("existing_id").and_then(|x| x.as_str()),
        Some("first-id"),
        "the 409 body must name the existing index that owns the root_path"
    );

    // Exactly one index must be registered — the collision must NOT have
    // built (and discarded) a second broken handle.
    assert_eq!(
        state.registry.list().len(),
        1,
        "only the first index may be registered after a rejected collision"
    );
}

/// Re-POSTing the SAME id at the SAME root_path is the existing idempotent
/// "already exists" path (unaffected by the new collision guard, since it is
/// checked before the guard runs).
#[tokio::test]
async fn create_index_same_id_same_root_is_idempotent_not_a_collision() {
    let state = mock_state_async().await;
    let (_dir, root) = temp_root("ts-2336-idempotent-");

    let first = super::indexes::create_index_handler(
        State(Arc::clone(&state)),
        Json(create_req("same-id", root.clone())),
    )
    .await;
    assert_eq!(first.status(), StatusCode::OK);

    let second = super::indexes::create_index_handler(
        State(Arc::clone(&state)),
        Json(create_req("same-id", root.clone())),
    )
    .await;
    assert_eq!(
        second.status(),
        StatusCode::OK,
        "re-registering the same id must stay idempotent (already exists), not 409"
    );
    let body = to_bytes(second.into_body(), 4096).await.expect("body");
    let v: serde_json::Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(v.get("created").and_then(|x| x.as_bool()), Some(false));
}

/// Two indexes at genuinely distinct roots must both register successfully —
/// the guard must not over-trigger on non-colliding paths.
#[tokio::test]
async fn create_index_distinct_roots_both_succeed() {
    let state = mock_state_async().await;
    let (_dir_a, root_a) = temp_root("ts-2336-distinct-a-");
    let (_dir_b, root_b) = temp_root("ts-2336-distinct-b-");

    let a = super::indexes::create_index_handler(
        State(Arc::clone(&state)),
        Json(create_req("index-a", root_a)),
    )
    .await;
    assert_eq!(a.status(), StatusCode::OK);

    let b = super::indexes::create_index_handler(
        State(Arc::clone(&state)),
        Json(create_req("index-b", root_b)),
    )
    .await;
    assert_eq!(
        b.status(),
        StatusCode::OK,
        "distinct root_path registrations must not collide"
    );
    assert_eq!(state.registry.list().len(), 2);
}

// ── relocate_index_handler root_path collision guard ──────────────────────

/// `PATCH /indexes/:id` that would relocate onto a root_path already owned by
/// a DIFFERENT registered index must be rejected with `409 Conflict`.
#[tokio::test]
async fn relocate_index_rejects_root_path_owned_by_another_index() {
    use super::indexes_relocate::{relocate_index_handler, RelocateIndexRequest};

    let state = mock_state_async().await;
    let (_dir_a, root_a) = temp_root("ts-2336-relocate-a-");
    let (_dir_b, root_b) = temp_root("ts-2336-relocate-b-");

    let create_a = super::indexes::create_index_handler(
        State(Arc::clone(&state)),
        Json(create_req("index-a", root_a.clone())),
    )
    .await;
    assert_eq!(create_a.status(), StatusCode::OK);

    let create_b = super::indexes::create_index_handler(
        State(Arc::clone(&state)),
        Json(create_req("index-b", root_b.clone())),
    )
    .await;
    assert_eq!(create_b.status(), StatusCode::OK);

    // Attempt to relocate index-b onto index-a's root — must be rejected.
    let relocate = relocate_index_handler(
        State(Arc::clone(&state)),
        Path("index-b".to_string()),
        Json(RelocateIndexRequest {
            root_path: root_a.clone(),
        }),
    )
    .await;
    assert_eq!(
        relocate.status(),
        StatusCode::CONFLICT,
        "relocating onto another index's root_path must be rejected with 409"
    );
    let body = to_bytes(relocate.into_body(), 4096).await.expect("body");
    let v: serde_json::Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(
        v.get("existing_id").and_then(|x| x.as_str()),
        Some("index-a"),
        "the 409 body must name the index that already owns the target root_path"
    );

    // index-b's root_path must be unchanged after the rejected relocation.
    let handle_b = state
        .registry
        .get(&crate::core::registry::IndexId::new("index-b"))
        .expect("index-b must still be registered");
    assert_eq!(
        handle_b.root_path, root_b,
        "a rejected relocation must not mutate the existing handle's root_path"
    );
}

/// Relocating an index onto ITS OWN current root_path (a no-op PATCH) must
/// NOT be treated as a collision — the guard excludes the index's own id.
#[tokio::test]
async fn relocate_index_onto_own_current_root_is_not_a_collision() {
    use super::indexes_relocate::{relocate_index_handler, RelocateIndexRequest};

    let state = mock_state_async().await;
    let (_dir, root) = temp_root("ts-2336-relocate-self-");

    let create = super::indexes::create_index_handler(
        State(Arc::clone(&state)),
        Json(create_req("self-relocate", root.clone())),
    )
    .await;
    assert_eq!(create.status(), StatusCode::OK);

    let relocate = relocate_index_handler(
        State(Arc::clone(&state)),
        Path("self-relocate".to_string()),
        Json(RelocateIndexRequest {
            root_path: root.clone(),
        }),
    )
    .await;
    assert_eq!(
        relocate.status(),
        StatusCode::OK,
        "relocating an index onto its own current root must succeed, not 409"
    );
}
