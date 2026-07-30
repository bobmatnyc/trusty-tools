//! `GET /api/agents/:name/kg*` proxy tests (#4290).
//!
//! Why: This module's entire contract is "pass trusty-memory's KG JSON
//! through, and NEVER fail the route for a condition the browser should render
//! as an empty state". Both halves need pinning: a pass-through that quietly
//! reshaped the upstream body, or a degraded path that 500'd, would each be
//! the bug this route was written to avoid. These tests drive `kg_proxy_at`
//! directly against a `tempfile::TempDir` (the `agent_stores.rs` pattern, so
//! they don't race sibling tests on cwd/`$HOME`) with a mock trusty-memory
//! daemon, plus full-router tests proving all four routes are actually wired.
//! What: pass-through for each of the four reads; forwarded query params;
//! no-palace-bound → 200 empty state; daemon undiscoverable / unreachable /
//! palace 404 / unreadable body → 200 + `connected: false` + reason; unknown
//! agent → 404; traversal name → 400; missing `subject` → 400; malformed TOML
//! → 200 + `config_error`; and the read-only posture (no POST/DELETE route).
//! Test: This module IS the test.

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use axum::{
    Json, Router,
    extract::{Path, Query},
    http::StatusCode as AxumStatus,
    routing::get,
};
use serde_json::json;
use std::collections::HashMap;
use tokio::net::TcpListener;
use tower::ServiceExt;

use crate::api::server::agent_kg::{KgRead, kg_proxy_at};
use crate::api::server::routes::build_router;
use crate::api::server::state::AppState;

/// An agent binding a store WITH a palace — the only shape that can reach
/// trusty-memory at all.
const BOUND_FIXTURE: &str = r#"[agent]
name = "izzie"
role = "assistant"
model = "claude-sonnet-4-6"
description = "test"

[[stores]]
name = "bob-kb"
index = "bob-kb"
palace = "owner-profile"
"#;

/// A store binding with NO palace — a document-only store. The ordinary
/// "nothing to browse yet" state, not an error.
const NO_PALACE_FIXTURE: &str = r#"[agent]
name = "plain"
role = "assistant"
model = "claude-sonnet-4-6"
description = "test"

[[stores]]
name = "docs-only"
"#;

/// A palace that the mock daemon does not know about.
const GHOST_PALACE_FIXTURE: &str = r#"[agent]
name = "ghosty"
role = "assistant"
model = "claude-sonnet-4-6"
description = "test"

[[stores]]
name = "ghost-kb"
palace = "no-such-palace"
"#;

fn subjects_read() -> KgRead {
    KgRead::new("kg/subjects_with_counts", Vec::new(), json!([]))
}

fn count_read() -> KgRead {
    KgRead::new("kg/count", Vec::new(), json!({ "active": 0 }))
}

