//! `search.project.ensure` on a parked index (#11417).
//!
//! Why: ensure answered `created: false` for a cold-parked row and left it
//! parked, so `review_pr`, which lists live indexes only, found no index.
//! Test: this module.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use trusty_common::uds::server::{RpcResponse, RpcRouter};

use crate::core::registry::{IndexId, IndexRegistry};
use crate::core::Embedder;
use crate::service::persistence::PersistedIndex;
use crate::service::rpc::project_ensure::{self, METHOD_PROJECT_ENSURE};
use crate::service::rpc::{project_map, reads};
use crate::service::server::SearchAppState;

const KEY: &str = "acme/widget-11417";

struct Fixture {
    tmp: tempfile::TempDir,
    _root_guard: tempfile::TempDir,
    root: PathBuf,
    id: String,
    state: Arc<SearchAppState>,
    router: RpcRouter,
}

impl Fixture {
    async fn call(&self, method: &str, params: serde_json::Value) -> RpcResponse {
        let frame = serde_json::to_vec(&serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": method, "params": params,
        }))
        .expect("frame");
        self.router.dispatch(&frame).await
    }

    async fn ok(&self, method: &str, params: serde_json::Value) -> serde_json::Value {
        let r = self.call(method, params).await;
        r.result
            .unwrap_or_else(|| panic!("{method} failed: {:?}", r.error))
    }

    async fn ensure(&self) -> RpcResponse {
        let params = serde_json::json!({ "project_key": KEY, "name": "widget", "root": self.root });
        self.call(METHOD_PROJECT_ENSURE, params).await
    }

    fn registry(&self) -> PathBuf {
        self.tmp.path().join("indexes.toml")
    }

    fn row(&self) -> PersistedIndex {
        crate::service::persistence::find_index_registry_entry_at(&self.registry(), &self.id)
            .expect("read indexes.toml")
            .expect("the row is kept")
    }

    fn live(&self) -> bool {
        self.state
            .registry
            .get(&IndexId::new(self.id.clone()))
            .is_some()
    }
}

/// An allowlisted root whose `.git` is `git` (`dir` or `file`), registered in
/// `indexes.toml` and in the cold store only: not resident, no
/// `repo_identity`, no corpus — the #11417 row.
async fn cold_fixture(prefix: &str, git: &str, embedder: bool) -> Fixture {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (root_guard, root) = crate::service::server::test_support::allowlisted_index_root(prefix);
    match git {
        "dir" => std::fs::create_dir_all(root.join(".git")).expect(".git dir"),
        _ => std::fs::write(root.join(".git"), "gitdir: elsewhere\n").expect(".git file"),
    }
    std::fs::write(root.join("a.rs"), "fn a() {}\n").expect("file");
    let id = trusty_common::derive_index_id(&root);
    let row = PersistedIndex {
        colocated: true,
        ..PersistedIndex::new(id.clone(), &root)
    };
    let registry = tmp.path().join("indexes.toml");
    crate::service::persistence::save_index_registry_at(&registry, std::slice::from_ref(&row))
        .expect("seed indexes.toml");
    let state = SearchAppState::new(IndexRegistry::new()).with_registry_path(registry);
    if embedder {
        let embedder: Arc<dyn Embedder> = Arc::new(crate::core::embed::MockEmbedder::new(8));
        state.install_embedder(embedder).await;
    }
    let state = Arc::new(state);
    state.cold_store.register_cold_entries(vec![row]);
    let router = project_ensure::register(RpcRouter::new(), &state);
    let router = project_map::register(reads::register(router, &state), &state);
    Fixture {
        tmp,
        _root_guard: root_guard,
        root,
        id,
        state,
        router,
    }
}

