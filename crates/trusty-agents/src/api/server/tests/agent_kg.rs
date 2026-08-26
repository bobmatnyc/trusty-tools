//! `GET /api/agents/:name/kg*` proxy tests (#4290, #6286).
//!
//! Why: This module's entire contract is "pass trusty-memory's KG JSON
//! through, and NEVER fail the route for a condition the browser should render
//! as an empty state". Both halves need pinning: a pass-through that quietly
//! reshaped the upstream body, or a degraded path that 500'd, would each be
//! the bug this route was written to avoid. These tests drive `kg_proxy_at`
//! directly against a `tempfile::TempDir` (the `agent_stores.rs` pattern, so
//! they don't race sibling tests on cwd/`$HOME`) with a mock trusty-memory
//! daemon on a temp socket, plus full-router tests proving all four routes are
//! actually wired.
//!
//! The mock is [`crate::uds_mock`] rather than an axum server on a loopback
//! port: ADR-0032 moved trusty-memory onto a Unix socket, and a test that kept
//! stubbing HTTP would pass against a transport the route no longer speaks.
//!
//! What: pass-through for each of the four reads; forwarded params;
//! no-palace-bound → 200 empty state; daemon undiscoverable / unreachable /
//! palace absent / a projected field missing → 200 + `connected: false` +
//! reason; unknown agent → 404; traversal name → 400; missing `subject` → 400;
//! malformed TOML → 200 + `config_error`; and the read-only posture (no
//! POST/DELETE route).
//! Test: This module IS the test.

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use serde_json::{Value, json};
use tower::ServiceExt;

use crate::api::server::agent_kg::{KgRead, kg_proxy_at};
use crate::api::server::routes::build_router;
use crate::api::server::state::AppState;
use crate::uds_mock::{self, MockMemoryDaemon, RpcError};

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
    KgRead::new(
        "memory.kg_subjects_with_counts",
        "palace_id",
        Vec::new(),
        json!([]),
    )
}

fn count_read() -> KgRead {
    KgRead::new(
        "memory.kg_count",
        "palace_id",
        Vec::new(),
        json!({ "active": 0 }),
    )
}

