//! `GET /api/listener-events` + `POST /api/listener-events/filter` tests
//! (#3820).
//!
//! Why: The route's whole job is to pass through `EventStore` faithfully —
//! these tests pin the router-level JSON contract (shape, status codes) that
//! `EventStore`'s own unit tests (`crate::listeners::store::tests`) don't
//! cover, since those exercise the store directly, not the HTTP surface.
//! What: `list_returns_empty_when_no_log`, `filter_post_persists_and_list_reflects_it`,
//! `filter_post_rejects_empty_event_type`.
//! Test: This module IS the test.
#![allow(clippy::await_holding_lock)]

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

use crate::api::server::routes::build_router;
use crate::api::server::state::AppState;
use crate::test_env::HOME_LOCK;

fn router() -> Router {
    build_router(AppState::default())
}

fn set_test_home(dir: &std::path::Path) {
    // SAFETY: caller holds `HOME_LOCK` for the duration of the test (see
    // `crate::test_env`'s module doc) so no other thread observes `HOME`
    // mid-mutation.
    unsafe {
        std::env::set_var("HOME", dir);
    }
}

#[tokio::test]
async fn list_returns_empty_when_no_log() {
    let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    set_test_home(tmp.path());

    let req = Request::builder()
        .uri("/api/listener-events")
        .body(Body::empty())
        .unwrap();
    let resp = router().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["events"], serde_json::json!([]));
}

#[tokio::test]
async fn filter_post_persists_and_list_reflects_it() {
    let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    set_test_home(tmp.path());

    // Append one event directly via the store (the HTTP surface has no
    // write path for events themselves — only the poller writes those).
    crate::listeners::store::EventStore::append(&crate::listeners::store::StoredEvent {
        id: "gmail-personal:m1".to_string(),
        listener_id: "gmail-personal".to_string(),
        provider: "gmail".to_string(),
        event_type: "message.received".to_string(),
        ts: "2026-07-24T12:00:00Z".to_string(),
        from: Some("dad@family.com".to_string()),
        subject: Some("Hi".to_string()),
        snippet: Some("snippet".to_string()),
        included: true,
    })
    .await
    .unwrap();

    let app = router();
    let post_req = Request::builder()
        .method("POST")
        .uri("/api/listener-events/filter")
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::json!({ "event_type": "message.received", "included": false }).to_string(),
        ))
        .unwrap();
    let post_resp = app.clone().oneshot(post_req).await.unwrap();
    assert_eq!(post_resp.status(), StatusCode::OK);

    let list_req = Request::builder()
        .uri("/api/listener-events")
        .body(Body::empty())
        .unwrap();
    let list_resp = app.oneshot(list_req).await.unwrap();
    assert_eq!(list_resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(list_resp.into_body(), 64 * 1024)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let events = body["events"].as_array().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["included"], false);
}

#[tokio::test]
async fn filter_post_rejects_empty_event_type() {
    let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    set_test_home(tmp.path());

    let req = Request::builder()
        .method("POST")
        .uri("/api/listener-events/filter")
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::json!({ "event_type": "", "included": true }).to_string(),
        ))
        .unwrap();
    let resp = router().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}
