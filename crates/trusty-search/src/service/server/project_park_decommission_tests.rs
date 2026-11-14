//! A decommission park over the socket: a main checkout unloads, its watcher
//! stops and its corpus stays (#10989).
//!
//! Why: tm decommissions a project's own index, which a plain park refuses
//! (a main checkout). These cases prove the decommission park against a real
//! loaded, watched index: the watcher stops, the corpus and registration stay,
//! and a park whose unload is refused leaves nothing parked.
//! Test: this module.

use std::path::PathBuf;
use std::sync::Arc;

use trusty_common::uds::server::{RpcResponse, RpcRouter};

use super::state::SearchAppState;
use crate::core::registry::{IndexId, IndexRegistry};
use crate::core::Embedder;
use crate::service::persistence::PersistedIndex;
use crate::service::rpc::error::CODE_CONFLICT;
use crate::service::rpc::project_map;

const REPO: &str = "bobmatnyc/decom";

/// A resident, watched main checkout with one indexed file on disk.
struct Fixture {
    tmp: tempfile::TempDir,
    _root_guard: tempfile::TempDir,
    root: PathBuf,
    id: &'static str,
    state: Arc<SearchAppState>,
    router: RpcRouter,
}

impl Fixture {
    fn corpus(&self) -> PathBuf {
        self.root.join(".trusty-search").join("index.redb")
    }

    fn map_path(&self) -> PathBuf {
        crate::core::project_map_io::map_path_beside(&self.tmp.path().join("indexes.toml"))
    }

    async fn call(&self, params: serde_json::Value) -> RpcResponse {
        self.call_method(project_map::METHOD_PROJECT_PARK, params)
            .await
    }

    async fn call_method(&self, method: &str, params: serde_json::Value) -> RpcResponse {
        let frame = serde_json::to_vec(&serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": method, "params": params,
        }))
        .expect("frame");
        self.router.dispatch(&frame).await
    }

    async fn watching(&self) -> bool {
        self.state
            .watcher_manager
            .is_watching(&IndexId::new(self.id))
            .await
    }
}

/// Register, load and watch a main checkout under `id` (ids are per test:
/// the reindex claim slot is process-global, #10307).
async fn fixture(id: &'static str) -> Fixture {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (root_guard, root) = super::test_support::allowlisted_index_root("ts-10989-decom-");
    std::fs::create_dir_all(root.join(".git")).expect("a main checkout's .git directory");
    let row = PersistedIndex {
        colocated: true,
        repo_identity: Some(REPO.to_string()),
        ..PersistedIndex::new(id, &root)
    };
    let registry = tmp.path().join("indexes.toml");
    crate::service::persistence::save_index_registry_at(&registry, std::slice::from_ref(&row))
        .expect("seed indexes.toml");
    let state = SearchAppState::new(IndexRegistry::new()).with_registry_path(registry);
    let embedder: Arc<dyn Embedder> = Arc::new(crate::core::embed::MockEmbedder::new(8));
    state.install_embedder(embedder).await;
    let state = Arc::new(state);
    state.cold_store.register_cold_entries(vec![row]);
    let req = serde_json::from_value(serde_json::json!({
        "path": "src/a.rs", "content": "fn decommission() -> bool { true }",
    }))
    .expect("IndexFileRequest");
    super::files::index_file_report(&state, id, req)
        .await
        .expect("the write loads the main checkout");
    let handle = state.registry.get(&IndexId::new(id)).expect("resident");
    state.watcher_manager.spawn_for_index(&handle).await;
    let router = project_map::register(RpcRouter::new(), &state);
    let router = crate::service::rpc::project::register(router, &state);
    Fixture {
        tmp,
        _root_guard: root_guard,
        root,
        id,
        state,
        router,
    }
}

/// Why: acceptance 1 of #10989 — a main checkout parks for a decommission:
/// unloaded, unwatched, corpus and registration kept. A plain park of the
/// same index is still refused.
/// Test: this test. Fails if the watcher still runs after the park or the
/// corpus is removed.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::parallel]
async fn a_decommission_park_unloads_a_main_checkout_stops_its_watcher_and_keeps_its_corpus() {
    let fx = fixture("decom-main-10989").await;
    let id = IndexId::new(fx.id);
    assert!(fx.corpus().exists(), "premise: the fixture wrote a corpus");
    assert!(fx.watching().await, "premise: the main checkout is watched");

    let plain = fx.call(serde_json::json!({ "index_id": fx.id })).await;
    let err = plain
        .error
        .expect("a plain park still refuses a main checkout");
    assert_eq!(err.code, CODE_CONFLICT);
    assert_eq!(err.data.expect("data")["error"], "park_refused");

    let parked = fx
        .call(serde_json::json!({ "index_id": fx.id, "decommission": true }))
        .await
        .result
        .expect("a decommission park of a main checkout succeeds");
    assert_eq!(parked["parked"], true, "{parked}");
    assert_eq!(parked["reason"], "decommissioned", "{parked}");
    assert_eq!(parked["unloaded"], true, "{parked}");
    assert!(fx.state.registry.get(&id).is_none(), "unloaded");
    assert!(!fx.state.cold_store.contains(&id), "never lazy-loaded");
    assert!(!fx.watching().await, "the watcher stopped");
    assert!(fx.corpus().exists(), "the corpus is kept");
    let rows =
        crate::service::persistence::load_index_registry_at(&fx.tmp.path().join("indexes.toml"))
            .expect("rows");
    assert!(
        rows.iter().any(|e| e.id == fx.id),
        "the registration is kept"
    );
}

