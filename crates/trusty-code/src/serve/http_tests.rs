//! Tests for `serve::http`'s axum router assembly (`build_axum_router`) and
//! its routes (`POST /rpc`, `GET /health`, `GET /sessions/{id}/events`).
//!
//! Why: split out of `http.rs` per the crate's `_tests.rs` sibling-file
//! convention (see `events_tests`/`workstreams::model_tests` for precedent)
//! so the router's test surface — which grew with issue #3297's new
//! `SharedWorkstreamStore` parameter and its `GET /workstreams/{id}/events`
//! merge — doesn't push the production file past its 500-SLOC cap; test
//! files carry the 1500-SLOC cap.
//! What: `POST /rpc` ping/malformed-JSON, `GET /health`, every REST resource
//! group's merge-in smoke test, and the `GET /sessions/{id}/events` SSE
//! replay/live/404 behaviour.
//! Test: this module is itself the test surface.

use super::*;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tower::util::ServiceExt;
use trusty_common::mcp::error_codes;

async fn router_and_sessions() -> (Arc<Router>, Arc<SessionRegistry>, SharedWorkstreamStore) {
    let sessions = Arc::new(SessionRegistry::new());
    let workstreams = test_workstreams_store().await;
    let mut router = Router::new();
    crate::serve::methods::register(&mut router);
    crate::session::protocol::register(&mut router, sessions.clone(), workstreams.clone());
    (Arc::new(router), sessions, workstreams)
}

/// A fresh, tempdir-backed `SharedWorkstreamStore` with no workstreams
/// yet — every test in this file that doesn't specifically exercise
/// `workstream.*` just needs SOMETHING to pass to `build_axum_router`
/// (issue #3297 added the store as a required parameter so
/// `GET /workstreams/{id}/events` can be merged in).
async fn test_workstreams_store() -> SharedWorkstreamStore {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("workstreams-test.json");
    let store = crate::workstreams::WorkstreamStore::load(path)
        .await
        .expect("load");
    std::mem::forget(dir);
    Arc::new(tokio::sync::Mutex::new(store))
}

