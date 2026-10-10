//! Parking over the socket: the map method, park, unpark and what a parked
//! index answers (#9169 PR-B1).
//!
//! Why: the rules are pinned in `project_map_state_tests.rs`; these cases
//! prove the serving side against a real loaded index — the corpus survives,
//! the index unloads and reloads, a direct search is refused with the
//! canonical id, and the listing still shows it.
//! Test: this module.

use std::path::PathBuf;
use std::sync::Arc;

use trusty_common::uds::server::{RpcResponse, RpcRouter};

use super::state::SearchAppState;
use crate::core::registry::{IndexId, IndexRegistry};
use crate::core::Embedder;
use crate::service::persistence::PersistedIndex;
use crate::service::rpc::error::CODE_CONFLICT;
use crate::service::rpc::{project_map, queries, reads};

const REPO: &str = "bobmatnyc/apex";
const MAIN: &str = "apex-main";
const WT: &str = "apex-wt";

struct Fixture {
    tmp: tempfile::TempDir,
    _wt_guard: tempfile::TempDir,
    wt: PathBuf,
    state: Arc<SearchAppState>,
    router: RpcRouter,
}

impl Fixture {
    fn registry(&self) -> PathBuf {
        self.tmp.path().join("indexes.toml")
    }

    fn corpus(&self) -> PathBuf {
        self.wt.join(".trusty-search").join("index.redb")
    }

    async fn call(&self, method: &str, params: serde_json::Value) -> RpcResponse {
        let frame = serde_json::to_vec(&serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": method, "params": params,
        }))
        .expect("frame");
        self.router.dispatch(&frame).await
    }

    async fn ok(&self, method: &str, params: serde_json::Value) -> serde_json::Value {
        let response = self.call(method, params).await;
        response
            .result
            .unwrap_or_else(|| panic!("{method} failed: {:?}", response.error))
    }
}

/// A main checkout (unloaded) and a linked worktree of one repo, the worktree
/// loaded with one indexed file so its colocated corpus exists on disk.
async fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().expect("tempdir");
    let main = tmp.path().join("apex");
    std::fs::create_dir_all(main.join(".git")).expect("main checkout");
    let (wt_guard, wt) = super::test_support::allowlisted_index_root("ts-9169-park-");
    std::fs::write(wt.join(".git"), "gitdir: elsewhere\n").expect("linked .git");
    let row = |id: &str, root: &std::path::Path| PersistedIndex {
        colocated: true,
        repo_identity: Some(REPO.to_string()),
        ..PersistedIndex::new(id, root)
    };
    let wt_row = row(WT, &wt);
    let registry = tmp.path().join("indexes.toml");
    crate::service::persistence::save_index_registry_at(
        &registry,
        &[row(MAIN, &main), wt_row.clone()],
    )
    .expect("seed indexes.toml");

    let state = SearchAppState::new(IndexRegistry::new()).with_registry_path(registry);
    let embedder: Arc<dyn Embedder> = Arc::new(crate::core::embed::MockEmbedder::new(8));
    state.install_embedder(embedder).await;
    let state = Arc::new(state);
    state.cold_store.register_cold_entries(vec![wt_row]);
    let req = serde_json::from_value(serde_json::json!({
        "path": "src/a.rs", "content": "fn authenticate() -> bool { true }",
    }))
    .expect("IndexFileRequest");
    super::files::index_file_report(&state, WT, req)
        .await
        .expect("the write loads the worktree index");
    let router = reads::register(RpcRouter::new(), &state);
    let router = queries::register(router, &state);
    let router = project_map::register(router, &state);
    Fixture {
        tmp,
        _wt_guard: wt_guard,
        wt,
        state,
        router,
    }
}

