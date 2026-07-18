//! HTTP round-trip tests for `commands::sync_assets` (issue #2444).
//!
//! Why: mirrors `commands::managed_tests`'s `spawn_test_daemon` pattern — the
//! real daemon router is booted on an ephemeral port against an isolated,
//! empty managed-session store, so a 404 here proves the actual merged
//! `sync_assets::router()` is reachable (both the literal fleet-wide path and
//! the `{id}` per-session path), not a hand-rolled mock.
//! What: `session_sync_assets_not_found_errors` covers the per-session 404
//! path (an unresolvable id/name); `session_sync_assets_all_empty_fleet_ok`
//! covers the fleet-wide route against an empty store (proves the STATIC
//! `/api/v1/sessions/managed/sync-assets` path is not swallowed by the
//! `/{id}` param route it is registered alongside).
//! Test: this file IS the test module.

use super::{session_sync_assets, session_sync_assets_all};

/// Spawn the daemon's real HTTP API on a random loopback port, rooted in a
/// throwaway temp directory. Mirrors `managed_tests::spawn_test_daemon`.
async fn spawn_test_daemon() -> String {
    use std::future::IntoFuture as _;
    use trusty_mpm::daemon::{api, state::DaemonState};
    let root = tempfile::tempdir().unwrap().keep();
    let state = std::sync::Arc::new(DaemonState::with_root_isolated_managed(root).await);
    let router = api::router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(axum::serve(listener, router).into_future());
    format!("http://{addr}")
}

/// A nonexistent id/name must be a hard `Err`, never a silently-swallowed
/// `Ok(())` (matching the #2457 exit-code convention every other managed
/// verb in this CLI follows).
#[tokio::test]
async fn session_sync_assets_not_found_errors() {
    let url = spawn_test_daemon().await;
    let client = reqwest::Client::new();
    let err = session_sync_assets(&client, &url, "nonexistent-id".to_string())
        .await
        .expect_err("a missing managed session must be a hard failure, not a silent Ok(())");
    assert!(
        err.to_string().contains("nonexistent-id"),
        "error should name the missing id/name: {err}"
    );
}

/// The fleet-wide `--all` route must be reachable and return an empty report
/// against a store with no sessions — proves the STATIC
/// `/api/v1/sessions/managed/sync-assets` path resolves correctly rather than
/// being captured by the `/{id}` parameter route it sits beside.
#[tokio::test]
async fn session_sync_assets_all_empty_fleet_ok() {
    let url = spawn_test_daemon().await;
    let client = reqwest::Client::new();
    session_sync_assets_all(&client, &url)
        .await
        .expect("an empty fleet must be a successful (zero-work) no-op");
}
