//! Unit tests for `commands::managed` — split out of `managed.rs` (test-file
//! budget: 1500 SLOC) so the #2457 HTTP-round-trip coverage below doesn't push
//! the production file toward the 500-SLOC cap.
//!
//! Why: `session_stop`/`session_resume`/`session_decommission`/`session_activity`
//! previously printed "not found" (or, for resume, a conflict message) and
//! returned `Ok(())` on a 404/409 from the daemon — a genuine failure reported
//! as a successful exit code (#2457). The tests below drive each function
//! against a real hermetic daemon (mirroring `client::executor::tests`'s
//! `spawn_test_daemon` pattern) and assert `Err` is returned instead.
//! What: the `session_*_not_found_errors` tests cover the exit-code fix (the
//! `session_resume` 409/conflict branch got the identical mechanical fix —
//! print+`Ok(())` to `bail!` — but seeding a managed session in a state that
//! rejects `resume` needs the daemon's `SessionManager::create_with_id` +
//! tmux-driver test scaffolding that lives in `tests/session_manager_mvp.rs`,
//! not this unit-test file, so it is left to that integration suite); the
//! pre-existing `truncate_*`/`short_timestamp_*`/
//! `decommission_message_reflects_workspace_removed` unit tests are carried
//! over unchanged from the inline module this file replaced.
//! Test: this file IS the test module for `commands::managed`.

use std::future::IntoFuture as _;

use super::{session_activity, session_decommission, session_resume, session_stop};
use super::{short_timestamp, truncate};

#[test]
fn truncate_clips_and_appends_ellipsis() {
    assert_eq!(truncate("hello", 10), "hello");
    assert_eq!(truncate("hello world", 5), "hell\u{2026}");
    assert_eq!(truncate("", 5), "");
    assert_eq!(truncate("abcde", 5), "abcde");
}

#[test]
fn short_timestamp_formats_correctly() {
    assert_eq!(short_timestamp("2025-06-27T14:32:00Z"), "2025-06-27 14:32");
    assert_eq!(short_timestamp("short"), "short");
    assert_eq!(short_timestamp("2025-06-27T14:32"), "2025-06-27 14:32");
}

#[test]
fn decommission_message_reflects_workspace_removed() {
    // Guard that the key field names used in session_decommission match the
    // daemon's DecommissionResponse serde output. If the daemon renames those
    // keys this test catches the drift before the JSON decodes silently to None.
    let owned_removed = serde_json::json!({
        "id": "abc-123",
        "workspace_removed": true,
        "workspace_path_was": "/some/workspace/path"
    });
    assert_eq!(
        owned_removed
            .get("workspace_removed")
            .and_then(|v| v.as_bool()),
        Some(true)
    );
    assert_eq!(
        owned_removed
            .get("workspace_path_was")
            .and_then(|v| v.as_str()),
        Some("/some/workspace/path")
    );
    let adopted_not_removed = serde_json::json!({
        "id": "xyz-456",
        "workspace_removed": false
    });
    assert_eq!(
        adopted_not_removed
            .get("workspace_removed")
            .and_then(|v| v.as_bool()),
        Some(false)
    );
    assert!(adopted_not_removed.get("workspace_path_was").is_none());
}

/// Spawn the daemon's real HTTP API on a random loopback port, rooted in a
/// throwaway temp directory.
///
/// Why: mirrors `client::executor::tests::spawn_test_daemon` — an empty
/// isolated managed-session store means ANY id is a genuine 404, so these
/// tests exercise the real daemon route (not a hand-rolled mock response).
/// What: builds `daemon::api::router(DaemonState::with_root_isolated_managed(...))`,
/// binds an ephemeral port, serves it on a background task, and returns the
/// base URL.
async fn spawn_test_daemon() -> String {
    use trusty_mpm::daemon::{api, state::DaemonState};
    let root = tempfile::tempdir().unwrap().keep();
    let state = std::sync::Arc::new(DaemonState::with_root_isolated_managed(root).await);
    let router = api::router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(axum::serve(listener, router).into_future());
    format!("http://{addr}")
}

/// #2457: a 404 from `runtime-stop` on a nonexistent id must propagate as
/// `Err`, not a printed "not found" with `Ok(())`.
#[tokio::test]
async fn session_stop_not_found_errors() {
    let url = spawn_test_daemon().await;
    let client = reqwest::Client::new();
    let err = session_stop(&client, &url, "nonexistent-id".to_string())
        .await
        .expect_err("a missing managed session must be a hard failure, not a silent Ok(())");
    assert!(
        err.to_string().contains("nonexistent-id"),
        "error should name the missing id: {err}"
    );
}

/// #2457: a 404 from `resume` on a nonexistent id must propagate as `Err`.
#[tokio::test]
async fn session_resume_not_found_errors() {
    let url = spawn_test_daemon().await;
    let client = reqwest::Client::new();
    let err = session_resume(&client, &url, "nonexistent-id".to_string())
        .await
        .expect_err("a missing managed session must be a hard failure, not a silent Ok(())");
    assert!(
        err.to_string().contains("nonexistent-id"),
        "error should name the missing id: {err}"
    );
}

/// #2457: a 404 from `decommission` on a nonexistent id must propagate as
/// `Err` — `prune.rs`'s bulk loop relies on this via `?`.
#[tokio::test]
async fn session_decommission_not_found_errors() {
    let url = spawn_test_daemon().await;
    let client = reqwest::Client::new();
    let err = session_decommission(&client, &url, "nonexistent-id".to_string())
        .await
        .expect_err("a missing managed session must be a hard failure, not a silent Ok(())");
    assert!(
        err.to_string().contains("nonexistent-id"),
        "error should name the missing id: {err}"
    );
}

/// #2457: a 404 from `activity` on a nonexistent id must propagate as `Err`.
#[tokio::test]
async fn session_activity_not_found_errors() {
    let url = spawn_test_daemon().await;
    let client = reqwest::Client::new();
    let err = session_activity(&client, &url, "nonexistent-id".to_string())
        .await
        .expect_err("a missing managed session must be a hard failure, not a silent Ok(())");
    assert!(
        err.to_string().contains("nonexistent-id"),
        "error should name the missing id: {err}"
    );
}
