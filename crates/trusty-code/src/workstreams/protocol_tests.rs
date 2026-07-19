//! Tests for `workstream.*` RPC handlers (DOC-48 §5.1/§6, issues #3294/#3295).

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

/// Seed a workstream directly via the store (bypassing the
/// `workstream.create` RPC handler, which some tests below exercise
/// directly under its own name — named distinctly so the two never collide).
async fn seed_workstream(store: &SharedWorkstreamStore, name: &str) -> WorkstreamId {
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

/// `workstream.activate`/`workstream.deactivate` must be reachable through a
/// `Router` built by `register` (proves the wiring, not just the free
/// functions).
#[tokio::test]
async fn register_wires_activate_and_deactivate() {
    let (store, _dir) = shared_store().await;
    let id = seed_workstream(&store, "t").await;
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

/// `workstream.create`/`get`/`list`/`close` must be reachable through a
/// `Router` built by `register` (proves the wiring, not just the free
/// functions) — the #3295 counterpart to
/// `register_wires_activate_and_deactivate` above.
#[tokio::test]
async fn register_wires_create_get_list_close() {
    let (store, _dir) = shared_store().await;
    let mut router = Router::new();
    register(&mut router, store);

    let resp = router
        .dispatch(req("workstream.create", json!({"name": "A"})), &test_ctx())
        .await;
    assert!(resp.error.is_none(), "create failed: {:?}", resp.error);
    let id = resp.result.unwrap()["id"].clone();

    for (method, params) in [
        ("workstream.get", json!({"id": id})),
        ("workstream.list", json!({})),
        ("workstream.close", json!({"id": id})),
    ] {
        let resp = router.dispatch(req(method, params), &test_ctx()).await;
        assert!(resp.error.is_none(), "{method} failed: {:?}", resp.error);
    }
}

/// Activating with no prior active workstream must return `active_id` and a
/// null `prior_id`.
#[tokio::test]
async fn activate_succeeds_with_no_prior_active() {
    let (store, _dir) = shared_store().await;
    let id = seed_workstream(&store, "t").await;

    let result = activate(&store, json!({"id": id.to_string()}), test_ctx())
        .await
        .expect("activate must succeed");
    assert_eq!(result["active_id"], json!(id));
    assert_eq!(result["prior_id"], Value::Null);
}

/// Re-activating the already-active workstream (force omitted, defaults to
/// false) must be idempotent, not an error.
#[tokio::test]
async fn activate_already_active_is_idempotent() {
    let (store, _dir) = shared_store().await;
    let id = seed_workstream(&store, "t").await;
    activate(&store, json!({"id": id.to_string()}), test_ctx())
        .await
        .expect("first activate");

    let result = activate(&store, json!({"id": id.to_string()}), test_ctx())
        .await
        .expect("re-activate must succeed");
    assert_eq!(result["active_id"], json!(id));
    assert_eq!(result["prior_id"], Value::Null);
}

/// Activating a different workstream without `force` while one is active
/// must map to `-32008 active_conflict`, carrying the currently-active id.
#[tokio::test]
async fn activate_without_force_maps_to_active_conflict() {
    let (store, _dir) = shared_store().await;
    let a = seed_workstream(&store, "a").await;
    let b = seed_workstream(&store, "b").await;
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
    let a = seed_workstream(&store, "a").await;
    let b = seed_workstream(&store, "b").await;
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
    let id = seed_workstream(&store, "t").await;
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
    let a = seed_workstream(&store, "a").await;
    let b = seed_workstream(&store, "b").await;
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
    let id = seed_workstream(&shared, "t").await;
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

#[tokio::test]
async fn create_returns_new_id() {
    let (store, _dir) = shared_store().await;
    let result = create(&store, json!({"name": "Token rotation"}), test_ctx())
        .await
        .expect("create");
    assert!(result["id"].is_string());
}

#[tokio::test]
async fn create_without_name_defaults_to_empty() {
    let (store, _dir) = shared_store().await;
    let result = create(&store, json!({}), test_ctx()).await.expect("create");
    let id = result["id"].clone();
    let ws = get(&store, json!({"id": id}), test_ctx())
        .await
        .expect("get");
    assert_eq!(ws["name"], "");
}

#[tokio::test]
async fn get_returns_workstream_with_state() {
    let (store, _dir) = shared_store().await;
    let created = create(&store, json!({"name": "A"}), test_ctx())
        .await
        .expect("create");
    let id = created["id"].clone();

    let ws = get(&store, json!({"id": id}), test_ctx())
        .await
        .expect("get");
    assert_eq!(ws["name"], "A");
    assert_eq!(ws["state"], "idle");
    assert!(ws["session_ids"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn get_unknown_id_maps_to_not_found() {
    let (store, _dir) = shared_store().await;
    let err = get(
        &store,
        json!({"id": WorkstreamId::new().to_string()}),
        test_ctx(),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, -32002);
}

#[tokio::test]
async fn get_invalid_params_maps_to_invalid_params() {
    let (store, _dir) = shared_store().await;
    let err = get(&store, json!({"id": "not-a-uuid"}), test_ctx())
        .await
        .unwrap_err();
    assert_eq!(err.code, trusty_common::mcp::error_codes::INVALID_PARAMS);
}

#[tokio::test]
async fn list_returns_active_workstream_id_and_records() {
    let (store, _dir) = shared_store().await;
    let a = create(&store, json!({"name": "A"}), test_ctx())
        .await
        .expect("create A")["id"]
        .clone();
    create(&store, json!({"name": "B"}), test_ctx())
        .await
        .expect("create B");

    let a_id: WorkstreamId = serde_json::from_value(a.clone()).unwrap();
    store
        .lock()
        .await
        .set_active(Some(a_id))
        .await
        .expect("activate A");

    let result = list(&store, json!({}), test_ctx()).await.expect("list");
    assert_eq!(result["active_workstream_id"], a);
    let workstreams = result["workstreams"].as_array().unwrap();
    assert_eq!(workstreams.len(), 2);
    let a_view = workstreams.iter().find(|w| w["id"] == a).unwrap();
    assert_eq!(a_view["state"], "active");
}

#[tokio::test]
async fn list_missing_params_defaults_to_include_closed_false() {
    let (store, _dir) = shared_store().await;
    let result = list(&store, Value::Null, test_ctx()).await.expect("list");
    assert!(result["workstreams"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn list_default_excludes_closed() {
    let (store, _dir) = shared_store().await;
    let id = create(&store, json!({"name": "A"}), test_ctx())
        .await
        .expect("create")["id"]
        .clone();
    close(&store, json!({"id": id}), test_ctx())
        .await
        .expect("close");

    let result = list(&store, json!({}), test_ctx()).await.expect("list");
    assert!(
        result["workstreams"].as_array().unwrap().is_empty(),
        "closed workstream must be excluded by default"
    );
}

#[tokio::test]
async fn list_include_closed_true_includes_closed() {
    let (store, _dir) = shared_store().await;
    let id = create(&store, json!({"name": "A"}), test_ctx())
        .await
        .expect("create")["id"]
        .clone();
    close(&store, json!({"id": id}), test_ctx())
        .await
        .expect("close");

    let result = list(&store, json!({"include_closed": true}), test_ctx())
        .await
        .expect("list");
    let workstreams = result["workstreams"].as_array().unwrap();
    assert_eq!(workstreams.len(), 1);
    assert_eq!(workstreams[0]["state"], "closed");
}

#[tokio::test]
async fn close_succeeds_and_clears_active_pointer() {
    let (store, _dir) = shared_store().await;
    let id = create(&store, json!({"name": "A"}), test_ctx())
        .await
        .expect("create")["id"]
        .clone();
    let ws_id: WorkstreamId = serde_json::from_value(id.clone()).unwrap();
    store
        .lock()
        .await
        .set_active(Some(ws_id))
        .await
        .expect("activate");

    let result = close(&store, json!({"id": id}), test_ctx())
        .await
        .expect("close");
    assert_eq!(result, json!({}));

    let list_result = list(&store, json!({"include_closed": true}), test_ctx())
        .await
        .expect("list");
    assert_eq!(list_result["active_workstream_id"], Value::Null);
}

/// (issue #3297) `workstream.close` must publish
/// `Event::WorkstreamStateInferred{state: "closed"}`.
#[tokio::test]
async fn close_publishes_state_inferred() {
    let (store, _dir) = shared_store().await;
    let id = create(&store, json!({"name": "A"}), test_ctx())
        .await
        .expect("create")["id"]
        .clone();
    let ws_id: WorkstreamId = serde_json::from_value(id.clone()).unwrap();

    let mut rx = crate::events::subscribe();
    close(&store, json!({"id": id}), test_ctx())
        .await
        .expect("close");

    // `crate::events::bus()` is a process-wide singleton shared by every
    // concurrently-running test (issue #3297 CI: `cargo test` runs tests in
    // parallel by default, so a raw `rx.recv().await` can observe another
    // test's unrelated envelope first) — loop past anything that doesn't
    // name THIS test's own workstream id, mirroring
    // `workstreams::activation_tests::next_state_inferred_for`.
    let target = ws_id.to_string();
    let (state, _reason) = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let envelope = rx.recv().await.expect("event bus channel closed");
            if let crate::events::Event::WorkstreamStateInferred {
                workstream_id,
                state,
                reason,
            } = envelope.event
                && workstream_id == target
            {
                return (state, reason);
            }
        }
    })
    .await
    .expect("timed out waiting for WorkstreamStateInferred naming this workstream");
    assert_eq!(state, "closed");
}

#[tokio::test]
async fn close_unknown_id_maps_to_not_found() {
    let (store, _dir) = shared_store().await;
    let err = close(
        &store,
        json!({"id": WorkstreamId::new().to_string()}),
        test_ctx(),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, -32002);
}
