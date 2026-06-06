//! CTRL session HTTP e2e tests (#406).
//!
//! Why: The `om session` CLI workflow is wired through HTTP handlers backed by
//! `SessionStore`. These tests drive the real axum router end-to-end so
//! regressions in route wiring, query-param filtering, or status codes are
//! caught. Split from the parent `tests` module to keep each file under the
//! 500-line cap.
//! What: Full CRUD lifecycle, project filter, status filter, store
//! persistence across instances, and 404 on unknown id.
//! Test: This module IS the test — run via `cargo test session_e2e`.

use super::test_router;
use crate::ctrl_session::{
    Session as CtrlSession, SessionStatus as CtrlSessionStatus, SessionStore,
};
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt; // for `oneshot`

// -------- CTRL session e2e tests (#406) ----------
//
// Why: The `om session` CLI workflow (v0.15.0) is wired through HTTP
// handlers backed by `SessionStore`. Unit tests in `ctrl_session.rs`
// cover the store in isolation; these tests drive the real axum router
// end-to-end so regressions in route wiring, query-param filtering, or
// status codes get caught.
// What: Five tests covering full CRUD lifecycle, project filter, status
// filter, store persistence across instances, and 404 on unknown id.
// Test: This module IS the test — run via `cargo test session_e2e`.

