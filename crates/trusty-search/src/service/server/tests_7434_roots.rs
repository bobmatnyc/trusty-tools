//! `POST /indexes/:id/roots` and `POST /indexes {roots}` — the #7434 mutation.
//!
//! Why: slice 1 made a multi-root index walkable, searchable and guarded, but
//! only warm boot could produce one. This slice is the door, and a door onto
//! the root table is a door onto the #2305/#2336 shared-corpus hazard: the
//! collision rule has to be re-asked per candidate, against every OTHER
//! index's whole table, live or cold. These tests pin that, plus the two
//! properties that are invisible until they fail — a concurrent add losing the
//! other one's root, and a reindex that started before the add reporting the
//! new root `Ready` through a shared `stages` `Arc`.
//!
//! What: drives `indexes_roots::add_index_roots_report` and
//! `indexes::create_index_handler` directly against a `MockEmbedder` state, on
//! real allowlisted temp roots.
//!
//! Test: run with `cargo test -p trusty-search tests_7434_roots`.

use super::*;
use crate::core::embed::{Embedder, MockEmbedder};
use crate::core::indexer::CodeIndexer;
use crate::core::registry::{IndexHandle, IndexId, IndexRegistry, StageState};
use crate::service::persistence::PersistedIndex;
use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::RwLock;

/// A fresh, empty registry with a mock embedder installed.
async fn mock_state_async() -> Arc<SearchAppState> {
    let state = SearchAppState::new(IndexRegistry::new());
    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(8));
    state.install_embedder(embedder).await;
    Arc::new(state)
}

