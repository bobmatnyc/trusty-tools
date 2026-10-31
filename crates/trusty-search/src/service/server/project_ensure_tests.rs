//! `search.project.ensure` (#10531): create, idempotence, the overlap refusal.
//!
//! Why: the call is tm's only way to give a registered project its index, so
//! the repeat call must change nothing and a nested root must register nothing.
//! Test: this module.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use trusty_common::uds::server::{RpcError, RpcResponse, RpcRouter};

use crate::core::registry::{IndexId, IndexRegistry};
use crate::core::Embedder;
use crate::service::rpc::error::CODE_CONFLICT;
use crate::service::rpc::project_ensure::{self, METHOD_PROJECT_ENSURE};
use crate::service::server::SearchAppState;

async fn dispatch(rpc: &RpcRouter, method: &str, params: serde_json::Value) -> RpcResponse {
    let frame = serde_json::to_vec(&serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": method, "params": params,
    }))
    .expect("frame");
    rpc.dispatch(&frame).await
}

async fn rpc_ok(rpc: &RpcRouter, method: &str, params: serde_json::Value) -> serde_json::Value {
    let r = dispatch(rpc, method, params).await;
    r.result
        .unwrap_or_else(|| panic!("{method} failed: {:?}", r.error))
}

async fn rpc_err(rpc: &RpcRouter, method: &str, params: serde_json::Value) -> RpcError {
    let r = dispatch(rpc, method, params).await;
    r.error
        .unwrap_or_else(|| panic!("{method} must be refused: {:?}", r.result))
}

struct Fixture {
    _tmp: tempfile::TempDir,
    state: Arc<SearchAppState>,
    router: RpcRouter,
}

async fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = SearchAppState::new(IndexRegistry::new())
        .with_registry_path(tmp.path().join("indexes.toml"));
    let embedder: Arc<dyn Embedder> = Arc::new(crate::core::embed::MockEmbedder::new(8));
    state.install_embedder(embedder).await;
    let state = Arc::new(state);
    let router = project_ensure::register(RpcRouter::new(), &state);
    Fixture {
        _tmp: tmp,
        state,
        router,
    }
}

fn params(key: &str, name: &str, root: &Path) -> serde_json::Value {
    serde_json::json!({ "project_key": key, "name": name, "root": root })
}

/// A fresh allowlisted directory and a child of it named `child`.
fn root_with_child(prefix: &str, child: &str) -> (tempfile::TempDir, PathBuf, PathBuf) {
    let (guard, root) = crate::service::server::test_support::allowlisted_index_root(prefix);
    let inner = root.join(child);
    std::fs::create_dir_all(&inner).expect("child dir");
    std::fs::write(inner.join("a.rs"), "fn a() {}\n").expect("file");
    (guard, root, inner)
}

fn pins(fx: &Fixture) -> Vec<String> {
    let ctx = crate::service::project_map_state::MapContext::capture(&fx.state).expect("ctx");
    let map = crate::core::project_map_io::load(&ctx.map_path).expect("map read");
    map.map(|m| m.projects.into_iter().map(|p| p.index_id).collect())
        .unwrap_or_default()
}

/// Why: acceptance 1 — register gives one pinned index with the naming id.
/// Test: this test.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::parallel]
async fn ensure_creates_pins_and_queues_one_reindex() {
    let fx = fixture().await;
    let (_g, root, _) = root_with_child("ts-10531-new-", "src");
    let out = rpc_ok(&fx.router, METHOD_PROJECT_ENSURE, params("o/r", "r", &root)).await;
    let id = out["index_id"].as_str().expect("id").to_string();
    assert_eq!(out["created"], true, "{out}");
    assert_eq!(out["reindex_queued"], true, "{out}");
    assert_eq!(out["profile_applied"], false);
    assert_eq!(id, trusty_common::derive_index_id(&root), "{out}");
    assert!(fx.state.registry.get(&IndexId::new(id.clone())).is_some());
    assert_eq!(pins(&fx), vec![id], "the group is pinned");
}

