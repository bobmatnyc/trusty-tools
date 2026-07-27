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

/// The live cto-assistant shape (HIGH, code-critic PR #4119): one curated
/// `[[stores]]` OKG binding PLUS a tier-2 `[tools].search_indexes` attachment
/// pointing at a large, genuinely queryable corpus. Before this fix the
/// route silently omitted `search_indexes` entirely — the majority of this
/// agent's real retrievable knowledge would never have reached the pane.
const CTO_ASSISTANT_SHAPE_FIXTURE: &str = r#"[agent]
name = "cto-assistant"
role = "assistant"
model = "claude-sonnet-4-6"
description = "test"

[[stores]]
name = "cto-assistant"
index = "cto-assistant"

[tools]
allow = ["vector_search"]
search_indexes = ["cto-duetto"]
enforce_search_indexes = true
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

/// HIGH regression (code-critic, PR #4119): the cto-assistant shape's
/// `[tools].search_indexes` attachment must appear in the response — before
/// this fix it was parsed and silently dropped.
#[tokio::test]
async fn knowledge_route_surfaces_attached_search_indexes() {
    with_sandboxed_home(|_home| async move {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("cto-assistant.toml"),
            CTO_ASSISTANT_SHAPE_FIXTURE,
        )
        .unwrap();
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

        // The curated OKG binding is still reported under `stores`.
        let stores = body["stores"].as_array().unwrap();
        assert_eq!(stores.len(), 1);
        assert_eq!(stores[0]["index"], "cto-assistant");

        // The tier-2 attachment must be visible too — same field names
        // `/stores` already uses, so the two routes can never disagree.
        let search_indexes = body["search_indexes"]
            .as_array()
            .expect("search_indexes must be present");
        assert_eq!(
            search_indexes,
            &[serde_json::Value::String("cto-duetto".to_string())],
            "the attached corpus must not be silently omitted: {body:?}"
        );
        assert_eq!(body["enforce_search_indexes"], true);
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
        // MEDIUM (code-critic, PR #4119): the bare "plain" agent declares no
        // capability at all, so it must never be reported as `granted_for_agent`
        // even for an enabled connection — this one happens to be disabled too,
        // so the field should read `false` regardless of grant state.
        assert_eq!(memory["granted_for_agent"], false);
    })
    .await;
}

/// CRITICAL regression (code-critic, PR #4119 / issue #4115): a store whose
/// corpus failed to open (HTTP 2xx, `status: "ready"`, `chunk_count: 0`, but
/// every staged-pipeline lane `Failed`) must render `connected: false` — the
/// exact `cto-duetto` false-green this route inherited from `/stores` before
/// `stores::status::resolve_store_statuses` was fixed at the source.
#[tokio::test]
async fn knowledge_route_reports_corpus_open_failed_as_not_connected() {
    with_sandboxed_home(|_home| async move {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("cto-assistant.toml"),
            "[agent]\nname = \"cto-assistant\"\nrole = \"assistant\"\nmodel = \"claude-sonnet-4-6\"\ndescription = \"test\"\n\n[[stores]]\nname = \"cto-duetto-kb\"\nindex = \"cto-duetto\"\n",
        )
        .unwrap();

        let app = axum::Router::new().route(
            "/indexes/{id}/status",
            get(|Path(id): Path<String>| async move {
                if id == "cto-duetto" {
                    (
                        AxumStatus::OK,
                        Json(serde_json::json!({
                            "index_id": "cto-duetto",
                            "chunk_count": 0,
                            "status": "ready",
                            "stages": {
                                "lexical": {"status": "failed"},
                                "semantic": {"status": "failed"},
                                "graph": {"status": "failed"},
                            },
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
        let base = format!("http://{addr}");

        let resp = knowledge_at(
            &[dir.path().to_path_buf()],
            "cto-assistant",
            dir.path(),
            Some(&base),
            None,
        )
        .await;
        let body = body_json(resp).await;
        let store = &body["stores"][0];
        assert_eq!(
            store["connected"], false,
            "a reachable-but-broken corpus must not report connected: {body:?}"
        );
        assert!(
            store["reason"]
                .as_str()
                .unwrap()
                .contains("corpus failed to open")
        );
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
