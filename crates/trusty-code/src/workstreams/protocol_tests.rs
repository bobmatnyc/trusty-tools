//! Tests for `workstream.activate`/`workstream.deactivate` RPC handlers
//! (DOC-48 §5.1/§6, issue #3294).

use super::*;
use serde_json::json;
use tempfile::TempDir;
use tokio::sync::mpsc;
use trusty_common::mcp::Request;

use crate::workstreams::store::WorkstreamStore;

fn test_ctx() -> ConnectionContext {
    let (tx, _rx) = mpsc::unbounded_channel();
    ConnectionContext::new(tx)
}

/// A fresh `SharedWorkstreamStore` backed by a tempfile path, plus the
/// `TempDir` guard (dropped -> file removed) — mirrors `activation_tests`'
/// own helper.
async fn shared_store() -> (SharedWorkstreamStore, TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("workstreams-test.json");
    let store = WorkstreamStore::load(path).await.expect("load fresh store");
    (std::sync::Arc::new(tokio::sync::Mutex::new(store)), dir)
}

async fn create(store: &SharedWorkstreamStore, name: &str) -> WorkstreamId {
    store.lock().await.create(name).await.expect("create")
}

fn req(method: &str, params: serde_json::Value) -> Request {
    Request {
        jsonrpc: Some("2.0".to_string()),
        id: Some(json!(1)),
        method: method.to_string(),
        params: Some(params),
    }
}

/// Both methods must be reachable through a `Router` built by `register`
/// (proves the wiring, not just the free functions).
#[tokio::test]
async fn register_wires_activate_and_deactivate() {
    let (store, _dir) = shared_store().await;
    let id = create(&store, "t").await;
    let mut router = Router::new();
    register(&mut router, store);

    let resp = router
        .dispatch(
            req("workstream.activate", json!({"id": id.to_string()})),
            &test_ctx(),
        )
        .await;
    assert!(
        resp.error.is_none(),
        "activate should succeed: {:?}",
        resp.error
    );

    let resp = router
        .dispatch(
            req("workstream.deactivate", json!({"id": id.to_string()})),
            &test_ctx(),
        )
        .await;
    assert!(
        resp.error.is_none(),
        "deactivate should succeed: {:?}",
        resp.error
    );
}

/// Activating with no prior active workstream must return `active_id` and a
/// null `prior_id`.
#[tokio::test]
async fn activate_succeeds_with_no_prior_active() {
    let (store, _dir) = shared_store().await;
    let id = create(&store, "t").await;

    let result = activate(&store, json!({"id": id.to_string()}), test_ctx())
        .await
        .expect("activate must succeed");
    assert_eq!(result["active_id"], json!(id));
    assert_eq!(result["prior_id"], serde_json::Value::Null);
}

/// Re-activating the already-active workstream (force omitted, defaults to
/// false) must be idempotent, not an error.
#[tokio::test]
async fn activate_already_active_is_idempotent() {
    let (store, _dir) = shared_store().await;
    let id = create(&store, "t").await;
    activate(&store, json!({"id": id.to_string()}), test_ctx())
        .await
        .expect("first activate");

    let result = activate(&store, json!({"id": id.to_string()}), test_ctx())
        .await
        .expect("re-activate must succeed");
    assert_eq!(result["active_id"], json!(id));
    assert_eq!(result["prior_id"], serde_json::Value::Null);
}

/// Activating a different workstream without `force` while one is active
/// must map to `-32008 active_conflict`, carrying the currently-active id.
#[tokio::test]
async fn activate_without_force_maps_to_active_conflict() {
    let (store, _dir) = shared_store().await;
    let a = create(&store, "a").await;
    let b = create(&store, "b").await;
    activate(&store, json!({"id": a.to_string()}), test_ctx())
        .await
        .expect("activate a");

    let err = activate(&store, json!({"id": b.to_string()}), test_ctx())
        .await
        .expect_err("must conflict");
    assert_eq!(err.code, -32008);
    assert_eq!(err.data.expect("data")["active_id"], json!(a.to_string()));
}

/// `force: true` must switch the active workstream and report the prior id.
#[tokio::test]
async fn activate_with_force_switches() {
    let (store, _dir) = shared_store().await;
    let a = create(&store, "a").await;
    let b = create(&store, "b").await;
    activate(&store, json!({"id": a.to_string()}), test_ctx())
        .await
        .expect("activate a");

    let result = activate(
        &store,
        json!({"id": b.to_string(), "force": true}),
        test_ctx(),
    )
    .await
    .expect("force switch must succeed");
    assert_eq!(result["active_id"], json!(b));
    assert_eq!(result["prior_id"], json!(a));
}

/// Activating an id that names no existing workstream must map to `-32002
/// not_found`.
#[tokio::test]
async fn activate_unknown_id_maps_to_not_found() {
    let (store, _dir) = shared_store().await;
    let unknown = WorkstreamId::new();

    let err = activate(&store, json!({"id": unknown.to_string()}), test_ctx())
        .await
        .expect_err("must not find id");
    assert_eq!(err.code, -32002);
}

/// A malformed UUID string in `id` must map to `-32602 Invalid params`, not
/// panic.
#[tokio::test]
async fn activate_malformed_id_maps_to_invalid_params() {
    let (store, _dir) = shared_store().await;

    let err = activate(&store, json!({"id": "not-a-uuid"}), test_ctx())
        .await
        .expect_err("must reject malformed id");
    assert_eq!(err.code, trusty_common::mcp::error_codes::INVALID_PARAMS);
}

/// Deactivating the currently active workstream must clear the pointer and
/// return `{}`.
#[tokio::test]
async fn deactivate_active_clears_pointer() {
    let (store, _dir) = shared_store().await;
    let id = create(&store, "t").await;
    activate(&store, json!({"id": id.to_string()}), test_ctx())
        .await
        .expect("activate");

    let result = deactivate(&store, json!({"id": id.to_string()}), test_ctx())
        .await
        .expect("deactivate must succeed");
    assert_eq!(result, json!({}));
    assert_eq!(
        store.lock().await.active_workstream_id().await.unwrap(),
        None
    );
}

/// Deactivating an idle (non-active) workstream must be an idempotent
/// success, not an error.
#[tokio::test]
async fn deactivate_idle_is_idempotent_noop() {
    let (store, _dir) = shared_store().await;
    let a = create(&store, "a").await;
    let b = create(&store, "b").await;
    activate(&store, json!({"id": a.to_string()}), test_ctx())
        .await
        .expect("activate a");

    let result = deactivate(&store, json!({"id": b.to_string()}), test_ctx())
        .await
        .expect("deactivate idle must succeed");
    assert_eq!(result, json!({}));
    assert_eq!(
        store.lock().await.active_workstream_id().await.unwrap(),
        Some(a)
    );
}

/// The active pointer must survive a fresh `WorkstreamStore::load` of the
/// same file after going through the RPC layer (AC-1.4), not just the
/// `activation` module's own direct-store test.
#[tokio::test]
async fn activation_persists_across_store_reload_through_rpc() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("workstreams-test.json");
    let store = WorkstreamStore::load(&path)
        .await
        .expect("load fresh store");
    let shared: SharedWorkstreamStore = std::sync::Arc::new(tokio::sync::Mutex::new(store));
    let id = create(&shared, "t").await;
    activate(&shared, json!({"id": id.to_string()}), test_ctx())
        .await
        .expect("activate");

    let mut reloaded = WorkstreamStore::load(&path).await.expect("reload");
    assert_eq!(
        reloaded.active_workstream_id().await.unwrap(),
        Some(id),
        "active pointer must persist across a fresh load"
    );
}