async fn body_json(response: axum::response::Response) -> Value {
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

/// `POST /rpc` with a `ping` request must return HTTP 200 and the same
/// `{"pong": true}` result the STDIO transport returns.
#[tokio::test]
async fn http_rpc_ping_returns_pong() {
    let (router, sessions, workstreams) = router_and_sessions().await;
    let app = build_axum_router(router, sessions, workstreams);
    let req_body = json!({"jsonrpc": "2.0", "id": 1, "method": "ping"}).to_string();

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/rpc")
                .header("content-type", "application/json")
                .body(Body::from(req_body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let v = body_json(resp).await;
    assert_eq!(v["result"], json!({"pong": true}));
    assert_eq!(v["id"], 1);
}

/// `POST /rpc` with a malformed body must still return HTTP 200 with a
/// JSON-RPC `-32700 Parse error` envelope, not a bare HTTP 400.
#[tokio::test]
async fn http_rpc_malformed_json_returns_parse_error() {
    let (router, sessions, workstreams) = router_and_sessions().await;
    let app = build_axum_router(router, sessions, workstreams);

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/rpc")
                .header("content-type", "application/json")
                .body(Body::from("not json at all"))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let v = body_json(resp).await;
    assert_eq!(v["error"]["code"], error_codes::PARSE_ERROR);
}

/// `GET /health` must return the exact payload `health_payload()`
/// (and thus the `health` JSON-RPC method) returns.
#[tokio::test]
async fn http_health_matches_jsonrpc_health_payload() {
    let (router, sessions, workstreams) = router_and_sessions().await;
    let app = build_axum_router(router, sessions, workstreams);

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let v = body_json(resp).await;
    assert_eq!(v, health_payload());
}

/// `GET /sessions` (the #2983 Slice 2 REST route group, merged in by
/// `build_axum_router`) must be reachable on the SAME router `POST /rpc`
/// and `GET /sessions/{id}/events` are — proving the merge in
/// `build_axum_router` actually wires `rest::sessions::routes` rather
/// than silently dropping it. Route-level behaviour (found/missing/
/// transcript/readiness/goals) is covered by `rest::sessions::tests`.
#[tokio::test]
async fn http_rest_sessions_route_is_merged_in() {
    let (router, sessions, workstreams) = router_and_sessions().await;
    let app = build_axum_router(router, sessions, workstreams);

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/sessions")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let v = body_json(resp).await;
    assert!(v["sessions"].is_array());
}

/// `POST /sessions` (the #2983 Slice 3 REST write route group, merged in
/// by `build_axum_router` via `rest::sessions_write::routes`) must be
/// reachable on the SAME router `GET /sessions` is (the previous test) —
/// proving axum's `.merge()` doesn't silently drop one side when two
/// sub-routers register the SAME literal path under different HTTP
/// methods. `GET`+`POST` both claim `/sessions`; `PUT`+`DELETE` both
/// claim `/sessions/{id}/goal` — this test pins both same-literal-path
/// pairs in one pass, driving the created session's id through a real
/// `PUT` then `DELETE` on `/goal` and asserting a clean `200` round trip
/// (seeding a `pm_transcript` first via the registry so `set_goal`/
/// `clear_goal`'s "has a transcript" precondition passes — otherwise a
/// merge bug that silently 404'd the route would be indistinguishable
/// from a legitimate `400 invalid_argument`). Per-route error/malformed-
/// body behaviour is already covered by `rest::sessions_write::tests`;
/// this test only pins the merge itself.
#[tokio::test]
async fn http_rest_post_sessions_route_is_merged_in() {
    let (router, sessions, workstreams) = router_and_sessions().await;
    let sessions_for_seeding = sessions.clone();
    let app = build_axum_router(router, sessions, workstreams);

    let create_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/sessions")
                .header("content-type", "application/json")
                .body(Body::from(json!({"task": "do it"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(create_resp.status(), StatusCode::CREATED);
    let created = body_json(create_resp).await;
    assert_eq!(created["status"], "running");
    assert_eq!(created["task"], "do it");
    let session_id = created["id"].as_str().unwrap().to_string();

    let transcript = sessions_for_seeding
        .begin_pm_transcript(&session_id, "you are the pm", "first task")
        .unwrap();
    sessions_for_seeding.store_pm_transcript(&session_id, transcript);

    let put_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/sessions/{session_id}/goal"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"slot": 1, "text": "ship it"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(put_resp.status(), StatusCode::OK);
    assert_eq!(body_json(put_resp).await, json!({}));

    let delete_resp = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/sessions/{session_id}/goal"))
                .header("content-type", "application/json")
                .body(Body::from(json!({"slot": 1}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(delete_resp.status(), StatusCode::OK);
    assert_eq!(body_json(delete_resp).await, json!({}));
}

/// `POST /tasks` (the #2983 Slice 4 REST route group, merged in by
/// `build_axum_router` via `rest::tasks::routes`) must be reachable on
/// the SAME router `POST /sessions` is — proving the merge actually
/// wires `rest::tasks::routes` rather than silently dropping it.
/// Per-route error/status-code behaviour is already covered by
/// `rest::tasks::tests`; this test only pins the merge itself.
#[tokio::test]
async fn http_rest_post_tasks_route_is_merged_in() {
    let _guard = crate::task::mock_llm::MOCK_LLM_ENV_LOCK.lock().await;
    // SAFETY: test-only env mutation; serialized by `MOCK_LLM_ENV_LOCK`.
    unsafe {
        std::env::set_var(
            crate::task::mock_llm::MOCK_LLM_ENV,
            crate::task::mock_llm::MOCK_LLM_ECHO,
        );
    }
    // `router_and_sessions()` does not register `task.run` (it is not
    // needed by any other test in this file) — build a router that also
    // has it wired, mirroring `task::protocol::tests::agents_dir`.
    let sessions = Arc::new(SessionRegistry::new());
    let agents = tempfile::tempdir().expect("agents tempdir");
    std::fs::write(
        agents.path().join("pm.md"),
        "---\nname: pm\nmodel: openai/gpt-4o-mini\n---\n\nYou are the PM.\n",
    )
    .expect("write pm.md");
    let project = tempfile::tempdir().expect("project tempdir");
    let workstreams = test_workstreams_store().await;
    let mut router = Router::new();
    crate::serve::methods::register(&mut router);
    crate::session::protocol::register(&mut router, sessions.clone(), workstreams.clone());
    crate::task::protocol::register(
        &mut router,
        sessions.clone(),
        crate::binding::ProjectBinding::resolve(Some(project.path().to_path_buf()))
            .expect("tempdir must bind"),
        agents.path().to_path_buf(),
        workstreams.clone(),
    );
    let app = build_axum_router(Arc::new(router), sessions, workstreams);

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/tasks")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"task_description": "say hi"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    unsafe {
        std::env::remove_var(crate::task::mock_llm::MOCK_LLM_ENV);
    }

    assert_eq!(resp.status(), StatusCode::ACCEPTED);
    let v = body_json(resp).await;
    assert_eq!(v["status"], "running");
}

/// `GET /fs` (the #2983 Slice 5 REST route group, merged in by
/// `build_axum_router` via `rest::fs::routes`) must be reachable on the
/// SAME router `POST /rpc` is — proving the merge actually wires
/// `rest::fs::routes` rather than silently dropping it. Per-route
/// error/status-code behaviour is already covered by `rest::fs::tests`;
/// this test only pins the merge itself.
#[tokio::test]
async fn http_rest_get_fs_route_is_merged_in() {
    // `router_and_sessions()` does not register `fs.list_dir` (it is not
    // needed by any other test in this file) — build a router that also
    // has it wired.
    let sessions = Arc::new(SessionRegistry::new());
    let mut router = Router::new();
    crate::serve::methods::register(&mut router);
    crate::session::protocol::register(
        &mut router,
        sessions.clone(),
        test_workstreams_store().await,
    );
    crate::fs_browse::protocol::register(&mut router);
    let app = build_axum_router(Arc::new(router), sessions, test_workstreams_store().await);
    let tmp = tempfile::tempdir().expect("tempdir");

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/fs?path={}", tmp.path().display()))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let v = body_json(resp).await;
    assert!(v["entries"].is_array());
}

/// `GET /projects` (issue #3365's REST route group, merged in by
/// `build_axum_router` via `rest::projects::routes`) must be reachable on
/// the SAME router `POST /rpc` is — proving the merge actually wires
/// `rest::projects::routes` rather than silently dropping it. Per-route
/// behaviour (the `entries` array shape) is already covered by
/// `rest::projects::tests`; this test only pins the merge itself.
#[tokio::test]
async fn http_rest_get_projects_route_is_merged_in() {
    // `router_and_sessions()` does not register `fs.list_projects` (it is
    // not needed by any other test in this file) — build a router that
    // also has it wired, mirroring `http_rest_get_fs_route_is_merged_in`.
    let sessions = Arc::new(SessionRegistry::new());
    let mut router = Router::new();
    crate::serve::methods::register(&mut router);
    crate::session::protocol::register(
        &mut router,
        sessions.clone(),
        test_workstreams_store().await,
    );
    crate::fs_browse::protocol::register(&mut router);
    let app = build_axum_router(Arc::new(router), sessions, test_workstreams_store().await);

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/projects")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let v = body_json(resp).await;
    assert!(v["entries"].is_array());
}

/// `GET /agents`/`GET /skills` (issue #3449's Foundry GUI catalog-management
/// REST groups, merged in by `build_axum_router` via
/// `rest::agent_catalog::routes`/`rest::skill_catalog::routes`) must be
/// reachable on the SAME router `POST /rpc` is — proving the merge actually
/// wires both groups rather than silently dropping them. Per-route behaviour
/// is already covered by `rest::agent_catalog::tests`/
/// `rest::skill_catalog::tests`; this test only pins the merge itself.
#[tokio::test]
async fn http_rest_get_agent_and_skill_catalog_routes_are_merged_in() {
    let sessions = Arc::new(SessionRegistry::new());
    let project = tempfile::tempdir().expect("project tempdir");
    let mut router = Router::new();
    crate::serve::methods::register(&mut router);
    crate::session::protocol::register(
        &mut router,
        sessions.clone(),
        test_workstreams_store().await,
    );
    crate::agents::protocol::register(
        &mut router,
        crate::agents::protocol::AgentsCatalogState::new(project.path().to_path_buf(), true),
    );
    crate::skills::protocol::register(
        &mut router,
        crate::skills::protocol::SkillsCatalogState::new(Some(project.path())),
    );
    let app = build_axum_router(Arc::new(router), sessions, test_workstreams_store().await);

    let agents_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/agents")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(agents_resp.status(), StatusCode::OK);
    assert!(body_json(agents_resp).await["agents"].is_array());

    let skills_resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/skills")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(skills_resp.status(), StatusCode::OK);
    assert!(body_json(skills_resp).await["skills"].is_array());
}

/// `GET /sessions/{id}/agents` (the #2983 Slice 6 REST route group,
/// merged in by `build_axum_router` via `rest::agents::routes`) must be
/// reachable on the SAME router `GET /sessions/{id}/events` is —
/// proving the merge actually wires `rest::agents::routes` rather than
/// silently dropping it. Per-route error/status-code behaviour is
/// already covered by `rest::agents::tests`; this test only pins the
/// merge itself.
#[tokio::test]
async fn http_rest_get_session_agents_route_is_merged_in() {
    let (router, sessions, workstreams) = router_and_sessions().await;
    let session = sessions.create("t".to_string(), None, crate::binding::ProjectBinding::None);
    let app = build_axum_router(router, sessions, workstreams);

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/sessions/{}/agents", session.id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let v = body_json(resp).await;
    assert!(v["agents"].is_array());
}

/// `GET /sessions/{id}/budget` (issue #3015, part of the `rest::sessions`
/// route group `build_axum_router` already merges via
/// `rest::sessions::routes`) must be reachable on the SAME router
/// `GET /sessions/{id}/events` is — proving the merge actually wires the
/// route rather than silently dropping it. Per-route error/status-code
/// behaviour is already covered by `rest::sessions::tests`; this test
/// only pins the merge itself.
#[tokio::test]
async fn http_rest_get_session_budget_route_is_merged_in() {
    let (router, sessions, workstreams) = router_and_sessions().await;
    let session = sessions.create("t".to_string(), None, crate::binding::ProjectBinding::None);
    let app = build_axum_router(router, sessions, workstreams);

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/sessions/{}/budget", session.id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let v = body_json(resp).await;
    assert_eq!(v["status"], "never_recorded");
}

/// `GET /sessions/{id}/search-audit` (issue #3072, `rest::search_audit`'s
/// route group `build_axum_router` merges above) must be reachable on
/// the SAME router `GET /sessions/{id}/events` is — proving the merge
/// actually wires `rest::search_audit::routes` rather than silently
/// dropping it. Per-route error/status-code behaviour is already covered
/// by `rest::search_audit::tests`; this test only pins the merge itself.
#[tokio::test]
async fn http_rest_get_session_search_audit_route_is_merged_in() {
    let (router, sessions, workstreams) = router_and_sessions().await;
    let session = sessions.create("t".to_string(), None, crate::binding::ProjectBinding::None);
    let app = build_axum_router(router, sessions, workstreams);

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/sessions/{}/search-audit", session.id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let v = body_json(resp).await;
    assert_eq!(v["search_audit"].as_array().unwrap().len(), 0);
}

/// `GET /workstreams` (issue #3295, `rest::workstreams`'s route group
/// `build_axum_router` merges above) must be reachable on the SAME
/// router `GET /sessions/{id}/events` is — proving the merge actually
/// wires `rest::workstreams::routes` rather than silently dropping it.
/// Also hits `GET /workstreams/{id}/events` (issue #3297,
/// `crate::workstreams::sse::routes`, merged in the SAME call) on the
/// SAME router — proving that merge too, without a second whole test
/// function. Per-route behaviour for both groups is already covered by
/// `rest::workstreams::tests`/`crate::workstreams::sse::sse_tests`; this
/// test only pins the merges themselves.
#[tokio::test]
async fn http_rest_get_workstreams_route_is_merged_in() {
    let sessions = Arc::new(SessionRegistry::new());
    let dir = tempfile::tempdir().expect("tempdir");
    let store = crate::workstreams::WorkstreamStore::load(dir.path().join("workstreams-test.json"))
        .await
        .expect("load");
    let workstreams = Arc::new(tokio::sync::Mutex::new(store));
    let mut router = Router::new();
    crate::serve::methods::register(&mut router);
    crate::session::protocol::register(&mut router, sessions.clone(), workstreams.clone());
    crate::workstreams::protocol::register(&mut router, workstreams.clone());
    let id = workstreams.lock().await.create("t").await.expect("create");
    let app = build_axum_router(Arc::new(router), sessions, workstreams);

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/workstreams")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v = body_json(resp).await;
    assert!(v["workstreams"].is_array());

    let events_resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/workstreams/{id}/events"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(events_resp.status(), StatusCode::OK);
}

/// `GET /sessions/{id}/events` on an unknown session must 404 with a
/// JSON-RPC `session_not_found` envelope.
#[tokio::test]
async fn http_session_events_sse_unknown_session_returns_404() {
    let (router, sessions, workstreams) = router_and_sessions().await;
    let app = build_axum_router(router, sessions, workstreams);

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/sessions/does-not-exist/events")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let v = body_json(resp).await;
    assert_eq!(v["error"]["code"], -32007);
}

/// `GET /sessions/{id}/events` on a real session must stream the
/// ring-buffer replay as SSE `data:` frames.
///
/// Why: the response body never terminates (the live half subscribes to
/// the bus forever), so this reads a bounded prefix of the body as a
/// `Stream` (rather than `axum::body::to_bytes`, which would hang or
/// error waiting for a completion that never comes) until the expected
/// content shows up or a timeout fires.
#[tokio::test]
async fn http_session_events_sse_streams_replay() {
    let (router, sessions, workstreams) = router_and_sessions().await;
    let session = sessions.create("t".to_string(), None, crate::binding::ProjectBinding::None);
    let app = build_axum_router(router, sessions, workstreams);

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/sessions/{}/events", session.id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let mut stream = resp.into_body().into_data_stream();
    let mut collected = Vec::new();
    let read_replay = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !String::from_utf8_lossy(&collected).contains("session_status_changed") {
            match stream.next().await {
                Some(Ok(chunk)) => collected.extend_from_slice(&chunk),
                _ => break,
            }
        }
    })
    .await;

    assert!(
        read_replay.is_ok(),
        "timed out waiting for the SSE replay burst"
    );
    let text = String::from_utf8(collected).unwrap();
    assert!(text.contains("session_started"), "body so far: {text}");
    assert!(
        text.contains("session_status_changed"),
        "body so far: {text}"
    );
    // #2055: the SSE frames must carry the full envelope, not a bare
    // event — `seq` and the top-level `kind` field must both appear.
    assert!(text.contains("\"seq\":1"), "body so far: {text}");
    assert!(
        text.contains("\"kind\":\"session_started\""),
        "body so far: {text}"
    );
}