fn listed_ids(listed: &serde_json::Value) -> Vec<String> {
    listed["indexes"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter_map(|r| r["id"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Why: #11417 criterion 1 — a cold-parked main checkout becomes live, records
/// `repo_identity` from `project_key`, and is listed under that identity, which
/// is the lookup `review_pr` runs.
/// Test: this test.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::parallel]
async fn ensure_reactivates_a_cold_parked_root_and_records_its_identity() {
    let fx = cold_fixture("ts-11417-cold-", "dir", true).await;
    assert!(!fx.live(), "the fixture starts cold");
    let out = fx.ensure().await.result.expect("ensure answers");
    assert_eq!(out["index_id"], fx.id.as_str(), "{out}");
    assert_eq!(out["created"], false, "{out}");
    assert_eq!(out["unparked"], true, "{out}");
    assert_eq!(out["reindex_queued"], true, "{out}");
    assert!(fx.live(), "the index is resident after ensure");
    assert_eq!(fx.row().repo_identity.as_deref(), Some(KEY));
    let listed = fx
        .ok(
            reads::METHOD_INDEXES_LIST,
            serde_json::json!({ "details": true, "repo_identity": KEY }),
        )
        .await;
    assert!(listed_ids(&listed).contains(&fx.id), "{listed}");

    // A second call finds it live: nothing to re-activate, nothing queued.
    let again = fx.ensure().await.result.expect("ensure answers");
    assert_eq!(again["unparked"], false, "{again}");
    assert_eq!(again["reindex_queued"], false, "{again}");
}

/// Why: #11417 — a main checkout parked in the project map (a decommission
/// park) is unparked by ensure, so the map no longer refuses it.
/// Test: this test.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::parallel]
async fn ensure_unparks_a_map_parked_main_checkout() {
    let fx = cold_fixture("ts-11417-map-", "dir", true).await;
    let parked = fx
        .ok(
            project_map::METHOD_PROJECT_PARK,
            serde_json::json!({ "index_id": fx.id, "decommission": true }),
        )
        .await;
    assert_eq!(parked["parked"], true, "{parked}");
    assert!(!fx.live());
    let out = fx.ensure().await.result.expect("ensure answers");
    assert_eq!(out["index_id"], fx.id.as_str(), "{out}");
    assert_eq!(out["created"], false, "{out}");
    assert_eq!(out["unparked"], true, "{out}");
    assert!(fx.live(), "reloaded");
    let map = fx
        .ok(project_map::METHOD_PROJECT_MAP, serde_json::json!({}))
        .await;
    let still = map["parked"]
        .as_array()
        .is_some_and(|rows| rows.iter().any(|r| r["index_id"] == fx.id.as_str()));
    assert!(!still, "the park row is cleared: {map}");
}

/// Why: #11417 — a worktree root keeps today's behaviour: found, not
/// re-activated, nothing queued.
/// Test: this test.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::parallel]
async fn ensure_leaves_a_cold_worktree_root_parked() {
    let fx = cold_fixture("ts-11417-wt-", "file", true).await;
    let out = fx.ensure().await.result.expect("ensure answers");
    assert_eq!(out["created"], false, "{out}");
    assert_ne!(out["unparked"], true, "{out}");
    assert_eq!(out["reindex_queued"], false, "{out}");
    assert!(!fx.live(), "a worktree stays cold");
    assert_eq!(fx.row().repo_identity, None);
}

/// Why: #11417 — a missing root keeps today's behaviour: refused by the root
/// gate, and the cold row is not touched.
/// Test: this test.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::parallel]
async fn ensure_on_a_missing_cold_root_is_refused_and_changes_nothing() {
    let fx = cold_fixture("ts-11417-gone-", "dir", true).await;
    std::fs::remove_dir_all(&fx.root).expect("remove root");
    let r = fx.ensure().await;
    assert!(
        r.result.is_none(),
        "a missing root is refused: {:?}",
        r.result
    );
    assert!(!fx.live());
    assert_eq!(fx.row().repo_identity, None);
    assert!(Path::new(&fx.row().root_path).ends_with(fx.root.file_name().expect("name")));
}

/// Why: #11417 Fail-Open Check — an un-park that cannot load the index is an
/// error, never `created: false` success.
/// Test: this test.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::parallel]
async fn ensure_reports_an_unpark_that_cannot_load_as_an_error() {
    let fx = cold_fixture("ts-11417-fail-", "dir", false).await;
    let r = fx.ensure().await;
    assert!(
        r.result.is_none(),
        "an un-park with no embedder must fail: {:?}",
        r.result
    );
    assert!(r.error.is_some());
    assert!(!fx.live());
}
