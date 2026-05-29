// Why: These tests use `crate::test_env::HOME_LOCK` (a `std::sync::Mutex`)
// to serialize cross-module mutation of `$HOME` while the test body
// performs async I/O. Holding a sync mutex across `.await` would be a
// bug in production code, but here the lock is held intentionally for
// the full test body so two tests don't race on the global env var. The
// tokio multi-threaded test runtime keeps the lock from causing deadlock.
#![allow(clippy::await_holding_lock)]

mod ctrl_sessions;
mod listing;

use super::routes::{build_router, build_router_with_config};
use super::state::{AppState, MAX_RETAINED};
use crate::api::types::{PmResponse, PmStatus};
use crate::registry::ProjectRegistry;
use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use tower::ServiceExt; // for `oneshot`

fn test_router() -> Router {
    build_router(AppState::default())
}

#[tokio::test]
async fn health_returns_ok_and_version() {
    let app = test_router();
    let req = Request::builder()
        .uri("/api/health")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body_bytes = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(body["status"], "ok");
    assert!(body["version"].is_string());
}

#[tokio::test]
async fn unknown_task_id_returns_404() {
    let app = test_router();
    let req = Request::builder()
        .uri("/api/task/nope-does-not-exist")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn list_tasks_empty_store_returns_empty_array() {
    let app = test_router();
    let req = Request::builder()
        .uri("/api/tasks")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body_bytes = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
    assert!(v.is_array());
    assert_eq!(v.as_array().unwrap().len(), 0);
}

// -- #450: TM session management routes --

/// Why: All `/api/tm/*` routes must return 503 with `{"error":"tmux not
/// available"}` when `AppState::tm_manager` is `None`, so the WebUI can
/// degrade gracefully on hosts without tmux.
/// What: For each route, build a default `AppState` (no manager), oneshot
/// the request, assert 503 and the canonical error JSON.
/// Test: This very function.
async fn assert_tm_503(method: Method, path: &str, body: Body) {
    let app = build_router(AppState::default());
    let req = Request::builder()
        .method(method)
        .uri(path)
        .header(header::CONTENT_TYPE, "application/json")
        .body(body)
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "expected 503 for {path}"
    );
    let bytes = axum::body::to_bytes(resp.into_body(), 256).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["error"], "tmux not available");
}

#[tokio::test]
async fn tm_list_sessions_returns_503_without_manager() {
    assert_tm_503(Method::GET, "/api/tm/sessions", Body::empty()).await;
}