/// Build a `CreateIndexRequest` with every optional field defaulted, varying
/// only `id`, `root_path` and the new `roots` list.
fn create_req(
    id: &str,
    root_path: PathBuf,
    roots: Option<Vec<String>>,
) -> super::router::CreateIndexRequest {
    super::router::CreateIndexRequest {
        id: id.to_string(),
        root_path,
        roots,
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

/// `POST /indexes` for `id` at `root`, asserting it succeeded.
async fn create_index(state: &Arc<SearchAppState>, id: &str, root: &Path) {
    let resp = super::indexes::create_index_handler(
        State(Arc::clone(state)),
        Json(create_req(id, root.to_path_buf(), None)),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK, "create '{id}' must succeed");
}

/// `POST /indexes/:id/roots` with `roots`.
async fn add_roots(
    state: &Arc<SearchAppState>,
    id: &str,
    roots: Vec<PathBuf>,
) -> Result<serde_json::Value, (StatusCode, serde_json::Value)> {
    super::indexes_roots::add_index_roots_report(
        state,
        id,
        super::indexes_roots::AddRootsRequest { roots },
    )
    .await
}

/// Register a bare multi-root handle directly, bypassing the create handler —
/// the cheapest way to stand up the index a collision must be detected
/// against.
fn register_multi_root(
    state: &Arc<SearchAppState>,
    id: &str,
    primary: &Path,
    additional: Vec<PathBuf>,
) {
    let mut handle = IndexHandle::bare(
        IndexId::new(id),
        Arc::new(RwLock::new(CodeIndexer::new(id, primary))),
        primary.to_path_buf(),
    );
    handle.additional_roots = additional;
    state.registry.register(handle);
}

// ── the two properties that fail silently ────────────────────────────────

/// A reindex queued BEFORE the add must not be able to report the added root
/// `Ready`.
///
/// Why: `spawn_reindex_with_cleanup` captures its `Arc<IndexHandle>` at spawn
/// time, so a run already queued walks the OLD root table. If the replacement
/// handle `Arc::clone`s `stages`, that run's completion lands in the handle
/// that now advertises a tree it never saw — the index reports `Ready` over
/// coverage nothing produced. The same argument applies to `walk_diagnostics`
/// (which would claim a clean walk of a root it did not visit) and
/// `last_indexed_at`.
/// What: takes the pre-add handle, adds a root, then writes a marker into each
/// of the three OLD `Arc`s exactly as a stale run's completion would, and
/// asserts none of it is visible through the registry. Against an
/// `Arc::clone`ing implementation every one of these assertions fails.
/// Test: this test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn add_root_during_reindex_does_not_mark_new_root_ready() {
    let state = mock_state_async().await;
    let (_d1, primary) = super::test_support::allowlisted_index_root("ts-7434-stale-p-");
    let (_d2, extra) = super::test_support::allowlisted_index_root("ts-7434-stale-x-");
    create_index(&state, "ts-7434-stale", &primary).await;

    let before = state
        .registry
        .get(&IndexId::new("ts-7434-stale"))
        .expect("registered");

    add_roots(&state, "ts-7434-stale", vec![extra.clone()])
        .await
        .expect("the add must succeed");

    let after = state
        .registry
        .get(&IndexId::new("ts-7434-stale"))
        .expect("still registered");
    assert_eq!(
        after.additional_roots,
        vec![extra.clone()],
        "the added root must be on the handle"
    );
    assert!(
        !Arc::ptr_eq(&before.stages, &after.stages),
        "#7434: the replacement handle must NOT share the pre-add `stages` Arc"
    );
    assert!(
        !Arc::ptr_eq(&before.walk_diagnostics, &after.walk_diagnostics),
        "#7434: nor the pre-add `walk_diagnostics` Arc"
    );
    assert!(
        !Arc::ptr_eq(&before.last_indexed_at, &after.last_indexed_at),
        "#7434: nor the pre-add `last_indexed_at` Arc"
    );

    // Now write what a run that started before the add would write on
    // completion, into the handle that run still holds.
    const MARKER: &str = "stale-run-marker-7434";
    before.stages.write().await.lexical = StageState::failed(MARKER);
    before
        .walk_diagnostics
        .write()
        .await
        .last_walk_error
        .replace(MARKER.to_string());
    before
        .last_indexed_at
        .write()
        .await
        .replace(MARKER.to_string());

    assert_ne!(
        after.stages.read().await.lexical.failure.as_deref(),
        Some(MARKER),
        "#7434: a reindex that predates the add must not write through to the \
         handle that advertises the new root"
    );
    assert_ne!(
        after
            .walk_diagnostics
            .read()
            .await
            .last_walk_error
            .as_deref(),
        Some(MARKER),
        "#7434: nor claim a walk of a root it never visited"
    );
    assert_ne!(
        after.last_indexed_at.read().await.as_deref(),
        Some(MARKER),
        "#7434: nor stamp the index as indexed at the new table"
    );
}

/// Concurrent adds must both land.
///
/// Why: the append is read-modify-write over the handle's root table. A
/// caller that reads `additional_roots` before taking the per-index permit —
/// or takes no permit at all — writes back a list that predates its sibling's
/// append, and the losing root vanishes with a `200` reported for it. The
/// positions in that list are corpus-path ordinals, so a lost entry is not
/// just missing coverage: it renumbers nothing but leaves stored `@root<n>/…`
/// paths pointing into a table shorter than they assume.
/// What: four concurrent adds of four distinct roots against one index; every
/// one must be present at the end.
/// Test: this test.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_add_roots_both_survive() {
    let state = mock_state_async().await;
    let (_dp, primary) = super::test_support::allowlisted_index_root("ts-7434-conc-p-");
    create_index(&state, "ts-7434-conc", &primary).await;

    let mut guards = Vec::new();
    let mut roots = Vec::new();
    for i in 0..4 {
        let (dir, root) =
            super::test_support::allowlisted_index_root(&format!("ts-7434-conc-{i}-"));
        guards.push(dir);
        roots.push(root);
    }

    let mut tasks = Vec::new();
    for root in roots.clone() {
        let state = Arc::clone(&state);
        tasks.push(tokio::spawn(async move {
            add_roots(&state, "ts-7434-conc", vec![root])
                .await
                .expect("each concurrent add must succeed")
        }));
    }
    for t in tasks {
        t.await.expect("task must not panic");
    }

    let handle = state
        .registry
        .get(&IndexId::new("ts-7434-conc"))
        .expect("registered");
    for root in &roots {
        assert!(
            handle.additional_roots.contains(root),
            "#7434: a concurrent add must not lose {} — got {:?}",
            root.display(),
            handle.additional_roots,
        );
    }
    assert_eq!(
        handle.additional_roots.len(),
        roots.len(),
        "every add appends exactly one root"
    );
}

