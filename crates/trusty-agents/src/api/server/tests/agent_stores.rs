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
    let app = Router::new()
        .route(
            "/indexes/{id}/status",
            get(|Path(id): Path<String>| async move {
                if id == "bob-kb" {
                    (
                        AxumStatus::OK,
                        Json(serde_json::json!({
                            "index_id": "bob-kb",
                            "chunk_count": 552,
                            "root_path": "/Users/masa/trusty-agents/bob-kb",
                            "status": "ready",
                        })),
                    )
                } else {
                    (AxumStatus::NOT_FOUND, Json(serde_json::json!({})))
                }
            }),
        )
        .route(
            "/api/v1/palaces/{id}/drawers",
            get(|Path(id): Path<String>| async move {
                if id == "owner-profile" {
                    (AxumStatus::OK, Json(serde_json::json!({"drawers": []})))
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

    let resp = stores_at(
        &[dir.path().to_path_buf()],
        "izzie",
        Some(&base),
        Some(&base),
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

    let resp = stores_at(
        &[dir.path().to_path_buf()],
        "ghosty",
        Some(&base),
        Some(&base),
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