/// Why: the unload path stops a watch the index is no longer resident for
/// (#10989), so "parked" always means "unwatched".
/// What: the handle leaves the registry with its watch still running, then a
/// decommission park takes the non-resident branch of `detach_parked`.
/// Test: this test. Fails if `detach_parked` stops the watcher only for a
/// resident index.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::parallel]
async fn a_decommission_park_stops_a_watch_left_on_a_non_resident_index() {
    let fx = fixture("decom-cold-10989").await;
    let id = IndexId::new(fx.id);
    assert!(fx.state.registry.unregister(&id), "premise: was resident");
    assert!(
        fx.watching().await,
        "premise: the watch outlived the handle"
    );

    let parked = fx
        .call(serde_json::json!({ "index_id": fx.id, "decommission": true }))
        .await
        .result
        .expect("parked");
    assert_eq!(parked["unloaded"], true, "{parked}");
    assert!(!fx.watching().await, "the stale watch stopped");
    assert!(fx.corpus().exists(), "the corpus is kept");
}

/// Why: Fail-Open Check (#10989) — a decommission park whose unload is
/// refused (a reindex runs) must not answer parked while the index stays
/// loaded, and must leave it resolvable so a retry finds it.
/// Test: this test. Fails if the rollback arm in `park_report` is removed.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::parallel]
async fn a_decommission_park_whose_unload_is_refused_leaves_the_index_unparked() {
    let fx = fixture("decom-busy-10989").await;
    let id = IndexId::new(fx.id);
    let running = Arc::new(crate::service::reindex::ReindexProgress::new());
    running
        .status
        .store(crate::service::reindex::ReindexStatus::Running);
    fx.state.reindex_progress.insert(id.clone(), running);

    let err = fx
        .call(serde_json::json!({ "index_id": fx.id, "decommission": true }))
        .await
        .error
        .expect("a refused unload fails the decommission park");
    assert_eq!(err.code, CODE_CONFLICT);
    let data = err.data.expect("data");
    assert_eq!(data["error"], "park_unload_refused", "{data}");
    assert_eq!(data["rolled_back"], true, "{data}");
    assert!(fx.state.registry.get(&id).is_some(), "still resident");
    let map = crate::core::project_map_io::load(&fx.map_path())
        .expect("map readable")
        .expect("map exists");
    assert!(!map.is_parked(fx.id), "no parked row is left behind");
    assert_eq!(
        map.pin_for(REPO).map(|p| p.index_id.as_str()),
        Some(fx.id),
        "the dropped pin is restored"
    );
}

/// Why: #10989 HIGH finding — a project started again after a decommission
/// must get its index back: tm unparks it on spawn, and the unpark must
/// reload the index, restart the watcher the park stopped, and leave the
/// project resolvable.
/// Test: this test. Fails if `reindex_after_unpark` does not restart the
/// watcher (the index would stay unwatched until its first query).
#[tokio::test(flavor = "multi_thread")]
#[serial_test::parallel]
async fn a_decommission_park_then_unpark_reloads_the_index_and_restarts_its_watcher() {
    let fx = fixture("decom-unpark-10989").await;
    let id = IndexId::new(fx.id);
    let parked = fx
        .call(serde_json::json!({ "index_id": fx.id, "decommission": true }))
        .await
        .result
        .expect("parked");
    assert_eq!(parked["unloaded"], true, "{parked}");
    assert!(
        !fx.watching().await,
        "premise: the park stopped the watcher"
    );

    let unparked = fx
        .call_method(
            project_map::METHOD_PROJECT_UNPARK,
            serde_json::json!({ "index_id": fx.id }),
        )
        .await
        .result
        .expect("the unpark succeeds");

    assert!(fx.watching().await, "the unpark restarted the watcher");
    assert_eq!(unparked["unparked"], true, "{unparked}");
    assert_eq!(unparked["watching"], true, "{unparked}");
    assert!(
        fx.state.registry.get(&id).is_some(),
        "the index is reloaded"
    );
    let map = crate::core::project_map_io::load(&fx.map_path())
        .expect("map readable")
        .expect("map exists");
    assert!(!map.is_parked(fx.id), "the parked row is gone");
    let resolved = fx
        .call_method(
            crate::service::rpc::project::METHOD_PROJECT_RESOLVE,
            serde_json::json!({ "project": REPO }),
        )
        .await
        .result
        .expect("the project resolves again");
    assert_eq!(resolved["index_id"], fx.id, "{resolved}");
    fx.state.watcher_manager.stop_for_index(&id).await;
}
