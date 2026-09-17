//! `GET /api/agents/:name/stores` handler tests (#3816/#3864).
//!
//! Why: This endpoint's whole point is to replace a hardcoded "not
//! connected" placeholder with an OBSERVED status — a route that always
//! reported the same thing would be the bug it was written to fix. These
//! tests drive `stores_at` directly against a `tempfile::TempDir` (the
//! `agent_patch.rs` pattern, so they don't race sibling tests on cwd/`$HOME`)
//! with a mock trusty-search/-memory daemon, plus one full-router test
//! proving the route is actually wired into `build_router`.
//! What: connected-with-stats, missing-index → not-connected + reason,
//! unbound agent → empty list, unknown agent → 404, invalid name → 400,
//! malformed TOML → 200 with `config_error` (degrades, never 500), and
//! router wiring.
//! Test: This module IS the test.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::{Json, Router, extract::Path, http::StatusCode as AxumStatus, routing::get};
use tokio::net::TcpListener;
use tower::ServiceExt;

use crate::api::server::agent_stores::stores_at;
use crate::api::server::routes::build_router;
use crate::api::server::state::AppState;

const BOUND_FIXTURE: &str = r#"[agent]
name = "izzie"
role = "assistant"
model = "claude-sonnet-4-6"
description = "test"

[[stores]]
name = "bob-kb"
tree = "okg://izzie"
index = "bob-kb"
palace = "owner-profile"
"#;

const UNBOUND_FIXTURE: &str = r#"[agent]
name = "plain"
role = "assistant"
model = "claude-sonnet-4-6"
description = "test"
"#;

const MISSING_INDEX_FIXTURE: &str = r#"[agent]
name = "ghosty"
role = "assistant"
model = "claude-sonnet-4-6"
description = "test"

[[stores]]
name = "ghost-kb"
"#;