/// Why: `search.project.map` is tm's only view of the map; park refuses a
/// main checkout with a structured conflict.
/// Test: this test.
#[tokio::test(flavor = "multi_thread")]
async fn the_map_method_lists_pins_and_parks() {
    let fx = fixture().await;
    let map = fx
        .ok(project_map::METHOD_PROJECT_MAP, serde_json::Value::Null)
        .await;
    assert!(
        map["path"]
            .as_str()
            .is_some_and(|p| p.ends_with("project-map.toml")),
        "{map}"
    );
    assert_eq!(map["schema"], 1);
    assert_eq!(map["projects"][0]["index_id"], MAIN, "{map}");
    assert_eq!(map["projects"][0]["pinned_by"], "migration");
    assert_eq!(map["parked"], serde_json::json!([]));

    let err = fx
        .call(
            project_map::METHOD_PROJECT_PARK,
            serde_json::json!({ "index_id": MAIN }),
        )
        .await
        .error
        .expect("a main checkout is refused");
    assert_eq!(err.code, CODE_CONFLICT);
    assert_eq!(err.data.expect("data")["error"], "park_refused");

    fx.ok(
        project_map::METHOD_PROJECT_PARK,
        serde_json::json!({ "index_id": WT }),
    )
    .await;
    let map = fx
        .ok(project_map::METHOD_PROJECT_MAP, serde_json::Value::Null)
        .await;
    assert_eq!(map["parked"][0]["index_id"], WT, "{map}");
    assert_eq!(map["parked"][0]["reason"], "worktree");
}

/// Why: parking keeps the corpus and the registration and unloads the index;
/// unparking reloads it and queues its reindex.
/// Test: this test.
#[tokio::test(flavor = "multi_thread")]
async fn park_and_unpark_keep_the_corpus_and_queue_a_reindex() {
    let fx = fixture().await;
    let id = IndexId::new(WT);
    assert!(fx.corpus().exists(), "the fixture wrote a corpus");

    let parked = fx
        .ok(
            project_map::METHOD_PROJECT_PARK,
            serde_json::json!({ "index_id": WT }),
        )
        .await;
    assert_eq!(parked["parked"], true);
    assert_eq!(parked["reason"], "worktree");
    assert_eq!(parked["canonical_index_id"], MAIN, "{parked}");
    assert_eq!(parked["unloaded"], true, "{parked}");
    assert!(fx.state.registry.get(&id).is_none(), "unloaded");
    assert!(!fx.state.cold_store.contains(&id), "never lazy-loaded");
    assert!(fx.corpus().exists(), "the corpus is kept");
    let rows = crate::service::persistence::load_index_registry_at(&fx.registry()).expect("rows");
    assert!(rows.iter().any(|e| e.id == WT), "the registration is kept");

    let listed = fx
        .ok(
            crate::service::rpc::reads::METHOD_INDEXES_LIST,
            serde_json::json!({}),
        )
        .await;
    let row = listed["parked"]
        .as_array()
        .and_then(|rows| rows.iter().find(|r| r["id"] == WT))
        .unwrap_or_else(|| panic!("a parked index is listed: {listed}"));
    assert_eq!(row["parked"], true);
    assert_eq!(row["canonical_index_id"], MAIN);

    let unparked = fx
        .ok(
            project_map::METHOD_PROJECT_UNPARK,
            serde_json::json!({ "index_id": WT }),
        )
        .await;
    assert_eq!(unparked["unparked"], true);
    assert_eq!(unparked["reindex_queued"], true, "{unparked}");
    assert!(fx.state.registry.get(&id).is_some(), "reloaded");
    assert!(fx.corpus().exists());
}

/// Why: ruling Q4 — a search on a parked id is never redirected and never
/// answers empty hits: it is `-32009` naming the canonical index. A reindex
/// is refused the same way.
/// Test: this test.
#[tokio::test(flavor = "multi_thread")]
async fn a_search_on_a_parked_id_is_a_conflict_naming_the_canonical_index() {
    let fx = fixture().await;
    fx.ok(
        project_map::METHOD_PROJECT_PARK,
        serde_json::json!({ "index_id": WT }),
    )
    .await;
    let response = fx
        .call(
            queries::METHOD_QUERY,
            serde_json::json!({ "index_id": WT, "body": { "text": "authenticate" } }),
        )
        .await;
    let err = response
        .error
        .unwrap_or_else(|| panic!("a parked id must not answer: {:?}", response.result));
    assert_eq!(err.code, CODE_CONFLICT, "{err:?}");
    let data = err.data.expect("the conflict carries data");
    assert_eq!(data["parked"], true);
    assert_eq!(data["canonical_index_id"], MAIN);
    assert_eq!(data["key"], REPO);

    let (status, body) = super::reindex_handlers::reindex_report(&fx.state, WT, None)
        .await
        .expect_err("a parked index is never reindexed");
    assert_eq!(status, axum::http::StatusCode::CONFLICT);
    assert_eq!(body["parked"], true);
}