/// Sandbox HOME and run a closure with the api router.
/// Why: Each session_e2e test must isolate `~/.trusty-agents/sessions/...`
/// from the developer's real home and from sibling tests. Mirrors the
/// pattern in `connect_project_persists_to_registry`.
async fn with_sandboxed_home<F, Fut, R>(f: F) -> R
where
    F: FnOnce(tempfile::TempDir) -> Fut,
    Fut: std::future::Future<Output = R>,
{
    // #409: This helper must hold the HOME override across the AWAIT of
    // the inner future, not just across construction. The previous
    // implementation took a closure returning an arbitrary `R` and reset
    // HOME before the caller could `.await` the future, so the actual
    // request handlers ran with the developer's real `~/.trusty-agents` and
    // saw cross-test pollution. We now take an async closure, hold the
    // mutex guard for the lifetime of the future, and clean up after it
    // resolves.
    let _g = crate::test_env::HOME_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let prev_home = std::env::var_os("HOME");
    // SAFETY: HOME_LOCK held; restored before drop.
    unsafe {
        std::env::set_var("HOME", tmp.path());
    }
    let result = f(tmp).await;
    // SAFETY: HOME_LOCK still held (guard dropped at function return).
    unsafe {
        match prev_home {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
    }
    result
}

/// Issue a request through the test router and return (status, body json).
async fn send_json(
    app: Router,
    method: &str,
    uri: &str,
    body: Option<serde_json::Value>,
) -> (StatusCode, serde_json::Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    let body = if let Some(b) = body {
        builder = builder.header("content-type", "application/json");
        Body::from(serde_json::to_vec(&b).unwrap())
    } else {
        Body::empty()
    };
    let req = builder.body(body).unwrap();
    let resp = app.oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}

#[tokio::test]
async fn session_e2e_full_crud_lifecycle() {
    // Why: Verifies create → list → get → kill → get-after-kill round trip.
    // What: Drives every CTRL session route with a single project path.
    with_sandboxed_home(|tmp| async move {
        let project_dir = tmp.path().join("crud-proj");
        std::fs::create_dir(&project_dir).unwrap();

        // 1. Create.
        let (status, created) = send_json(
            test_router(),
            "POST",
            "/api/ctrl/sessions",
            Some(serde_json::json!({
                "project_path": project_dir.to_string_lossy(),
                "name": "feature-x",
                "agent": "pm",
                "worktree": false,
            })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "create: {created}");
        let id = created["id"].as_str().expect("id present").to_string();
        assert_eq!(created["name"], "feature-x");
        assert_eq!(created["agent"], "pm");
        assert_eq!(created["status"], "idle");

        // 2. List shows it.
        let (status, listed) = send_json(test_router(), "GET", "/api/ctrl/sessions", None).await;
        assert_eq!(status, StatusCode::OK);
        let arr = listed["sessions"].as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["id"], id);

        // 3. Get by id.
        let (status, got) = send_json(
            test_router(),
            "GET",
            &format!("/api/ctrl/sessions/{id}"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(got["id"], id);
        assert_eq!(got["name"], "feature-x");
        assert_eq!(got["status"], "idle");

        // 4. Kill.
        let (status, killed) = send_json(
            test_router(),
            "DELETE",
            &format!("/api/ctrl/sessions/{id}"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "delete: {killed}");
        assert_eq!(killed["status"], "terminated");

        // 5. Get after kill — record kept, status flipped.
        let (status, after) = send_json(
            test_router(),
            "GET",
            &format!("/api/ctrl/sessions/{id}"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(after["status"], "terminated");
    })
    .await
}

#[tokio::test]
async fn session_e2e_filter_by_project() {
    // Why: `?project=<path>` lets the CLI scope the listing — must not
    // leak sessions from sibling projects.
    with_sandboxed_home(|tmp| async move {
        let proj_a = tmp.path().join("proj-a");
        let proj_b = tmp.path().join("proj-b");
        std::fs::create_dir(&proj_a).unwrap();
        std::fs::create_dir(&proj_b).unwrap();

        for (path, name) in [(&proj_a, "a-sess"), (&proj_b, "b-sess")] {
            let (status, _) = send_json(
                test_router(),
                "POST",
                "/api/ctrl/sessions",
                Some(serde_json::json!({
                    "project_path": path.to_string_lossy(),
                    "name": name,
                    "agent": "pm",
                    "worktree": false,
                })),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
        }

        // Handler matches against canonicalized project_path.
        let canonical_a = proj_a.canonicalize().unwrap().to_string_lossy().to_string();
        // Tempdir paths on macOS/Linux contain only `/` and alphanumerics,
        // safe to embed in a query string without percent-encoding.
        let uri = format!("/api/ctrl/sessions?project={canonical_a}");
        let (status, listed) = send_json(test_router(), "GET", &uri, None).await;
        assert_eq!(status, StatusCode::OK);
        let arr = listed["sessions"].as_array().unwrap();
        assert_eq!(arr.len(), 1, "expected only proj-a, got {arr:?}");
        assert_eq!(arr[0]["name"], "a-sess");
    })
    .await
}

#[tokio::test]
async fn session_e2e_filter_by_status() {
    // Why: `?status=<idle|terminated>` powers `om session list --status`.
    with_sandboxed_home(|tmp| async move {
        let project_dir = tmp.path().join("status-proj");
        std::fs::create_dir(&project_dir).unwrap();

        let (_, created) = send_json(
            test_router(),
            "POST",
            "/api/ctrl/sessions",
            Some(serde_json::json!({
                "project_path": project_dir.to_string_lossy(),
                "name": "to-kill",
                "agent": "pm",
                "worktree": false,
            })),
        )
        .await;
        let id = created["id"].as_str().unwrap().to_string();

        // Before kill: idle filter has 1, terminated has 0.
        let (_, idle_list) =
            send_json(test_router(), "GET", "/api/ctrl/sessions?status=idle", None).await;
        assert_eq!(idle_list["sessions"].as_array().unwrap().len(), 1);
        let (_, term_list) = send_json(
            test_router(),
            "GET",
            "/api/ctrl/sessions?status=terminated",
            None,
        )
        .await;
        assert_eq!(term_list["sessions"].as_array().unwrap().len(), 0);

        // Kill it.
        let (status, _) = send_json(
            test_router(),
            "DELETE",
            &format!("/api/ctrl/sessions/{id}"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        // After kill: filters flip.
        let (_, idle_list) =
            send_json(test_router(), "GET", "/api/ctrl/sessions?status=idle", None).await;
        assert_eq!(
            idle_list["sessions"].as_array().unwrap().len(),
            0,
            "no idle sessions after kill"
        );
        let (_, term_list) = send_json(
            test_router(),
            "GET",
            "/api/ctrl/sessions?status=terminated",
            None,
        )
        .await;
        let arr = term_list["sessions"].as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["id"], id);
    })
    .await
}

#[tokio::test]
async fn session_e2e_store_persists_across_instances() {
    // Why: SessionStore is a stateless type that re-reads the JSON file on
    // every call. This test confirms a session written by one logical
    // "instance" (load + upsert + save) is observable by a subsequent
    // `load` that simulates a fresh process.
    with_sandboxed_home(|_tmp| async move {
        let session = CtrlSession::new(
            std::path::PathBuf::from("/tmp/persisted-proj"),
            "persisted".to_string(),
            "pm".to_string(),
            9999,
        );
        let id = session.id;
        SessionStore::upsert(session).unwrap();

        // Simulated fresh process: drop nothing, just call `load` again.
        let reloaded = SessionStore::load();
        assert_eq!(reloaded.len(), 1);
        assert_eq!(reloaded[0].id, id);
        assert_eq!(reloaded[0].name, "persisted");
        assert_eq!(reloaded[0].port, 9999);

        let by_id = SessionStore::find(&id).expect("findable after reload");
        assert_eq!(by_id.id, id);
        assert!(matches!(by_id.status, CtrlSessionStatus::Idle));
    })
    .await
}

#[tokio::test]
async fn session_e2e_kill_unknown_id_returns_404() {
    // Why: DELETE on a nonexistent id must 404 (not 200) so the CLI can
    // distinguish "killed" from "no such session" without parsing bodies.
    with_sandboxed_home(|_tmp| async move {
        // Use a well-formed UUID that doesn't exist in the store.
        let bogus = "00000000-0000-0000-0000-000000000000";
        let (status, _) = send_json(
            test_router(),
            "DELETE",
            &format!("/api/ctrl/sessions/{bogus}"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        // GET on the same id also 404s.
        let (status, _) = send_json(
            test_router(),
            "GET",
            &format!("/api/ctrl/sessions/{bogus}"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    })
    .await
}