/// Mock trusty-memory serving the four KG reads for palace `owner-profile`
/// only; every other palace 404s (the daemon's real behaviour for an unknown
/// palace). `/kg/all` and `/kg` echo the query string back inside the payload
/// so a test can prove the parameters were forwarded verbatim.
async fn mock_memory() -> String {
    async fn subjects(Path(id): Path<String>) -> (AxumStatus, Json<serde_json::Value>) {
        if id != "owner-profile" {
            return (AxumStatus::NOT_FOUND, Json(json!({})));
        }
        (
            AxumStatus::OK,
            Json(json!([
                { "subject": "Bob", "count": 3 },
                { "subject": "trusty-search", "count": 1 },
            ])),
        )
    }
    async fn all(
        Path(id): Path<String>,
        Query(q): Query<HashMap<String, String>>,
    ) -> (AxumStatus, Json<serde_json::Value>) {
        if id != "owner-profile" {
            return (AxumStatus::NOT_FOUND, Json(json!({})));
        }
        (
            AxumStatus::OK,
            Json(json!([{
                "subject": "Bob",
                "predicate": "prefers",
                "object": "Rust",
                "echoed_limit": q.get("limit"),
                "echoed_offset": q.get("offset"),
            }])),
        )
    }
    async fn query(
        Path(id): Path<String>,
        Query(q): Query<HashMap<String, String>>,
    ) -> (AxumStatus, Json<serde_json::Value>) {
        if id != "owner-profile" {
            return (AxumStatus::NOT_FOUND, Json(json!({})));
        }
        (
            AxumStatus::OK,
            Json(json!([{ "subject": q.get("subject"), "predicate": "is", "object": "owner" }])),
        )
    }
    async fn count(Path(id): Path<String>) -> (AxumStatus, Json<serde_json::Value>) {
        if id != "owner-profile" {
            return (AxumStatus::NOT_FOUND, Json(json!({})));
        }
        (AxumStatus::OK, Json(json!({ "active": 4 })))
    }
    /// A palace that answers 2xx with a body that is NOT JSON — the
    /// "unreadable upstream" degradation path.
    async fn garbage() -> (
        AxumStatus,
        [(axum::http::HeaderName, &'static str); 1],
        &'static str,
    ) {
        (
            AxumStatus::OK,
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            "{ not json",
        )
    }

    let app = Router::new()
        .route(
            "/api/v1/palaces/{id}/kg/subjects_with_counts",
            get(subjects),
        )
        .route("/api/v1/palaces/{id}/kg/all", get(all))
        .route("/api/v1/palaces/{id}/kg", get(query))
        .route("/api/v1/palaces/{id}/kg/count", get(count))
        .route(
            "/api/v1/palaces/garbage/kg/subjects_with_counts",
            get(garbage),
        );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

async fn body_json(resp: axum::response::Response) -> serde_json::Value {
    let bytes = axum::body::to_bytes(resp.into_body(), 256 * 1024)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

/// Write `fixture` as `<name>.toml` into a fresh tempdir and return it.
fn agent_dir(name: &str, fixture: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(format!("{name}.toml")), fixture).unwrap();
    dir
}

// ---------------------------------------------------------------------------
// Pass-through
// ---------------------------------------------------------------------------

#[tokio::test]
async fn kg_subjects_route_passes_upstream_through() {
    let dir = agent_dir("izzie", BOUND_FIXTURE);
    let base = mock_memory().await;

    let resp = kg_proxy_at(
        &[dir.path().to_path_buf()],
        "izzie",
        Some(&base),
        subjects_read(),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["palace"], "owner-profile");
    assert_eq!(body["connected"], true);
    assert!(
        body["reason"].is_null(),
        "a connected read carries no reason"
    );
    // The upstream array must arrive byte-for-byte — same order, same field
    // names, no re-ranking or reshaping in this crate.
    assert_eq!(
        body["data"],
        json!([
            { "subject": "Bob", "count": 3 },
            { "subject": "trusty-search", "count": 1 },
        ])
    );
}

#[tokio::test]
async fn kg_count_route_passes_upstream_object_through() {
    let dir = agent_dir("izzie", BOUND_FIXTURE);
    let base = mock_memory().await;

    let resp = kg_proxy_at(
        &[dir.path().to_path_buf()],
        "izzie",
        Some(&base),
        count_read(),
    )
    .await;
    let body = body_json(resp).await;
    assert_eq!(body["connected"], true);
    assert_eq!(body["data"], json!({ "active": 4 }));
}

#[tokio::test]
async fn kg_all_route_forwards_limit_and_offset() {
    let dir = agent_dir("izzie", BOUND_FIXTURE);
    let base = mock_memory().await;

    let read = KgRead::new(
        "kg/all",
        vec![("limit", "25".to_string()), ("offset", "50".to_string())],
        json!([]),
    );
    let resp = kg_proxy_at(&[dir.path().to_path_buf()], "izzie", Some(&base), read).await;
    let body = body_json(resp).await;
    assert_eq!(body["connected"], true);
    assert_eq!(body["data"][0]["echoed_limit"], "25");
    assert_eq!(body["data"][0]["echoed_offset"], "50");
}

#[tokio::test]
async fn kg_query_route_forwards_subject() {
    let dir = agent_dir("izzie", BOUND_FIXTURE);
    let base = mock_memory().await;

    let read = KgRead::new("kg", vec![("subject", "Bob & Co".to_string())], json!([]));
    let resp = kg_proxy_at(&[dir.path().to_path_buf()], "izzie", Some(&base), read).await;
    let body = body_json(resp).await;
    assert_eq!(
        body["data"][0]["subject"], "Bob & Co",
        "the subject must survive percent-encoding intact"
    );
}

// ---------------------------------------------------------------------------
// Empty state + degradation — none of these may be an HTTP error
// ---------------------------------------------------------------------------

#[tokio::test]
async fn kg_route_empty_state_when_no_palace_bound() {
    let dir = agent_dir("plain", NO_PALACE_FIXTURE);
    let base = mock_memory().await;

    let resp = kg_proxy_at(
        &[dir.path().to_path_buf()],
        "plain",
        Some(&base),
        subjects_read(),
    )
    .await;
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "an agent with no palace is an empty state, not an error"
    );
    let body = body_json(resp).await;
    assert!(body["palace"].is_null());
    assert_eq!(body["connected"], false);
    assert_eq!(body["data"], json!([]), "the empty shape is still an array");
    assert!(
        body["reason"]
            .as_str()
            .unwrap()
            .contains("no memory palace"),
        "reason was: {}",
        body["reason"]
    );
}

#[tokio::test]
async fn kg_count_route_empty_state_keeps_object_shape() {
    // The one read whose payload is an object: a client must never have to
    // branch on `data`'s TYPE, only on `connected`.
    let dir = agent_dir("plain", NO_PALACE_FIXTURE);
    let resp = kg_proxy_at(&[dir.path().to_path_buf()], "plain", None, count_read()).await;
    let body = body_json(resp).await;
    assert_eq!(body["connected"], false);
    assert_eq!(body["data"], json!({ "active": 0 }));
}

#[tokio::test]
async fn kg_route_degrades_when_memory_undiscoverable() {
    let dir = agent_dir("izzie", BOUND_FIXTURE);
    let resp = kg_proxy_at(&[dir.path().to_path_buf()], "izzie", None, subjects_read()).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(
        body["palace"], "owner-profile",
        "the claim is still reported"
    );
    assert_eq!(body["connected"], false);
    assert!(
        body["reason"]
            .as_str()
            .unwrap()
            .contains("not discoverable"),
        "reason was: {}",
        body["reason"]
    );
}

#[tokio::test]
async fn kg_route_degrades_when_memory_unreachable() {
    let dir = agent_dir("izzie", BOUND_FIXTURE);
    // A port nothing is listening on: bind, capture the address, drop it.
    let dead = {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        drop(l);
        format!("http://{addr}")
    };

    let resp = kg_proxy_at(
        &[dir.path().to_path_buf()],
        "izzie",
        Some(&dead),
        subjects_read(),
    )
    .await;
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "a down daemon must never 500 the browser"
    );
    let body = body_json(resp).await;
    assert_eq!(body["connected"], false);
    assert_eq!(body["data"], json!([]));
    assert!(
        body["reason"].as_str().unwrap().contains("unreachable"),
        "reason was: {}",
        body["reason"]
    );
}

#[tokio::test]
async fn kg_route_degrades_when_palace_missing() {
    let dir = agent_dir("ghosty", GHOST_PALACE_FIXTURE);
    let base = mock_memory().await;

    let resp = kg_proxy_at(
        &[dir.path().to_path_buf()],
        "ghosty",
        Some(&base),
        subjects_read(),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["palace"], "no-such-palace");
    assert_eq!(body["connected"], false);
    assert!(
        body["reason"].as_str().unwrap().contains("does not exist"),
        "reason was: {}",
        body["reason"]
    );
}

#[tokio::test]
async fn kg_route_degrades_on_unreadable_upstream_body() {
    let dir = agent_dir(
        "garbage",
        "[agent]\nname = \"garbage\"\n\n[[stores]]\nname = \"g\"\npalace = \"garbage\"\n",
    );
    let base = mock_memory().await;

    let resp = kg_proxy_at(
        &[dir.path().to_path_buf()],
        "garbage",
        Some(&base),
        subjects_read(),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["connected"], false);
    assert!(
        body["reason"].as_str().unwrap().contains("unreadable"),
        "reason was: {}",
        body["reason"]
    );
}

#[tokio::test]
async fn kg_route_degrades_on_malformed_toml() {
    let dir = agent_dir("broken", "not = = toml");
    let resp = kg_proxy_at(&[dir.path().to_path_buf()], "broken", None, subjects_read()).await;
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "a hand-edit typo must not 500 the pane"
    );
    let body = body_json(resp).await;
    assert!(body["palace"].is_null());
    assert_eq!(body["connected"], false);
    assert!(body["config_error"].is_string());
}

