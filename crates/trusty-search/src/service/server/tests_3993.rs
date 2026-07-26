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
//! against `state.registry.list_handles()`.
//!
//! Test: `reindex_root_override_rejects_collision_with_live_sibling` (Gap E),
//! `lazy_restore_rejects_cold_entry_colliding_with_live_root_path` (Gap F).
//!
//! ## Second round (adversarial re-review, BLOCK)
//!
//! The first round's Gap F fix (above) only caught the collision from the
//! FAR side: once a cold entry's root_path had already been stolen by a new
//! live registration, the cold entry's eventual restore attempt detected the
//! live collision and marked ITSELF failed — punishing the pre-existing,
//! legitimate entry instead of rejecting the interloper. Worse, the write
//! side (`create_index_handler`, `relocate_index_handler`, and Gap E's
//! `reindex_handler` override) never consulted `state.cold_store` at all, so
//! the theft required NO race whatsoever: a cold entry could sit parked,
//! untouched, and a later `create_index`/`relocate_index`/reindex-override
//! at the same root would silently succeed.
//!
//! Fix: `find_root_path_collision` (`helpers.rs`) now takes a `cold_entries`
//! slice alongside `handles`, and all three write-side call sites pass
//! `state.cold_store.snapshot()`. First-claimant-wins now holds regardless
//! of whether the first claimant is live or cold. `restore_index_on_demand`
//! additionally gained a `corpus_open_failed` ground-truth backstop
//! (mirroring `create_index_handler`/`relocate_index_handler`) for the
//! residual genuine race — two different cold entries, sharing a root_path
//! only through pre-existing on-disk corruption, restored concurrently.
//!
//! Test (second round): `create_index_rejects_root_path_owned_by_cold_entry`,
//! `relocate_index_rejects_root_path_owned_by_cold_entry`,
//! `reindex_root_override_rejects_collision_with_cold_entry`,
//! `lazy_restore_concurrent_cold_entries_at_same_root_only_one_wins`.

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

// ── Second round (adversarial BLOCK): write-side guards must also see cold
//    entries, not just live handles ────────────────────────────────────────

/// Adversarial-review reproduction, executed exactly as reported: park a
/// cold entry FIRST (a pre-existing index that survived a daemon restart and
/// has not been queried yet this session), THEN call `create_index_handler`
/// with a DIFFERENT id at the SAME root. No race is involved — the cold
/// entry sits parked, untouched, before the create call ever arrives.
///
/// Without the fix this assertion fails: the handler returns `200
/// {"created": true}`, silently letting `index-new` claim `index-old`'s
/// on-disk corpus.
#[tokio::test]
async fn create_index_rejects_root_path_owned_by_cold_entry() {
    let state = mock_state_async().await;
    let (_dir, root) = super::test_support::allowlisted_index_root("ts-3993-r2-create-vs-cold-");

    let cold_entry = PersistedIndex {
        id: "index-old".to_string(),
        root_path: root.clone(),
        colocated: true,
        ..Default::default()
    };
    state.cold_store.register_cold_entries(vec![cold_entry]);
    assert!(
        state.cold_store.contains(&IndexId::new("index-old")),
        "cold entry must be parked before the create attempt"
    );

    let created = create_index_handler(
        State(Arc::clone(&state)),
        Json(create_req("index-new", root.clone())),
    )
    .await;

    assert_eq!(
        created.status(),
        StatusCode::CONFLICT,
        "create_index must reject a root_path already parked by a cold entry \
         (issue #3993 second round) instead of silently registering a live \
         sibling over it"
    );
    assert!(
        state.registry.get(&IndexId::new("index-new")).is_none(),
        "the interloping create must never have registered a live handle"
    );
    assert!(
        state.cold_store.contains(&IndexId::new("index-old")),
        "the pre-existing cold entry must remain parked, untouched — \
         first-claimant-wins even when the first claimant is cold"
    );
}

/// A `create_index` at a root NOT claimed by any cold entry must still
/// succeed — the widened guard must not over-trigger against unrelated cold
/// entries.
#[tokio::test]
async fn create_index_distinct_root_succeeds_with_unrelated_cold_entry_present() {
    let state = mock_state_async().await;
    let (_dir_cold, root_cold) =
        super::test_support::allowlisted_index_root("ts-3993-r2-create-cold-other-");
    let (_dir_new, root_new) =
        super::test_support::allowlisted_index_root("ts-3993-r2-create-new-");

    let cold_entry = PersistedIndex {
        id: "index-old".to_string(),
        root_path: root_cold.clone(),
        colocated: true,
        ..Default::default()
    };
    state.cold_store.register_cold_entries(vec![cold_entry]);

    let created = create_index_handler(
        State(Arc::clone(&state)),
        Json(create_req("index-new", root_new.clone())),
    )
    .await;
    assert_eq!(
        created.status(),
        StatusCode::OK,
        "an unrelated cold entry at a different root must not block a genuinely \
         distinct create_index"
    );
}

