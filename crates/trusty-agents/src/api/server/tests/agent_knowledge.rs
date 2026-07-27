//! `GET /api/agents/:name/knowledge` handler tests (#3935, DOC-57 §4).
//!
//! Why: This route's entire value is honest degradation — a bound-but-dead
//! store, an ungranted knowledge tool, and a disabled MCP endpoint must all be
//! REPORTED, never hidden or fabricated as working. The tests exercise the
//! three sub-surfaces (K-a/K-b/K-c) independently, including their degraded
//! states, plus one full-router test proving the route is wired.
//! What: `knowledge_at` driven against a `tempfile::TempDir` (the
//! `agent_stores`/`agent_skills` pattern) with a mock trusty-search daemon for
//! K-a and a sandboxed `$HOME` for K-c (`GlobalConfig::load()` reads
//! `$HOME/.trusty-agents/config.toml`).
//! Test: This module IS the test.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::{Json, Router, extract::Path, http::StatusCode as AxumStatus, routing::get};
use tokio::net::TcpListener;
use tower::ServiceExt;

use crate::api::server::agent_knowledge::knowledge_at;
use crate::api::server::routes::build_router;
use crate::api::server::state::AppState;
use crate::test_env::HOME_LOCK;

/// A bound, connected store plus a granted knowledge tool.
const BOUND_FIXTURE: &str = r#"[agent]
name = "cto-assistant"
role = "assistant"
model = "claude-sonnet-4-6"
description = "test"

[[stores]]
name = "cto-assistant"
index = "cto-assistant"

[tools]
allow = ["vector_search", "memory_recall"]
"#;

/// No `[[stores]]`, no capability declared at all.
const BARE_FIXTURE: &str = r#"[agent]
name = "plain"
role = "assistant"
model = "claude-sonnet-4-6"
description = "test"
"#;

/// A store bound to a nonexistent index (C-03.1).
const DEAD_STORE_FIXTURE: &str = r#"[agent]
name = "ghosty"
role = "assistant"
model = "claude-sonnet-4-6"
description = "test"

[[stores]]
name = "ghost-kb"
"#;