/// Why: the fan-out must skip a parked index even while it is still loaded
/// (an unload that could not run, or a park written by another process).
/// Test: this test.
#[tokio::test(flavor = "multi_thread")]
async fn the_fan_out_skips_a_parked_index_that_is_still_resident() {
    let fx = fixture().await;
    let query = serde_json::json!({ "query": "authenticate" });
    let before = fx.ok(queries::METHOD_QUERY_ALL, query.clone()).await;
    assert_eq!(before["total_indexes"], 1, "{before}");

    let map_path = crate::core::project_map_io::map_path_beside(&fx.registry());
    crate::core::project_map_io::update(&map_path, |map| {
        let mut map = map.unwrap_or_default();
        map.park(crate::core::project_map::ParkedIndex {
            index_id: WT.to_string(),
            key: REPO.to_string(),
            root_path: fx.wt.clone(),
            reason: crate::core::project_map::ParkReason::Operator,
            parked_unix: 1,
        });
        Ok((Some(map), ()))
    })
    .expect("park in the map only");
    assert!(
        fx.state.registry.get(&IndexId::new(WT)).is_some(),
        "still resident"
    );
    let after = fx.ok(queries::METHOD_QUERY_ALL, query).await;
    assert_eq!(after["total_indexes"], 0, "{after}");
}

const OTHER: &str = "apex-b";

async fn dispatch(router: &RpcRouter, method: &str, params: serde_json::Value) -> RpcResponse {
    let frame = serde_json::to_vec(&serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": method, "params": params,
    }))
    .expect("frame");
    router.dispatch(&frame).await
}

fn map_path(fx: &Fixture) -> PathBuf {
    crate::core::project_map_io::map_path_beside(&fx.registry())
}

/// Park `WT` in the map only, so it stays resident.
fn park_in_map_only(fx: &Fixture) {
    crate::core::project_map_io::update(&map_path(fx), |map| {
        let mut map = map.unwrap_or_default();
        map.park(crate::core::project_map::ParkedIndex {
            index_id: WT.to_string(),
            key: REPO.to_string(),
            root_path: fx.wt.clone(),
            reason: crate::core::project_map::ParkReason::Operator,
            parked_unix: 1,
        });
        Ok((Some(map), ()))
    })
    .expect("park in the map only");
}

/// Why: a pin can outlive its index's ability to answer. A pinned main
/// checkout whose clone moved is orphaned, its group answers with another
/// clone, and parking it must name that clone — never the parked id itself,
/// which would send a client in a loop — and drop the pin.
/// Test: this test.
#[tokio::test(flavor = "multi_thread")]
async fn parking_a_pinned_orphan_names_the_live_winner_and_clears_the_pin() {
    let fx = fixture().await;
    let other = fx.tmp.path().join(OTHER);
    std::fs::create_dir_all(other.join(".git")).expect("second main checkout");
    let mut rows =
        crate::service::persistence::load_index_registry_at(&fx.registry()).expect("rows");
    rows.push(PersistedIndex {
        colocated: true,
        repo_identity: Some(REPO.to_string()),
        ..PersistedIndex::new(OTHER, &other)
    });
    crate::service::persistence::save_index_registry_at(&fx.registry(), &rows).expect("add B");
    fx.ok(
        project_map::METHOD_PROJECT_PIN,
        serde_json::json!({ "index_id": MAIN }),
    )
    .await;
    std::fs::remove_dir_all(fx.tmp.path().join("apex")).expect("move the pinned clone");

    let parked = fx
        .ok(
            project_map::METHOD_PROJECT_PARK,
            serde_json::json!({ "index_id": MAIN }),
        )
        .await;
    let response = fx
        .call(
            queries::METHOD_QUERY,
            serde_json::json!({ "index_id": MAIN, "body": { "text": "authenticate" } }),
        )
        .await;
    let data = response
        .error
        .and_then(|e| e.data)
        .unwrap_or_else(|| panic!("a parked id must not answer: {:?}", response.result));
    assert_eq!(data["canonical_index_id"], OTHER, "{data}");
    assert_eq!(parked["canonical_index_id"], OTHER, "{parked}");
    assert_eq!(parked["reason"], "orphaned");
    assert_eq!(parked["unpinned"], serde_json::json!([REPO]), "{parked}");
    let listed = fx
        .ok(
            crate::service::rpc::reads::METHOD_INDEXES_LIST,
            serde_json::json!({}),
        )
        .await;
    let row = listed["parked"]
        .as_array()
        .and_then(|rows| rows.iter().find(|r| r["id"] == MAIN))
        .unwrap_or_else(|| panic!("listed: {listed}"));
    assert_eq!(row["canonical_index_id"], OTHER);
    let map = crate::core::project_map_io::load(&map_path(&fx))
        .expect("load")
        .expect("map");
    assert!(map.projects.iter().all(|p| p.index_id != MAIN), "{map:?}");
}