/// Relocating a LIVE index onto a cold entry's parked root must be rejected
/// — the identical root-theft as the create_index case above, via the PATCH
/// path instead.
///
/// Without the fix this assertion fails: `relocate_index_handler` returns
/// `200 {"relocated": true}`, re-pointing `index-a` onto `index-cold`'s root.
#[tokio::test]
async fn relocate_index_rejects_root_path_owned_by_cold_entry() {
    use super::indexes_relocate::{relocate_index_handler, RelocateIndexRequest};

    let state = mock_state_async().await;
    let (_dir_a, root_a) = super::test_support::allowlisted_index_root("ts-3993-r2-relocate-a-");
    let (_dir_cold, root_cold) =
        super::test_support::allowlisted_index_root("ts-3993-r2-relocate-cold-");

    let created = create_index_handler(
        State(Arc::clone(&state)),
        Json(create_req("index-a", root_a.clone())),
    )
    .await;
    assert_eq!(created.status(), StatusCode::OK);

    let cold_entry = PersistedIndex {
        id: "index-cold".to_string(),
        root_path: root_cold.clone(),
        colocated: true,
        ..Default::default()
    };
    state.cold_store.register_cold_entries(vec![cold_entry]);

    let relocate = relocate_index_handler(
        State(Arc::clone(&state)),
        Path("index-a".to_string()),
        Json(RelocateIndexRequest {
            root_path: root_cold.clone(),
        }),
    )
    .await;

    assert_eq!(
        relocate.status(),
        StatusCode::CONFLICT,
        "relocating onto a cold entry's parked root_path must be rejected with 409"
    );
    let handle_a = state
        .registry
        .get(&IndexId::new("index-a"))
        .expect("index-a must still be registered");
    assert_eq!(
        handle_a.root_path, root_a,
        "a rejected relocation must not mutate the existing handle's root_path"
    );
    assert!(
        state.cold_store.contains(&IndexId::new("index-cold")),
        "the cold entry must remain parked, untouched"
    );
}

/// A `POST /indexes/:id/reindex` `root_path` override pointed at a cold
/// entry's parked root must be rejected with `409`, same as the live-sibling
/// case Gap E already covers.
///
/// Without the fix this assertion fails: `reindex_handler` returns `200
/// {"queued": true}` and re-points `index-b` onto `index-cold`'s root.
#[tokio::test]
async fn reindex_root_override_rejects_collision_with_cold_entry() {
    let (_dir_b, root_b) = super::test_support::allowlisted_index_root("ts-3993-r2-reindex-b-");
    let (_dir_cold, root_cold) =
        super::test_support::allowlisted_index_root("ts-3993-r2-reindex-cold-");

    let registry = IndexRegistry::new();
    registry.register(IndexHandle::bare(
        IndexId::new("index-b"),
        Arc::new(RwLock::new(CodeIndexer::new("index-b", &root_b))),
        root_b.clone(),
    ));
    let state = Arc::new(SearchAppState::new(registry));

    let cold_entry = PersistedIndex {
        id: "index-cold".to_string(),
        root_path: root_cold.clone(),
        colocated: true,
        ..Default::default()
    };
    state.cold_store.register_cold_entries(vec![cold_entry]);

    let result = reindex_handler(
        State(Arc::clone(&state)),
        Path("index-b".to_string()),
        Some(Json(ReindexRequest {
            root_path: Some(root_cold.clone()),
            force: None,
            background: None,
        })),
    )
    .await;

    let err = result.expect_err(
        "reindex root_path override colliding with a cold entry's root must be rejected",
    );
    assert_eq!(err.0, StatusCode::CONFLICT);
    assert_eq!(
        err.1 .0.get("existing_id").and_then(|v| v.as_str()),
        Some("index-cold"),
        "the 409 body must name the cold entry that already owns the root_path"
    );

    let b_root_after = state
        .registry
        .get(&IndexId::new("index-b"))
        .expect("index-b still registered")
        .root_path
        .clone();
    assert_eq!(
        b_root_after, root_b,
        "index-b's root_path must remain unchanged after a rejected collision override"
    );
}

/// Genuine concurrent race: two DIFFERENT cold entries sharing one root_path
/// (the only way this can arise post-fix is pre-existing on-disk corruption
/// — the write-side guards above now prevent any NEW registration from
/// creating this state) are restored via `restore_index_on_demand` at the
/// same instant, so NEITHER sees the other as a live handle. The
/// `corpus_open_failed` ground-truth backstop (mirroring
/// `create_index_handler`/`relocate_index_handler`) must ensure redb's real
/// single-open semantics decide the winner — exactly one of the two may end
/// up live; both observing success would silently double-register one
/// on-disk corpus (the exact #2305/#2336/#3993 hazard this whole issue
/// chain is about).
#[tokio::test]
async fn lazy_restore_concurrent_cold_entries_at_same_root_only_one_wins() {
    let state = mock_state_async().await;
    let (_dir, root) = super::test_support::allowlisted_index_root("ts-3993-r2-concurrent-cold-");

    let entry_a = PersistedIndex {
        id: "racer-a".to_string(),
        root_path: root.clone(),
        colocated: true,
        ..Default::default()
    };
    let entry_b = PersistedIndex {
        id: "racer-b".to_string(),
        root_path: root.clone(),
        colocated: true,
        ..Default::default()
    };
    // Park both cold — mirrors the corrupted-on-disk-state precondition this
    // scenario requires (see doc comment above).
    state
        .cold_store
        .register_cold_entries(vec![entry_a.clone(), entry_b.clone()]);

    let embedder = state
        .current_embedder()
        .await
        .expect("mock embedder installed");

    let state_a = Arc::clone(&state);
    let embedder_a = Arc::clone(&embedder);
    let state_b = Arc::clone(&state);
    let embedder_b = Arc::clone(&embedder);

    tokio::join!(
        crate::service::lazy_restore::restore_index_on_demand(&state_a, &embedder_a, entry_a),
        crate::service::lazy_restore::restore_index_on_demand(&state_b, &embedder_b, entry_b),
    );

    let live_count = [
        state.registry.get(&IndexId::new("racer-a")).is_some(),
        state.registry.get(&IndexId::new("racer-b")).is_some(),
    ]
    .into_iter()
    .filter(|live| *live)
    .count();
    assert_eq!(
        live_count, 1,
        "exactly one of the two racing cold restores over the same root_path may \
         end up live — both live would silently double-register one on-disk corpus"
    );
}