/// Why: acceptance 2 — a second call returns the same id and queues nothing.
/// Test: this test.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::parallel]
async fn a_second_ensure_returns_the_same_id_and_queues_no_reindex() {
    let fx = fixture().await;
    let (_g, root, _) = root_with_child("ts-10531-twice-", "src");
    let first = rpc_ok(&fx.router, METHOD_PROJECT_ENSURE, params("o/r", "r", &root)).await;
    let second = rpc_ok(
        &fx.router,
        METHOD_PROJECT_ENSURE,
        params("o/r", "renamed", &root),
    )
    .await;
    assert_eq!(second["index_id"], first["index_id"], "{second}");
    assert_eq!(second["created"], false, "{second}");
    assert_eq!(second["reindex_queued"], false, "{second}");
    assert!(second.get("reindex").is_none(), "{second}");
    assert_eq!(fx.state.registry.list().len(), 1, "no second index");
    assert_eq!(pins(&fx).len(), 1);
}

/// Why: #6866 criterion 1 — `cto-assistant` inside `cto`.
/// Test: this test.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::parallel]
async fn a_root_inside_a_registered_root_is_refused_with_its_id() {
    let fx = fixture().await;
    let (_g, outer, inner) = root_with_child("ts-10531-inside-", "assistant");
    let first = rpc_ok(
        &fx.router,
        METHOD_PROJECT_ENSURE,
        params("o/cto", "cto", &outer),
    )
    .await;
    let err = rpc_err(
        &fx.router,
        METHOD_PROJECT_ENSURE,
        params("o/a", "assistant", &inner),
    )
    .await;
    assert_refused_naming(&err, "inside_existing_root", &first["index_id"]);
    assert_eq!(fx.state.registry.list().len(), 1, "nothing registered");
}

/// Why: the other direction — an ancestor would swallow a registered root.
/// Test: this test.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::parallel]
async fn a_root_enclosing_a_registered_root_is_refused_with_its_id() {
    let fx = fixture().await;
    let (_g, outer, inner) = root_with_child("ts-10531-encl-", "assistant");
    let first = rpc_ok(
        &fx.router,
        METHOD_PROJECT_ENSURE,
        params("o/a", "a", &inner),
    )
    .await;
    let err = rpc_err(
        &fx.router,
        METHOD_PROJECT_ENSURE,
        params("o/cto", "cto", &outer),
    )
    .await;
    assert_refused_naming(&err, "encloses_existing_root", &first["index_id"]);
    assert_eq!(fx.state.registry.list().len(), 1, "nothing registered");
}

fn assert_refused_naming(err: &RpcError, overlap: &str, id: &serde_json::Value) {
    assert_eq!(err.code, CODE_CONFLICT, "{err:?}");
    let data = err.data.as_ref().expect("the refusal carries data");
    assert_eq!(data["error"], "project_root_overlap", "{data}");
    assert_eq!(data["overlap"], overlap, "{data}");
    assert_eq!(&data["overlapping_index_id"], id, "{data}");
}

/// Why: two projects whose roots share a basename must not share an id.
/// Test: this test.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::parallel]
async fn a_taken_basename_falls_back_to_the_registry_id() {
    let fx = fixture().await;
    let (_g1, _, a) = root_with_child("ts-10531-a-", "app");
    let (_g2, _, b) = root_with_child("ts-10531-b-", "app");
    let first = rpc_ok(
        &fx.router,
        METHOD_PROJECT_ENSURE,
        params("o/app", "app", &a),
    )
    .await;
    let second = rpc_ok(
        &fx.router,
        METHOD_PROJECT_ENSURE,
        params("p/app", "app", &b),
    )
    .await;
    assert_eq!(first["index_id"], "app");
    assert_eq!(
        second["index_id"],
        trusty_common::derive_registry_index_id("app", "p/app"),
        "{second}"
    );
}

/// Why: a malformed request registers nothing and says why.
/// Test: this test.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::parallel]
async fn an_unindexable_or_empty_request_is_refused() {
    let fx = fixture().await;
    let (_g, root, _) = root_with_child("ts-10531-bad-", "src");
    let empty = dispatch(&fx.router, METHOD_PROJECT_ENSURE, params("", "r", &root)).await;
    assert!(empty.error.is_some(), "an empty key is refused");
    let relative = rpc_err(
        &fx.router,
        METHOD_PROJECT_ENSURE,
        params("o/r", "r", Path::new("relative/dir")),
    )
    .await;
    assert!(relative.message.contains("absolute"), "{relative:?}");
    let missing = dispatch(
        &fx.router,
        METHOD_PROJECT_ENSURE,
        serde_json::json!({ "project_key": "o/r", "name": "r", "root": root, "bogus": 1 }),
    )
    .await;
    assert!(missing.error.is_some(), "unknown fields are refused");
    assert!(fx.state.registry.list().is_empty(), "nothing registered");
}