// ── the collision rule, re-asked per candidate ───────────────────────────

/// A tree that is another index's PRIMARY root must be refused.
#[tokio::test]
async fn add_root_refuses_another_indexes_primary_root() {
    let state = mock_state_async().await;
    let (_da, root_a) = super::test_support::allowlisted_index_root("ts-7434-pri-a-");
    let (_db, root_b) = super::test_support::allowlisted_index_root("ts-7434-pri-b-");
    create_index(&state, "ts-7434-pri-a", &root_a).await;
    create_index(&state, "ts-7434-pri-b", &root_b).await;

    let (status, body) = add_roots(&state, "ts-7434-pri-a", vec![root_b.clone()])
        .await
        .expect_err("adding another index's primary root must be refused");
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["existing_id"], "ts-7434-pri-b");
    assert!(
        state
            .registry
            .get(&IndexId::new("ts-7434-pri-a"))
            .expect("still registered")
            .additional_roots
            .is_empty(),
        "a refused add must not mutate the root table"
    );
}

/// A tree that is another index's ADDITIONAL root must be refused too — that
/// is the any-of-N half of the guard, and the half a `root_path`-only check
/// would miss.
#[tokio::test]
async fn add_root_refuses_another_indexes_additional_root() {
    let state = mock_state_async().await;
    let (_da, root_a) = super::test_support::allowlisted_index_root("ts-7434-add-a-");
    let (_db, root_b) = super::test_support::allowlisted_index_root("ts-7434-add-b-");
    let (_dx, root_x) = super::test_support::allowlisted_index_root("ts-7434-add-x-");
    create_index(&state, "ts-7434-add-a", &root_a).await;
    register_multi_root(&state, "ts-7434-add-b", &root_b, vec![root_x.clone()]);

    let (status, body) = add_roots(&state, "ts-7434-add-a", vec![root_x.clone()])
        .await
        .expect_err("adding a tree another index already covers must be refused");
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(
        body["existing_id"], "ts-7434-add-b",
        "the refusal must name the index that covers the tree, whichever slot \
         it occupies"
    );
}

/// A tree parked on a COLD entry is as claimed as a live one (#3993).
#[tokio::test]
async fn add_root_refuses_cold_entrys_root() {
    let state = mock_state_async().await;
    let (_da, root_a) = super::test_support::allowlisted_index_root("ts-7434-cold-a-");
    let (_dc, root_cold) = super::test_support::allowlisted_index_root("ts-7434-cold-c-");
    create_index(&state, "ts-7434-cold-a", &root_a).await;
    state.cold_store.register_cold_entries(vec![PersistedIndex {
        id: "ts-7434-cold-parked".to_string(),
        root_path: root_cold.clone(),
        colocated: true,
        ..Default::default()
    }]);

    let (status, body) = add_roots(&state, "ts-7434-cold-a", vec![root_cold.clone()])
        .await
        .expect_err("a cold entry's parked root must be refused");
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["existing_id"], "ts-7434-cold-parked");
    assert!(
        state
            .cold_store
            .contains(&IndexId::new("ts-7434-cold-parked")),
        "the cold entry must remain parked, untouched"
    );
}

// ── the two no-op shapes ─────────────────────────────────────────────────

/// Adding the index's OWN primary root changes nothing.
///
/// Why: the primary root is slot 0 of the table under a different encoding
/// (stored bare, not `@root<n>/…`). Appending it would give every file under
/// it two stored spellings and a second ordinal that the longest-match encoder
/// would then prefer — a corpus that decodes but no longer matches what the
/// prune pass computes.
/// What: the call succeeds, the table stays empty, `added` is empty, and no
/// walk is queued.
/// Test: this test.
#[tokio::test]
async fn add_root_of_own_root_is_idempotent() {
    let state = mock_state_async().await;
    let (_d, root) = super::test_support::allowlisted_index_root("ts-7434-own-");
    create_index(&state, "ts-7434-own", &root).await;

    let body = add_roots(&state, "ts-7434-own", vec![root.clone()])
        .await
        .expect("adding the index's own root is a no-op, not an error");
    assert_eq!(
        body["added"],
        serde_json::json!([]),
        "nothing was added; got {body}"
    );
    assert_eq!(
        body["reindex_queued"],
        serde_json::json!(false),
        "a no-op must not queue a walk"
    );
    assert_eq!(
        body["roots"],
        serde_json::json!([root.display().to_string()]),
        "the response must still report the full table"
    );
    assert!(
        state
            .registry
            .get(&IndexId::new("ts-7434-own"))
            .expect("registered")
            .additional_roots
            .is_empty(),
        "the primary root must never appear in the additional table"
    );
}