/// Mock daemon serving both the trusty-search status route and the
/// trusty-memory drawers route. `bob-kb` / `owner-profile` exist; everything
/// else 404s.
async fn mock_daemon() -> String {
    let app = Router::new().route(
        "/indexes/{id}/status",
        get(|Path(id): Path<String>| async move {
            if id == "bob-kb" {
                (
                    AxumStatus::OK,
                    Json(serde_json::json!({
                        "index_id": "bob-kb",
                        "chunk_count": 552,
                        // Inert: nothing in this file asserts on `root_path`.
                        // A neutral path keeps a developer's home directory out
                        // of the fixture (#6286 review, finding 8).
                        "root_path": "/tmp/trusty-agents/bob-kb",
                        "status": "ready",
                    })),
                )
            } else {
                (AxumStatus::NOT_FOUND, Json(serde_json::json!({})))
            }
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

/// A stub trusty-memory answering the palace-existence probe (#6286).
///
/// `owner-profile` answers an empty drawer page; every other palace is refused
/// not-found, which is what the daemon does for a palace that was never
/// created.
///
/// It answers ONLY `memory.drawers_list` (#6286 review, finding 9). The mock
/// used to ignore the method name, so `stores_at` could have been rewired onto
/// any other method — `palace_create`, say, which a status probe must never
/// call because it CREATES the palace it is asking about — and these tests
/// would still have reported `palace_connected: true`.
async fn mock_memory() -> crate::uds_mock::MockMemoryDaemon {
    crate::uds_mock::spawn(|method: &str, params: serde_json::Value| {
        let method = method.to_string();
        let palace = params
            .get("palace_id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string();
        Box::pin(async move {
            if method != "memory.drawers_list" {
                return Err(crate::uds_mock::RpcError::method_not_found(
                    &method,
                    &["memory.drawers_list"],
                ));
            }
            if palace == "owner-profile" {
                Ok(serde_json::json!({ "drawers": [] }))
            } else {
                Err(crate::uds_mock::RpcError::new(
                    trusty_common::memory_rpc::CODE_NOT_FOUND,
                    format!("palace not found: {palace}"),
                ))
            }
        })
    })
    .await
}

async fn body_json(resp: axum::response::Response) -> serde_json::Value {
    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn stores_route_reports_connected_binding_with_stats() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("izzie.toml"), BOUND_FIXTURE).unwrap();
    let base = mock_daemon().await;
    let memory = mock_memory().await;

    let resp = stores_at(
        &[dir.path().to_path_buf()],
        "izzie",
        Some(&base),
        Some(memory.socket()),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    let stores = body["stores"].as_array().unwrap();
    assert_eq!(stores.len(), 1);
    assert_eq!(stores[0]["connected"], true);
    assert_eq!(stores[0]["index"], "bob-kb");
    assert_eq!(stores[0]["tree"], "okg://izzie");
    assert_eq!(stores[0]["chunk_count"], 552);
    assert_eq!(stores[0]["palace_connected"], true);
    assert!(body["issues"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn stores_route_reports_missing_index_with_reason() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("ghosty.toml"), MISSING_INDEX_FIXTURE).unwrap();
    let base = mock_daemon().await;
    let memory = mock_memory().await;

    let resp = stores_at(
        &[dir.path().to_path_buf()],
        "ghosty",
        Some(&base),
        Some(memory.socket()),
    )
    .await;
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "a down store is not an HTTP error"
    );
    let body = body_json(resp).await;
    let store = &body["stores"][0];
    assert_eq!(store["connected"], false);
    let reason = store["reason"].as_str().unwrap();
    assert!(reason.contains("not registered"), "reason was: {reason}");
    // Derived defaults must still be reported so the card can name what is
    // disconnected: index defaults to the store name, tree to okg://<agent>.
    assert_eq!(store["index"], "ghost-kb");
    assert_eq!(store["tree"], "okg://ghosty");
}

#[tokio::test]
async fn stores_route_empty_for_unbound_agent() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("plain.toml"), UNBOUND_FIXTURE).unwrap();

    let resp = stores_at(&[dir.path().to_path_buf()], "plain", None, None).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert!(body["stores"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn stores_route_unknown_agent_404() {
    let dir = tempfile::tempdir().unwrap();
    let resp = stores_at(&[dir.path().to_path_buf()], "nobody", None, None).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn stores_route_rejects_traversal_name() {
    let dir = tempfile::tempdir().unwrap();
    let resp = stores_at(&[dir.path().to_path_buf()], "../etc", None, None).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn stores_route_degrades_on_malformed_toml() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("broken.toml"), "not = = toml").unwrap();

    let resp = stores_at(&[dir.path().to_path_buf()], "broken", None, None).await;
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "a hand-edit typo must not 500 the panel"
    );
    let body = body_json(resp).await;
    assert!(body["stores"].as_array().unwrap().is_empty());
    assert!(body["config_error"].is_string());
}

/// Point the assistants root at `dir` under `ENV_LOCK`, restoring on drop.
///
/// #7902: `search_slots` resolves an assistant's knowledge state out of
/// `assistants_root()`, so a test that does not redirect it would read the
/// developer's own provisioned assistants.
struct AssistantsRoot {
    prev: Option<std::ffi::OsString>,
    // Dropped last, so the lock outlives the restore.
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl AssistantsRoot {
    fn set(dir: &std::path::Path) -> Self {
        let lock = crate::test_env::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let prev = std::env::var_os(crate::assistants::ASSISTANTS_DIR_ENV);
        // SAFETY: ENV_LOCK is held for this guard's lifetime.
        unsafe { std::env::set_var(crate::assistants::ASSISTANTS_DIR_ENV, dir) };
        Self { prev, _lock: lock }
    }
}

impl Drop for AssistantsRoot {
    fn drop(&mut self) {
        // SAFETY: the lock is still held — it drops after this body.
        unsafe {
            match self.prev.take() {
                Some(v) => std::env::set_var(crate::assistants::ASSISTANTS_DIR_ENV, v),
                None => std::env::remove_var(crate::assistants::ASSISTANTS_DIR_ENV),
            }
        }
    }
}

/// Provision protected knowledge for `izzie` under `root`, recording `legacy`
/// as the legacy binding. Returns the protected extraction index id.
fn provision_izzie(root: &std::path::Path, legacy: crate::stores::AgentStoreBinding) -> String {
    let home = crate::assistants::AssistantHome::under(
        root.to_path_buf(),
        crate::assistants::AssistantInstanceId::new("izzie").expect("instance id"),
    );
    let store = crate::knowledge::KnowledgeStore::new(home);
    let state = store
        .initialize(chrono::Utc::now(), None)
        .expect("provision knowledge");
    store
        .confirm_binding_with_legacy(&state.revision, Some(legacy))
        .expect("confirm binding");
    state.store.index_id
}

/// #7902 closure condition: the response says which index the default
/// `vector_search` slot resolves to when it is NOT the declared binding.
///
/// Here the recorded legacy binding no longer matches the declared one, so the
/// resolver refuses and the default slot resolves to nothing — while `stores`
/// beside it still reports `bob-kb` as a live binding. That disagreement was
/// invisible in this payload, which is the whole of #7902's remainder.
///
/// Pre-change this fails on the first assertion: the payload carries no
/// `search_slot` key at all.
#[tokio::test]
async fn stores_route_reports_a_default_slot_that_differs_from_the_declared_index() {
    let assistants = tempfile::tempdir().unwrap();
    let root = assistants.path().canonicalize().unwrap();
    let _root = AssistantsRoot::set(&root);
    provision_izzie(
        &root,
        crate::stores::AgentStoreBinding {
            name: "other-kb".into(),
            index: Some("other-kb".into()),
            ..Default::default()
        },
    );

    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("izzie.toml"), BOUND_FIXTURE).unwrap();
    let resp = stores_at(&[dir.path().to_path_buf()], "izzie", None, None).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    let slot = &body["search_slot"];

    assert_eq!(slot["declared_index"], "bob-kb", "the declared binding");
    assert!(
        slot["default_index"].is_null(),
        "the default slot resolves to nothing: {slot}"
    );
    assert_eq!(
        slot["differs_from_declared"], true,
        "and the payload says so rather than leaving a reader to compare: {slot}"
    );
    assert!(
        slot["error"]
            .as_str()
            .is_some_and(|e| e.contains("binding changed")),
        "with the resolver's own reason: {slot}"
    );
    assert_eq!(
        body["stores"][0]["index"], "bob-kb",
        "the store list still reports the declared index — the two are meant to be comparable"
    );
}

/// #7902: the matching case reports no difference, and still names the
/// protected extraction index the declared one does not displace.
///
/// Pre-change this fails on the first assertion: there is no `search_slot`.
#[tokio::test]
async fn stores_route_reports_no_difference_when_the_declared_index_answers() {
    let assistants = tempfile::tempdir().unwrap();
    let root = assistants.path().canonicalize().unwrap();
    let _root = AssistantsRoot::set(&root);
    let protected = provision_izzie(
        &root,
        crate::stores::AgentStoreBinding {
            name: "bob-kb".into(),
            tree: Some("okg://izzie".into()),
            index: Some("bob-kb".into()),
            palace: Some("owner-profile".into()),
            ..Default::default()
        },
    );

    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("izzie.toml"), BOUND_FIXTURE).unwrap();
    let resp = stores_at(&[dir.path().to_path_buf()], "izzie", None, None).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    let slot = &body["search_slot"];

    assert_eq!(slot["declared_index"], "bob-kb");
    assert_eq!(
        slot["default_index"], "bob-kb",
        "the declared index answers"
    );
    assert_eq!(slot["differs_from_declared"], false, "{slot}");
    assert!(slot["error"].is_null(), "nothing to report: {slot}");
    assert_eq!(
        slot["protected_index"], protected,
        "the protected index is named even though it is not the default: {slot}"
    );
}

/// Proves the route is wired into `build_router` (not just that the core
/// function works). Unknown agent under the real agents dirs → 404, never a
/// 405/404-from-no-such-route.
#[tokio::test]
async fn stores_route_is_wired_into_router() {
    let app: Router = build_router(AppState::default());
    let req = Request::builder()
        .uri("/api/agents/definitely-not-an-agent-3816/stores")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body = body_json(resp).await;
    assert_eq!(
        body["error"], "unknown agent",
        "a 404 from the handler, not from an unrouted path"
    );
}
