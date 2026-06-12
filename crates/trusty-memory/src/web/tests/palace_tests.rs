//! Tests for status, palace routing, drawer create, and memories alias.

use super::super::router;
use super::test_state;
use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use serde_json::{json, Value};
use tower::util::ServiceExt;
use trusty_common::memory_core::palace::{Palace, PalaceId};

#[tokio::test]
async fn status_endpoint_returns_payload() {
    let state = test_state();
    let app = router().with_state(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/status")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = to_bytes(resp.into_body(), 1024).await.unwrap();
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    assert!(v["version"].is_string());
    assert_eq!(v["palace_count"], 0);
}

#[tokio::test]
async fn unknown_api_returns_404() {
    let state = test_state();
    let app = router().with_state(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/does-not-exist")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

/// Issue #70 — `…/memories` is a working alias for `…/drawers`.
///
/// Why: Clients that POST/GET against `…/memories` previously hit a 404
/// because only `/drawers` was registered, which silently broke every
/// store call (and pushed callers onto an OOM-prone CLI fallback). The
/// alias must route to the same handler as `/drawers`.
/// What: Creates a real palace via the registry, then GETs the `/memories`
/// alias and asserts a 200 with a JSON array body (the list-drawers shape).
/// Uses GET, not POST, so the test stays embedder-free (no ONNX load).
/// Test: this test.
#[tokio::test]
async fn memories_alias_routes_to_drawers() {
    let state = test_state();
    let palace = Palace {
        id: PalaceId::new("alias-test"),
        name: "alias-test".to_string(),
        description: None,
        created_at: chrono::Utc::now(),
        data_dir: state.data_root.join("alias-test"),
    };
    state
        .registry
        .create_palace(&state.data_root, palace)
        .expect("create_palace");

    let app = router().with_state(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/palaces/alias-test/memories")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "the /memories alias must resolve to list_drawers, not 404"
    );
    let bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    assert!(
        v.is_array(),
        "the alias must return the list-drawers array shape, got {v:?}"
    );
}

/// Issue #133 — `POST /api/v1/palaces/{id}/drawers` must trigger the
/// same auto-KG extraction as the MCP `memory_remember` tool.
///
/// Why: PR #106 wired auto-extract only into the MCP path; HTTP-origin
/// writes silently skipped it, leaving every palace populated via the
/// HTTP API with an empty KG. This regression test posts a drawer over
/// HTTP and then queries the KG to confirm the expected `tag:`,
/// `room:`, and `topic:` (`#hashtag`) auto-extracted triples landed.
/// What: creates a palace via the registry, posts a drawer with tags +
/// room + a `#hashtag` over the HTTP endpoint, reads
/// `/api/v1/palaces/{id}/kg/graph`, and asserts the auto-extracted
/// triples (provenance = `auto:remember`) appear.
/// Test: this test.
#[tokio::test]
async fn http_create_drawer_runs_auto_kg_extraction() {
    let state = test_state();
    let palace = Palace {
        id: PalaceId::new("kgauto-http"),
        name: "kgauto-http".to_string(),
        description: None,
        created_at: chrono::Utc::now(),
        data_dir: state.data_root.join("kgauto-http"),
    };
    state
        .registry
        .create_palace(&state.data_root, palace)
        .expect("create_palace");

    let app = router().with_state(state.clone());
    // Why: tag "test" is in the KG extraction deny-list (issue #278), so we
    // use "backend" and "kg" tags to exercise the auto-extraction path
    // without triggering the deny-list skip.
    let body = json!({
        "content": "trusty-memory is a Rust crate that ships an MCP server. \
                    It tracks #mcp and #rust topics with care.",
        "room": "Backend",
        "tags": ["backend", "kg"],
        "importance": 0.5,
    })
    .to_string();
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/palaces/kgauto-http/drawers")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "create_drawer must return 200 OK"
    );

    // Read the KG graph for the same palace and assert auto-extracted
    // triples landed. The exact set is exercised in
    // `tools::tests::auto_kg_extraction_hooks_into_memory_remember`; here
    // we only need to confirm the HTTP path now mirrors the MCP path.
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/palaces/kgauto-http/kg/graph")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    let triples = v["triples"].as_array().expect("triples array");
    assert!(
        !triples.is_empty(),
        "HTTP-origin drawer must populate the KG; got empty graph"
    );
    let auto: Vec<&Value> = triples
        .iter()
        .filter(|t| t["provenance"].as_str() == Some(crate::kg_extract::AUTO_PROVENANCE))
        .collect();
    assert!(
        !auto.is_empty(),
        "expected at least one auto-extracted triple in HTTP-populated KG; got: {triples:?}"
    );
    // Spot-check the tag-as-subject encoding survived (matches the MCP
    // path's behaviour and proves the extractor saw the body's tags).
    // Note: "test" is in the deny-list, so we use "backend" in the drawer
    // tags above (issue #278); assert on that tag instead.
    assert!(
        auto.iter()
            .any(|t| t["subject"].as_str() == Some("tag:backend")),
        "expected `tag:backend` auto-extracted edge, got: {auto:?}"
    );
    // Hashtag mention triples (room-aware extractor).
    assert!(
        auto.iter()
            .any(|t| t["predicate"].as_str() == Some("mentioned-in")),
        "expected at least one #hashtag mention triple, got: {auto:?}"
    );
}

#[tokio::test]
async fn create_then_list_palace() {
    let state = test_state();
    let app = router().with_state(state.clone());
    let body = json!({"name": "web-test", "description": "from test"}).to_string();
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/palaces")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/palaces")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    let arr = v.as_array().expect("array");
    assert!(arr.iter().any(|p| p["id"] == "web-test"));
}

/// Why: Issue #180 — verify the happy path: create an empty palace,
/// `DELETE /api/v1/palaces/{id}` returns 204, and a follow-up
/// `GET /api/v1/palaces/{id}` returns 404 because the directory is gone.
/// What: Drives the router through axum's `oneshot` testing layer; no
/// query parameters are passed so `force` defaults to `false`. A freshly
/// created palace has no drawers, so the conflict guard does not fire.
/// Test: This test itself.
#[tokio::test]
async fn delete_palace_removes_dir_when_empty() {
    let state = test_state();
    let app = router().with_state(state.clone());
    let body = json!({"name": "to-delete"}).to_string();
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/palaces")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/v1/palaces/to-delete")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    // Confirm the palace is gone from the on-disk registry.
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/palaces/to-delete")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    // And the on-disk directory itself was removed.
    let palace_dir = state.data_root.join("to-delete");
    assert!(
        !palace_dir.exists(),
        "palace dir should be removed: {}",
        palace_dir.display()
    );
}

/// Why: Issue #180 — without `force=true` we must refuse to drop a
/// palace that still has drawers, otherwise a stray DELETE could nuke
/// hours of memory in one request.
/// What: Create a palace, write a drawer into it, then DELETE without
/// `force`. Expect 409 Conflict and verify the palace and drawer are
/// still on disk.
/// Test: This test itself.
#[tokio::test]
async fn delete_palace_refuses_when_drawers_present() {
    let state = test_state();
    let app = router().with_state(state.clone());
    // Create the palace.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/palaces")
                .header("content-type", "application/json")
                .body(Body::from(json!({"name": "keep-me"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    // Add a drawer so the conflict guard fires.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/palaces/keep-me/drawers")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "content": "Important fact that should not be deleted accidentally.",
                        "tags": [],
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/v1/palaces/keep-me")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);

    // Palace still resolves.
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/palaces/keep-me")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}