async fn mock_search_daemon() -> String {
    let app = Router::new().route(
        "/indexes/{id}/status",
        get(|Path(id): Path<String>| async move {
            if id == "cto-assistant" {
                (
                    AxumStatus::OK,
                    Json(serde_json::json!({
                        "index_id": "cto-assistant",
                        "chunk_count": 200_090,
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

async fn body_json(resp: axum::response::Response) -> serde_json::Value {
    let bytes = axum::body::to_bytes(resp.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

/// Sandbox `$HOME` to an empty tempdir for the duration of the closure, held
/// under `HOME_LOCK` — mirrors `mcp::config::tests`' convention so
/// `GlobalConfig::load()` never reads (or races) the developer's real
/// `~/.trusty-agents/config.toml`.
async fn with_sandboxed_home<F, Fut, T>(f: F) -> T
where
    F: FnOnce(std::path::PathBuf) -> Fut,
    Fut: std::future::Future<Output = T>,
{
    let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home =
        std::env::temp_dir().join(format!("trusty-agents-knowledge-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&home).unwrap();
    // SAFETY: guarded by HOME_LOCK for the duration of this async block.
    unsafe { std::env::set_var("HOME", &home) };
    f(home).await
}

#[tokio::test]
async fn knowledge_route_reports_bound_store_and_granted_tool() {
    with_sandboxed_home(|_home| async move {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("cto-assistant.toml"), BOUND_FIXTURE).unwrap();
        let base = mock_search_daemon().await;

        let resp = knowledge_at(
            &[dir.path().to_path_buf()],
            "cto-assistant",
            dir.path(),
            Some(&base),
            None,
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;

        let stores = body["stores"].as_array().unwrap();
        assert_eq!(stores.len(), 1);
        assert_eq!(stores[0]["connected"], true);
        assert_eq!(stores[0]["chunk_count"], 200_090);

        let tools = body["tools"].as_array().unwrap();
        let vs = tools
            .iter()
            .find(|t| t["tool"] == "vector_search")
            .expect("vector_search must appear as a knowledge tool");
        assert_eq!(vs["available"], true);
        assert_eq!(vs["bound_store"], "cto-assistant");
        assert_eq!(vs["reason"], serde_json::Value::Null);

        let recall = tools
            .iter()
            .find(|t| t["tool"] == "memory_recall")
            .expect("memory_recall must appear as a knowledge tool");
        assert_eq!(recall["available"], true);
        assert_eq!(recall["bound_store"], serde_json::Value::Null);
    })
    .await;
}

#[tokio::test]
async fn knowledge_route_reports_ungranted_tool_and_empty_stores_for_bare_agent() {
    with_sandboxed_home(|_home| async move {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("plain.toml"), BARE_FIXTURE).unwrap();

        let resp = knowledge_at(&[dir.path().to_path_buf()], "plain", dir.path(), None, None).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert!(body["stores"].as_array().unwrap().is_empty());

        let tools = body["tools"].as_array().unwrap();
        assert!(!tools.is_empty(), "every catalog knowledge skill is listed");
        assert!(
            tools.iter().all(|t| t["available"] == false),
            "no capability declared ⇒ nothing granted: {tools:?}"
        );
        assert!(
            tools
                .iter()
                .all(|t| t["reason"].as_str().unwrap().contains("not granted"))
        );
    })
    .await;
}

#[tokio::test]
async fn knowledge_route_reports_dead_store_with_reason_not_500() {
    with_sandboxed_home(|_home| async move {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("ghosty.toml"), DEAD_STORE_FIXTURE).unwrap();
        let base = mock_search_daemon().await;

        let resp = knowledge_at(
            &[dir.path().to_path_buf()],
            "ghosty",
            dir.path(),
            Some(&base),
            None,
        )
        .await;
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "a dead store is not an HTTP error"
        );
        let body = body_json(resp).await;
        let store = &body["stores"][0];
        assert_eq!(store["connected"], false);
        assert!(store["reason"].as_str().unwrap().contains("not registered"));
    })
    .await;
}

#[tokio::test]
async fn knowledge_route_reports_disabled_endpoint_never_as_connected() {
    with_sandboxed_home(|home| async move {
        let config_dir = home.join(".trusty-agents");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(
            config_dir.join("config.toml"),
            r#"
[tool_registry]
scope_enforcement = "deny"

[[tool_registry.endpoints]]
name = "trusty-memory"
driver = "direct"
command = "trusty-memory"
enabled = false
scopes = ["memory.read", "memory.write"]
"#,
        )
        .unwrap();

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("plain.toml"), BARE_FIXTURE).unwrap();

        let resp = knowledge_at(&[dir.path().to_path_buf()], "plain", dir.path(), None, None).await;
        let body = body_json(resp).await;
        let mcp = body["mcp"].as_array().unwrap();
        let memory = mcp
            .iter()
            .find(|m| m["name"] == "trusty-memory")
            .expect("disabled endpoint must still be reported (C-03.2)");
        assert_eq!(memory["enabled"], false);
        assert_eq!(
            memory["connected"], false,
            "never rendered as connected for a disabled endpoint"
        );
        assert!(memory["reason"].as_str().unwrap().contains("disabled"));
    })
    .await;
}

#[tokio::test]
async fn knowledge_route_unknown_agent_404() {
    with_sandboxed_home(|_home| async move {
        let dir = tempfile::tempdir().unwrap();
        let resp = knowledge_at(
            &[dir.path().to_path_buf()],
            "nobody",
            dir.path(),
            None,
            None,
        )
        .await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    })
    .await;
}

#[tokio::test]
async fn knowledge_route_rejects_traversal_name() {
    with_sandboxed_home(|_home| async move {
        let dir = tempfile::tempdir().unwrap();
        let resp = knowledge_at(
            &[dir.path().to_path_buf()],
            "../etc",
            dir.path(),
            None,
            None,
        )
        .await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    })
    .await;
}

#[tokio::test]
async fn knowledge_route_degrades_on_malformed_toml() {
    with_sandboxed_home(|_home| async move {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("broken.toml"), "not = = toml").unwrap();

        let resp = knowledge_at(
            &[dir.path().to_path_buf()],
            "broken",
            dir.path(),
            None,
            None,
        )
        .await;
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "a hand-edit typo must not 500 the panel"
        );
        let body = body_json(resp).await;
        assert!(body["stores"].as_array().unwrap().is_empty());
        assert!(body["config_error"].is_string());
    })
    .await;
}

/// Proves the route is wired into `build_router` (not just that the core
/// function works).
#[tokio::test]
async fn knowledge_route_is_wired_into_router() {
    with_sandboxed_home(|_home| async move {
        let app: Router = build_router(AppState::default());
        let req = Request::builder()
            .uri("/api/agents/definitely-not-an-agent-3935/knowledge")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let body = body_json(resp).await;
        assert_eq!(
            body["error"], "unknown agent",
            "a 404 from the handler, not from an unrouted path"
        );
    })
    .await;
}