/// Why: ids come from the root path, so a park row a delete leaves behind
/// parks the next index registered at that root with no explicit park.
/// Test: this test.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial]
async fn a_deleted_parked_index_recreated_at_its_root_is_served() {
    let data = super::tests_components::IsolatedDataDir::new();
    let (_guard, wt) = super::test_support::allowlisted_index_root("ts-9169-recreate-");
    std::fs::write(wt.join(".git"), "gitdir: elsewhere\n").expect("linked .git");
    let state = SearchAppState::new(IndexRegistry::new());
    let embedder: Arc<dyn Embedder> = Arc::new(crate::core::embed::MockEmbedder::new(8));
    state.install_embedder(embedder).await;
    let state = Arc::new(state);
    let create = || -> super::CreateIndexRequest {
        serde_json::from_value(serde_json::json!({ "id": WT, "root_path": wt }))
            .expect("create request")
    };
    super::create_index_report(&state, create())
        .await
        .map_err(|(status, body)| format!("{status}: {body}"))
        .expect("create");
    let router = project_map::register(RpcRouter::new(), &state);
    let router = queries::register(router, &state);
    let parked = dispatch(
        &router,
        project_map::METHOD_PROJECT_PARK,
        serde_json::json!({ "index_id": WT }),
    )
    .await;
    assert!(parked.error.is_none(), "{:?}", parked.error);
    let params = super::DeleteIndexParams {
        delete_data: false,
        expected_root_path: None,
    };
    super::delete_index_report(&state, WT, params)
        .await
        .map_err(|(status, body)| format!("{status}: {body}"))
        .expect("delete");
    super::create_index_report(&state, create())
        .await
        .map_err(|(status, body)| format!("{status}: {body}"))
        .expect("re-create");

    let response = dispatch(
        &router,
        queries::METHOD_QUERY,
        serde_json::json!({ "index_id": WT, "body": { "text": "authenticate" } }),
    )
    .await;
    assert!(
        response.error.is_none(),
        "#9169: the re-created index was parked by a stale row: {:?}",
        response.error
    );
    let map = crate::core::project_map_io::load(&data.path().join("project-map.toml"))
        .expect("load")
        .expect("map");
    assert!(map.parked.is_empty(), "{map:?}");
}

