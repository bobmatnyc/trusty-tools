//! E2E: liveness probe.

use crate::harness::TestDaemon;

/// `GET /health` returns `200` with the `ok` liveness body.
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
    let body = resp.text().await.expect("health body");
    assert_eq!(body, "ok");
}