#[tokio::test]
async fn tm_create_session_returns_503_without_manager() {
    let body = Body::from(r#"{"project_path":"/tmp"}"#);
    assert_tm_503(Method::POST, "/api/tm/sessions", body).await;
}

#[tokio::test]
async fn tm_kill_session_returns_503_without_manager() {
    assert_tm_503(Method::DELETE, "/api/tm/sessions/foo", Body::empty()).await;
}

#[tokio::test]
async fn tm_pause_session_returns_503_without_manager() {
    assert_tm_503(Method::POST, "/api/tm/sessions/foo/pause", Body::empty()).await;
}

#[tokio::test]
async fn tm_resume_session_returns_503_without_manager() {
    assert_tm_503(Method::POST, "/api/tm/sessions/foo/resume", Body::empty()).await;
}

#[tokio::test]
async fn tm_send_message_returns_503_without_manager() {
    let body = Body::from(r#"{"message":"hi"}"#);
    assert_tm_503(Method::POST, "/api/tm/sessions/foo/send", body).await;
}

#[tokio::test]
async fn tm_capture_pane_returns_503_without_manager() {
    assert_tm_503(Method::GET, "/api/tm/sessions/foo/pane", Body::empty()).await;
}

#[tokio::test]
async fn tm_set_favorite_returns_503_without_manager() {
    assert_tm_503(Method::POST, "/api/tm/sessions/foo/favorite", Body::empty()).await;
}

#[tokio::test]
async fn tm_unset_favorite_returns_503_without_manager() {
    assert_tm_503(
        Method::DELETE,
        "/api/tm/sessions/foo/favorite",
        Body::empty(),
    )
    .await;
}

#[tokio::test]
async fn tm_tell_returns_503_without_manager() {
    let body = Body::from(r#"{"project":"open-mpm","message":"hi"}"#);
    assert_tm_503(Method::POST, "/api/tm/tell", body).await;
}

#[tokio::test]
async fn app_state_trims_to_max_retained() {
    let state = AppState::default();
    // Push MAX_RETAINED + 5.
    for i in 0..(MAX_RETAINED + 5) {
        let id = format!("id-{i}");
        state.upsert(id.clone(), PmResponse::running(&id)).await;
    }
    let list = state.list().await;
    assert_eq!(list.len(), MAX_RETAINED);
    // Newest should be at the front.
    assert_eq!(list[0].id, format!("id-{}", MAX_RETAINED + 4));
}

// -- #181: auth middleware --

fn auth_router(token: &str) -> Router {
    build_router_with_config(AppState::default(), Some(token.to_string()))
}

#[tokio::test]
async fn auth_middleware_rejects_missing_token() {
    let app = auth_router("secret");
    let req = Request::builder()
        .uri("/api/tasks")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let body = axum::body::to_bytes(resp.into_body(), 256).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["error"], "unauthorized");
}

#[tokio::test]
async fn auth_middleware_rejects_wrong_token() {
    let app = auth_router("secret");
    let req = Request::builder()
        .uri("/api/tasks")
        .header(header::AUTHORIZATION, "Bearer wrong")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn auth_middleware_accepts_valid_token() {
    let app = auth_router("secret");
    let req = Request::builder()
        .uri("/api/tasks")
        .header(header::AUTHORIZATION, "Bearer secret")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn auth_middleware_allows_health_without_token() {
    let app = auth_router("secret");
    let req = Request::builder()
        .uri("/api/health")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn config_endpoint_reports_auth_required_true() {
    let app = auth_router("secret");
    let req = Request::builder()
        .uri("/api/config")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), 256).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["auth_required"], true);
}

#[tokio::test]
async fn config_endpoint_reports_auth_required_false() {
    let app = build_router(AppState::default());
    let req = Request::builder()
        .uri("/api/config")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), 256).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["auth_required"], false);
}

#[tokio::test]
async fn list_projects_returns_array() {
    // Why: The handler must always return a JSON array (possibly empty)
    // so the WebUI can render without special-casing 500s.
    let app = test_router();
    let req = Request::builder()
        .uri("/api/projects")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), 8 * 1024 * 1024)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(v.is_array(), "expected array, got {v}");
}

#[tokio::test]
async fn connect_project_persists_to_registry() {
    // Why: #405 — POST /api/projects must write to the same
    // ~/.open-mpm/projects.json that GET /api/projects reads from,
    // otherwise `om connect <path>` silently no-ops.
    // What: Sandbox $HOME to a tempdir, POST a project path, then
    // load the registry directly and assert the entry is present.
    // Test guards against the regression where connect_project only
    // returned metadata without persisting.
    let _g = crate::test_env::HOME_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let prev_home = std::env::var_os("HOME");
    // Use the tempdir itself as both HOME and the project path — the
    // path must exist for connect_project to succeed.
    // SAFETY: HOME_LOCK held for entire test body; restored before drop.
    unsafe {
        std::env::set_var("HOME", tmp.path());
    }

    let project_dir = tmp.path().join("myproject");
    std::fs::create_dir(&project_dir).unwrap();

    let app = test_router();
    let body = serde_json::json!({
        "path": project_dir.to_string_lossy(),
        "name": "myproject",
    });
    let req = Request::builder()
        .method("POST")
        .uri("/api/projects")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "POST should succeed");

    // Read the registry directly (same source GET reads from).
    let registry = ProjectRegistry::new().unwrap();
    let entries = registry.load().await.unwrap();
    let canonical = project_dir
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .to_string();
    assert!(
        entries.contains_key(&canonical),
        "registry should contain {canonical} after POST; got keys {:?}",
        entries.keys().collect::<Vec<_>>()
    );

    // Restore HOME so other tests are unaffected.
    // SAFETY: HOME_LOCK still held.
    unsafe {
        match prev_home {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
    }
}

