//! Handler-level tests for the runtime KG/vector component toggle on
//! `PATCH /indexes/:id/config` (issue #2984 Phase 1).
//!
//! Why: `components.rs`'s `tests_pure` module covers the pure
//! `resolve_component_toggle` decision function; these tests drive the real
//! `patch_index_config_handler` end to end — soft-disable is instantaneous
//! and observable in the SAME response, soft-enable spawns a background
//! catch-up that this module polls for completion (mirrors
//! `service::reindex::defer_embed::tests`'s polling pattern), and the 409
//! concurrency guard is exercised by holding the background semaphore permit
//! before calling the handler.
//! What: handler-level tests driving `patch_index_config_handler` against an
//! in-process registry. Every test that reaches a successful persist (any
//! `kg`/`vector` field present) isolates `TRUSTY_DATA_DIR` to a tempdir AND
//! is `#[serial_test::serial]` — mirrors `tests_index_config`'s
//! `patch_updates_only_supplied_fields` convention. `#[serial_test::serial]`
//! alone is not enough: it only excludes other `#[serial]`-tagged tests
//! within this same process, not every unmarked test that might touch the
//! real default data dir concurrently, so isolating the path is required
//! too, not optional hardening.
//! Test: this module.

use super::index_config::{patch_index_config_handler, PatchIndexConfigRequest};
use super::state::SearchAppState;
use crate::core::indexer::CodeIndexer;
use crate::core::registry::{IndexHandle, IndexId, IndexRegistry, StageStatus};
use axum::body::to_bytes;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Register a bare handle (both components enabled) under `id` and return
/// the shared state.
fn state_with_index(id: &str) -> Arc<SearchAppState> {
    let registry = IndexRegistry::new();
    let tmp = std::env::temp_dir();
    registry.register(IndexHandle::bare(
        IndexId::new(id),
        Arc::new(RwLock::new(CodeIndexer::new(id, &tmp))),
        tmp,
    ));
    Arc::new(SearchAppState::new(registry))
}

/// RAII guard that points `TRUSTY_DATA_DIR` at a fresh tempdir for the
/// duration of the test and restores the env var on drop (even on panic).
///
/// Why: every test that drives a component toggle through to a successful
/// persist writes `indexes.toml`. Without isolating the path, it races
/// against every OTHER concurrently-running test that touches the real
/// default data dir (not just other `#[serial]`-tagged tests — serial_test's
/// lock only excludes tests sharing its tag). See the module doc comment.
/// What: sets `TRUSTY_DATA_DIR` in `new()`; `Drop` removes it.
struct IsolatedDataDir {
    _tmp: tempfile::TempDir,
}

impl IsolatedDataDir {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("TRUSTY_DATA_DIR", tmp.path()) };
        Self { _tmp: tmp }
    }
}

impl Drop for IsolatedDataDir {
    fn drop(&mut self) {
        unsafe { std::env::remove_var("TRUSTY_DATA_DIR") };
    }
}

async fn body_json(resp: axum::response::Response) -> serde_json::Value {
    let bytes = to_bytes(resp.into_body(), 1 << 20).await.expect("body");
    serde_json::from_slice(&bytes).expect("json")
}