// ---------------------------------------------------------------------------
// Client-side faults that DO keep a non-200
// ---------------------------------------------------------------------------

#[tokio::test]
async fn kg_route_unknown_agent_404() {
    let dir = tempfile::tempdir().unwrap();
    let resp = kg_proxy_at(&[dir.path().to_path_buf()], "nobody", None, subjects_read()).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn kg_route_rejects_traversal_name() {
    let dir = tempfile::tempdir().unwrap();
    let resp = kg_proxy_at(&[dir.path().to_path_buf()], "../etc", None, subjects_read()).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn kg_query_route_requires_subject() {
    // A missing `subject` must be a loud 400, never a silent full-graph fetch.
    let app: Router = build_router(AppState::default());
    let req = Request::builder()
        .uri("/api/agents/definitely-not-an-agent-4290/kg")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = body_json(resp).await;
    assert!(
        body["error"].as_str().unwrap().contains("subject"),
        "error was: {}",
        body["error"]
    );
}

// ---------------------------------------------------------------------------
// Router wiring + read-only posture
// ---------------------------------------------------------------------------

/// Every one of the four routes is reachable through `build_router` — an
/// unknown agent must 404 FROM THE HANDLER (`{"error": "unknown agent"}`),
/// not from an unrouted path.
#[tokio::test]
async fn kg_routes_are_wired_into_router() {
    for path in [
        "/api/agents/definitely-not-an-agent-4290/kg?subject=x",
        "/api/agents/definitely-not-an-agent-4290/kg/subjects",
        "/api/agents/definitely-not-an-agent-4290/kg/all",
        "/api/agents/definitely-not-an-agent-4290/kg/count",
    ] {
        let app: Router = build_router(AppState::default());
        let req = Request::builder().uri(path).body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND, "for {path}");
        let body = body_json(resp).await;
        assert_eq!(body["error"], "unknown agent", "for {path}");
    }
}

/// Owner decision (#4290): the proxy is READ-ONLY. trusty-memory's assert
/// (`POST /kg`) and retract (`DELETE /kg/triples/{id}`) are deliberately not
/// exposed, so those verbs must not resolve to a handler here.
#[tokio::test]
async fn kg_write_verbs_are_not_proxied() {
    for (method, path) in [
        (Method::POST, "/api/agents/izzie/kg"),
        (Method::DELETE, "/api/agents/izzie/kg/triples/abc"),
    ] {
        let app: Router = build_router(AppState::default());
        let req = Request::builder()
            .method(method.clone())
            .uri(path)
            .header("content-type", "application/json")
            .body(Body::from("{}"))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert!(
            resp.status() == StatusCode::METHOD_NOT_ALLOWED
                || resp.status() == StatusCode::NOT_FOUND
                || resp.status() == StatusCode::FORBIDDEN,
            "{method} {path} resolved to a handler ({}) — the KG proxy must stay read-only",
            resp.status()
        );
    }
}