#[tokio::test]
async fn get_project_config_falls_back_to_registry_when_toml_missing() {
    // Why: #465 — projects added via `POST /api/projects` without an
    // `adapter` only land in `~/.open-mpm/projects.json`. Before this fix,
    // `GET /api/projects/:name` returned 404 because it only inspected
    // `.open-mpm/projects/<name>.toml`, so projects registered through
    // the no-adapter path vanished from the detail endpoint after a
    // restart even though they were visible in the list endpoint.
    // What: Sandbox HOME, register a project via `ProjectRegistry`
    // directly (mirroring what `POST /api/projects` does), then
    // `GET /api/projects/<name>` and assert the synthesized config is
    // returned. Uses a project name that is guaranteed not to collide
    // with any file in the repo's `.open-mpm/projects/` directory.
    // Test: Asserts 200 + JSON with matching name/path.
    let _g = crate::test_env::HOME_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let prev_home = std::env::var_os("HOME");
    // SAFETY: HOME_LOCK held for the entire test body.
    unsafe {
        std::env::set_var("HOME", tmp.path());
    }

    let project_name = "restart-persistence-fallback-test-465";
    let project_dir = tmp.path().join(project_name);
    std::fs::create_dir(&project_dir).unwrap();

    let registry = ProjectRegistry::new().unwrap();
    registry.register_pm_start(&project_dir).await.unwrap();

    let app = test_router();
    let req = Request::builder()
        .uri(format!("/api/projects/{}", project_name))
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "registry-only project should resolve via fallback"
    );
    let body = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["project"]["name"], project_name);
    // Path may be canonicalized by `register_pm_start` (e.g. `/private/`
    // prefix on macOS); assert it ends with the project basename rather
    // than over-fitting to the canonical form.
    let returned_path = v["project"]["path"].as_str().unwrap_or("");
    assert!(
        returned_path.ends_with(project_name),
        "expected path to end with {project_name}, got {returned_path}"
    );

    // SAFETY: HOME_LOCK still held.
    unsafe {
        match prev_home {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
    }
}

#[tokio::test]
async fn get_project_config_returns_404_when_neither_source_has_entry() {
    // Why: #465 — fallback to the registry must not turn unrelated 404s
    // into bogus 200s. When neither the TOML store nor the registry
    // knows about `:name`, the endpoint must still return 404.
    let _g = crate::test_env::HOME_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let prev_home = std::env::var_os("HOME");
    // SAFETY: HOME_LOCK held.
    unsafe {
        std::env::set_var("HOME", tmp.path());
    }

    let app = test_router();
    let req = Request::builder()
        .uri("/api/projects/this-name-does-not-exist-anywhere-465")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    // SAFETY: HOME_LOCK still held.
    unsafe {
        match prev_home {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
    }
}

#[tokio::test]
async fn list_projects_accepts_all_query_param() {
    // Why: `?all=true` must not error and must still return an array.
    let app = test_router();
    let req = Request::builder()
        .uri("/api/projects?all=true")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), 8 * 1024 * 1024)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(v.is_array());
}

#[tokio::test]
async fn app_state_get_returns_stored() {
    let state = AppState::default();
    state.upsert("abc".into(), PmResponse::running("abc")).await;
    let r = state.get("abc").await.unwrap();
    assert_eq!(r.id, "abc");
    assert_eq!(r.status, PmStatus::Running);
}