/// Poll `handle.stages` up to 2s until `pred` is true. Mirrors the polling
/// pattern in `service::reindex::defer_embed::tests`.
async fn poll_stages<F: Fn(&crate::core::registry::IndexStages) -> bool>(
    handle: &Arc<IndexHandle>,
    pred: F,
) {
    for _ in 0..100 {
        let stages = handle.stages.read().await;
        if pred(&stages) {
            return;
        }
        drop(stages);
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("condition never became true within 2s");
}

/// `PATCH { kg: false }` on an index with KG enabled must be an instantaneous
/// soft-disable: `skip_kg` flips, the graph stage flips to `Skipped` in the
/// SAME response (no catch-up needed for a turn-off), and the in-memory
/// symbol graph is dropped.
///
/// Why: pins D2 (soft-disable is synchronous, on-disk data untouched) at the
/// handler level.
/// What: PATCHes `kg: false`, asserts the response's `components.kg == false`
/// and `catch_up_started == false`, and that the registered handle's
/// `skip_kg` + graph stage reflect the disable immediately (no polling
/// needed).
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn patch_kg_off_is_instantaneous_soft_disable() {
    let _isolated = IsolatedDataDir::new();
    let state = state_with_index("comp-kg-off");
    let resp = patch_index_config_handler(
        State(Arc::clone(&state)),
        Path("comp-kg-off".to_string()),
        Json(PatchIndexConfigRequest {
            kg: Some(false),
            ..Default::default()
        }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["components"]["kg"].as_bool(), Some(false));
    assert_eq!(
        body["components"]["catch_up_started"].as_bool(),
        Some(false)
    );

    let handle = state
        .registry
        .get(&IndexId::new("comp-kg-off"))
        .expect("handle");
    assert!(handle.skip_kg, "skip_kg must be set immediately");
    let stages = handle.stages.read().await;
    assert_eq!(
        stages.graph.status,
        StageStatus::Skipped,
        "graph stage must flip to Skipped in the same request"
    );
}

/// `PATCH { vector: false }` mirrors the KG soft-disable but must NOT touch
/// the graph stage — the two components are orthogonal (the vector-off/KG-on
/// quadrant, issue #2984's headline scenario).
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn patch_vector_off_leaves_kg_untouched() {
    let _isolated = IsolatedDataDir::new();
    let state = state_with_index("comp-vector-off");
    let resp = patch_index_config_handler(
        State(Arc::clone(&state)),
        Path("comp-vector-off".to_string()),
        Json(PatchIndexConfigRequest {
            vector: Some(false),
            ..Default::default()
        }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["components"]["vector"].as_bool(), Some(false));
    assert_eq!(body["components"]["kg"].as_bool(), Some(true));

    let handle = state
        .registry
        .get(&IndexId::new("comp-vector-off"))
        .expect("handle");
    assert!(handle.skip_vector);
    assert!(
        !handle.skip_kg,
        "kg must remain enabled — orthogonal toggle"
    );
    let stages = handle.stages.read().await;
    assert_eq!(stages.semantic.status, StageStatus::Skipped);
}

/// `PATCH { kg: true }` on an index with KG already disabled must spawn a
/// background catch-up: the response reports `catch_up_started: true`
/// immediately, the graph stage is `InProgress` right away, and it
/// eventually settles to `Ready` once `catch_up_symbol_graph` completes
/// (cheap rebuild on the empty test corpus).
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn patch_kg_on_spawns_catch_up_and_reaches_ready() {
    let _isolated = IsolatedDataDir::new();
    let state = state_with_index("comp-kg-on");
    // First disable it.
    let resp = patch_index_config_handler(
        State(Arc::clone(&state)),
        Path("comp-kg-on".to_string()),
        Json(PatchIndexConfigRequest {
            kg: Some(false),
            ..Default::default()
        }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);

    // Now re-enable — must trigger a catch-up.
    let resp = patch_index_config_handler(
        State(Arc::clone(&state)),
        Path("comp-kg-on".to_string()),
        Json(PatchIndexConfigRequest {
            kg: Some(true),
            ..Default::default()
        }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["components"]["kg"].as_bool(), Some(true));
    assert_eq!(body["components"]["catch_up_started"].as_bool(), Some(true));

    let handle = state
        .registry
        .get(&IndexId::new("comp-kg-on"))
        .expect("handle");
    assert!(!handle.skip_kg);
    poll_stages(&handle, |s| s.graph.status == StageStatus::Ready).await;
}

/// `PATCH { vector: true }` on an index with vector already disabled must
/// spawn a background embed catch-up (issue #923's `embed_deferred_chunks`,
/// generalised) that settles the semantic stage to `Ready` (the bare test
/// indexer has no embedder/store wired, so `embed_deferred_chunks` is a
/// documented no-op that still marks Ready — see `defer_embed.rs` tests).
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn patch_vector_on_spawns_catch_up_and_reaches_ready() {
    let _isolated = IsolatedDataDir::new();
    let state = state_with_index("comp-vector-on");
    let resp = patch_index_config_handler(
        State(Arc::clone(&state)),
        Path("comp-vector-on".to_string()),
        Json(PatchIndexConfigRequest {
            vector: Some(false),
            ..Default::default()
        }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);

    let resp = patch_index_config_handler(
        State(Arc::clone(&state)),
        Path("comp-vector-on".to_string()),
        Json(PatchIndexConfigRequest {
            vector: Some(true),
            ..Default::default()
        }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["components"]["catch_up_started"].as_bool(), Some(true));

    let handle = state
        .registry
        .get(&IndexId::new("comp-vector-on"))
        .expect("handle");
    assert!(!handle.skip_vector);
    poll_stages(&handle, |s| s.semantic.status == StageStatus::Ready).await;
}

/// A component turn-on request that arrives while THIS index's per-index
/// mutual-exclusion permit is already held (a concurrent reindex or another
/// catch-up on the SAME index) must return `409 Conflict` with NO state
/// mutated — not queue behind it.
///
/// Why: pins the #2984 Phase 1 CRITICAL finding 2 fix — the conflict guard
/// must be scoped per `IndexId` (`service::reindex::index_semaphore`), not
/// the process-global `background_reindex_semaphore` the pre-fix code
/// mistakenly used (which both produced false 409s for unrelated indexes AND
/// missed a real same-index conflict against the separate interactive
/// reindex semaphore — see `patch_component_toggle_succeeds_for_different_index_while_global_background_semaphore_busy`
/// for the false-409 half of the regression).
/// What: acquires the per-index (1-permit) semaphore for `comp-409` directly
/// in the test — the same semaphore `runner::run_reindex` and
/// `defer_embed::spawn_deferred_embed_pass` hold for the duration of their
/// work on this index — then PATCHes `kg: true` and asserts 409, a
/// correctly-scoped error message, and that `skip_kg` is untouched.
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn patch_component_turn_on_409s_when_same_index_catch_up_in_progress() {
    let _isolated = IsolatedDataDir::new();
    let state = state_with_index("comp-409");
    // Disable KG first so the next PATCH is a turn-on (needs the permit).
    let resp = patch_index_config_handler(
        State(Arc::clone(&state)),
        Path("comp-409".to_string()),
        Json(PatchIndexConfigRequest {
            kg: Some(false),
            ..Default::default()
        }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);

    // Hold THIS index's per-index permit for the duration of this block —
    // simulates a concurrent reindex or another in-flight catch-up on the
    // SAME index.
    let index_id = IndexId::new("comp-409");
    let _permit = crate::service::reindex::index_semaphore(&index_id)
        .try_acquire_owned()
        .expect("semaphore must be free at test start");

    let resp = patch_index_config_handler(
        State(Arc::clone(&state)),
        Path("comp-409".to_string()),
        Json(PatchIndexConfigRequest {
            kg: Some(true),
            ..Default::default()
        }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    let body = body_json(resp).await;
    assert!(
        body["error"]
            .as_str()
            .unwrap_or_default()
            .contains("comp-409"),
        "409 error message must correctly name THIS index (finding 2's \
         message-scope bug), got: {body}"
    );

    let handle = state.registry.get(&index_id).expect("handle");
    assert!(
        handle.skip_kg,
        "409 must leave skip_kg untouched (still disabled)"
    );
    let stages = handle.stages.read().await;
    assert_eq!(
        stages.graph.status,
        StageStatus::Skipped,
        "409 must leave the graph stage untouched"
    );
}

/// A component turn-on request for one index must NEVER be blocked by an
/// unrelated index's background embed holding the process-global
/// `background_reindex_semaphore` — the pre-fix false-409 half of CRITICAL
/// finding 2.
///
/// Why: before the fix, the toggle handler `try_acquire`d the SAME
/// process-wide 1-permit semaphore shared by every index's background
/// reindex/deferred-embed tasks, so ANY unrelated index's routine background
/// embed (routine — `defer_embed` defaults `true`) produced a false `409`
/// here.
/// What: holds the global `background_reindex_semaphore` permit (simulating
/// an unrelated index's background embed in flight elsewhere), then PATCHes
/// `kg: true` on a DIFFERENT index and asserts it still succeeds.
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn patch_component_toggle_succeeds_for_different_index_while_global_background_semaphore_busy(
) {
    let _isolated = IsolatedDataDir::new();
    let state = state_with_index("comp-other-index");
    // Disable KG first so the next PATCH is a turn-on (needs a permit).
    let resp = patch_index_config_handler(
        State(Arc::clone(&state)),
        Path("comp-other-index".to_string()),
        Json(PatchIndexConfigRequest {
            kg: Some(false),
            ..Default::default()
        }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);

    // Hold the process-global background-reindex semaphore — simulates an
    // unrelated index's deferred-embed pass in flight elsewhere in the fleet.
    let _global_permit = crate::service::reindex::background_reindex_semaphore()
        .try_acquire()
        .expect("global semaphore must be free at test start");

    let resp = patch_index_config_handler(
        State(Arc::clone(&state)),
        Path("comp-other-index".to_string()),
        Json(PatchIndexConfigRequest {
            kg: Some(true),
            ..Default::default()
        }),
    )
    .await;
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "a toggle on an unrelated index must never be blocked by the \
         global background-reindex semaphore (#2984 Phase 1 CRITICAL finding 2)"
    );
    let body = body_json(resp).await;
    assert_eq!(body["components"]["catch_up_started"].as_bool(), Some(true));
}

/// CRITICAL regression (#2984 Phase 1 finding 1): a runtime KG re-enable on
/// an index whose `CodeIndexer` was constructed via
/// `build_indexer_from_entry` with `skip_kg: true` — the REAL production
/// wiring path, NOT `CodeIndexer::new` (which defaults `skip_kg` to `false`
/// and would silently hide this exact bug, as every pre-existing test in this
/// module does) — must actually reload the persisted symbol graph, not
/// merely settle the stage to `Ready` while the graph stays empty.
///
/// Why: `apply_component_transition` used to flip `IndexHandle::skip_kg` but
/// never touch `CodeIndexer`'s OWN internal `skip_kg` copy (set once at
/// construction and consulted directly by `load_or_rebuild_symbol_graph`), so
/// `catch_up_symbol_graph` silently no-op'd against a `skip_kg=true` indexer
/// — the stage still reached `Ready`, but `node_count()` stayed `0` forever
/// (until a daemon restart rebuilt the indexer fresh from the now-current
/// `indexes.toml`).
/// What: builds an indexer via `build_indexer_from_entry` with
/// `skip_kg=false`, indexes two files with a real call edge so the redb
/// corpus persists a non-empty graph, drops it, then rebuilds the SAME
/// id/root via `build_indexer_from_entry` with `skip_kg=true` (mirroring
/// `persistence_loader::tests`'s own regression pattern) — reproducing
/// exactly how a `skip_kg=true` index is wired in production. Registers that
/// indexer under a handle, PATCHes `{kg: true}`, polls until the graph stage
/// is `Ready`, and asserts `node_count() > 0` — not merely the stage
/// transition.
/// Test: this IS the test.
#[tokio::test]
#[serial_test::serial]
async fn patch_kg_on_reloads_persisted_graph_for_indexer_built_with_skip_kg_true() {
    use crate::service::persistence::PersistedIndex;
    use crate::service::persistence_loader::build_indexer_from_entry;

    let _isolated = IsolatedDataDir::new();
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let embedder: Arc<dyn crate::core::embed::Embedder> =
        Arc::new(trusty_common::embedder::MockEmbedder::new(8));

    let entry_no_skip = PersistedIndex {
        id: "comp-kg-reload".to_string(),
        root_path: root.clone(),
        colocated: true,
        skip_kg: false,
        ..Default::default()
    };

    // Phase 1: build with skip_kg=false, index real content with a call
    // edge so the redb corpus persists a non-empty graph, then drop
    // (releases the redb file lock).
    {
        let indexer = build_indexer_from_entry(&entry_no_skip, &embedder)
            .await
            .expect("build indexer");
        indexer
            .index_files_batch(&[
                ("src/caller.rs".into(), "fn caller() { callee(); }".into()),
                ("src/callee.rs".into(), "fn callee() {}".into()),
            ])
            .await
            .expect("index batch");
        let graph = indexer.snapshot_symbol_graph().await;
        assert!(graph.node_count() > 0, "sanity: real graph was built");
    }

    // Phase 2: rebuild the SAME id/root via the production skip_kg=true
    // wiring path (mirrors how a real `skip_kg=true` index is loaded).
    let entry_skip = PersistedIndex {
        skip_kg: true,
        ..entry_no_skip
    };
    let indexer = build_indexer_from_entry(&entry_skip, &embedder)
        .await
        .expect("build indexer with skip_kg=true");
    // Sanity: the loader must have honoured skip_kg at construction — the
    // graph is empty even though the corpus has a persisted one.
    assert_eq!(indexer.snapshot_symbol_graph().await.node_count(), 0);

    let mut handle = IndexHandle::bare(
        IndexId::new("comp-kg-reload"),
        Arc::new(RwLock::new(indexer)),
        root,
    );
    handle.skip_kg = true;
    let registry = IndexRegistry::new();
    registry.register(handle);
    let state = Arc::new(SearchAppState::new(registry));

    let resp = patch_index_config_handler(
        State(Arc::clone(&state)),
        Path("comp-kg-reload".to_string()),
        Json(PatchIndexConfigRequest {
            kg: Some(true),
            ..Default::default()
        }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);

    let handle = state
        .registry
        .get(&IndexId::new("comp-kg-reload"))
        .expect("handle");
    poll_stages(&handle, |s| s.graph.status == StageStatus::Ready).await;

    let graph = handle.indexer.read().await.snapshot_symbol_graph().await;
    assert!(
        graph.node_count() > 0,
        "CRITICAL regression (#2984 Phase 1 finding 1): catch-up must \
         actually reload the persisted graph, not merely settle the stage \
         to Ready while the graph stays empty"
    );
}

/// A hygiene-only PATCH (no `kg`/`vector` fields) must never touch component
/// state or require the semaphore — the no-op resolution
/// (`resolve_component_toggle_no_op_when_fields_absent`, unit-tested in
/// `components.rs`) must hold at the handler level too.
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn patch_hygiene_only_leaves_components_untouched() {
    let _isolated = IsolatedDataDir::new();
    let state = state_with_index("comp-hygiene-only");
    let resp = patch_index_config_handler(
        State(Arc::clone(&state)),
        Path("comp-hygiene-only".to_string()),
        Json(PatchIndexConfigRequest {
            include_docs: Some(false),
            ..Default::default()
        }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["components"]["kg"].as_bool(), Some(true));
    assert_eq!(body["components"]["vector"].as_bool(), Some(true));
    assert_eq!(
        body["components"]["catch_up_started"].as_bool(),
        Some(false)
    );
}

/// The component flags persist to `indexes.toml` across the PATCH — a
/// subsequent load of the registry file must reflect `skip_kg`/`skip_vector`.
///
/// Why: mirrors `tests_index_config::patch_persists_to_toml`'s intent, scoped
/// to the new component fields folded into the same write.
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn patch_component_toggle_persists_to_toml() {
    let _isolated = IsolatedDataDir::new();
    let state = state_with_index("comp-persist");
    let resp = patch_index_config_handler(
        State(Arc::clone(&state)),
        Path("comp-persist".to_string()),
        Json(PatchIndexConfigRequest {
            kg: Some(false),
            vector: Some(false),
            ..Default::default()
        }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);

    let entries = crate::service::persistence::load_index_registry().expect("load registry");
    let entry = entries
        .into_iter()
        .find(|e| e.id == "comp-persist")
        .expect("persisted entry");
    assert!(entry.skip_kg, "skip_kg must persist");
    assert!(entry.skip_vector, "skip_vector must persist");
}

/// Issue #2984 Phase 1: `GET /health` must surface per-component aggregate
/// counts — how many registered indexes have KG disabled, how many have
/// vector disabled, and how many have an enabled component currently
/// `InProgress` (a runtime catch-up or an ordinary in-flight reindex).
///
/// Why: pins the health-handler scan added alongside the runtime component
/// toggle so operators can see per-component suppression across the fleet
/// without polling every index's `GET /indexes/:id/status` individually.
/// What: registers three handles — one with `skip_kg=true`, one with
/// `skip_vector=true`, one with an enabled-but-`InProgress` graph stage
/// (simulating a catch-up in flight) — and asserts each counter reflects
/// exactly the handles that match.
/// Test: this test.
#[tokio::test]
async fn health_surfaces_component_disabled_counts() {
    use crate::core::registry::{IndexRegistry, StageState, StageStatus};

    let registry = IndexRegistry::new();

    let mut kg_off = IndexHandle::bare(
        IndexId::new("health-kg-off"),
        Arc::new(RwLock::new(CodeIndexer::new(
            "health-kg-off",
            "/tmp/health-kg-off",
        ))),
        "/tmp/health-kg-off".into(),
    );
    kg_off.skip_kg = true;
    registry.register(kg_off);

    let mut vector_off = IndexHandle::bare(
        IndexId::new("health-vector-off"),
        Arc::new(RwLock::new(CodeIndexer::new(
            "health-vector-off",
            "/tmp/health-vector-off",
        ))),
        "/tmp/health-vector-off".into(),
    );
    vector_off.skip_vector = true;
    registry.register(vector_off);

    let catching_up = IndexHandle::bare(
        IndexId::new("health-catching-up"),
        Arc::new(RwLock::new(CodeIndexer::new(
            "health-catching-up",
            "/tmp/health-catching-up",
        ))),
        "/tmp/health-catching-up".into(),
    );
    catching_up.stages.write().await.graph = StageState {
        status: StageStatus::InProgress,
        ..Default::default()
    };
    registry.register(catching_up);

    let state = Arc::new(SearchAppState::new(registry));
    let Json(resp) = super::health::health_handler(State(state)).await;
    assert_eq!(resp.indexes_kg_disabled, 1, "only health-kg-off counts");
    assert_eq!(
        resp.indexes_vector_disabled, 1,
        "only health-vector-off counts"
    );
    assert_eq!(
        resp.indexes_component_catch_up_in_progress, 1,
        "only health-catching-up (enabled + InProgress) counts"
    );
}