/// Why: fail closed — a delete that cannot read the map would leave a stale
/// park row, so it refuses and tears nothing down.
/// Test: this test.
#[tokio::test(flavor = "multi_thread")]
async fn a_delete_with_a_corrupt_map_refuses_and_keeps_the_index() {
    let fx = fixture().await;
    std::fs::write(map_path(&fx), "garbage [[").expect("corrupt");
    let params = super::DeleteIndexParams {
        delete_data: false,
        expected_root_path: None,
    };
    let (status, body) = super::delete_index_report(&fx.state, WT, params)
        .await
        .expect_err("#9169: a delete without the map must refuse");
    assert_eq!(status, axum::http::StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(body["error"], "project_map_unreadable", "{body}");
    assert!(fx.state.registry.get(&IndexId::new(WT)).is_some(), "kept");
    assert_eq!(
        std::fs::read_to_string(map_path(&fx)).expect("map"),
        "garbage [["
    );
}

/// Why: the runtime reaper must never deregister a parked index that is
/// still resident, and must reap nothing when it cannot read the map.
/// What: the third pass, with a readable map that parks nothing, proves the
/// fixture really is reapable.
/// Test: this test.
#[tokio::test(flavor = "multi_thread")]
async fn the_reaper_never_reaps_a_parked_index_and_reaps_nothing_without_the_map() {
    let fx = fixture().await;
    let id = IndexId::new(WT);
    park_in_map_only(&fx);
    std::fs::remove_dir_all(&fx.wt).expect("orphan the root");
    assert_eq!(super::tickers::reap_orphans_once(&fx.state).await, 0);
    assert!(
        fx.state.registry.get(&id).is_some(),
        "a parked index is kept"
    );

    std::fs::write(map_path(&fx), "garbage [[").expect("corrupt");
    assert_eq!(super::tickers::reap_orphans_once(&fx.state).await, 0);
    assert!(fx.state.registry.get(&id).is_some(), "no map, no reap");

    std::fs::write(map_path(&fx), "schema = 1\n").expect("empty map");
    assert_eq!(super::tickers::reap_orphans_once(&fx.state).await, 1);
}

/// Why: Fail-Open Check 4 — a park whose unload is refused (a reindex runs)
/// still parks, and says the index is still loaded.
/// Test: this test.
#[tokio::test(flavor = "multi_thread")]
async fn a_park_whose_unload_is_refused_reports_unloaded_false() {
    let fx = fixture().await;
    let id = IndexId::new(WT);
    let running = Arc::new(crate::service::reindex::ReindexProgress::new());
    running
        .status
        .store(crate::service::reindex::ReindexStatus::Running);
    fx.state.reindex_progress.insert(id.clone(), running);
    let parked = fx
        .ok(
            project_map::METHOD_PROJECT_PARK,
            serde_json::json!({ "index_id": WT }),
        )
        .await;
    assert_eq!(parked["unloaded"], false, "{parked}");
    assert_eq!(parked["parked"], true);
    assert!(fx.state.registry.get(&id).is_some(), "still resident");
}

/// Why: Fail-Open Check 4 — an unpark whose reindex cannot be queued clears
/// the row and reports why, never `reindex_queued: true`.
/// Test: this test.
#[tokio::test(flavor = "multi_thread")]
async fn an_unpark_whose_reindex_cannot_queue_reports_the_error() {
    let fx = fixture().await;
    fx.ok(
        project_map::METHOD_PROJECT_PARK,
        serde_json::json!({ "index_id": WT }),
    )
    .await;
    let rows: Vec<_> = crate::service::persistence::load_index_registry_at(&fx.registry())
        .expect("rows")
        .into_iter()
        .filter(|e| e.id != WT)
        .collect();
    crate::service::persistence::save_index_registry_at(&fx.registry(), &rows).expect("drop row");
    let unparked = fx
        .ok(
            project_map::METHOD_PROJECT_UNPARK,
            serde_json::json!({ "index_id": WT }),
        )
        .await;
    assert_eq!(unparked["unparked"], true);
    assert_eq!(unparked["reindex_queued"], false, "{unparked}");
    assert_eq!(unparked["reindex_error"]["status"], 404, "{unparked}");
}

/// Why: Fail-Open Check 4 and the ruling — a request by explicit id is served
/// while the map cannot be read; it is never refused as parked.
/// Test: this test.
#[tokio::test(flavor = "multi_thread")]
async fn a_request_by_id_is_served_while_the_map_is_corrupt() {
    let fx = fixture().await;
    park_in_map_only(&fx);
    std::fs::write(map_path(&fx), "garbage [[").expect("corrupt");
    let response = fx
        .call(
            queries::METHOD_QUERY,
            serde_json::json!({ "index_id": WT, "body": { "text": "authenticate" } }),
        )
        .await;
    assert!(response.error.is_none(), "{:?}", response.error);
}