/// One root named twice in one request is added once.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn add_root_dedupes_within_request() {
    let state = mock_state_async().await;
    let (_dp, primary) = super::test_support::allowlisted_index_root("ts-7434-dedupe-p-");
    let (_dx, extra) = super::test_support::allowlisted_index_root("ts-7434-dedupe-x-");
    create_index(&state, "ts-7434-dedupe", &primary).await;

    let body = add_roots(
        &state,
        "ts-7434-dedupe",
        vec![extra.clone(), extra.clone(), extra.clone()],
    )
    .await
    .expect("a duplicated root is deduped, not refused");
    assert_eq!(
        body["added"],
        serde_json::json!([extra.display().to_string()]),
        "the same tree named three times is one root; got {body}"
    );

    let handle = state
        .registry
        .get(&IndexId::new("ts-7434-dedupe"))
        .expect("registered");
    assert_eq!(handle.additional_roots, vec![extra.clone()]);

    // A second request naming it again is the already-covered no-op.
    let again = add_roots(&state, "ts-7434-dedupe", vec![extra.clone()])
        .await
        .expect("re-adding a covered root is a no-op");
    assert_eq!(again["added"], serde_json::json!([]));
}

// ── create-time roots take the same path ─────────────────────────────────

/// `POST /indexes {roots}` populates the table at creation.
#[tokio::test]
async fn create_index_accepts_additional_roots() {
    let state = mock_state_async().await;
    let (_dp, primary) = super::test_support::allowlisted_index_root("ts-7434-create-p-");
    let (_dx, extra) = super::test_support::allowlisted_index_root("ts-7434-create-x-");

    let resp = super::indexes::create_index_handler(
        State(Arc::clone(&state)),
        Json(create_req(
            "ts-7434-create",
            primary.clone(),
            Some(vec![
                extra.display().to_string(),
                // The primary root and a duplicate both collapse away.
                primary.display().to_string(),
                extra.display().to_string(),
            ]),
        )),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);

    let handle = state
        .registry
        .get(&IndexId::new("ts-7434-create"))
        .expect("registered");
    assert_eq!(
        handle.additional_roots,
        vec![extra.clone()],
        "#7434: create-time roots run the same canonicalise/no-op/dedupe gate"
    );
    assert_eq!(
        handle.indexer.read().await.additional_roots,
        vec![extra],
        "the indexer's mirror of the table must match, or an additional-root \
         hit resolves to a path under the primary root that does not exist"
    );
}

/// A create-time root that another index already covers is refused, and no
/// index is registered.
#[tokio::test]
async fn create_index_refuses_a_root_another_index_owns() {
    let state = mock_state_async().await;
    let (_da, root_a) = super::test_support::allowlisted_index_root("ts-7434-crefuse-a-");
    let (_dp, primary) = super::test_support::allowlisted_index_root("ts-7434-crefuse-p-");
    create_index(&state, "ts-7434-crefuse-a", &root_a).await;

    let resp = super::indexes::create_index_handler(
        State(Arc::clone(&state)),
        Json(create_req(
            "ts-7434-crefuse-b",
            primary.clone(),
            Some(vec![root_a.display().to_string()]),
        )),
    )
    .await;
    assert_eq!(
        resp.status(),
        StatusCode::CONFLICT,
        "a create naming another index's tree as an additional root must be \
         refused, not registered alongside it"
    );
    assert!(
        state
            .registry
            .get(&IndexId::new("ts-7434-crefuse-b"))
            .is_none(),
        "a refused create must register nothing"
    );
}
