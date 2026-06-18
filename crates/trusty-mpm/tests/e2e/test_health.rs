//! E2E: liveness probe + HR-3 catalog-staleness fields.

use crate::harness::TestDaemon;

/// `GET /health` returns `200` with the JSON liveness body, including the HR-3
/// `catalog_stale` / `catalog_unknown` fields. Against a fresh test daemon with
/// no synced catalog, staleness must degrade to `catalog_unknown` (never an
/// error, never `catalog_stale`).
#[tokio::test]
async fn health_returns_ok() {
    let daemon = TestDaemon::spawn().await;
    let resp = daemon
        .client()
        .get(daemon.url("/health"))
        .send()
        .await
        .expect("health request");
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.expect("health json body");
    assert_eq!(body["status"], "ok");
    // The staleness fields are present and well-typed.
    assert!(body["catalog_stale"].is_boolean(), "catalog_stale present");
    assert!(
        body["catalog_unknown"].is_boolean(),
        "catalog_unknown present"
    );
    assert!(
        body["catalog_changes"].is_array(),
        "catalog_changes present"
    );
    // A fresh daemon with no synced catalog must never report stale.
    assert_eq!(body["catalog_stale"], false, "no false-positive staleness");
}