/// Mock trusty-memory answering the four KG reads for palace `owner-profile`
/// only; every other palace is refused not-found, which is the daemon's real
/// behaviour for an unknown palace.
///
/// `memory.kg_all` and `kg_query` echo their params back inside the payload so
/// a test can prove what was forwarded. `kg_query` answers the TOOL's wider
/// `{subject, triples, …}` shape, because that is what the proxy projects.
async fn mock_memory() -> MockMemoryDaemon {
    uds_mock::spawn(|method: &str, params: Value| {
        let method = method.to_string();
        Box::pin(async move {
            // Both key spellings, because the folded reads take `palace_id` and
            // the `kg_query` tool takes `palace`.
            let palace = params
                .get("palace_id")
                .or_else(|| params.get("palace"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if palace == "unreadable" && method == "memory.kg_subjects_with_counts" {
                // An answer without the field a projecting read wants; the
                // JSON-RPC framing makes an unparseable BODY impossible, so
                // this is the shape-disagreement degradation instead.
                return Ok(json!({ "unexpected": true }));
            }
            if palace != "owner-profile" {
                return Err(RpcError::new(
                    trusty_common::memory_rpc::CODE_NOT_FOUND,
                    format!("palace not found: {palace}"),
                ));
            }
            match method.as_str() {
                "memory.kg_subjects_with_counts" => Ok(json!([
                    { "subject": "Bob", "count": 3 },
                    { "subject": "trusty-search", "count": 1 },
                ])),
                "memory.kg_all" => Ok(json!([{
                    "subject": "Bob",
                    "predicate": "prefers",
                    "object": "Rust",
                    "echoed_limit": params.get("limit"),
                    "echoed_offset": params.get("offset"),
                }])),
                "memory.kg_count" => Ok(json!({ "active": 4 })),
                "kg_query" => Ok(json!({
                    "subject": params.get("subject"),
                    "kg_triple_count": 4,
                    "triples": [{
                        "subject": params.get("subject"),
                        "predicate": "is",
                        "object": "owner",
                    }],
                })),
                other => Err(RpcError::method_not_found(other, &[])),
            }
        })
    })
    .await
}

async fn body_json(resp: axum::response::Response) -> Value {
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
    let daemon = mock_memory().await;

    let resp = kg_proxy_at(
        &[dir.path().to_path_buf()],
        "izzie",
        Some(daemon.socket()),
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
    // The upstream array must arrive unchanged — same order, same field names,
    // no re-ranking or reshaping in this crate.
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
    let daemon = mock_memory().await;

    let resp = kg_proxy_at(
        &[dir.path().to_path_buf()],
        "izzie",
        Some(daemon.socket()),
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
    let daemon = mock_memory().await;

    let read = KgRead::new(
        "memory.kg_all",
        "palace_id",
        vec![("limit", json!(25)), ("offset", json!(50))],
        json!([]),
    );
    let resp = kg_proxy_at(
        &[dir.path().to_path_buf()],
        "izzie",
        Some(daemon.socket()),
        read,
    )
    .await;
    let body = body_json(resp).await;
    assert_eq!(body["connected"], true);
    assert_eq!(
        body["data"][0]["echoed_limit"], 25,
        "the page params must arrive as numbers, not the strings the query \
         string carried"
    );
    assert_eq!(body["data"][0]["echoed_offset"], 50);
}

/// Why (#6286): `/kg?subject=` is the one read whose method answers a wider
/// shape than this route's `data` contract — `kg_query` is a dispatcher tool
/// returning `{subject, triples, kg_triple_count}` where the retired route
/// returned a bare array. Passing the object through would change `data`'s TYPE
/// for one of the four reads, which is exactly what the envelope promises never
/// happens.
/// What: drives the projecting read and asserts `data` is the triples array,
/// with the subject forwarded intact.
/// Test: itself.
#[tokio::test]
async fn kg_query_route_projects_the_triples_array() {
    let dir = agent_dir("izzie", BOUND_FIXTURE);
    let daemon = mock_memory().await;

    let read = KgRead::new(
        "kg_query",
        "palace",
        vec![("subject", json!("Bob & Co"))],
        json!([]),
    )
    .projecting("triples");
    let resp = kg_proxy_at(
        &[dir.path().to_path_buf()],
        "izzie",
        Some(daemon.socket()),
        read,
    )
    .await;
    let body = body_json(resp).await;
    assert_eq!(body["connected"], true);
    assert!(
        body["data"].is_array(),
        "data must stay an array for this read: {}",
        body["data"]
    );
    assert_eq!(
        body["data"][0]["subject"], "Bob & Co",
        "the subject must arrive intact"
    );
    assert!(
        body["data"][0].get("kg_triple_count").is_none(),
        "only the triples array is lifted, not the whole envelope"
    );
}

// ---------------------------------------------------------------------------
// Empty state + degradation — none of these may be an HTTP error
// ---------------------------------------------------------------------------

#[tokio::test]
async fn kg_route_empty_state_when_no_palace_bound() {
    let dir = agent_dir("plain", NO_PALACE_FIXTURE);
    let daemon = mock_memory().await;

    let resp = kg_proxy_at(
        &[dir.path().to_path_buf()],
        "plain",
        Some(daemon.socket()),
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
    let dead = tempfile::tempdir().unwrap();

    let resp = kg_proxy_at(
        &[dir.path().to_path_buf()],
        "izzie",
        Some(&dead.path().join("absent.sock")),
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
    let daemon = mock_memory().await;

    let resp = kg_proxy_at(
        &[dir.path().to_path_buf()],
        "ghosty",
        Some(daemon.socket()),
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

/// Why (#6286): the framing makes an unparseable upstream BODY impossible — a
/// malformed frame is a transport error, not a 2xx carrying garbage — so the
/// old "unreadable body" degradation has no way to occur. What replaces it is a
/// daemon whose answer does not carry the field a projecting read lifts. Both
/// mean the same thing to the pane (the read did not produce data), and both
/// must be a reason rather than a silent empty array, because an empty array
/// says "this palace has no triples", which is a different claim.
/// What: asks a projecting read against a palace the mock answers without a
/// `triples` field.
/// Test: itself.
#[tokio::test]
async fn kg_route_degrades_when_the_answer_lacks_the_projected_field() {
    let dir = agent_dir(
        "unreadable",
        "[agent]\nname = \"unreadable\"\n\n[[stores]]\nname = \"g\"\npalace = \"unreadable\"\n",
    );
    let daemon = mock_memory().await;

    let read = KgRead::new(
        "memory.kg_subjects_with_counts",
        "palace_id",
        Vec::new(),
        json!([]),
    )
    .projecting("triples");
    let resp = kg_proxy_at(
        &[dir.path().to_path_buf()],
        "unreadable",
        Some(daemon.socket()),
        read,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["connected"], false);
    assert_eq!(body["data"], json!([]));
    assert!(
        body["reason"].as_str().unwrap().contains("triples"),
        "the reason must name the field that was missing: {}",
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
/// (`kg_assert`) and retract (`memory.kg_delete_triple`) are deliberately not
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
