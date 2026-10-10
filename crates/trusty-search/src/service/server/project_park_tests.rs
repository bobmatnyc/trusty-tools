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
